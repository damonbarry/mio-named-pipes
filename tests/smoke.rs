extern crate mio;
extern crate mio_uds_windows;
extern crate env_logger;
extern crate tempdir;
extern crate winapi;

#[macro_use]
extern crate log;

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::path::{Path, PathBuf};
use std::time::Duration;

use mio::{Poll, Ready, Token, PollOpt, Events};
use mio_uds_windows::{UnixListener, UnixStream};
use tempdir::TempDir;

macro_rules! t {
    ($e:expr) => (match $e {
        Ok(e) => e,
        Err(e) => panic!("{} failed with {}", stringify!($e), e),
    })
}

struct TempPath(TempDir);

impl TempPath {
    fn new() -> Result<TempPath, io::Error> {
        let dir = TempDir::new("mio-uds-windows")?;
        Ok(TempPath(dir))
    }

    fn name(&self) -> PathBuf {
        self.0.path().join("sock")
    }
}

fn server() -> (UnixListener, TempPath) {
    let path = t!(TempPath::new());
    let stream = t!(UnixListener::bind(path.name()));
    (stream, path)
}

fn client(path: &Path) -> UnixStream {
    t!(UnixStream::connect(path))
}

fn session() -> (UnixListener, UnixStream) {
    let (server, path) = server();
    (server, client(&path.name()))
}

#[test]
fn writable_after_register() {
    drop(env_logger::init());

    let (server, client) = session();
    let poll = t!(Poll::new());
    t!(poll.register(&server,
                     Token(0),
                     Ready::writable() | Ready::readable(),
                     PollOpt::edge()));
    t!(poll.register(&client,
                     Token(1),
                     Ready::writable(),
                     PollOpt::edge()));

    let mut events = Events::with_capacity(128);
    t!(poll.poll(&mut events, None));

    let events = events.iter().collect::<Vec<_>>();
    debug!("events {:?}", events);
    assert!(events.iter().any(|e| {
        e.token() == Token(0) && e.readiness() == Ready::writable()
    }));
    assert!(events.iter().any(|e| {
        e.token() == Token(1) && e.readiness() == Ready::writable()
    }));
}

#[test]
fn write_then_read() {
    drop(env_logger::init());

    let (server, mut client) = session();
    let poll = t!(Poll::new());
    t!(poll.register(&server,
                     Token(0),
                     Ready::readable() | Ready::writable(),
                     PollOpt::edge()));
    t!(poll.register(&client,
                     Token(1),
                     Ready::readable() | Ready::writable(),
                     PollOpt::edge()));

    let mut events = Events::with_capacity(128);

    loop {
        t!(poll.poll(&mut events, None));
        let events = events.iter().collect::<Vec<_>>();
        debug!("events {:?}", events);
        if let Some(event) = events.iter().find(|e| e.token() == Token(0)) {
            if event.readiness().is_readable() {
                break
            }
        }
    }

    let (mut conn, _) = server.accept().unwrap();

    assert_eq!(t!(client.write(b"1234")), 4);

    loop {
        t!(poll.poll(&mut events, None));
        let events = events.iter().collect::<Vec<_>>();
        debug!("events {:?}", events);
        if let Some(event) = events.iter().find(|e| e.token() == Token(0)) {
            if event.readiness().is_readable() {
                break
            }
        }
    }

    let mut buf = [0; 10];
    assert_eq!(t!(conn.read(&mut buf)), 4);
    assert_eq!(&buf[..4], b"1234");
}

#[test]
fn accept_before_client() {
    drop(env_logger::init());

    let (server, path) = server();
    let poll = t!(Poll::new());
    t!(poll.register(&server,
                     Token(0),
                     Ready::readable() | Ready::writable(),
                     PollOpt::edge()));

    let mut events = Events::with_capacity(128);
    t!(poll.poll(&mut events, Some(Duration::new(0, 0))));
    let e = events.iter().collect::<Vec<_>>();
    debug!("events {:?}", e);
    assert_eq!(e.len(), 0);
    assert_eq!(server.accept().err().unwrap().kind(),
               io::ErrorKind::WouldBlock);

    let client = client(&path.name());
    t!(poll.register(&client,
                     Token(1),
                     Ready::readable() | Ready::writable(),
                     PollOpt::edge()));
    loop {
        t!(poll.poll(&mut events, None));
        let e = events.iter().collect::<Vec<_>>();
        debug!("events {:?}", e);
        if let Some(event) = e.iter().find(|e| e.token() == Token(0)) {
            if event.readiness().is_writable() {
                break
            }
        }
    }
}

#[test]
fn accept_after_client() {
    drop(env_logger::init());

    let (server, path) = server();
    let poll = t!(Poll::new());
    t!(poll.register(&server,
                     Token(0),
                     Ready::readable() | Ready::writable(),
                     PollOpt::edge()));

    let mut events = Events::with_capacity(128);
    t!(poll.poll(&mut events, Some(Duration::new(0, 0))));
    let e = events.iter().collect::<Vec<_>>();
    debug!("events {:?}", e);
    assert_eq!(e.len(), 0);

    let client = client(&path.name());
    t!(poll.register(&client,
                     Token(1),
                     Ready::readable() | Ready::writable(),
                     PollOpt::edge()));
    t!(server.accept());
    loop {
        t!(poll.poll(&mut events, None));
        let e = events.iter().collect::<Vec<_>>();
        debug!("events {:?}", e);
        if let Some(event) = e.iter().find(|e| e.token() == Token(0)) {
            if event.readiness().is_writable() {
                break
            }
        }
    }
}

#[test]
fn write_then_drop() {
    drop(env_logger::init());

    let (server, mut client) = session();
    let (mut conn, _) = server.accept().unwrap();
    let poll = t!(Poll::new());
    t!(poll.register(&server,
                     Token(0),
                     Ready::readable() | Ready::writable(),
                     PollOpt::edge()));
    t!(poll.register(&client,
                     Token(1),
                     Ready::readable() | Ready::writable(),
                     PollOpt::edge()));
    assert_eq!(t!(client.write(b"1234")), 4);
    drop(client);

    let mut events = Events::with_capacity(128);

    loop {
        t!(poll.poll(&mut events, None));
        let events = events.iter().collect::<Vec<_>>();
        debug!("events {:?}", events);
        if let Some(event) = events.iter().find(|e| e.token() == Token(0)) {
            if event.readiness().is_readable() {
                break
            }
        }
    }

    let mut buf = [0; 10];
    assert_eq!(t!(conn.read(&mut buf)), 4);
    assert_eq!(&buf[..4], b"1234");
}

#[test]
fn connect_twice() {
    drop(env_logger::init());

    let (server, path) = server();
    let (mut conn1, _) = server.accept().unwrap();
    let c1 = client(&path.name());
    let poll = t!(Poll::new());
    t!(poll.register(&server,
                     Token(0),
                     Ready::readable() | Ready::writable(),
                     PollOpt::edge()));
    t!(poll.register(&c1,
                     Token(1),
                     Ready::readable() | Ready::writable(),
                     PollOpt::edge()));
    drop(c1);

    let mut events = Events::with_capacity(128);

    loop {
        t!(poll.poll(&mut events, None));
        let events = events.iter().collect::<Vec<_>>();
        debug!("events {:?}", events);
        if let Some(event) = events.iter().find(|e| e.token() == Token(0)) {
            if event.readiness().is_readable() {
                break
            }
        }
    }

    let mut buf = [0; 10];
    assert_eq!(t!(conn1.read(&mut buf)), 0);
    t!(conn1.shutdown(Shutdown::Both));
    assert_eq!(server.accept().err().unwrap().kind(),
               io::ErrorKind::WouldBlock);

    let c2 = client(&path.name());
    let _conn2 = server.accept().unwrap();
    t!(poll.register(&c2,
                     Token(2),
                     Ready::readable() | Ready::writable(),
                     PollOpt::edge()));

    loop {
        t!(poll.poll(&mut events, None));
        let events = events.iter().collect::<Vec<_>>();
        debug!("events {:?}", events);
        if let Some(event) = events.iter().find(|e| e.token() == Token(0)) {
            if event.readiness().is_writable() {
                break
            }
        }
    }
}
