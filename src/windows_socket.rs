//! Windows socket readiness for bounded synchronous protocol exchanges.

use std::io;
use std::net::TcpStream;
use std::os::windows::io::AsRawSocket;
use std::time::Duration;
use windows_sys::Win32::Networking::WinSock::{
    WSAGetLastError, WSAPoll, POLLERR, POLLHUP, POLLNVAL, POLLRDNORM, SOCKET_ERROR, WSAPOLLFD,
};

pub(crate) fn wait_readable(
    stream: &TcpStream,
    timeout: Duration,
    protocol: &str,
) -> io::Result<()> {
    let millis = timeout.as_millis().clamp(1, i32::MAX as u128) as i32;
    let mut descriptor = WSAPOLLFD {
        fd: stream.as_raw_socket() as _,
        events: POLLRDNORM,
        revents: 0,
    };
    // SAFETY: descriptor points to one initialized WSAPOLLFD for the live TcpStream.
    let result = unsafe { WSAPoll(&mut descriptor, 1, millis) };
    if result == SOCKET_ERROR {
        // SAFETY: WSAGetLastError reads the calling thread's last Winsock error.
        return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
    }
    if result == 0 {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("{protocol} socket poll timed out"),
        ));
    }
    if descriptor.revents & POLLNVAL != 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            format!("{protocol} socket is invalid"),
        ));
    }
    if descriptor.revents & POLLERR != 0 {
        return Err(stream.take_error()?.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::ConnectionAborted,
                format!("{protocol} socket poll failed"),
            )
        }));
    }
    if descriptor.revents & (POLLRDNORM | POLLHUP) != 0 {
        Ok(())
    } else {
        Err(io::Error::from(io::ErrorKind::WouldBlock))
    }
}
