// extern crate iovec;
extern crate bytes;
extern crate mio;
extern crate tempdir;
extern crate mio_uds_windows;

// use std::io::prelude::*;
// use std::time::Duration;
use std::io::{self, Read, Write};
use std::sync::mpsc::channel;
use std::thread;

// use iovec::IoVec;
use bytes::{Buf, MutBuf};
use mio::*;
use mio_uds_windows::*;
use mio_uds_windows::net;
use tempdir::TempDir;

macro_rules! t {
    ($e:expr) => (match $e {
        Ok(e) => e,
        Err(e) => panic!("{} failed with {}", stringify!($e), e),
    })
}

 // tests adapted from mio::test::test_tcp

#[test]
fn accept() {
    struct H { hit: bool, listener: UnixListener, shutdown: bool }

    let td = t!(TempDir::new("uds"));
    let l = UnixListener::bind(td.path().join("foo")).unwrap();
    let addr = l.local_addr().unwrap();

    let t = thread::spawn(move || {
        net::UnixStream::connect(addr.as_pathname().unwrap()).unwrap();
    });

    let poll = Poll::new().unwrap();

    poll.register(&l, Token(1), Ready::readable(), PollOpt::edge()).unwrap();

    let mut events = Events::with_capacity(128);

    let mut h = H { hit: false, listener: l, shutdown: false };
    while !h.shutdown {
        poll.poll(&mut events, None).unwrap();

        for event in &events {
            h.hit = true;
            assert_eq!(event.token(), Token(1));
            assert!(event.readiness().is_readable());
            assert!(h.listener.accept().is_ok());
            h.shutdown = true;
        }
    }
    assert!(h.hit);
    assert!(h.listener.accept().unwrap_err().kind() == io::ErrorKind::WouldBlock);
    t.join().unwrap();
}

#[test]
fn connect() {
    struct H { hit: u32, shutdown: bool }

    let td = t!(TempDir::new("uds"));
    let l = net::UnixListener::bind(td.path().join("foo")).unwrap();
    let addr = l.local_addr().unwrap();

    let (tx, rx) = channel();
    let (tx2, rx2) = channel();
    let t = thread::spawn(move || {
        let s = l.accept().unwrap();
        rx.recv().unwrap();
        drop(s);
        tx2.send(()).unwrap();
    });

    let poll = Poll::new().unwrap();
    let s = UnixStream::connect(addr.as_pathname().unwrap()).unwrap();

    poll.register(&s, Token(1), Ready::readable() | Ready::writable(), PollOpt::edge()).unwrap();

    let mut events = Events::with_capacity(128);

    let mut h = H { hit: 0, shutdown: false };
    while !h.shutdown {
        poll.poll(&mut events, None).unwrap();

        for event in &events {
            assert_eq!(event.token(), Token(1));
            match h.hit {
                0 => assert!(event.readiness().is_writable()),
                1 => assert!(event.readiness().is_readable()),
                _ => panic!(),
            }
            h.hit += 1;
            h.shutdown = true;
        }
    }
    assert_eq!(h.hit, 1);
    tx.send(()).unwrap();
    rx2.recv().unwrap();
    h.shutdown = false;
    while !h.shutdown {
        poll.poll(&mut events, None).unwrap();

        for event in &events {
            assert_eq!(event.token(), Token(1));
            match h.hit {
                0 => assert!(event.readiness().is_writable()),
                1 => assert!(event.readiness().is_readable()),
                _ => panic!(),
            }
            h.hit += 1;
            h.shutdown = true;
        }
    }
    assert_eq!(h.hit, 2);
    t.join().unwrap();
}

#[test]
fn read() {
    const N: usize = 16 * 1024 * 1024;
    struct H { amt: usize, socket: UnixStream, shutdown: bool }

    let td = t!(TempDir::new("uds"));
    let l = net::UnixListener::bind(td.path().join("foo")).unwrap();
    let addr = l.local_addr().unwrap();

    let t = thread::spawn(move || {
        let mut s = l.accept().unwrap().0;
        let b = [0; 1024];
        let mut amt = 0;
        while amt < N {
            amt += s.write(&b).unwrap();
        }
    });

    let poll = Poll::new().unwrap();
    let s = UnixStream::connect(addr.as_pathname().unwrap()).unwrap();

    poll.register(&s, Token(1), Ready::readable(), PollOpt::edge()).unwrap();

    let mut events = Events::with_capacity(128);

    let mut h = H { amt: 0, socket: s, shutdown: false };
    while !h.shutdown {
        poll.poll(&mut events, None).unwrap();

        for event in &events {
            assert_eq!(event.token(), Token(1));
            let mut b = [0; 1024];
            loop {
                if let Some(amt) = h.socket.try_read(&mut b).unwrap() {
                    h.amt += amt;
                } else {
                    break
                }
                if h.amt >= N {
                    h.shutdown = true;
                    break
                }
            }
        }
    }
    t.join().unwrap();
}

/*
 *
 * ===== Helpers =====
 *
 */

pub trait TryRead {
    fn try_read_buf<B: MutBuf>(&mut self, buf: &mut B) -> io::Result<Option<usize>>
        where Self : Sized
    {
        // Reads the length of the slice supplied by buf.mut_bytes into the buffer
        // This is not guaranteed to consume an entire datagram or segment.
        // If your protocol is msg based (instead of continuous stream) you should
        // ensure that your buffer is large enough to hold an entire segment (1532 bytes if not jumbo
        // frames)
        let res = self.try_read(unsafe { buf.mut_bytes() });

        if let Ok(Some(cnt)) = res {
            unsafe { buf.advance(cnt); }
        }

        res
    }

    fn try_read(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>>;
}

pub trait TryWrite {
    fn try_write_buf<B: Buf>(&mut self, buf: &mut B) -> io::Result<Option<usize>>
        where Self : Sized
    {
        let res = self.try_write(buf.bytes());

        if let Ok(Some(cnt)) = res {
            buf.advance(cnt);
        }

        res
    }

    fn try_write(&mut self, buf: &[u8]) -> io::Result<Option<usize>>;
}

impl<T: Read> TryRead for T {
    fn try_read(&mut self, dst: &mut [u8]) -> io::Result<Option<usize>> {
        self.read(dst).map_non_block()
    }
}

impl<T: Write> TryWrite for T {
    fn try_write(&mut self, src: &[u8]) -> io::Result<Option<usize>> {
        self.write(src).map_non_block()
    }
}

/// A helper trait to provide the map_non_block function on Results.
trait MapNonBlock<T> {
    /// Maps a `Result<T>` to a `Result<Option<T>>` by converting
    /// operation-would-block errors into `Ok(None)`.
    fn map_non_block(self) -> io::Result<Option<T>>;
}

impl<T> MapNonBlock<T> for io::Result<T> {
    fn map_non_block(self) -> io::Result<Option<T>> {
        use std::io::ErrorKind::WouldBlock;

        match self {
            Ok(value) => Ok(Some(value)),
            Err(err) => {
                if let WouldBlock = err.kind() {
                    Ok(None)
                } else {
                    Err(err)
                }
            }
        }
    }
}

// #[test]
// fn listener() {
//     let td = t!(TempDir::new("uds"));
//     let a = t!(UnixListener::bind(td.path().join("foo")));
//     assert!(t!(a.accept()).is_none());
//     t!(a.local_addr());
//     assert!(t!(a.take_error()).is_none());
//     let b = t!(a.try_clone());
//     assert!(t!(b.accept()).is_none());

//     let poll = t!(Poll::new());
//     let mut events = Events::with_capacity(1024);

//     t!(poll.register(&a, Token(1), Ready::readable(), PollOpt::edge()));

//     let s = t!(UnixStream::connect(td.path().join("foo")));

//     assert_eq!(t!(poll.poll(&mut events, None)), 1);

//     let (s2, addr) = t!(a.accept()).unwrap();

//     assert_eq!(t!(s.peer_addr()).as_pathname(), t!(s2.local_addr()).as_pathname());
//     assert_eq!(t!(s.local_addr()).as_pathname(), t!(s2.peer_addr()).as_pathname());
//     assert_eq!(addr.as_pathname(), t!(s.local_addr()).as_pathname());
// }

// #[test]
// fn stream() {
//     let poll = t!(Poll::new());
//     let mut events = Events::with_capacity(1024);
//     let (mut a, mut b) = t!(UnixStream::pair());

//     let both = Ready::readable() | Ready::writable();
//     t!(poll.register(&a, Token(1), both, PollOpt::edge()));
//     t!(poll.register(&b, Token(2), both, PollOpt::edge()));

//     assert_eq!(t!(poll.poll(&mut events, Some(Duration::new(0, 0)))), 2);
//     assert_eq!(events.get(0).unwrap().readiness(), Ready::writable());
//     assert_eq!(events.get(1).unwrap().readiness(), Ready::writable());

//     assert_eq!(t!(a.write(&[3])), 1);

//     assert_eq!(t!(poll.poll(&mut events, Some(Duration::new(0, 0)))), 1);
//     assert!(events.get(0).unwrap().readiness().is_readable());
//     assert_eq!(events.get(0).unwrap().token(), Token(2));

//     assert_eq!(t!(b.read(&mut [0; 1024])), 1);
// }

// #[test]
// fn stream_iovec() {
//     let poll = t!(Poll::new());
//     let mut events = Events::with_capacity(1024);
//     let (a, b) = t!(UnixStream::pair());

//     let both = Ready::readable() | Ready::writable();
//     t!(poll.register(&a, Token(1), both, PollOpt::edge()));
//     t!(poll.register(&b, Token(2), both, PollOpt::edge()));

//     assert_eq!(t!(poll.poll(&mut events, Some(Duration::new(0, 0)))), 2);
//     assert_eq!(events.get(0).unwrap().readiness(), Ready::writable());
//     assert_eq!(events.get(1).unwrap().readiness(), Ready::writable());

//     let send = b"Hello, World!";
//     let vecs: [&IoVec;2] = [ (&send[..6]).into(),
//                              (&send[6..]).into() ];

//     assert_eq!(t!(a.write_bufs(&vecs)), send.len());

//     assert_eq!(t!(poll.poll(&mut events, Some(Duration::new(0, 0)))), 1);
//     assert!(events.get(0).unwrap().readiness().is_readable());
//     assert_eq!(events.get(0).unwrap().token(), Token(2));

//     let mut recv = [0; 13];
//     {
//         let (mut first, mut last) = recv.split_at_mut(6);
//         let mut vecs: [&mut IoVec;2] = [ first.into(), last.into() ];
//         assert_eq!(t!(b.read_bufs(&mut vecs)), send.len());
//     }
//     assert_eq!(&send[..], &recv[..]);
// }