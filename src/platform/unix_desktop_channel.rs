//! The desktop channel's POSIX endpoint: a stream socket in a private directory per application
//! identity, inside the operating system's own directory for this account, whose peers must run
//! as this account.
#[cfg(target_os = "linux")]
use super::linux_peer_credentials as peer_credentials;
#[cfg(target_os = "macos")]
use super::macos_peer_credentials as peer_credentials;
use crate::desktop_channel::{ChannelError, Deadline};
use spaceterm_server::{Directory, ServerError};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

const SOCKET: &str = "d";

impl crate::desktop_channel::ChannelIo for UnixStream {
    fn close_write(&mut self) -> std::io::Result<()> {
        self.shutdown(std::net::Shutdown::Write)
    }
    fn limit_read(&mut self, wait: Duration) -> std::io::Result<()> {
        ready(self, libc::POLLIN, wait)
    }
    fn limit_write(&mut self, wait: Duration) -> std::io::Result<()> {
        ready(self, libc::POLLOUT, wait)
    }
}

/// Waits at most `wait` for `events` on a stream that never blocks. Socket timeouts cannot bound
/// this: some hosts refuse to change them once the client half-closed.
fn ready(stream: &UnixStream, events: libc::c_short, wait: Duration) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    stream.set_nonblocking(true)?;
    let mut descriptor = libc::pollfd {
        fd: stream.as_raw_fd(),
        events,
        revents: 0,
    };
    let milliseconds = libc::c_int::try_from(wait.as_millis().max(1)).unwrap_or(libc::c_int::MAX);
    // SAFETY: one live pollfd for the stream's open descriptor, which outlives the call.
    if unsafe { libc::poll(&mut descriptor, 1, milliseconds) } < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}

/// Nanoseconds since boot on the clock `Instant` reads: `CLOCK_UPTIME_RAW` on macOS and
/// `CLOCK_MONOTONIC` on Linux. Every process on the machine reads the same value.
pub(crate) fn monotonic_nanoseconds() -> u64 {
    #[cfg(target_os = "macos")]
    const CLOCK: libc::clockid_t = libc::CLOCK_UPTIME_RAW;
    #[cfg(target_os = "linux")]
    const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: writes only the live timespec above.
    unsafe { libc::clock_gettime(CLOCK, &mut now) };
    u64::try_from(now.tv_sec)
        .unwrap_or(0)
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::try_from(now.tv_nsec).unwrap_or(0))
}

pub(crate) fn directory(identity: &str) -> Result<PathBuf, ChannelError> {
    super::app_directories::desktop_channel_directory(effective_user(), identity)
        .map_err(|_| ChannelError::Unavailable)
}

fn effective_user() -> libc::uid_t {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

/// Only a peer running as this process's own account may use the channel.
pub(crate) fn same_account(peer: libc::uid_t, own: libc::uid_t) -> Result<(), ChannelError> {
    if peer == own {
        Ok(())
    } else {
        Err(ChannelError::AccountMismatch)
    }
}

/// Connects to this account's desktop of `identity` by `deadline`, refusing an endpoint that is
/// not private or a listener running as another account.
pub(crate) fn connect(identity: &str, deadline: Deadline<'_>) -> Result<UnixStream, ChannelError> {
    connect_at(&directory(identity)?, deadline)
}

fn connect_at(path: &std::path::Path, deadline: Deadline<'_>) -> Result<UnixStream, ChannelError> {
    let retained = Directory::open(path, false).map_err(|error| match error {
        ServerError::NotSetUp => ChannelError::Unavailable,
        ServerError::UnsafeStorage => ChannelError::UnsafeEndpoint,
        _ => ChannelError::Unavailable,
    })?;
    let wait = deadline().saturating_duration_since(std::time::Instant::now());
    if wait.is_zero() {
        return Err(ChannelError::Timeout);
    }
    let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)
        .map_err(|_| ChannelError::Unavailable)?;
    let address =
        socket2::SockAddr::unix(path.join(SOCKET)).map_err(|_| ChannelError::Unavailable)?;
    socket
        .connect_timeout(&address, wait)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => ChannelError::Timeout,
            _ => ChannelError::Unavailable,
        })?;
    let stream = UnixStream::from(std::os::fd::OwnedFd::from(socket));
    retained
        .verify_path(path)
        .map_err(|_| ChannelError::UnsafeEndpoint)?;
    let peer = peer_credentials::peer_user(&stream).map_err(|_| ChannelError::AccountMismatch)?;
    same_account(peer, effective_user())?;
    Ok(stream)
}

#[cfg(feature = "desktop")]
pub(crate) use listener::DesktopChannelListener;

#[cfg(feature = "desktop")]
mod listener {
    use super::*;
    use spaceterm_server::Lock;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    /// The one listening desktop of an application identity for this account. A second desktop
    /// process finds the lock held and listens nowhere.
    pub(crate) struct DesktopChannelListener {
        listener: UnixListener,
        socket: PathBuf,
        peer: fn(&UnixStream) -> std::io::Result<libc::uid_t>,
        own: libc::uid_t,
        _lock: Lock,
        _directory: Directory,
    }

    impl DesktopChannelListener {
        pub(crate) fn bind(identity: &str) -> Result<Self, ChannelError> {
            Self::bind_at(&directory(identity)?)
        }

        pub(super) fn bind_at(path: &std::path::Path) -> Result<Self, ChannelError> {
            let retained = Directory::open(path, true).map_err(|error| match error {
                ServerError::UnsafeStorage => ChannelError::UnsafeEndpoint,
                _ => ChannelError::Unavailable,
            })?;
            let lock = retained
                .try_lock("lock")
                .map_err(|_| ChannelError::UnsafeEndpoint)?
                .ok_or(ChannelError::Unavailable)?;
            // The lock proves that no other desktop of this identity listens, so a remaining
            // socket is stale.
            let socket = path.join(SOCKET);
            retained
                .remove(c"d")
                .map_err(|_| ChannelError::UnsafeEndpoint)?;
            let listener = UnixListener::bind(&socket).map_err(|_| ChannelError::Unavailable)?;
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| ChannelError::Unavailable)?;
            retained
                .verify_path(path)
                .map_err(|_| ChannelError::UnsafeEndpoint)?;
            Ok(Self {
                listener,
                socket,
                peer: peer_credentials::peer_user,
                own: effective_user(),
                _lock: lock,
                _directory: retained,
            })
        }

        #[cfg(test)]
        pub(crate) fn with_peer(
            mut self,
            peer: fn(&UnixStream) -> std::io::Result<libc::uid_t>,
        ) -> Self {
            self.peer = peer;
            self
        }
    }

    impl crate::desktop_channel::DesktopChannelEndpoint for DesktopChannelListener {
        fn accept(
            &self,
        ) -> std::io::Result<Option<Box<dyn crate::desktop_channel::ChannelStream>>> {
            let (stream, _) = self.listener.accept()?;
            let Ok(peer) = (self.peer)(&stream) else {
                return Ok(None);
            };
            if same_account(peer, self.own).is_err() {
                return Ok(None);
            }
            Ok(Some(Box::new(stream)))
        }
    }

    impl Drop for DesktopChannelListener {
        fn drop(&mut self) {
            // The held lock still proves that this socket is this listener's own.
            let _ = std::fs::remove_file(&self.socket);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_same_account_may_use_the_channel() {
        assert_eq!(same_account(501, 501), Ok(()));
        for other in [0, 502] {
            assert_eq!(same_account(other, 501), Err(ChannelError::AccountMismatch));
        }
    }

    /// A private endpoint directory beneath a temporary root without symbolic links, so tests
    /// never reach a running desktop's endpoint.
    struct Endpoint(PathBuf);

    impl Endpoint {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let root = std::env::temp_dir().canonicalize().unwrap();
            Self(root.join(format!(
                "spaceterm-desktop-channel-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )))
        }
    }

    impl Drop for Endpoint {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn connecting_without_a_listening_desktop_is_unavailable() {
        let endpoint = Endpoint::new();
        assert_eq!(
            connect_at(&endpoint.0, &|| std::time::Instant::now()
                + Duration::from_secs(1))
            .err(),
            Some(ChannelError::Unavailable)
        );
        Directory::open(&endpoint.0, true).unwrap();
        assert_eq!(
            connect_at(&endpoint.0, &|| std::time::Instant::now()
                + Duration::from_secs(1))
            .err(),
            Some(ChannelError::Unavailable)
        );
    }

    #[test]
    fn a_shared_endpoint_directory_is_refused_before_connecting() {
        use std::os::unix::fs::PermissionsExt;
        let endpoint = Endpoint::new();
        std::fs::create_dir(&endpoint.0).unwrap();
        std::fs::set_permissions(&endpoint.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            connect_at(&endpoint.0, &|| std::time::Instant::now()
                + Duration::from_secs(1))
            .err(),
            Some(ChannelError::UnsafeEndpoint)
        );
    }

    #[cfg(feature = "desktop")]
    mod listening {
        use super::*;
        use crate::desktop_channel::{
            DesktopChannelEndpoint, Reply, Request, SessionContext, exchange, serve,
        };

        const IDENTITY: &str = "spaceterm-development";

        fn show() -> Request {
            Request::Show {
                server: [1; 32],
                workspace: [2; 32],
                tab: None,
                pane: None,
            }
        }

        #[test]
        fn a_desktop_answers_a_client_of_its_own_account() {
            let endpoint = Endpoint::new();
            let listener = DesktopChannelListener::bind_at(&endpoint.0).unwrap();
            let served = std::thread::spawn(move || {
                let stream = listener.accept().unwrap().expect("the same account");
                let request_by = std::time::Instant::now() + Duration::from_secs(5);
                serve(stream, IDENTITY, request_by, |request, _| {
                    assert_eq!(request, show());
                    Some(Reply::Done)
                })
                .unwrap();
                listener
            });
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let client = connect_at(&endpoint.0, &|| deadline).unwrap();
            assert_eq!(
                exchange(client, IDENTITY, &show(), &|| deadline),
                Ok(Reply::Done)
            );
            drop(served.join().unwrap());
            assert!(
                !endpoint.0.join(SOCKET).exists(),
                "the socket outlived its desktop"
            );
        }

        /// The listener serves one connection at a time, so a request can wait behind a slow
        /// one. Its client's expiry still holds when the desktop finally reads it.
        #[test]
        fn a_request_queued_past_its_clients_deadline_never_acts() {
            let endpoint = Endpoint::new();
            let listener = DesktopChannelListener::bind_at(&endpoint.0).unwrap();
            let acted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let served = {
                let acted = std::sync::Arc::clone(&acted);
                std::thread::spawn(move || {
                    for slow in [true, false] {
                        let stream = listener.accept().unwrap().unwrap();
                        let request_by = std::time::Instant::now() + Duration::from_secs(5);
                        let _ = serve(stream, IDENTITY, request_by, |request, _| {
                            acted.lock().unwrap().push(request);
                            if slow {
                                std::thread::sleep(Duration::from_millis(800));
                            }
                            Some(Reply::Done)
                        });
                    }
                })
            };
            let first_deadline = std::time::Instant::now() + Duration::from_secs(5);
            let first = std::thread::spawn({
                let path = endpoint.0.clone();
                move || {
                    let client = connect_at(&path, &|| first_deadline).unwrap();
                    exchange(client, IDENTITY, &show(), &|| first_deadline)
                }
            });
            std::thread::sleep(Duration::from_millis(100));
            let queued = Request::Show {
                server: [1; 32],
                workspace: [3; 32],
                tab: None,
                pane: None,
            };
            let deadline = std::time::Instant::now() + Duration::from_millis(400);
            let client = connect_at(&endpoint.0, &|| deadline).unwrap();
            assert_eq!(
                exchange(client, IDENTITY, &queued, &|| deadline),
                Err(ChannelError::Timeout)
            );
            assert_eq!(first.join().unwrap(), Ok(Reply::Done));
            served.join().unwrap();
            assert_eq!(*acted.lock().unwrap(), [show()], "the queued request acted");
        }

        #[test]
        fn a_second_desktop_of_the_same_identity_listens_nowhere() {
            let endpoint = Endpoint::new();
            let first = DesktopChannelListener::bind_at(&endpoint.0).unwrap();
            assert_eq!(
                DesktopChannelListener::bind_at(&endpoint.0).err(),
                Some(ChannelError::Unavailable)
            );
            drop(first);
            // A desktop that ended without removing its socket leaves it stale.
            std::os::unix::net::UnixListener::bind(endpoint.0.join(SOCKET)).unwrap();
            DesktopChannelListener::bind_at(&endpoint.0).unwrap();
        }

        #[test]
        fn a_caller_from_another_account_is_closed_unread() {
            let endpoint = Endpoint::new();
            let listener = DesktopChannelListener::bind_at(&endpoint.0)
                .unwrap()
                .with_peer(|_| Ok(effective_user().wrapping_add(1)));
            let accepted = std::thread::spawn(move || listener.accept().unwrap().is_none());
            // The desktop may close the connection before a client could configure it, so this
            // client connects without the CLI's own checks.
            let client = UnixStream::connect(endpoint.0.join(SOCKET)).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let request = Request::Open {
                session: SessionContext {
                    server: [1; 32],
                    pane: [2; 32],
                    session: [3; 32],
                },
                path: "/private/fixture".into(),
                preview: false,
            };
            // The desktop closes before reading, so the request never arrives whole, or arrives
            // and gets no answer.
            assert!(matches!(
                exchange(client, IDENTITY, &request, &|| deadline),
                Err(ChannelError::Protocol | ChannelError::Unanswered)
            ));
            assert!(
                accepted.join().unwrap(),
                "another account's request was accepted"
            );
        }
    }
}
