// Copyright 2015 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![allow(non_camel_case_types)]

use std::io;
use std::mem;
use std::net::Shutdown;
use std::os::raw::{c_int, c_ulong};
use std::os::windows::io::{AsRawSocket, FromRawSocket, IntoRawSocket, RawSocket};
use std::ptr;
// use std::sync::Once;

use inner::{cvt, last_error};

use winapi::{
    shared::ntdef::HANDLE,
    shared::ws2def::{AF_UNIX, SOCKADDR, SOCK_STREAM, SO_ERROR, SOL_SOCKET},
    um::handleapi::SetHandleInformation,
    um::processthreadsapi::GetCurrentProcessId,
    um::winbase::HANDLE_FLAG_INHERIT,
    um::winsock2::{
        accept, closesocket, getsockopt as c_getsockopt, ioctlsocket, shutdown as c_shutdown,
        WSADuplicateSocketW, WSASocketW,
        FIONBIO, SD_BOTH, SD_RECEIVE, SD_SEND,
        INVALID_SOCKET, SOCKET, WSAPROTOCOL_INFOW, WSA_FLAG_OVERLAPPED,
    },
    um::ws2tcpip::socklen_t
};

pub struct Socket(SOCKET);

/// Checks whether the Windows socket interface has been started already, and
/// if not, starts it.
pub fn init() {
    // static START: Once = Once::new();

    // START.call_once(|| unsafe {
    //     let mut data: WSADATA = mem::zeroed();
    //     let ret = WSAStartup(0x202, // version 2.2
    //                             &mut data);
    //     assert_eq!(ret, 0);

    //     let _ = std::rt::at_exit(|| { WSACleanup(); });
    // });
}

#[doc(hidden)]
pub trait IsZero {
    fn is_zero(&self) -> bool;
}

macro_rules! impl_is_zero {
    ($($t:ident)*) => ($(impl IsZero for $t {
        fn is_zero(&self) -> bool {
            *self == 0
        }
    })*)
}

impl_is_zero! { i8 i16 i32 i64 isize u8 u16 u32 u64 usize }

fn cvt_z<I: IsZero>(i: I) -> io::Result<I> {
    if i.is_zero() {
        Err(io::Error::last_os_error())
    } else {
        Ok(i)
    }
}

impl Socket {
    pub fn new() -> io::Result<Socket> {
        let socket = unsafe {
            match WSASocketW(
                AF_UNIX,
                SOCK_STREAM,
                0,
                ptr::null_mut(),
                0,
                WSA_FLAG_OVERLAPPED,
            ) {
                INVALID_SOCKET => Err(last_error()),
                n => Ok(Socket(n)),
            }
        }?;
        socket.set_no_inherit()?;
        Ok(socket)
    }

    // socketpair() not supported on Windows
    // pub fn new_pair(fam: c_int, ty: c_int) -> io::Result<(Socket, Socket)> { ... }

    pub fn accept(&self, storage: *mut SOCKADDR, len: *mut c_int) -> io::Result<Socket> {
        let socket = unsafe {
            match accept(self.0, storage, len) {
                INVALID_SOCKET => Err(last_error()),
                n => Ok(Socket(n)),
            }
        }?;
        socket.set_no_inherit()?;
        Ok(socket)
    }

    pub fn duplicate(&self) -> io::Result<Socket> {
        let socket = unsafe {
            let mut info: WSAPROTOCOL_INFOW = mem::zeroed();
            cvt(WSADuplicateSocketW(
                self.0,
                GetCurrentProcessId(),
                &mut info,
            ))?;
            match WSASocketW(
                info.iAddressFamily,
                info.iSocketType,
                info.iProtocol,
                &mut info,
                0,
                WSA_FLAG_OVERLAPPED,
            ) {
                INVALID_SOCKET => Err(last_error()),
                n => Ok(Socket(n)),
            }
        }?;
        socket.set_no_inherit()?;
        Ok(socket)
    }

    fn set_no_inherit(&self) -> io::Result<()> {
        cvt_z(unsafe { SetHandleInformation(self.0 as HANDLE, HANDLE_FLAG_INHERIT, 0) })
            .map(|_| ())
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        let mut nonblocking = nonblocking as c_ulong;
        let r = unsafe { ioctlsocket(self.0, FIONBIO as c_int, &mut nonblocking) };
        if r == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        let how = match how {
            Shutdown::Write => SD_SEND,
            Shutdown::Read => SD_RECEIVE,
            Shutdown::Both => SD_BOTH,
        };
        cvt(unsafe { c_shutdown(self.0, how) })?;
        Ok(())
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        let raw: c_int = getsockopt(self, SOL_SOCKET, SO_ERROR)?;
        if raw == 0 {
            Ok(None)
        } else {
            Ok(Some(io::Error::from_raw_os_error(raw as i32)))
        }
    }
}

pub fn getsockopt<T: Copy>(sock: &Socket, opt: c_int,
                       val: c_int) -> io::Result<T> {
    unsafe {
        let mut slot: T = mem::zeroed();
        let mut len = mem::size_of::<T>() as socklen_t;
        cvt(c_getsockopt(sock.as_raw_socket() as usize, opt, val,
                          &mut slot as *mut _ as *mut _,
                          &mut len))?;
        assert_eq!(len as usize, mem::size_of::<T>());
        Ok(slot)
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        let _ = unsafe { closesocket(self.0) };
    }
}

impl AsRawSocket for Socket {
    fn as_raw_socket(&self) -> RawSocket {
        self.0 as RawSocket
    }
}

impl FromRawSocket for Socket {
    unsafe fn from_raw_socket(sock: RawSocket) -> Self {
        Socket(sock as SOCKET)
    }
}

impl IntoRawSocket for Socket {
    fn into_raw_socket(self) -> RawSocket {
        let ret = self.0 as RawSocket;
        mem::forget(self);
        ret
    }
}
