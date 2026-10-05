//! POSIX AskPass endpoint binding and peer authentication.

use super::app_paths::{
    ASKPASS_RUNTIME_OWNER_KIND, ASKPASS_RUNTIME_SOCKET_NAME, AppPaths, RegisteredRuntimeSocket,
    RuntimeOwner,
};
use super::askpass::{
    AskPassHelperConnector, AskPassLocalAccept, AskPassLocalIpc, AskPassLocalListener,
    AskPassUnavailable, BROKER_CANCELLATION_POLL_INTERVAL, BoundAskPassEndpoint,
    GpuiAskPassBrokerFactory as PortableAskPassBrokerFactory,
    dispatch_helper_from_environment as dispatch_portable_helper,
};
#[cfg(target_os = "linux")]
use super::linux_peer_credentials as peer_credentials;
#[cfg(target_os = "macos")]
use super::macos_peer_credentials as peer_credentials;
use gpui::{App, Window};
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const CONNECTION_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const BROKER_PID_HEX_BYTES: usize = 8;
const AUTHENTICATED_ENDPOINT_PREFIX_BYTES: usize = BROKER_PID_HEX_BYTES + 1;

struct UnixAskPassLocalIpc;

impl AskPassLocalIpc for UnixAskPassLocalIpc {
    fn bind(&self, paths: &AppPaths) -> Result<BoundAskPassEndpoint, AskPassUnavailable> {
        let runtime_owner = paths
            .create_runtime_owner(ASKPASS_RUNTIME_OWNER_KIND)
            .map_err(|_| AskPassUnavailable)?;
        let socket_path = runtime_owner
            .socket_path(ASKPASS_RUNTIME_SOCKET_NAME)
            .map_err(|_| AskPassUnavailable)?;
        let listener = UnixListener::bind(&socket_path).map_err(|_| AskPassUnavailable)?;
        let socket = runtime_owner
            .register_socket(ASKPASS_RUNTIME_SOCKET_NAME)
            .map_err(|_| AskPassUnavailable)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| AskPassUnavailable)?;
        let address = authenticated_endpoint(&socket_path, std::process::id());
        Ok(BoundAskPassEndpoint::new(
            address,
            Box::new(MacosAskPassListener {
                listener,
                _socket: socket,
                _runtime_owner: runtime_owner,
            }),
        ))
    }
}

struct MacosAskPassListener {
    listener: UnixListener,
    _socket: RegisteredRuntimeSocket,
    _runtime_owner: RuntimeOwner,
}

impl AskPassLocalListener for MacosAskPassListener {
    fn accept_authenticated(&self) -> AskPassLocalAccept {
        let stream = match self.listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return AskPassLocalAccept::Pending;
            }
            Err(_) => return AskPassLocalAccept::Failed,
        };
        if validate_same_user_peer(&stream).is_err() {
            return AskPassLocalAccept::Rejected;
        }
        if configure_broker_connection(&stream).is_err() {
            return AskPassLocalAccept::Rejected;
        }
        AskPassLocalAccept::Connected(Box::new(stream))
    }
}

fn validate_same_user_peer(stream: &UnixStream) -> Result<(), AskPassUnavailable> {
    let peer_user = peer_credentials::peer_user(stream).map_err(|_| AskPassUnavailable)?;
    // SAFETY: `geteuid` has no preconditions and does not dereference memory.
    let effective_user = unsafe { libc::geteuid() };
    if peer_user == effective_user {
        Ok(())
    } else {
        Err(AskPassUnavailable)
    }
}

fn configure_broker_connection(stream: &UnixStream) -> Result<(), AskPassUnavailable> {
    stream
        .set_nonblocking(false)
        .and_then(|()| stream.set_read_timeout(Some(BROKER_CANCELLATION_POLL_INTERVAL)))
        .and_then(|()| stream.set_write_timeout(Some(CONNECTION_WRITE_TIMEOUT)))
        .map_err(|_| AskPassUnavailable)
}

/// Dispatches helper mode before application startup, or returns `None` for the GUI role.
///
/// Invalid or incomplete helper transport input fails closed without presenting UI. The helper
/// writes only a successful response to stdout and never emits transport details to stderr.
pub(crate) fn dispatch_helper_from_environment() -> Option<i32> {
    dispatch_portable_helper(&UnixHelperConnector)
}

struct UnixHelperConnector;

impl AskPassHelperConnector for UnixHelperConnector {
    type Stream = UnixStream;

    fn connect(&self, endpoint: &OsStr) -> Result<Self::Stream, AskPassUnavailable> {
        let (expected_broker, socket_path) = parse_authenticated_endpoint(endpoint)?;
        let stream = UnixStream::connect(socket_path).map_err(|_| AskPassUnavailable)?;
        validate_broker_process(&stream, expected_broker)?;
        // The reply waits for a human prompt; its lifetime is governed by broker cancellation.
        stream
            .set_write_timeout(Some(CONNECTION_WRITE_TIMEOUT))
            .map_err(|_| AskPassUnavailable)?;
        Ok(stream)
    }
}

fn authenticated_endpoint(socket_path: &Path, broker_process: u32) -> OsString {
    let mut endpoint = format!("{broker_process:08x}:").into_bytes();
    endpoint.extend_from_slice(socket_path.as_os_str().as_bytes());
    OsString::from_vec(endpoint)
}

fn parse_authenticated_endpoint(
    endpoint: &OsStr,
) -> Result<(libc::pid_t, &Path), AskPassUnavailable> {
    let bytes = endpoint.as_bytes();
    if bytes.len() <= AUTHENTICATED_ENDPOINT_PREFIX_BYTES
        || bytes.get(BROKER_PID_HEX_BYTES) != Some(&b':')
    {
        return Err(AskPassUnavailable);
    }
    let broker_process = std::str::from_utf8(&bytes[..BROKER_PID_HEX_BYTES])
        .ok()
        .and_then(|text| u32::from_str_radix(text, 16).ok())
        .and_then(|process| libc::pid_t::try_from(process).ok())
        .filter(|process| *process > 0)
        .ok_or(AskPassUnavailable)?;
    let socket_path = Path::new(OsStr::from_bytes(
        &bytes[AUTHENTICATED_ENDPOINT_PREFIX_BYTES..],
    ));
    if !socket_path.is_absolute() {
        return Err(AskPassUnavailable);
    }
    Ok((broker_process, socket_path))
}

fn validate_broker_process(
    stream: &UnixStream,
    expected_broker: libc::pid_t,
) -> Result<(), AskPassUnavailable> {
    match peer_credentials::peer_process(stream) {
        Ok(peer_process) if peer_process == expected_broker => Ok(()),
        _ => Err(AskPassUnavailable),
    }
}

/// Starts each window's AskPass broker with the helper executable composition resolved once at
/// startup, so a later upgrade of the installed file cannot change what OpenSSH executes.
pub(super) struct AskPassWindowFactory {
    helper_path: Option<PathBuf>,
}

impl AskPassWindowFactory {
    /// `None` makes every attempt report AskPass as unavailable.
    pub(super) fn new(helper_path: Option<PathBuf>) -> Self {
        Self { helper_path }
    }
}

impl super::askpass::AskPassWindowFactory for AskPassWindowFactory {
    fn create(
        &self,
        window: &Window,
        cx: &mut App,
    ) -> Result<Arc<dyn super::askpass::AskPassAttemptFactory>, AskPassUnavailable> {
        PortableAskPassBrokerFactory::new(
            window,
            cx,
            Arc::new(UnixAskPassLocalIpc),
            self.helper_path.clone().ok_or(AskPassUnavailable)?,
        )
        .map(|factory| Arc::new(factory) as Arc<dyn super::askpass::AskPassAttemptFactory>)
    }
}

#[cfg(all(test, feature = "native-tests"))]
mod tests {
    use super::*;
    use crate::platform::app_directories::AppDirectoryEnvironment;
    use crate::platform::askpass::{
        AskPassHelperReply, AskPassPresentationFailure, AskPassPresenter, AskPassProtocolReply,
        CAPABILITY_ENV, ENDPOINT_ENV, read_reply, start_attempt_with_presenter, write_request,
    };
    use crate::platform::ssh_askpass::{AskPassPromptKind, AskPassRequest, AskPassSecret};
    use crate::platform::unix_secure_filesystem::UnixSecureFilesystem;
    use std::collections::VecDeque;
    use std::fs;
    use std::os::fd::AsRawFd;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::thread;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = crate::platform::unix_adapter_tests::short_temporary_root()
                .join(format!("sta-{}-{sequence}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn paths(&self) -> AppPaths {
            crate::platform::testing::resolve_app_paths(
                &AppDirectoryEnvironment {
                    home: None,
                    tmpdir: None,
                    xdg_config_home: Some(self.0.join("config").into_os_string()),
                    xdg_data_home: Some(self.0.join("data").into_os_string()),
                    xdg_state_home: Some(self.0.join("state").into_os_string()),
                    xdg_cache_home: Some(self.0.join("cache").into_os_string()),
                    xdg_runtime_dir: Some(self.0.join("runtime").into_os_string()),
                },
                Some(self.0.join("temporary")),
                104,
                Arc::new(UnixSecureFilesystem),
            )
            .unwrap()
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct FakePresenter {
        answers: Mutex<VecDeque<AskPassProtocolReply>>,
        prompts: Mutex<Vec<(String, AskPassPromptKind)>>,
        cancelled: AtomicBool,
    }

    impl FakePresenter {
        fn new(answers: impl IntoIterator<Item = AskPassProtocolReply>) -> Self {
            Self {
                answers: Mutex::new(answers.into_iter().collect()),
                prompts: Mutex::new(Vec::new()),
                cancelled: AtomicBool::new(false),
            }
        }
    }

    impl AskPassPresenter for FakePresenter {
        fn present(
            &self,
            request: AskPassRequest,
            _stop: &AtomicBool,
        ) -> Result<AskPassProtocolReply, AskPassPresentationFailure> {
            self.prompts
                .lock()
                .unwrap()
                .push((request.prompt().to_owned(), request.kind()));
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(AskPassPresentationFailure::Unavailable)
        }

        fn cancel_active(&self) {
            self.cancelled.store(true, Ordering::Release);
        }
    }

    fn lease_value(lease: &crate::platform::askpass::AskPassBrokerLease, name: &str) -> OsString {
        lease
            .entries()
            .find(|(entry_name, _)| *entry_name == name)
            .map(|(_, value)| value.to_os_string())
            .unwrap()
    }

    fn is_nonblocking(stream: &UnixStream) -> bool {
        // SAFETY: `fcntl` reads the status flags for this live socket and retains no pointer.
        let flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
        assert_ne!(flags, -1);
        flags & libc::O_NONBLOCK != 0
    }

    #[test]
    fn authenticated_endpoint_preserves_non_utf8_socket_path() {
        let path = PathBuf::from(OsString::from_vec(
            b"/private/tmp/spaceterm-askpass-\xff.sock".to_vec(),
        ));
        let endpoint = authenticated_endpoint(&path, 0x1020_3040);

        let (process, parsed_path) = parse_authenticated_endpoint(&endpoint).unwrap();

        assert_eq!(process, 0x1020_3040);
        assert_eq!(parsed_path, path);
    }

    #[test]
    fn askpass_endpoint_avoids_a_pre_created_shared_runtime_name() {
        let directory = TestDirectory::new();
        let temporary = directory.0.join("temporary");
        let elsewhere = directory.0.join("elsewhere");
        fs::create_dir_all(&temporary).unwrap();
        fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, temporary.join("spaceterm")).unwrap();
        let paths = crate::platform::testing::resolve_app_paths(
            &AppDirectoryEnvironment {
                home: Some(directory.0.join("home").into_os_string()),
                ..Default::default()
            },
            Some(temporary.clone()),
            104,
            Arc::new(UnixSecureFilesystem),
        )
        .unwrap();

        let attempt = start_attempt_with_presenter(
            &paths,
            PathBuf::from("/opt/spaceterm/bin/spaceterm"),
            &UnixAskPassLocalIpc,
            Arc::new(FakePresenter::new([])),
        )
        .unwrap();

        let endpoint = lease_value(&attempt.lease, ENDPOINT_ENV);
        let (_, socket_path) = parse_authenticated_endpoint(&endpoint).unwrap();
        let runtime = socket_path.parent().unwrap().parent().unwrap();
        assert_eq!(runtime.parent(), Some(temporary.as_path()));
        assert_ne!(runtime, temporary.join("spaceterm"));
        assert!(socket_path.exists());
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
        attempt.lease.cancel();
    }

    #[test]
    fn helper_rejects_an_unexpected_peer_process_before_writing() {
        let listener = super::super::unix_peer_credentials_tests::ChildListener::new();
        let endpoint = authenticated_endpoint(listener.socket_path(), std::process::id());

        let connection = UnixHelperConnector.connect(&endpoint);

        assert!(connection.is_err());
        listener.finish();
    }

    #[test]
    fn helper_accepts_the_exact_broker_process() {
        let listener = super::super::unix_peer_credentials_tests::ChildListener::new();
        let endpoint = authenticated_endpoint(listener.socket_path(), listener.process());

        let stream = UnixHelperConnector.connect(&endpoint).unwrap();
        assert_eq!(
            peer_credentials::peer_process(&stream).unwrap(),
            libc::pid_t::try_from(listener.process()).unwrap()
        );
        drop(stream);
        listener.finish();
    }

    #[test]
    fn broker_should_restore_blocking_io_while_helper_waits_without_a_read_deadline() {
        let directory = TestDirectory::new();
        let socket_path = directory.0.join("prompt-timeout.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = authenticated_endpoint(&socket_path, std::process::id());

        let helper = UnixHelperConnector.connect(&endpoint).unwrap();
        let (broker, _) = listener.accept().unwrap();
        // BSD accept inherits the listener's nonblocking flag and Linux accept does not, so the
        // test sets it to exercise the restoration on every host.
        broker.set_nonblocking(true).unwrap();
        assert!(is_nonblocking(&broker));
        configure_broker_connection(&broker).unwrap();

        assert_eq!(helper.read_timeout().unwrap(), None);
        assert_kernel_timeout(helper.write_timeout().unwrap(), CONNECTION_WRITE_TIMEOUT);
        assert!(!is_nonblocking(&broker));
        assert_kernel_timeout(
            broker.read_timeout().unwrap(),
            BROKER_CANCELLATION_POLL_INTERVAL,
        );
        assert_kernel_timeout(broker.write_timeout().unwrap(), CONNECTION_WRITE_TIMEOUT);
    }

    /// Linux stores socket timeouts in scheduler ticks and reports the rounded-up value, so a
    /// configured 15ms can read back as 16ms. BSD reports the configured value.
    fn assert_kernel_timeout(actual: Option<Duration>, configured: Duration) {
        let actual = actual.expect("the socket should carry a timeout");
        assert!(
            actual >= configured && actual - configured < Duration::from_millis(10),
            "{actual:?} does not round {configured:?}"
        );
    }

    #[test]
    fn native_connector_round_trips_through_the_portable_broker() {
        let directory = TestDirectory::new();
        let presenter = Arc::new(FakePresenter::new([AskPassProtocolReply::Secret(
            AskPassSecret::new(b"correct horse battery staple".to_vec()).unwrap(),
        )]));
        let attempt = start_attempt_with_presenter(
            &directory.paths(),
            PathBuf::from("/Applications/SpaceTerm.app/Contents/MacOS/spaceterm"),
            &UnixAskPassLocalIpc,
            Arc::clone(&presenter) as Arc<dyn AskPassPresenter>,
        )
        .unwrap();
        let endpoint = lease_value(&attempt.lease, ENDPOINT_ENV);
        let capability = lease_value(&attempt.lease, CAPABILITY_ENV);
        let mut stream = UnixHelperConnector.connect(&endpoint).unwrap();
        let request =
            AskPassRequest::new("Password:".to_owned(), AskPassPromptKind::Secret).unwrap();

        write_request(&mut stream, capability.as_os_str().as_bytes(), &request).unwrap();
        let reply = read_reply(&mut stream).unwrap();

        match reply {
            AskPassHelperReply::Secret(secret) => {
                assert_eq!(secret.as_slice(), b"correct horse battery staple")
            }
            _ => panic!("expected secret reply"),
        }
        assert_eq!(
            presenter.prompts.lock().unwrap().as_slice(),
            &[("Password:".to_owned(), AskPassPromptKind::Secret)]
        );
        assert_eq!(attempt.lease.entries().count(), 6);

        attempt.lease.cancel();
        assert!(presenter.cancelled.load(Ordering::Acquire));
        for _ in 0..100 {
            let (_, socket_path) = parse_authenticated_endpoint(&endpoint).unwrap();
            if !socket_path.exists() {
                return;
            }
            thread::sleep(Duration::from_millis(15));
        }
        panic!("AskPass socket was not removed after cancellation");
    }
}
