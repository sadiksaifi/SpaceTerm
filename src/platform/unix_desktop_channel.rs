//! The desktop channel's POSIX endpoint: a stream socket in an account-private directory named
//! by application identity and account, whose peers must run as this account.
#[cfg(target_os = "linux")]
use super::linux_peer_credentials as peer_credentials;
#[cfg(target_os = "macos")]
use super::macos_peer_credentials as peer_credentials;
use crate::desktop_channel::ChannelError;
use spaceterm_server::{Directory, ServerError};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

const SOCKET: &str = "d";

impl crate::desktop_channel::HalfClose for UnixStream {
    fn close_write(&mut self) -> std::io::Result<()> {
        self.shutdown(std::net::Shutdown::Write)
    }
}

fn directory(identity: &str) -> Result<PathBuf, ChannelError> {
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

/// Connects to this account's desktop of `identity`, refusing an endpoint that is not private
/// or a listener running as another account.
pub(crate) fn connect(identity: &str, timeout: Duration) -> Result<UnixStream, ChannelError> {
    connect_at(&directory(identity)?, timeout)
}

fn connect_at(path: &std::path::Path, timeout: Duration) -> Result<UnixStream, ChannelError> {
    let retained = Directory::open(path, false).map_err(|error| match error {
        ServerError::NotSetUp => ChannelError::Unavailable,
        ServerError::UnsafeStorage => ChannelError::UnsafeEndpoint,
        _ => ChannelError::Unavailable,
    })?;
    let stream = UnixStream::connect(path.join(SOCKET)).map_err(|_| ChannelError::Unavailable)?;
    retained
        .verify_path(path)
        .map_err(|_| ChannelError::UnsafeEndpoint)?;
    let peer = peer_credentials::peer_user(&stream).map_err(|_| ChannelError::AccountMismatch)?;
    same_account(peer, effective_user())?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|_| ChannelError::Protocol)?;
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

    /// How long the desktop waits for a connected client to finish its request.
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

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
            stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
            stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
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
            connect_at(&endpoint.0, Duration::from_secs(1)).err(),
            Some(ChannelError::Unavailable)
        );
        Directory::open(&endpoint.0, true).unwrap();
        assert_eq!(
            connect_at(&endpoint.0, Duration::from_secs(1)).err(),
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
            connect_at(&endpoint.0, Duration::from_secs(1)).err(),
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
                serve(stream, IDENTITY, |request| {
                    assert_eq!(request, show());
                    Reply::Done
                })
                .unwrap();
                listener
            });
            let client = connect_at(&endpoint.0, Duration::from_secs(5)).unwrap();
            assert_eq!(exchange(client, IDENTITY, &show()), Ok(Reply::Done));
            drop(served.join().unwrap());
            assert!(
                !endpoint.0.join(SOCKET).exists(),
                "the socket outlived its desktop"
            );
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
            let client = connect_at(&endpoint.0, Duration::from_secs(5)).unwrap();
            let request = Request::Open {
                session: SessionContext {
                    server: [1; 32],
                    pane: [2; 32],
                    session: [3; 32],
                },
                path: "/private/fixture".into(),
                preview: false,
            };
            assert_eq!(
                exchange(client, IDENTITY, &request),
                Err(ChannelError::Protocol)
            );
            assert!(
                accepted.join().unwrap(),
                "another account's request was accepted"
            );
        }
    }
}
