use crate::{ServerError, protocol, storage::Directory};
use spaceterm_directories::ServerDirectories;
use std::{
    ffi::CStr,
    fs,
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
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
    let stream = connect_socket(&paths.socket()).map_err(|_| ServerError::NotRunning)?;
    check_peer(&stream)?;
    Ok(stream)
}

#[derive(Clone, Copy)]
enum SocketUnavailable {
    Refused,
    Unavailable,
}

fn connect_socket(endpoint: &Path) -> Result<UnixStream, SocketUnavailable> {
    let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)
        .map_err(|_| SocketUnavailable::Unavailable)?;
    let address = socket2::SockAddr::unix(endpoint).map_err(|_| SocketUnavailable::Unavailable)?;
    socket
        .connect_timeout(&address, std::time::Duration::from_secs(2))
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::ConnectionRefused {
                SocketUnavailable::Refused
            } else {
                SocketUnavailable::Unavailable
            }
        })?;
    let descriptor: std::os::fd::OwnedFd = socket.into();
    Ok(UnixStream::from(descriptor))
}

pub(crate) fn serve(paths: &Paths) -> Result<(), ServerError> {
    let data = Directory::open(&paths.data, false)?;
    // Verify retained identity before changing any runtime state. Never generate a replacement.
    data.read()?;
    let runtime = Directory::open(&paths.runtime, true)?;
    let _lock = runtime.lock("server.lock", true)?;
    let socket = paths.socket();
    if let Some(identity) = runtime.socket_identity(c"s")? {
        runtime.verify_path(&paths.runtime)?;
        // Only a definite refusal proves staleness. Answering listeners and uncertain failures
        // remain untouched, even when their instance lock is missing or no longer held.
        if !matches!(connect_socket(&socket), Err(SocketUnavailable::Refused)) {
            return Err(ServerError::StorageUnavailable);
        }
        runtime.verify_path(&paths.runtime)?;
        runtime.remove_socket(c"s", identity)?;
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

#[cfg(test)]
mod startup_tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::{
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };

    const CHILD_TEST: &str = "host::startup_tests::server_start_child";
    const CHILD_ROOT: &str = "SPACETERM_SERVER_START_FIXTURE";

    struct Fixture {
        root: PathBuf,
        paths: Paths,
        device: ring::signature::Ed25519KeyPair,
        pin: [u8; 32],
    }
    impl Fixture {
        fn new() -> Self {
            let nonce = u64::from_be_bytes(protocol::random().unwrap()[..8].try_into().unwrap());
            let root = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("st530-{nonce:x}"));
            let paths = Paths {
                data: root.join("d"),
                runtime: root.join("r"),
            };
            let data = Directory::open(&paths.data, true).unwrap();
            let _lock = data.lock("setup.lock", false).unwrap();
            let device = protocol::key(&protocol::random().unwrap()).unwrap();
            let mut record = data.read_or_create().unwrap();
            let pin = protocol::public(&protocol::key(&record.seed).unwrap());
            record.paired.push(protocol::public(&device));
            data.write(&record).unwrap();
            Directory::open(&paths.runtime, true).unwrap();
            Self {
                root,
                paths,
                device,
                pin,
            }
        }
        fn start(&self) -> ChildGuard {
            ChildGuard(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", CHILD_TEST, "--ignored"])
                    .env(CHILD_ROOT, &self.root)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    #[ignore = "runs only as a child of the server startup tests"]
    fn server_start_child() {
        let root = PathBuf::from(std::env::var_os(CHILD_ROOT).expect("startup fixture root"));
        let result = serve(&Paths {
            data: root.join("d"),
            runtime: root.join("r"),
        });
        assert_eq!(result, Err(ServerError::StorageUnavailable));
    }

    #[test]
    fn startup_preserves_an_answering_listener_when_the_instance_lock_is_available() {
        let fixture = Fixture::new();
        let _listener = UnixListener::bind(fixture.paths.socket()).unwrap();
        fs::set_permissions(fixture.paths.socket(), fs::Permissions::from_mode(0o600)).unwrap();
        let original = fs::symlink_metadata(fixture.paths.socket()).unwrap().ino();
        let mut child = fixture.start();
        let deadline = Instant::now() + Duration::from_secs(2);
        let exit = loop {
            if let Some(exit) = child.0.try_wait().unwrap() {
                break Some(exit);
            }
            let current = fs::symlink_metadata(fixture.paths.socket()).unwrap().ino();
            if current != original || Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            fs::symlink_metadata(fixture.paths.socket()).unwrap().ino(),
            original,
            "startup must preserve the live socket's identity"
        );
        assert!(
            exit.is_some_and(|exit| exit.success()),
            "startup must refuse the competing endpoint"
        );
    }
    #[test]
    fn startup_replaces_a_stale_socket_and_authenticates_the_saved_device() {
        let fixture = Fixture::new();
        let stale = UnixListener::bind(fixture.paths.socket()).unwrap();
        fs::set_permissions(fixture.paths.socket(), fs::Permissions::from_mode(0o600)).unwrap();
        drop(stale);
        let _child = fixture.start();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut stream = loop {
            match UnixStream::connect(fixture.paths.socket()) {
                Ok(stream) => break stream,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(_) => panic!("the server did not replace its stale endpoint"),
            }
        };
        assert_eq!(
            protocol::authenticate_client(&mut stream, &fixture.device, &fixture.pin, &[2, 1]),
            Ok(crate::AuthenticatedStatus { protocol_major: 2 })
        );
    }

    #[test]
    fn stale_socket_removal_preserves_a_replacement_entry() {
        let fixture = Fixture::new();
        let runtime = Directory::open(&fixture.paths.runtime, false).unwrap();
        let _lock = runtime.lock("server.lock", true).unwrap();
        let stale = UnixListener::bind(fixture.paths.socket()).unwrap();
        fs::set_permissions(fixture.paths.socket(), fs::Permissions::from_mode(0o600)).unwrap();
        let expected = runtime.socket_identity(c"s").unwrap().unwrap();
        drop(stale);
        fs::rename(fixture.paths.socket(), fixture.paths.runtime.join("old")).unwrap();
        let _replacement = UnixListener::bind(fixture.paths.socket()).unwrap();
        fs::set_permissions(fixture.paths.socket(), fs::Permissions::from_mode(0o600)).unwrap();
        let replacement = fs::symlink_metadata(fixture.paths.socket()).unwrap().ino();
        assert_eq!(
            runtime.remove_socket(c"s", expected),
            Err(ServerError::UnsafeStorage)
        );
        assert_eq!(
            fs::symlink_metadata(fixture.paths.socket()).unwrap().ino(),
            replacement
        );
    }
}
