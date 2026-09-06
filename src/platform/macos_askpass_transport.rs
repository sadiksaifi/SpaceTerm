//! macOS AskPass endpoint binding and peer authentication.

use super::app_paths::{
    ASKPASS_RUNTIME_OWNER_KIND, ASKPASS_RUNTIME_SOCKET_NAME, AppPaths, RegisteredRuntimeSocket,
    RuntimeOwner,
};
use super::askpass::{
    AskPassHelperConnector, AskPassLocalAccept, AskPassLocalIpc, AskPassLocalListener,
    AskPassUnavailable, BoundAskPassEndpoint,
    GpuiAskPassBrokerFactory as PortableAskPassBrokerFactory,
    dispatch_helper_from_environment as dispatch_portable_helper,
};
use gpui::{App, Window};
use std::ffi::{OsStr, OsString};
use std::io;
use std::mem::size_of;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const CONNECTION_IO_TIMEOUT: Duration = Duration::from_secs(5);
const BROKER_PID_HEX_BYTES: usize = 8;
const AUTHENTICATED_ENDPOINT_PREFIX_BYTES: usize = BROKER_PID_HEX_BYTES + 1;

struct MacosAskPassLocalIpc;

impl AskPassLocalIpc for MacosAskPassLocalIpc {
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
        if set_connection_timeouts(&stream).is_err() {
            return AskPassLocalAccept::Rejected;
        }
        AskPassLocalAccept::Connected(Box::new(stream))
    }
}

fn validate_same_user_peer(stream: &UnixStream) -> Result<(), AskPassUnavailable> {
    let mut peer_user = 0;
    let mut peer_group = 0;
    // SAFETY: `getpeereid` only writes the supplied uid and gid values for this live socket.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut peer_user, &mut peer_group) };
    if result != 0 {
        return Err(AskPassUnavailable);
    }
    // SAFETY: `geteuid` has no preconditions and does not dereference memory.
    let effective_user = unsafe { libc::geteuid() };
    if peer_user == effective_user {
        Ok(())
    } else {
        Err(AskPassUnavailable)
    }
}

fn set_connection_timeouts(stream: &UnixStream) -> Result<(), AskPassUnavailable> {
    stream
        .set_read_timeout(Some(CONNECTION_IO_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(CONNECTION_IO_TIMEOUT)))
        .map_err(|_| AskPassUnavailable)
}

/// Dispatches helper mode before application startup, or returns `None` for the GUI role.
///
/// Invalid or incomplete helper transport input fails closed without presenting UI. The helper
/// writes only a successful response to stdout and never emits transport details to stderr.
pub(crate) fn dispatch_helper_from_environment() -> Option<i32> {
    dispatch_portable_helper(&MacosHelperConnector)
}

struct MacosHelperConnector;

impl AskPassHelperConnector for MacosHelperConnector {
    type Stream = UnixStream;

    fn connect(&self, endpoint: &OsStr) -> Result<Self::Stream, AskPassUnavailable> {
        let (expected_broker, socket_path) = parse_authenticated_endpoint(endpoint)?;
        let stream = UnixStream::connect(socket_path).map_err(|_| AskPassUnavailable)?;
        validate_broker_process(&stream, expected_broker)?;
        set_connection_timeouts(&stream)?;
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
    let mut peer_process: libc::pid_t = 0;
    let mut peer_process_size =
        libc::socklen_t::try_from(size_of::<libc::pid_t>()).map_err(|_| AskPassUnavailable)?;
    // SAFETY: the output pointer and length describe a writable `pid_t`; the socket remains live
    // for the call, and `getsockopt` does not retain either pointer.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut peer_process).cast::<libc::c_void>(),
            &raw mut peer_process_size,
        )
    };
    if result != 0
        || peer_process_size as usize != size_of::<libc::pid_t>()
        || peer_process != expected_broker
    {
        return Err(AskPassUnavailable);
    }
    Ok(())
}

pub(super) struct AskPassWindowFactory;

impl super::askpass::AskPassWindowFactory for AskPassWindowFactory {
    fn create(
        &self,
        window: &Window,
        cx: &mut App,
    ) -> Result<Arc<dyn super::askpass::AskPassAttemptFactory>, AskPassUnavailable> {
        PortableAskPassBrokerFactory::new(window, cx, Arc::new(MacosAskPassLocalIpc))
            .map(|factory| Arc::new(factory) as Arc<dyn super::askpass::AskPassAttemptFactory>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::app_paths::{AppPathEnvironment, AppPathHostFacts};
    use crate::platform::askpass::{
        AskPassHelperReply, AskPassPresentationFailure, AskPassPresenter, AskPassProtocolReply,
        CAPABILITY_ENV, ENDPOINT_ENV, read_reply, start_attempt_with_presenter, write_request,
    };
    use crate::platform::macos_secure_filesystem::MacosSecureFilesystem;
    use crate::platform::ssh_askpass::{AskPassPromptKind, AskPassRequest, AskPassSecret};
    use std::collections::VecDeque;
    use std::fs;
    use std::io::Read;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::thread;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = PathBuf::from(format!(
                "/private/tmp/sta-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn paths(&self) -> AppPaths {
            AppPaths::resolve(
                &AppPathEnvironment {
                    home: None,
                    xdg_config_home: Some(self.0.join("config").into_os_string()),
                    xdg_data_home: Some(self.0.join("data").into_os_string()),
                    xdg_state_home: Some(self.0.join("state").into_os_string()),
                    xdg_cache_home: Some(self.0.join("cache").into_os_string()),
                    xdg_runtime_dir: Some(self.0.join("runtime").into_os_string()),
                },
                &AppPathHostFacts::new(self.0.join("temporary"), 104).unwrap(),
                Arc::new(MacosSecureFilesystem),
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
    fn helper_rejects_an_unexpected_peer_process_before_writing() {
        let directory = TestDirectory::new();
        let socket_path = directory.0.join("wrong-broker.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let current_process = std::process::id();
        let wrong_process = if current_process == u32::MAX {
            current_process - 1
        } else {
            current_process + 1
        };
        let endpoint = authenticated_endpoint(&socket_path, wrong_process);

        let connection = MacosHelperConnector.connect(&endpoint);
        let (mut accepted, _) = listener.accept().unwrap();
        let mut byte = [0_u8; 1];

        assert!(connection.is_err());
        assert_eq!(accepted.read(&mut byte).unwrap(), 0);
    }

    #[test]
    fn helper_accepts_the_exact_broker_process() {
        let directory = TestDirectory::new();
        let socket_path = directory.0.join("broker.sock");
        let _listener = UnixListener::bind(&socket_path).unwrap();
        let endpoint = authenticated_endpoint(&socket_path, std::process::id());

        assert!(MacosHelperConnector.connect(&endpoint).is_ok());
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
            &MacosAskPassLocalIpc,
            Arc::clone(&presenter) as Arc<dyn AskPassPresenter>,
        )
        .unwrap();
        let endpoint = lease_value(&attempt.lease, ENDPOINT_ENV);
        let capability = lease_value(&attempt.lease, CAPABILITY_ENV);
        let mut stream = MacosHelperConnector.connect(&endpoint).unwrap();
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
