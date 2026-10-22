use crate::ServerError;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;

pub(crate) trait Transport: Read + Write + Send {
    fn read_fd(&self) -> RawFd;
    fn write_fd(&self) -> RawFd;
}
impl<T: Transport + ?Sized> Transport for Box<T> {
    fn read_fd(&self) -> RawFd {
        (**self).read_fd()
    }
    fn write_fd(&self) -> RawFd {
        (**self).write_fd()
    }
}

impl Transport for UnixStream {
    fn read_fd(&self) -> RawFd {
        self.as_raw_fd()
    }
    fn write_fd(&self) -> RawFd {
        self.as_raw_fd()
    }
}

pub(crate) fn nonblocking(fd: RawFd) -> Result<(), ServerError> {
    // SAFETY: fcntl only changes the status flags of the borrowed live descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(ServerError::Protocol);
    }
    Ok(())
}
