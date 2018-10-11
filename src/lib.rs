//! Windows named pipes bindings for mio.
//!
//! This crate implements bindings for named pipes for the mio crate. This
//! crate compiles on all platforms but only contains anything on Windows.
//! Currently this crate requires mio 0.6.2.
//!
//! On Windows, mio is implemented with an IOCP object at the heart of its
//! `Poll` implementation. For named pipes, this means that all I/O is done in
//! an overlapped fashion and the named pipes themselves are registered with
//! mio's internal IOCP object. Essentially, this crate is using IOCP for
//! bindings with named pipes.
//!
//! Note, though, that IOCP is a *completion* based model whereas mio expects a
//! *readiness* based model. As a result this crate, like with TCP objects in
//! mio, has internal buffering to translate the completion model to a readiness
//! model. This means that this crate is not a zero-cost binding over named
//! pipes on Windows, but rather approximates the performance of mio's TCP
//! implementation on Windows.
//!
//! # Trait implementations
//!
//! The `Read` and `Write` traits are implemented for `UnixStream` and for
//! `&UnixStream`. This represents that a named pipe can be concurrently read and
//! written to and also can be read and written to at all. Typically a named
//! pipe needs to be connected to a client before it can be read or written,
//! however.
//!
//! Note that for I/O operations on a named pipe to succeed then the named pipe
//! needs to be associated with an event loop. Until this happens all I/O
//! operations will return a "would block" error.
//!
//! # Managing connections
//!
//! The `UnixStream` type supports a `connect` method to connect to a client and
//! a `disconnect` method to disconnect from that client. These two methods only
//! work once a named pipe is associated with an event loop.
//!
//! The `connect` method will succeed asynchronously and a completion can be
//! detected once the object receives a writable notification.
//!
//! # Named pipe clients
//!
//! Currently to create a client of a named pipe server then you can use the
//! `OpenOptions` type in the standard library to create a `File` that connects
//! to a named pipe. Afterwards you can use the `into_raw_handle` method coupled
//! with the `UnixStream::from_raw_handle` method to convert that to a named pipe
//! that can operate asynchronously. Don't forget to pass the
//! `FILE_FLAG_OVERLAPPED` flag when opening the `File`.

#![cfg(windows)]
#![deny(missing_docs)]

#[macro_use]
extern crate log;
extern crate mio;
extern crate miow;
extern crate winapi;

#[cfg(test)]
extern crate tempdir;

mod from_raw_arc;
mod inner;

use std::fmt;
use std::io::prelude::*;
use std::io;
use std::mem;
use std::net::Shutdown;
use std::os::windows::io::*;
use std::path::Path;
use std::slice;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::SeqCst;

use mio::windows;
use mio::{Registration, Poll, Token, PollOpt, Ready, Evented, SetReadiness};
use miow::iocp::CompletionStatus;
use winapi::shared::ntdef::HANDLE;
use winapi::um::ioapiset::*;
use winapi::um::minwinbase::*;

use from_raw_arc::FromRawArc;
use inner::{AcceptAddrsBuf, SocketAddr, UnixListenerExt, UnixStreamExt};

macro_rules! offset_of {
    ($t:ty, $($field:ident).+) => (
        &(*(0 as *const $t)).$($field).+ as *const _ as usize
    )
}

macro_rules! overlapped2arc {
    ($e:expr, $t:ty, $($field:ident).+) => ({
        let offset = offset_of!($t, $($field).+);
        debug_assert!(offset < mem::size_of::<$t>());
        FromRawArc::from_raw(($e as usize - offset) as *mut $t)
    })
}

fn would_block() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "would block")
}

/// Representation of a Unix domain socket on Windows.
///
/// This structure internally contains a `SOCKET` which represents the Unix
/// domain socket, and also maintains state associated with the mio event loop
/// and active I/O operations that have been scheduled to translate IOCP to a
/// readiness model.
pub struct UnixStream {
    registered: AtomicBool,
    ready_registration: Registration,
    poll_registration: windows::Binding,
    inner: FromRawArc<InnerStream>,
}

struct InnerStream {
    socket: inner::UnixStream,
    readiness: SetReadiness,

    connect: windows::Overlapped,
    connecting: AtomicBool,

    read: windows::Overlapped,
    write: windows::Overlapped,

    io: Mutex<StreamIo>,
}

struct StreamIo {
    read: State<(Vec<u8>, usize), (Vec<u8>, usize)>,
    write: State<(Vec<u8>, usize), (Vec<u8>, usize)>,
    deferred_connect: Option<SocketAddr>,
    connect_error: Option<io::Error>,
}

enum State<T, U> {
    None,           // no I/O operation in progress
    Pending(T),     // an I/O operation is in progress
    Ok(U),          // I/O has finished with this value
    Err(io::Error), // there was an I/O error
}

fn _assert_kinds() {
    fn _assert_send<T: Send>() {}
    fn _assert_sync<T: Sync>() {}
    _assert_send::<UnixStream>();
    _assert_sync::<UnixStream>();
}

impl UnixStream {
    fn new(socket: RawSocket, addr: Option<SocketAddr>) -> UnixStream {
        let (r, s) = Registration::new2();
        UnixStream {
            registered: AtomicBool::new(false),
            ready_registration: r,
            poll_registration: windows::Binding::new(),
            inner: FromRawArc::new(InnerStream {
                socket: unsafe { inner::UnixStream::from_raw_socket(socket) },
                readiness: s,
                connecting: AtomicBool::new(false),
                // transmutes to straddle winapi versions (mio 0.6 is on an
                // older winapi)
                connect: windows::Overlapped::new(unsafe { mem::transmute(connect_done as fn(_)) }),
                read: windows::Overlapped::new(unsafe { mem::transmute(read_done as fn(_)) }),
                write: windows::Overlapped::new(unsafe { mem::transmute(write_done as fn(_)) }),
                io: Mutex::new(StreamIo {
                    read: State::None,
                    write: State::None,
                    deferred_connect: addr,
                    connect_error: None,
                }),
            }),
        }
    }

    /// Connects to the socket named by `path`.
    ///
    /// The socket returned may not be readable and/or writable yet, as the
    /// connection may be in progress. The socket should be registered with an
    /// event loop to wait on both of these properties being available.
    pub fn connect<P: AsRef<Path>>(path: P) -> io::Result<UnixStream> {
        UnixStream::_connect(path.as_ref())
    }

    fn _connect(path: &Path) -> io::Result<UnixStream> {
        let sock = inner::UnixStream::new()?;
        sock.set_nonblocking(true)?;
        let addr = SocketAddr::from_path(path)?;
        let sock = UnixStream::new(sock.into_raw_socket(), Some(addr));

        // "Acquire the connecting lock" or otherwise just make sure we're the
        // only operation that's using the `connect` overlapped instance.
        let prev = sock.inner.connecting.swap(true, SeqCst);
        assert!(!prev);

        Ok(sock)
    }

    /// Takes any internal error that has happened after the last I/O operation
    /// which hasn't been retrieved yet.
    ///
    /// This is particularly useful when detecting failed attempts to `connect`.
    /// After a completed `connect` flags this pipe as writable then callers
    /// must invoke this method to determine whether the connection actually
    /// succeeded. If this function returns `None` then a client is connected,
    /// otherwise it returns an error of what happened and a client shouldn't be
    /// connected.
    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        Ok(self.inner.io.lock().unwrap().connect_error.take())
    }

    /// Creates a new independently owned handle to the underlying socket.
    pub fn try_clone(&self) -> io::Result<UnixStream> {
        self.inner.socket.try_clone().map(|s| unsafe {
            UnixStream::from_raw_socket(s.into_raw_socket())
        })
    }

    /// Returns the socket address of the local half of this connection.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.socket.local_addr()
    }

    /// Returns the socket address of the remote half of this connection.
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.inner.socket.peer_addr()
    }

    /// Shuts down the read, write, or both halves of this connection.
    ///
    /// This function will cause all pending and future I/O on the specified
    /// portions to return immediately with an appropriate value (see the
    /// documentation of `Shutdown`).
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.inner.socket.shutdown(how)?;
        self.inner.readiness.set_readiness(Ready::empty())
                 .expect("event loop seems gone");
        Ok(())
    }

    fn registered(&self) -> bool {
        self.registered.load(SeqCst)
    }
}

impl Read for UnixStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        <&UnixStream as Read>::read(&mut &*self, buf)
    }
}

impl Write for UnixStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        <&UnixStream as Write>::write(&mut &*self, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        <&UnixStream as Write>::flush(&mut &*self)
    }
}

impl<'a> Read for &'a UnixStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Make sure we're registered
        if !self.registered() {
            return Err(would_block())
        }

        let mut state = self.inner.io.lock().unwrap();
        match mem::replace(&mut state.read, State::None) {
            // In theory not possible with `ready_registration` checked above,
            // but return would block for now.
            State::None => Err(would_block()),

            // A read is in flight, still waiting for it to finish
            State::Pending((buf, amt)) => {
                state.read = State::Pending((buf, amt));
                Err(would_block())
            }

            // We previously read something into `data`, try to copy out some
            // data. If we copy out all the data schedule a new read and
            // otherwise store the buffer to get read later.
            State::Ok((data, cur)) => {
                let n = {
                    let mut remaining = &data[cur..];
                    try!(remaining.read(buf))
                };
                let next = cur + n;
                if next != data.len() {
                    state.read = State::Ok((data, next));
                } else {
                    InnerStream::schedule_read(&self.inner, &mut state);
                }
                Ok(n)
            }

            // Looks like an in-flight read hit an error, return that here while
            // we schedule a new one.
            State::Err(e) => {
                InnerStream::schedule_read(&self.inner, &mut state);
                Err(e)
            }
        }
    }
}

impl<'a> Write for &'a UnixStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Make sure we're registered
        if !self.registered() {
            return Err(would_block())
        }

        // Make sure there's no writes pending
        let mut io = self.inner.io.lock().unwrap();
        match io.write {
            State::None => {}
            _ => return Err(would_block())
        }

        // Move `buf` onto the heap and fire off the write
        //
        // TODO: need to be smarter about buffer management here
        InnerStream::schedule_write(&self.inner, buf.to_vec(), 0, &mut io);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        // TODO: `FlushFileBuffers` somehow?
        Ok(())
    }
}

impl Evented for UnixStream {
    fn register(&self,
                poll: &Poll,
                token: Token,
                interest: Ready,
                opts: PollOpt) -> io::Result<()> {
        // First, register the handle with the event loop
        unsafe {
            try!(self.poll_registration.register_socket(&self.inner.socket,
                                                        token,
                                                        poll));
        }
        try!(poll.register(&self.ready_registration, token, interest, opts));
        self.registered.store(true, SeqCst);

        // If we were connected before being registered process that request
        // here and go along our merry ways. Note that the callback for a
        // successful connect will worry about generating writable/readable
        // events and scheduling a new read.
        if let Some(addr) = self.inner.io.lock().unwrap().deferred_connect.take() {
            return InnerStream::schedule_connect(&self.inner, &addr).map(|_| ())
        }

        InnerStream::post_register(&self.inner);
        Ok(())
    }

    fn reregister(&self,
                  poll: &Poll,
                  token: Token,
                  interest: Ready,
                  opts: PollOpt) -> io::Result<()> {
        // Validate `Poll` and that we were previously registered
        unsafe {
            try!(self.poll_registration.reregister_socket(&self.inner.socket,
                                                          token,
                                                          poll));
        }

        // At this point we should for sure have `ready_registration` unless
        // we're racing with `register` above, so just return a bland error if
        // the borrow fails.
        try!(poll.reregister(&self.ready_registration, token, interest, opts));

        InnerStream::post_register(&self.inner);

        Ok(())
    }

    fn deregister(&self, poll: &Poll) -> io::Result<()> {
        // Validate `Poll` and deregister ourselves
        unsafe {
            try!(self.poll_registration.deregister_socket(&self.inner.socket, poll));
        }
        poll.deregister(&self.ready_registration)
    }
}

impl AsRawSocket for UnixStream {
    fn as_raw_socket(&self) -> RawSocket {
        self.inner.socket.as_raw_socket()
    }
}

impl FromRawSocket for UnixStream {
    unsafe fn from_raw_socket(socket: RawSocket) -> UnixStream {
        UnixStream::new(socket, None)
    }
}

impl fmt::Debug for UnixStream {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.inner.socket.fmt(f)
    }
}

impl Drop for UnixStream {
    fn drop(&mut self) {
        // Cancel pending reads/connects, but don't cancel writes to ensure that
        // everything is flushed out.
        unsafe {
            if self.inner.connecting.load(SeqCst) {
                drop(cancel(&self.inner.socket, &self.inner.connect));
            }
            let io = self.inner.io.lock().unwrap();
            match io.read {
                State::Pending(..) => {
                    drop(cancel(&self.inner.socket, &self.inner.read));
                }
                _ => {}
            }
        }
    }
}

impl InnerStream {
    fn schedule_connect(me: &FromRawArc<InnerStream>, addr: &SocketAddr) -> io::Result<()> {
        unsafe {
            let overlapped = me.connect.as_mut_ptr() as *mut _;
            me.socket.connect_overlapped(&addr, &[], overlapped)?;
        }

        mem::forget(me.clone());
        Ok(())
    }

    /// Schedules a read to happen in the background, executing an overlapped
    /// operation.
    ///
    /// This function returns `true` if a normal error happens or if the read
    /// is scheduled in the background. If the pipe is no longer connected
    /// (ERROR_PIPE_LISTENING) then `false` is returned and no read is
    /// scheduled.
    fn schedule_read(me: &FromRawArc<InnerStream>, io: &mut StreamIo) -> bool {
        // Check to see if a read is already scheduled/completed
        match io.read {
            State::None => {}
            _ => return true,
        }

        // Turn off our read readiness
        let ready = me.readiness.readiness();
        me.readiness.set_readiness(ready & !Ready::readable())
                    .expect("event loop seems gone");

        // Allocate a buffer and schedule the read.
        //
        // TODO: need to be smarter about buffer management here
        let mut buf = Vec::with_capacity(8 * 1024);
        let e = unsafe {
            let overlapped = me.read.as_mut_ptr() as *mut _;
            let slice = slice::from_raw_parts_mut(buf.as_mut_ptr(),
                                                  buf.capacity());
            me.socket.read_overlapped(slice, overlapped)
        };

        match e {
            // See `connect` above for the rationale behind `forget`
            Ok(e) => {
                trace!("schedule read success: {:?}", e);
                io.read = State::Pending((buf, 0)); // 0 is ignored on read side
                mem::forget(me.clone());
                true
            }

            // If some other error happened, though, we're now readable to give
            // out the error.
            Err(e) => {
                trace!("schedule read error: {}", e);
                io.read = State::Err(e);
                me.readiness.set_readiness(ready | Ready::readable())
                            .expect("event loop still seems gone");
                true
            }
        }
    }

    fn schedule_write(me: &FromRawArc<InnerStream>,
                      buf: Vec<u8>,
                      pos: usize,
                      io: &mut StreamIo) {
        // Very similar to `schedule_read` above, just done for the write half.
        let ready = me.readiness.readiness();
        me.readiness.set_readiness(ready & !Ready::writable())
                    .expect("event loop seems gone");

        let e = unsafe {
            let overlapped = me.write.as_mut_ptr() as *mut _;
            me.socket.write_overlapped(&buf[pos..], overlapped)
        };

        match e {
            // See `connect` above for the rationale behind `forget`
            Ok(e) => {
                trace!("schedule write success: {:?}", e);
                io.write = State::Pending((buf, pos));
                mem::forget(me.clone())
            }
            Err(e) => {
                trace!("schedule write error: {}", e);
                io.write = State::Err(e);
                me.add_readiness(Ready::writable());
            }
        }
    }

    fn add_readiness(&self, ready: Ready) {
        self.readiness.set_readiness(ready | self.readiness.readiness())
                      .expect("event loop still seems gone");
    }

    fn post_register(me: &FromRawArc<InnerStream>) {
        let mut io = me.io.lock().unwrap();
        if InnerStream::schedule_read(&me, &mut io) {
            if let State::None = io.write {
                me.add_readiness(Ready::writable());
            }
        }
    }
}

unsafe fn cancel(socket: &AsRawSocket,
                 overlapped: &windows::Overlapped) -> io::Result<()> {
    let ret = CancelIoEx(socket.as_raw_socket() as HANDLE, overlapped.as_mut_ptr() as *mut _);
    if ret == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn connect_done(status: &OVERLAPPED_ENTRY) {
    let status = CompletionStatus::from_entry(status);
    trace!("connect done");

    // Acquire the `FromRawArc<InnerStream>`. Note that we should be guaranteed that
    // the refcount is available to us due to the `mem::forget` in
    // `connect` above.
    let me = unsafe {
        overlapped2arc!(status.overlapped(), InnerStream, connect)
    };

    // Flag ourselves as no longer using the `connect` overlapped instances.
    let prev = me.connecting.swap(false, SeqCst);
    assert!(prev, "wasn't previously connecting");

    // Stash away our connect error if one happened, otherwise complete the
    // connection.
    debug_assert_eq!(status.bytes_transferred(), 0);
    if let Err(e) = unsafe { me.socket.result(status.overlapped()) }
        .and_then(|(n, _)| {
            debug_assert_eq!(n, 0);
            me.socket.connect_complete()
        })
    {
        me.io.lock().unwrap().connect_error = Some(e);
    }

    // We essentially just finished a registration, so kick off a
    // read and register write readiness.
    InnerStream::post_register(&me);
}

fn read_done(status: &OVERLAPPED_ENTRY) {
    let status = CompletionStatus::from_entry(status);
    trace!("read finished, bytes={}", status.bytes_transferred());

    // Acquire the `FromRawArc<InnerStream>`. Note that we should be guaranteed that
    // the refcount is available to us due to the `mem::forget` in
    // `schedule_read` above.
    let me = unsafe {
        overlapped2arc!(status.overlapped(), InnerStream, read)
    };

    // Move from the `Pending` to `Ok` state.
    let mut io = me.io.lock().unwrap();
    let mut buf = match mem::replace(&mut io.read, State::None) {
        State::Pending((buf, _)) => buf,
        _ => unreachable!(),
    };
    unsafe {
        match me.socket.result(status.overlapped()) {
            Ok((n, _)) => {
                debug_assert_eq!(status.bytes_transferred() as usize, n);
                buf.set_len(status.bytes_transferred() as usize);
                io.read = State::Ok((buf, 0));
            }
            Err(e) => {
                debug_assert_eq!(status.bytes_transferred(), 0);
                io.read = State::Err(e);
            }
        }
    }

    // Flag our readiness that we've got data.
    me.add_readiness(Ready::readable());
}

fn write_done(status: &OVERLAPPED_ENTRY) {
    let status = CompletionStatus::from_entry(status);
    trace!("write finished, bytes={}", status.bytes_transferred());
    // Acquire the `FromRawArc<InnerStream>`. Note that we should be guaranteed that
    // the refcount is available to us due to the `mem::forget` in
    // `schedule_write` above.
    let me = unsafe {
        overlapped2arc!(status.overlapped(), InnerStream, write)
    };

    // Make the state change out of `Pending`. If we wrote the entire buffer
    // then we're writable again and otherwise we schedule another write.
    let mut io = me.io.lock().unwrap();
    let (buf, pos) = match mem::replace(&mut io.write, State::None) {
        State::Pending((buf, pos)) => (buf, pos),
        _ => unreachable!(),
    };

    unsafe {
        match me.socket.result(status.overlapped()) {
            Ok((n, _)) => {
                debug_assert_eq!(status.bytes_transferred() as usize, n);
                let new_pos = pos + (status.bytes_transferred() as usize);
                if new_pos == buf.len() {
                    me.add_readiness(Ready::writable());
                } else {
                    InnerStream::schedule_write(&me, buf, new_pos, &mut io);
                }
            }
            Err(e) => {
                debug_assert_eq!(status.bytes_transferred(), 0);
                io.write = State::Err(e);
                me.add_readiness(Ready::writable());
            }
        }
    }
}

/// Listens for connections to a Unix domain socket on Windows.
pub struct UnixListener {
    registered: AtomicBool,
    ready_registration: Registration,
    poll_registration: windows::Binding,
    inner: FromRawArc<InnerListener>,
}

struct InnerListener {
    socket: inner::UnixListener,
    readiness: SetReadiness,

    accept: windows::Overlapped,

    io: Mutex<ListenerIo>,
}

struct ListenerIo {
    accept: State<inner::UnixStream, (UnixStream, SocketAddr)>,
    accept_buf: AcceptAddrsBuf,
}

impl UnixListener {
    /// Binds a new instance of UnixListener to a path
    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<UnixListener> {
        let sock = inner::UnixListener::bind(path)?;
        let sock = unsafe {
            UnixListener::from_raw_socket(sock.into_raw_socket())
        };
        Ok(sock)
    }

    /// Accepts a new incoming connection to this listener.
    ///
    /// This may return an `Err(e)` where `e.kind()` is
    /// `io::ErrorKind::WouldBlock`. This means a stream may be ready at a later
    /// point and one should wait for a notification before calling `accept`
    /// again.
    ///
    /// If an accepted stream is returned, the remote address of the peer is
    /// returned along with it.
    pub fn accept(&self) -> io::Result<Option<(UnixStream, SocketAddr)>> {
        // Make sure we're associated with an IOCP object
        if !self.registered() {
            return Err(io::ErrorKind::WouldBlock.into());
        }

        let mut io = self.inner.io.lock().unwrap();
        let ret = match mem::replace(&mut io.accept, State::None) {
            State::None => return Err(io::ErrorKind::WouldBlock.into()),

            State::Pending(s) => {
                io.accept = State::Pending(s);
                return Err(io::ErrorKind::WouldBlock.into());
            }

            State::Ok((s, a)) => Ok(Some((s, a))),

            State::Err(e) => {
                assert!(e.kind() != io::ErrorKind::WouldBlock);
                Err(e)
            }
        };

        InnerListener::schedule_accept(&self.inner, &mut io);

        ret
    }

    /// Takes any internal error that has happened after the last I/O operation
    /// which hasn't been retrieved yet.
    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.inner.socket.take_error()
    }

    /// Creates a new independently owned handle to the underlying socket.
    pub fn try_clone(&self) -> io::Result<UnixListener> {
        self.inner.socket.try_clone().map(|s| unsafe {
            UnixListener::from_raw_socket(s.into_raw_socket())
        })
    }

    /// Returns the socket address of the local half of this connection.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.socket.local_addr()
    }

    fn registered(&self) -> bool {
        self.registered.load(SeqCst)
    }
}

impl Evented for UnixListener {
    fn register(&self,
                poll: &Poll,
                token: Token,
                interest: Ready,
                opts: PollOpt) -> io::Result<()> {
        // First, register the handle with the event loop
        unsafe {
            try!(self.poll_registration.register_socket(&self.inner.socket,
                                                        token,
                                                        poll));
        }
        try!(poll.register(&self.ready_registration, token, interest, opts));
        self.registered.store(true, SeqCst);
        InnerListener::post_register(&self.inner);
        Ok(())
    }

    fn reregister(&self,
                  poll: &Poll,
                  token: Token,
                  interest: Ready,
                  opts: PollOpt) -> io::Result<()> {
        // Validate `Poll` and that we were previously registered
        unsafe {
            try!(self.poll_registration.reregister_socket(&self.inner.socket,
                                                          token,
                                                          poll));
        }

        // At this point we should for sure have `ready_registration` unless
        // we're racing with `register` above, so just return a bland error if
        // the borrow fails.
        try!(poll.reregister(&self.ready_registration, token, interest, opts));

        InnerListener::post_register(&self.inner);

        Ok(())
    }

    fn deregister(&self, poll: &Poll) -> io::Result<()> {
        // Validate `Poll` and deregister ourselves
        unsafe {
            try!(self.poll_registration.deregister_socket(&self.inner.socket, poll));
        }
        poll.deregister(&self.ready_registration)
    }
}

impl AsRawSocket for UnixListener {
    fn as_raw_socket(&self) -> RawSocket {
        self.inner.socket.as_raw_socket()
    }
}

impl FromRawSocket for UnixListener {
    unsafe fn from_raw_socket(socket: RawSocket) -> UnixListener {
        let (r, s) = Registration::new2();
        UnixListener {
            registered: AtomicBool::new(false),
            ready_registration: r,
            poll_registration: windows::Binding::new(),
            inner: FromRawArc::new(InnerListener {
                socket: inner::UnixListener::from_raw_socket(socket),
                readiness: s,
                // transmutes to straddle winapi versions (mio 0.6 is on an
                // older winapi)
                accept: windows::Overlapped::new(mem::transmute(accept_done as fn(_))),
                io: Mutex::new(ListenerIo {
                    accept: State::None,
                    accept_buf: AcceptAddrsBuf::new(),
                }),
            }),
        }
    }
}

impl fmt::Debug for UnixListener {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.inner.socket.fmt(f)
    }
}

impl Drop for UnixListener {
    fn drop(&mut self) {
        // Cancel pending accepts, but don't cancel writes to ensure that
        // everything is flushed out.
        unsafe {
            let io = self.inner.io.lock().unwrap();
            match io.accept {
                State::Pending(_) => {
                    trace!("cancelling active TCP accept");
                    drop(cancel(&self.inner.socket, &self.inner.accept));
                }
                _ => {}
            }
        }
    }
}

impl InnerListener {
    fn schedule_accept(me: &FromRawArc<InnerListener>, io: &mut ListenerIo) {
        match io.accept {
            State::None => {}
            _ => return,
        }

        // Turn off our read readiness
        let ready = me.readiness.readiness();
        me.readiness.set_readiness(ready & !Ready::readable())
                    .expect("event loop seems gone");

        trace!("scheduling an accept");

        let res = inner::UnixStream::new()
            .and_then(|sock| {
                // TODO: need to be smarter about buffer management here
                let overlapped = me.accept.as_mut_ptr() as *mut _;
                unsafe {
                    me.socket.accept_overlapped(&sock, &mut io.accept_buf, overlapped)
                }.map(|_| sock)
            });

        match res {
            Ok(sock) => {
                // See `connect` above for the rationale behind `forget`
                trace!("accept is ready");
                let s = unsafe {
                    inner::UnixStream::from_raw_socket(sock.into_raw_socket())
                };
                io.accept = State::Pending(s);
                mem::forget(me.clone());
            }

            Err(e) => {
                trace!("schedule accept error: {}", e);
                io.accept = State::Err(e);
                me.readiness.set_readiness(ready | Ready::readable())
                            .expect("event loop still seems gone");
            }
        }
    }

    fn add_readiness(&self, ready: Ready) {
        self.readiness.set_readiness(ready | self.readiness.readiness())
                      .expect("event loop still seems gone");
    }

    fn post_register(me: &FromRawArc<InnerListener>) {
        let mut io = me.io.lock().unwrap();
        InnerListener::schedule_accept(&me, &mut io);
    }
}

fn accept_done(status: &OVERLAPPED_ENTRY) {
    let status = CompletionStatus::from_entry(status);

    // Acquire the `FromRawArc<InnerStream>`. Note that we should be guaranteed that
    // the refcount is available to us due to the `mem::forget` in
    // `schedule_accept` above.
    let me = unsafe {
        overlapped2arc!(status.overlapped(), InnerListener, accept)
    };

    // Move from the `Pending` to `Ok` state.
    let mut io = me.io.lock().unwrap();
    let sock = match mem::replace(&mut io.accept, State::None) {
        State::Pending(s) => s,
        _ => unreachable!(),
    };

    trace!("accept finished");

    let res = me.socket.accept_complete(&sock)
        .and_then(|()| io.accept_buf.parse(&me.socket))
        .and_then(|buf| {
            buf.remote().ok_or_else(|| {
                io::Error::new(io::ErrorKind::Other, "could not obtain remote address")
            })
        });

    io.accept = match res {
        Ok(remote_addr) => {
            let outer = unsafe { UnixStream::from_raw_socket(sock.into_raw_socket()) };
            State::Ok((outer, remote_addr))
        }

        Err(e) => State::Err(e),
    };

    // Flag our readiness that we've got data.
    me.add_readiness(Ready::readable());
}

#[allow(missing_docs)]
pub mod net {
    pub use inner::*;
}