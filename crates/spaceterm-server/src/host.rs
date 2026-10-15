use crate::{ServerError, protocol, storage::Directory};
use spaceterm_directories::ServerDirectories;
use std::{
    ffi::CStr,
    fs,
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::PathBuf,
};

#[derive(Clone)]
pub(crate) struct Paths {
    pub data: PathBuf,
    pub runtime: PathBuf,
}
impl Paths {
    pub fn for_identity(identity: &str) -> Result<Self, ServerError> {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return Err(ServerError::UnsafeStorage);
        }
        // Use the owning account, never HOME/XDG/TMPDIR from a client or service environment.
        let mut record: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0; 65536];
        // SAFETY: all pointers refer to appropriately sized writable storage retained through use.
        let code = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                &mut record,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if code != 0 || result.is_null() || record.pw_dir.is_null() {
            return Err(ServerError::StorageUnavailable);
        }
        // SAFETY: successful getpwuid_r stores a terminated string within buffer, retained here.
        let home = unsafe { CStr::from_ptr(record.pw_dir) };
        let home = PathBuf::from(std::ffi::OsStr::from_bytes(home.to_bytes()));
        // SAFETY: geteuid has no preconditions.
        let directories =
            ServerDirectories::for_account(&home, unsafe { libc::geteuid() }, identity)
                .map_err(|_| ServerError::StorageUnavailable)?;
        Ok(Self {
            data: directories.data,
            runtime: directories.runtime,
        })
    }
    pub fn socket(&self) -> PathBuf {
        self.runtime.join("s")
    }
}

pub(crate) fn check_peer(stream: &UnixStream) -> Result<(), ServerError> {
    #[cfg(target_os = "macos")]
    let user = {
        let mut user = 0;
        let mut group = 0;
        // SAFETY: getpeereid writes two uid/gid outputs for the retained socket.
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut user, &mut group) } != 0 {
            return Err(ServerError::AuthenticationRefused);
        }
        user
    };
    #[cfg(target_os = "linux")]
    let user = {
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: output pointer and length describe writable ucred; the socket is retained.
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&raw mut credentials).cast(),
                &mut length,
            )
        } != 0
            || length as usize != std::mem::size_of::<libc::ucred>()
        {
            return Err(ServerError::AuthenticationRefused);
        }
        credentials.uid
    };
    check_account(user)
}
fn check_account(user: libc::uid_t) -> Result<(), ServerError> {
    // SAFETY: geteuid has no preconditions.
    if user != unsafe { libc::geteuid() } {
        return Err(ServerError::AuthenticationRefused);
    }
    Ok(())
}

#[cfg(feature = "client")]
pub(crate) fn connect(paths: &Paths) -> Result<UnixStream, ServerError> {
    Directory::open(&paths.runtime, false).map_err(|error| {
        if error == ServerError::NotPaired {
            ServerError::NotRunning
        } else {
            error
        }
    })?;
    let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)
        .map_err(|_| ServerError::NotRunning)?;
    let address =
        socket2::SockAddr::unix(paths.socket()).map_err(|_| ServerError::UnsafeStorage)?;
    socket
        .connect_timeout(&address, std::time::Duration::from_secs(2))
        .map_err(|_| ServerError::NotRunning)?;
    let descriptor: std::os::fd::OwnedFd = socket.into();
    let stream = UnixStream::from(descriptor);
    check_peer(&stream)?;
    Ok(stream)
}

pub(crate) fn serve(paths: &Paths) -> Result<(), ServerError> {
    let data = Directory::open(&paths.data, false)?;
    // Verify retained identity before changing any runtime state. Never generate a replacement.
    data.read()?;
    let runtime = Directory::open(&paths.runtime, true)?;
    let _lock = runtime.lock("server.lock", true)?;
    let socket = paths.socket();
    if let Ok(meta) = fs::symlink_metadata(&socket) {
        // SAFETY: geteuid has no preconditions.
        if meta.uid() != unsafe { libc::geteuid() }
            || !meta.file_type().is_socket()
            || meta.mode() & 0o7777 != 0o600
        {
            return Err(ServerError::UnsafeStorage);
        }
        fs::remove_file(&socket).map_err(|_| ServerError::StorageUnavailable)?;
    }
    let listener = UnixListener::bind(&socket).map_err(|_| ServerError::StorageUnavailable)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
        .map_err(|_| ServerError::StorageUnavailable)?;
    // A bounded synchronous exchange avoids unbounded worker allocation. Every connection has
    // an absolute five-second deadline; no unauthenticated request can perform an operation.
    for connection in listener.incoming() {
        let Ok(mut stream) = connection else { continue };
        if check_peer(&stream).is_err() {
            continue;
        }
        let Ok(record) = data.read() else { continue };
        let Ok(key) = protocol::key(&record.seed) else {
            continue;
        };
        let _ = protocol::authenticate_server(&mut stream, &key, &record.paired);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_another_account_before_protocol_processing() {
        // SAFETY: geteuid has no preconditions.
        assert_eq!(
            check_account(unsafe { libc::geteuid() }.wrapping_add(1)),
            Err(ServerError::AuthenticationRefused)
        );
    }
    #[test]
    fn validates_real_private_ipc_peers() {
        let (client, server) = UnixStream::pair().unwrap();
        check_peer(&client).unwrap();
        check_peer(&server).unwrap();
    }
}
