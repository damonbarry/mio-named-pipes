// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.


use std::fmt;
use std::io::{self};
use std::mem;
use std::net::Shutdown;
use std::os::raw::c_int;
use std::os::windows::io::{AsRawSocket, FromRawSocket, IntoRawSocket, RawSocket};
use std::path::Path;

use winapi::um::winsock2::{bind, connect, getpeername, getsockname, listen};

use inner::socket::{init, Socket};
use inner::{c, cvt, sockaddr_un, SocketAddr};

/// A Unix stream socket.
///
/// # Examples
///
/// ```ignore
/// use std::os::windows::net::UnixStream;
/// use std::io::prelude::*;
///
/// let mut stream = UnixStream::connect("/path/to/my/socket").unwrap();
/// stream.write_all(b"hello world").unwrap();
/// let mut response = String::new();
/// stream.read_to_string(&mut response).unwrap();
/// println!("{}", response);
/// ```
pub struct UnixStream(Socket);

impl fmt::Debug for UnixStream {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        let mut builder = fmt.debug_struct("UnixStream");
        builder.field("socket", &self.0.as_raw_socket());
        if let Ok(addr) = self.local_addr() {
            builder.field("local", &addr);
        }
        if let Ok(addr) = self.peer_addr() {
            builder.field("peer", &addr);
        }
        builder.finish()
    }
}

impl UnixStream {
    pub fn new() -> io::Result<UnixStream> {
        let sock = Socket::new()?;
        let sock = unsafe {
            UnixStream::from_raw_socket(sock.into_raw_socket())
        };
        Ok(sock)
    }

    /// Connects to the socket named by `path`.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::os::windows::net::UnixStream;
    ///
    /// let socket = match UnixStream::connect("/tmp/sock") {
    ///     Ok(sock) => sock,
    ///     Err(e) => {
    ///         println!("Couldn't connect: {:?}", e);
    ///         return
    ///     }
    /// };
    /// ```
    pub fn connect<P: AsRef<Path>>(path: P) -> io::Result<UnixStream> {
        init();
        fn inner(path: &Path) -> io::Result<UnixStream> {
            unsafe {
                let inner = Socket::new()?;
                let (addr, len) = sockaddr_un(path)?;

                cvt(connect(inner.as_raw_socket() as usize, &addr as *const _ as *const _, len as i32))?;
                Ok(UnixStream(inner))
            }
        }
        inner(path.as_ref())
    }

    /// Creates an unnamed pair of connected sockets.
    ///
    /// Returns two `UnixStream`s which are connected to each other.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::os::windows::net::UnixStream;
    ///
    /// let (sock1, sock2) = match UnixStream::pair() {
    ///     Ok((sock1, sock2)) => (sock1, sock2),
    ///     Err(e) => {
    ///         println!("Couldn't create a pair of sockets: {:?}", e);
    ///         return
    ///     }
    /// };
    /// ```
    // Windows dosn't support socketpair()...this would need to be emulated
    // pub fn pair() -> io::Result<(UnixStream, UnixStream)> {
    //     init();
    //     let (i1, i2) = Socket::new_pair(AF_UNIX, SOCK_STREAM)?;
    //     Ok((UnixStream(i1), UnixStream(i2)))
    // }

    /// Creates a new independently owned handle to the underlying socket.
    ///
    /// The returned `UnixStream` is a reference to the same stream that this
    /// object references. Both handles will read and write the same stream of
    /// data, and options set on one stream will be propagated to the other
    /// stream.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::os::windows::net::UnixStream;
    ///
    /// let socket = UnixStream::connect("/tmp/sock").unwrap();
    /// let sock_copy = socket.try_clone().expect("Couldn't clone socket");
    /// ```
    pub fn try_clone(&self) -> io::Result<UnixStream> {
        self.0.duplicate().map(UnixStream)
    }

    /// Returns the socket address of the local half of this connection.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::os::windows::net::UnixStream;
    ///
    /// let socket = UnixStream::connect("/tmp/sock").unwrap();
    /// let addr = socket.local_addr().expect("Couldn't get local address");
    /// ```
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        SocketAddr::new(|addr, len| unsafe { getsockname(self.0.as_raw_socket() as usize, addr, len) })
    }

    /// Returns the socket address of the remote half of this connection.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::os::windows::net::UnixStream;
    ///
    /// let socket = UnixStream::connect("/tmp/sock").unwrap();
    /// let addr = socket.peer_addr().expect("Couldn't get peer address");
    /// ```
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        SocketAddr::new(|addr, len| unsafe { getpeername(self.0.as_raw_socket() as usize, addr, len) })
    }

    /// Moves the socket into or out of nonblocking mode.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::os::windows::net::UnixStream;
    ///
    /// let socket = UnixStream::connect("/tmp/sock").unwrap();
    /// socket.set_nonblocking(true).expect("Couldn't set nonblocking");
    /// ```
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.0.set_nonblocking(nonblocking)
    }

    /// Returns the value of the `SO_ERROR` option.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::os::windows::net::UnixStream;
    ///
    /// let socket = UnixStream::connect("/tmp/sock").unwrap();
    /// if let Ok(Some(err)) = socket.take_error() {
    ///     println!("Got error: {:?}", err);
    /// }
    /// ```
    ///
    /// # Platform specific
    /// On Redox this always returns None.
    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.0.take_error()
    }

    /// Shuts down the read, write, or both halves of this connection.
    ///
    /// This function will cause all pending and future I/O calls on the
    /// specified portions to immediately return with an appropriate value
    /// (see the documentation of [`Shutdown`]).
    ///
    /// [`Shutdown`]: ../../../../std/net/enum.Shutdown.html
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::os::windows::net::UnixStream;
    /// use std::net::Shutdown;
    ///
    /// let socket = UnixStream::connect("/tmp/sock").unwrap();
    /// socket.shutdown(Shutdown::Both).expect("shutdown function failed");
    /// ```
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.0.shutdown(how)
    }
}

impl AsRawSocket for UnixStream {
    fn as_raw_socket(&self) -> RawSocket {
        self.0.as_raw_socket()
    }
}

impl FromRawSocket for UnixStream {
    unsafe fn from_raw_socket(sock: RawSocket) -> Self {
        UnixStream(Socket::from_raw_socket(sock))
    }
}

impl IntoRawSocket for UnixStream {
    fn into_raw_socket(self) -> RawSocket {
        let ret = self.0.as_raw_socket();
        mem::forget(self);
        ret
    }
}


/// A structure representing a Unix domain socket server.
///
/// # Examples
///
/// ```ignore
/// use std::thread;
/// use std::os::windows::net::{UnixStream, UnixListener};
///
/// fn handle_client(stream: UnixStream) {
///     // ...
/// }
///
/// let listener = UnixListener::bind("/path/to/the/socket").unwrap();
///
/// // accept connections and process them, spawning a new thread for each one
/// for stream in listener.incoming() {
///     match stream {
///         Ok(stream) => {
///             /* connection succeeded */
///             thread::spawn(|| handle_client(stream));
///         }
///         Err(err) => {
///             /* connection failed */
///             break;
///         }
///     }
/// }
/// ```
pub struct UnixListener(Socket);

impl fmt::Debug for UnixListener {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        let mut builder = fmt.debug_struct("UnixListener");
        builder.field("socket", &self.0.as_raw_socket());
        if let Ok(addr) = self.local_addr() {
            builder.field("local", &addr);
        }
        builder.finish()
    }
}

impl UnixListener {
    /// Creates a new `UnixListener` bound to the specified socket.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::os::windows::net::UnixListener;
    ///
    /// let listener = match UnixListener::bind("/path/to/the/socket") {
    ///     Ok(sock) => sock,
    ///     Err(e) => {
    ///         println!("Couldn't connect: {:?}", e);
    ///         return
    ///     }
    /// };
    /// ```
    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<UnixListener> {
        init();
        fn inner(path: &Path) -> io::Result<UnixListener> {
            unsafe {
                let inner = Socket::new()?;
                let (addr, len) = sockaddr_un(path)?;

                cvt(bind(inner.as_raw_socket() as usize, &addr as *const _ as *const _, len as _))?;
                cvt(listen(inner.as_raw_socket() as usize, 128))?;

                Ok(UnixListener(inner))
            }
        }
        inner(path.as_ref())
    }

    /// Accepts a new incoming connection to this listener.
    ///
    /// This function will block the calling thread until a new Unix connection
    /// is established. When established, the corresponding [`UnixStream`] and
    /// the remote peer's address will be returned.
    ///
    /// [`UnixStream`]: ../../../../std/os/unix/net/struct.UnixStream.html
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::os::windows::net::UnixListener;
    ///
    /// let listener = UnixListener::bind("/path/to/the/socket").unwrap();
    ///
    /// match listener.accept() {
    ///     Ok((socket, addr)) => println!("Got a client: {:?}", addr),
    ///     Err(e) => println!("accept function failed: {:?}", e),
    /// }
    /// ```
    pub fn accept(&self) -> io::Result<(UnixStream, SocketAddr)> {
        let mut storage: c::sockaddr_un = unsafe { mem::zeroed() };
        let mut len = mem::size_of_val(&storage) as c_int;
        let sock = self.0.accept(&mut storage as *mut _ as *mut _, &mut len)?;
        let addr = SocketAddr::from_parts(storage, len)?;
        Ok((UnixStream(sock), addr))
    }

    /// Creates a new independently owned handle to the underlying socket.
    ///
    /// The returned `UnixListener` is a reference to the same socket that this
    /// object references. Both handles can be used to accept incoming
    /// connections and options set on one listener will affect the other.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::os::windows::net::UnixListener;
    ///
    /// let listener = UnixListener::bind("/path/to/the/socket").unwrap();
    ///
    /// let listener_copy = listener.try_clone().expect("try_clone failed");
    /// ```
    pub fn try_clone(&self) -> io::Result<UnixListener> {
        self.0.duplicate().map(UnixListener)
    }

    /// Returns the local socket address of this listener.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::os::windows::net::UnixListener;
    ///
    /// let listener = UnixListener::bind("/path/to/the/socket").unwrap();
    ///
    /// let addr = listener.local_addr().expect("Couldn't get local address");
    /// ```
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        SocketAddr::new(|addr, len| unsafe { getsockname(self.0.as_raw_socket() as usize, addr, len) })
    }

    /// Moves the socket into or out of nonblocking mode.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::os::windows::net::UnixListener;
    ///
    /// let listener = UnixListener::bind("/path/to/the/socket").unwrap();
    ///
    /// listener.set_nonblocking(true).expect("Couldn't set non blocking");
    /// ```
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.0.set_nonblocking(nonblocking)
    }

    /// Returns the value of the `SO_ERROR` option.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::os::windows::net::UnixListener;
    ///
    /// let listener = UnixListener::bind("/tmp/sock").unwrap();
    ///
    /// if let Ok(Some(err)) = listener.take_error() {
    ///     println!("Got error: {:?}", err);
    /// }
    /// ```
    ///
    /// # Platform specific
    /// On Redox this always returns None.
    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.0.take_error()
    }

    /// Returns an iterator over incoming connections.
    ///
    /// The iterator will never return [`None`] and will also not yield the
    /// peer's [`SocketAddr`] structure.
    ///
    /// [`None`]: ../../../../std/option/enum.Option.html#variant.None
    /// [`SocketAddr`]: struct.SocketAddr.html
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::thread;
    /// use std::os::windows::net::{UnixStream, UnixListener};
    ///
    /// fn handle_client(stream: UnixStream) {
    ///     // ...
    /// }
    ///
    /// let listener = UnixListener::bind("/path/to/the/socket").unwrap();
    ///
    /// for stream in listener.incoming() {
    ///     match stream {
    ///         Ok(stream) => {
    ///             thread::spawn(|| handle_client(stream));
    ///         }
    ///         Err(err) => {
    ///             break;
    ///         }
    ///     }
    /// }
    /// ```
    pub fn incoming<'a>(&'a self) -> Incoming<'a> {
        Incoming { listener: self }
    }
}

impl AsRawSocket for UnixListener {
    fn as_raw_socket(&self) -> RawSocket {
        self.0.as_raw_socket()
    }
}

impl FromRawSocket for UnixListener {
    unsafe fn from_raw_socket(sock: RawSocket) -> Self {
        UnixListener(Socket::from_raw_socket(sock))
    }
}

impl IntoRawSocket for UnixListener {
    fn into_raw_socket(self) -> RawSocket {
        let ret = self.0.as_raw_socket();
        mem::forget(self);
        ret
    }
}

impl<'a> IntoIterator for &'a UnixListener {
    type Item = io::Result<UnixStream>;
    type IntoIter = Incoming<'a>;

    fn into_iter(self) -> Incoming<'a> {
        self.incoming()
    }
}

/// An iterator over incoming connections to a [`UnixListener`].
///
/// It will never return [`None`].
///
/// [`None`]: ../../../../std/option/enum.Option.html#variant.None
/// [`UnixListener`]: struct.UnixListener.html
///
/// # Examples
///
/// ```ignore
/// use std::thread;
/// use std::os::windows::net::{UnixStream, UnixListener};
///
/// fn handle_client(stream: UnixStream) {
///     // ...
/// }
///
/// let listener = UnixListener::bind("/path/to/the/socket").unwrap();
///
/// for stream in listener.incoming() {
///     match stream {
///         Ok(stream) => {
///             thread::spawn(|| handle_client(stream));
///         }
///         Err(err) => {
///             break;
///         }
///     }
/// }
/// ```
#[derive(Debug)]
pub struct Incoming<'a> {
    listener: &'a UnixListener,
}

impl<'a> Iterator for Incoming<'a> {
    type Item = io::Result<UnixStream>;

    fn next(&mut self) -> Option<io::Result<UnixStream>> {
        Some(self.listener.accept().map(|s| s.0))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (usize::max_value(), None)
    }
}

#[cfg(test)]
mod test {
    use std::io;
    use std::path::PathBuf;
    use std::thread;

    use tempdir::TempDir;

    use super::*;

    macro_rules! or_panic {
        ($e:expr) => {
            match $e {
                Ok(e) => e,
                Err(e) => panic!("{}", e),
            }
        }
    }

    fn tmpdir() -> Result<(TempDir, PathBuf), io::Error> {
        let dir = TempDir::new("mio-uds-windows")?;
        let path = dir.path().join("sock");
        Ok((dir, path))
    }

    #[test]
    fn basic() {
        let (_dir, socket_path) = or_panic!(tmpdir());
        // let msg1 = b"hello";
        // let msg2 = b"world!";

        let listener = or_panic!(UnixListener::bind(&socket_path));
        let thread = thread::spawn(move || {
            let mut _stream = or_panic!(listener.accept()).0;
            // let mut buf = [0; 5];
            // or_panic!(stream.read(&mut buf));
            // assert_eq!(&msg1[..], &buf[..]);
            // or_panic!(stream.write_all(msg2));
        });

        let stream = or_panic!(UnixStream::connect(&socket_path));
        assert_eq!(Some(&*socket_path),
                   stream.peer_addr().unwrap().as_pathname());
        // or_panic!(stream.write_all(msg1));
        // let mut buf = vec![];
        // or_panic!(stream.read_to_end(&mut buf));
        // assert_eq!(&msg2[..], &buf[..]);
        drop(stream);

        thread.join().unwrap();
    }

    // #[test]
    // fn pair() {
    //     let msg1 = b"hello";
    //     let msg2 = b"world!";

    //     let (mut s1, mut s2) = or_panic!(UnixStream::pair());
    //     let thread = thread::spawn(move || {
    //         // s1 must be moved in or the test will hang!
    //         let mut buf = [0; 5];
    //         or_panic!(s1.read(&mut buf));
    //         assert_eq!(&msg1[..], &buf[..]);
    //         or_panic!(s1.write_all(msg2));
    //     });

    //     or_panic!(s2.write_all(msg1));
    //     let mut buf = vec![];
    //     or_panic!(s2.read_to_end(&mut buf));
    //     assert_eq!(&msg2[..], &buf[..]);
    //     drop(s2);

    //     thread.join().unwrap();
    // }

    #[test]
    fn try_clone() {
        let (_dir, socket_path) = or_panic!(tmpdir());
        // let msg1 = b"hello";
        // let msg2 = b"world";

        let listener = or_panic!(UnixListener::bind(&socket_path));
        let thread = thread::spawn(move || {
            #[allow(unused_mut)]
            let mut _stream = or_panic!(listener.accept()).0;
            // or_panic!(stream.write_all(msg1));
            // or_panic!(stream.write_all(msg2));
        });

        let stream = or_panic!(UnixStream::connect(&socket_path));
        let stream2 = or_panic!(stream.try_clone());
        assert_eq!(Some(&*socket_path),
                   stream2.peer_addr().unwrap().as_pathname());

        // let mut buf = [0; 5];
        // or_panic!(stream.read(&mut buf));
        // assert_eq!(&msg1[..], &buf[..]);
        // or_panic!(stream2.read(&mut buf));
        // assert_eq!(&msg2[..], &buf[..]);

        thread.join().unwrap();
    }

    #[test]
    fn iter() {
        let (_dir, socket_path) = or_panic!(tmpdir());

        let listener = or_panic!(UnixListener::bind(&socket_path));
        let thread = thread::spawn(move || {
            for stream in listener.incoming().take(2) {
                let mut _stream = or_panic!(stream);
                // let mut buf = [0];
                // or_panic!(stream.read(&mut buf));
            }
        });

        for _ in 0..2 {
            let mut _stream = or_panic!(UnixStream::connect(&socket_path));
            // or_panic!(stream.write_all(&[0]));
        }

        thread.join().unwrap();
    }

    #[test]
    fn long_path() {
        let dir = or_panic!(TempDir::new("mio-uds-windows"));
        let socket_path = dir.path()
                             .join("asdfasdfasdfasdfasdfasdfasdfasdfasdfasdfasdfasdfasdfasdfasdfa\
                                    sasdfasdfasdasdfasdfasdfadfasdfasdfasdfasdfasdf");
        match UnixStream::connect(&socket_path) {
            Err(ref e) if e.kind() == io::ErrorKind::InvalidInput => {}
            Err(e) => panic!("unexpected error {}", e),
            Ok(_) => panic!("unexpected success"),
        }

        match UnixListener::bind(&socket_path) {
            Err(ref e) if e.kind() == io::ErrorKind::InvalidInput => {}
            Err(e) => panic!("unexpected error {}", e),
            Ok(_) => panic!("unexpected success"),
        }
    }

    // #[test]
    // fn timeouts() {
    //     let (_dir, socket_path) = or_panic!(tmpdir());

    //     let _listener = or_panic!(UnixListener::bind(&socket_path));

    //     let stream = or_panic!(UnixStream::connect(&socket_path));
    //     let dur = Duration::new(15410, 0);

    //     assert_eq!(None, or_panic!(stream.read_timeout()));

    //     or_panic!(stream.set_read_timeout(Some(dur)));
    //     assert_eq!(Some(dur), or_panic!(stream.read_timeout()));

    //     assert_eq!(None, or_panic!(stream.write_timeout()));

    //     or_panic!(stream.set_write_timeout(Some(dur)));
    //     assert_eq!(Some(dur), or_panic!(stream.write_timeout()));

    //     or_panic!(stream.set_read_timeout(None));
    //     assert_eq!(None, or_panic!(stream.read_timeout()));

    //     or_panic!(stream.set_write_timeout(None));
    //     assert_eq!(None, or_panic!(stream.write_timeout()));
    // }

    // #[test]
    // fn test_read_timeout() {
    //     let (_dir, socket_path) = or_panic!(tmpdir());

    //     let _listener = or_panic!(UnixListener::bind(&socket_path));

    //     let mut stream = or_panic!(UnixStream::connect(&socket_path));
    //     or_panic!(stream.set_read_timeout(Some(Duration::from_millis(1000))));

    //     let mut buf = [0; 10];
    //     let kind = stream.read(&mut buf).err().expect("expected error").kind();
    //     assert!(kind == io::ErrorKind::WouldBlock || kind == io::ErrorKind::TimedOut);
    // }

    // #[test]
    // fn test_read_with_timeout() {
    //     let (_dir, socket_path) = or_panic!(tmpdir());

    //     let listener = or_panic!(UnixListener::bind(&socket_path));

    //     let mut stream = or_panic!(UnixStream::connect(&socket_path));
    //     or_panic!(stream.set_read_timeout(Some(Duration::from_millis(1000))));

    //     #[allow(unused_mut)]
    //     let mut other_end = or_panic!(listener.accept()).0;
    //     or_panic!(other_end.write_all(b"hello world"));

    //     let mut buf = [0; 11];
    //     or_panic!(stream.read(&mut buf));
    //     assert_eq!(b"hello world", &buf[..]);

    //     let kind = stream.read(&mut buf).err().expect("expected error").kind();
    //     assert!(kind == io::ErrorKind::WouldBlock || kind == io::ErrorKind::TimedOut);
    // }

    // // Ensure the `set_read_timeout` and `set_write_timeout` calls return errors
    // // when passed zero Durations
    // #[test]
    // fn test_unix_stream_timeout_zero_duration() {
    //     let (_dir, socket_path) = or_panic!(tmpdir());
    //     let listener = or_panic!(UnixListener::bind(&socket_path));
    //     let stream = or_panic!(UnixStream::connect(&socket_path));

    //     let result = stream.set_write_timeout(Some(Duration::new(0, 0)));
    //     let err = result.unwrap_err();
    //     assert_eq!(err.kind(), io:ErrorKind::InvalidInput);

    //     let result = stream.set_read_timeout(Some(Duration::new(0, 0)));
    //     let err = result.unwrap_err();
    //     assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

    //     drop(listener);
    // }

    #[test]
    fn abstract_namespace_not_allowed() {
        assert!(UnixStream::connect("\0asdf").is_err());
    }
}
