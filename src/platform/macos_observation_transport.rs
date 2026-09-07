use crate::observation::AcceptanceObservationError;
use crate::observation::ObservationTransport;
use std::io;
use std::{
    fs::Metadata,
    os::fd::AsRawFd,
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        net::UnixStream,
    },
    path::Path,
    time::Duration,
};
const SOCKET_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) fn connect(path: &Path) -> Result<Transport, AcceptanceObservationError> {
    if !path.is_absolute() {
        return Err(AcceptanceObservationError::InvalidSocket);
    }
    let Some(parent) = path.parent() else {
        return Err(AcceptanceObservationError::InvalidSocket);
    };
    let parent_metadata = parent.symlink_metadata()?;
    let socket_metadata = path.symlink_metadata()?;
    if parent_metadata.file_type().is_symlink()
        || !parent_metadata.file_type().is_dir()
        || !is_private_owner(&parent_metadata)
        || !socket_metadata.file_type().is_socket()
        || !is_private_owner(&socket_metadata)
    {
        return Err(AcceptanceObservationError::InvalidSocket);
    }
    let stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))?;
    set_close_on_exec(&stream)?;
    Ok(Transport(stream))
}

fn set_close_on_exec(stream: &UnixStream) -> io::Result<()> {
    let fd = stream.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn is_private_owner(metadata: &Metadata) -> bool {
    metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0
}

pub(crate) struct Transport(pub(crate) UnixStream);
fn closed_io(error: io::Error) -> io::Error {
    io::Error::from(error.kind())
}
impl io::Read for Transport {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        io::Read::read(&mut self.0, bytes).map_err(closed_io)
    }
}
impl io::Write for Transport {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        io::Write::write(&mut self.0, bytes).map_err(closed_io)
    }
    fn flush(&mut self) -> io::Result<()> {
        io::Write::flush(&mut self.0).map_err(closed_io)
    }
}
impl ObservationTransport for Transport {
    fn set_read_timeout(&self, value: Option<Duration>) -> Result<(), AcceptanceObservationError> {
        self.0.set_read_timeout(value).map_err(Into::into)
    }
    fn set_write_timeout(&self, value: Option<Duration>) -> Result<(), AcceptanceObservationError> {
        self.0.set_write_timeout(value).map_err(Into::into)
    }
    fn close(&self) -> Result<(), AcceptanceObservationError> {
        self.0
            .shutdown(std::net::Shutdown::Both)
            .map_err(Into::into)
    }
}

#[cfg(all(test, feature = "macos-native-tests"))]
mod tests {
    use super::*;
    use std::{
        os::unix::{fs::PermissionsExt, net::UnixListener},
        time::{SystemTime, UNIX_EPOCH},
    };
    #[test]
    fn private_transport_should_reject_public_socket_and_keep_close_on_exec() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("st-observer-{}-{unique}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("peer");
        let listener = UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let client = connect(&path).unwrap();
        let (server, _) = listener.accept().unwrap();
        assert_ne!(
            unsafe { libc::fcntl(client.0.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        drop(client);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(connect(&path).is_err());
        drop(server);
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }
}
