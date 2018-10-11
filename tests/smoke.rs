// extern crate iovec;
extern crate mio;
extern crate tempdir;
extern crate mio_uds_windows;

// use std::io::prelude::*;
// use std::time::Duration;
use std::io;
use std::sync::mpsc::channel;
use std::thread;

// use iovec::IoVec;
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