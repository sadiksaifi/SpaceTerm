#![expect(
    dead_code,
    reason = "the AskPass broker lands before control-connection integration"
)]

use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use gpui::{App, Task, Window};
use thiserror::Error;
use zeroize::Zeroizing;

use super::app_paths::{AppPaths, AppPathsError, RegisteredRuntimeSocket, RuntimeOwner};
use super::macos_askpass::{
    AskPassPresentationError, AskPassPresenter, AskPassPromptKind, AskPassRequest, AskPassResult,
    MacosAskPassPresenter,
};

const PROTOCOL_VERSION: u8 = 1;
const CAPABILITY_BYTES: usize = 32;
const CAPABILITY_TEXT_BYTES: usize = CAPABILITY_BYTES * 2;
const MAX_PROMPT_BYTES: usize = 4 * 1024;
const MAX_SECRET_BYTES: usize = 16 * 1024;
const MAX_REQUEST_FRAME_BYTES: usize = CAPABILITY_TEXT_BYTES + MAX_PROMPT_BYTES + 8;
const MAX_REPLY_FRAME_BYTES: usize = MAX_SECRET_BYTES + 1;
const SOCKET_NAME: &str = "broker.sock";
const HELPER_MODE_ENV: &str = "SPACETERM_SSH_ASKPASS_MODE";
const SOCKET_ENV: &str = "SPACETERM_SSH_ASKPASS_SOCKET";
const CAPABILITY_ENV: &str = "SPACETERM_SSH_ASKPASS_CAPABILITY";
const HELPER_MODE: &str = "broker-v1";
const DISPLAY_MARKER: &str = "spaceterm-askpass";
const SSH_PROMPT_KIND_ENV: &str = "SSH_ASKPASS_PROMPT";
const HELPER_SUCCESS: i32 = 0;
const HELPER_CANCELLED: i32 = 1;
const HELPER_FAILED: i32 = 2;
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(15);
const CONNECTION_IO_TIMEOUT: Duration = Duration::from_secs(5);

const REQUEST_SECRET: u8 = 1;
const REQUEST_CONFIRMATION: u8 = 2;
const REPLY_SECRET: u8 = 1;
const REPLY_CONFIRMATION_YES: u8 = 2;
const REPLY_CONFIRMATION_NO: u8 = 3;
const REPLY_CANCELLED: u8 = 4;
const REPLY_FAILED: u8 = 5;

struct CapabilityToken {
    text: Zeroizing<String>,
}

impl CapabilityToken {
    fn generate() -> Result<Self, AskPassBrokerError> {
        let mut bytes = Zeroizing::new([0_u8; CAPABILITY_BYTES]);
        // SAFETY: `bytes` names a writable allocation of exactly the length passed to
        // `getentropy`. macOS guarantees either a full fill or failure for this bounded request.
        let result = unsafe {
            libc::getentropy(
                bytes.as_mut_ptr().cast::<libc::c_void>(),
                bytes.len() as libc::size_t,
            )
        };
        if result != 0 {
            return Err(AskPassBrokerError::Random(io::Error::last_os_error()));
        }
        Ok(Self::from_random_bytes(bytes.as_slice()))
    }

    fn from_random_bytes(bytes: &[u8]) -> Self {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut text = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            text.push(char::from(HEX[usize::from(byte >> 4)]));
            text.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        Self {
            text: Zeroizing::new(text),
        }
    }

    fn as_str(&self) -> &str {
        self.text.as_str()
    }

    fn matches(&self, candidate: &[u8]) -> bool {
        let expected = self.text.as_bytes();
        if candidate.len() != expected.len() {
            return false;
        }
        expected
            .iter()
            .zip(candidate)
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
    }
}

pub(crate) struct AskPassEnvironment {
    helper_path: PathBuf,
    socket_path: PathBuf,
    capability: Arc<CapabilityToken>,
}

impl AskPassEnvironment {
    /// Returns the complete AskPass overlay for a single control connection.
    ///
    /// The iterator borrows every value so the capability is never cloned into a long-lived
    /// command model. Callers should apply these six entries immediately before spawning ssh.
    pub(crate) fn entries(&self) -> impl Iterator<Item = (&'static str, &OsStr)> {
        [
            ("SSH_ASKPASS", self.helper_path.as_os_str()),
            ("SSH_ASKPASS_REQUIRE", OsStr::new("force")),
            ("DISPLAY", OsStr::new(DISPLAY_MARKER)),
            (HELPER_MODE_ENV, OsStr::new(HELPER_MODE)),
            (SOCKET_ENV, self.socket_path.as_os_str()),
            (CAPABILITY_ENV, OsStr::new(self.capability.as_str())),
        ]
        .into_iter()
    }
}

pub(crate) struct AskPassBroker {
    environment: AskPassEnvironment,
    presenter: Arc<dyn BrokerPresenter>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    presentation_task: Option<Task<()>>,
    cancellation_task: Option<Task<()>>,
    _socket: RegisteredRuntimeSocket,
    _runtime_owner: RuntimeOwner,
}

impl AskPassBroker {
    pub(crate) fn start_native(
        paths: &AppPaths,
        window: &Window,
        cx: &mut App,
    ) -> Result<Self, AskPassBrokerError> {
        let native_presenter =
            Rc::new(std::cell::RefCell::new(MacosAskPassPresenter::new(window)?));
        let (request_sender, request_receiver) = async_channel::bounded(1);
        let (cancellation_sender, cancellation_receiver) = async_channel::bounded(1);
        let presenter = Arc::new(ChannelBrokerPresenter {
            requests: request_sender,
            cancellations: cancellation_sender,
        });
        let request_presenter = native_presenter.clone();
        let presentation_task = cx.spawn(async move |_cx| {
            while let Ok(job) = request_receiver.recv().await {
                let (completion_sender, completion_receiver) = async_channel::bounded(1);
                let presentation = request_presenter.borrow_mut().present(
                    job.request,
                    Box::new(move |result| {
                        let _ = completion_sender.try_send(result);
                    }),
                );
                if presentation.is_err() {
                    let _ = job.response.send(Err(BrokerPresentationFailure::Rejected));
                    continue;
                }
                let response = completion_receiver
                    .recv()
                    .await
                    .map(map_native_result)
                    .map_err(|_| BrokerPresentationFailure::Unavailable);
                let _ = job.response.send(response);
            }
        });
        let cancellation_presenter = native_presenter;
        let cancellation_task = cx.spawn(async move |_cx| {
            while cancellation_receiver.recv().await.is_ok() {
                cancellation_presenter.borrow_mut().cancel_active();
            }
        });
        let mut broker = Self::start_with_presenter(paths, std::env::current_exe()?, presenter)?;
        broker.presentation_task = Some(presentation_task);
        broker.cancellation_task = Some(cancellation_task);
        Ok(broker)
    }

    fn start_with_presenter(
        paths: &AppPaths,
        helper_path: PathBuf,
        presenter: Arc<dyn BrokerPresenter>,
    ) -> Result<Self, AskPassBrokerError> {
        if !helper_path.is_absolute() {
            return Err(AskPassBrokerError::HelperPathNotAbsolute);
        }
        let runtime_owner = paths.create_runtime_owner("askpass")?;
        let socket_path = runtime_owner.socket_path(SOCKET_NAME)?;
        let listener = UnixListener::bind(&socket_path).map_err(AskPassBrokerError::Bind)?;
        let socket = match runtime_owner.register_socket(SOCKET_NAME) {
            Ok(socket) => socket,
            Err(error) => {
                drop(listener);
                let _ = std::fs::remove_file(&socket_path);
                return Err(error.into());
            }
        };
        listener
            .set_nonblocking(true)
            .map_err(AskPassBrokerError::ConfigureSocket)?;

        let capability = Arc::new(CapabilityToken::generate()?);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_capability = capability.clone();
        let worker_presenter = presenter.clone();
        let worker = thread::Builder::new()
            .name("spaceterm-askpass-broker".to_owned())
            .spawn(move || {
                run_broker(listener, worker_capability, worker_presenter, worker_stop);
            })
            .map_err(AskPassBrokerError::StartWorker)?;

        Ok(Self {
            environment: AskPassEnvironment {
                helper_path,
                socket_path,
                capability,
            },
            presenter,
            stop,
            worker: Some(worker),
            presentation_task: None,
            cancellation_task: None,
            _socket: socket,
            _runtime_owner: runtime_owner,
        })
    }

    pub(crate) fn environment(&self) -> &AskPassEnvironment {
        &self.environment
    }
}

impl Drop for AskPassBroker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.presenter.cancel_active();
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
        // A worker waiting for a native sheet is intentionally detached. Cancelling the sheet
        // completes its one-shot callback, after which the worker observes `stop` and exits. The
        // RuntimeOwner unlinks the socket now, so no new helper can connect during that handoff.
    }
}

#[derive(Debug, Error)]
pub(crate) enum AskPassBrokerError {
    #[error(transparent)]
    Paths(#[from] AppPathsError),
    #[error("failed to resolve the SpaceTerm helper executable: {0}")]
    CurrentExecutable(#[from] io::Error),
    #[error("the AskPass helper executable path must be absolute")]
    HelperPathNotAbsolute,
    #[error("failed to bind the private AskPass socket: {0}")]
    Bind(io::Error),
    #[error("failed to configure the private AskPass socket: {0}")]
    ConfigureSocket(io::Error),
    #[error("failed to generate the AskPass capability: {0}")]
    Random(io::Error),
    #[error("failed to start the AskPass broker: {0}")]
    StartWorker(io::Error),
    #[error(transparent)]
    Presentation(#[from] AskPassPresentationError),
}

enum BrokerAnswer {
    Secret(Zeroizing<Vec<u8>>),
    Confirmation(bool),
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrokerPresentationFailure {
    Unavailable,
    Rejected,
}

trait BrokerPresenter: Send + Sync {
    fn present(&self, request: AskPassRequest) -> Result<BrokerAnswer, BrokerPresentationFailure>;

    fn cancel_active(&self);
}

struct PresentationJob {
    request: AskPassRequest,
    response: mpsc::SyncSender<Result<BrokerAnswer, BrokerPresentationFailure>>,
}

struct ChannelBrokerPresenter {
    requests: async_channel::Sender<PresentationJob>,
    cancellations: async_channel::Sender<()>,
}

impl BrokerPresenter for ChannelBrokerPresenter {
    fn present(&self, request: AskPassRequest) -> Result<BrokerAnswer, BrokerPresentationFailure> {
        let (response, receiver) = mpsc::sync_channel(1);
        self.requests
            .send_blocking(PresentationJob { request, response })
            .map_err(|_| BrokerPresentationFailure::Unavailable)?;
        receiver
            .recv()
            .map_err(|_| BrokerPresentationFailure::Unavailable)?
    }

    fn cancel_active(&self) {
        let _ = self.cancellations.try_send(());
    }
}

fn map_native_result(result: AskPassResult) -> BrokerAnswer {
    match result {
        AskPassResult::Secret(secret) => {
            BrokerAnswer::Secret(Zeroizing::new(secret.as_bytes().to_vec()))
        }
        AskPassResult::Confirmation(confirmed) => BrokerAnswer::Confirmation(confirmed),
        AskPassResult::Cancelled => BrokerAnswer::Cancelled,
        AskPassResult::Failed(_) => BrokerAnswer::Failed,
    }
}

fn run_broker(
    listener: UnixListener,
    capability: Arc<CapabilityToken>,
    presenter: Arc<dyn BrokerPresenter>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((mut stream, _address)) => {
                if stream
                    .set_read_timeout(Some(CONNECTION_IO_TIMEOUT))
                    .and_then(|()| stream.set_write_timeout(Some(CONNECTION_IO_TIMEOUT)))
                    .is_err()
                {
                    continue;
                }
                let _ = handle_connection(
                    &mut stream,
                    capability.as_ref(),
                    &MacosPeerValidator,
                    presenter.as_ref(),
                );
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(_) => break,
        }
    }
}

trait PeerValidator {
    fn validate(&self, stream: &UnixStream) -> Result<(), ConnectionError>;
}

struct MacosPeerValidator;

impl PeerValidator for MacosPeerValidator {
    fn validate(&self, stream: &UnixStream) -> Result<(), ConnectionError> {
        let mut peer_uid: libc::uid_t = 0;
        let mut peer_gid: libc::gid_t = 0;
        // SAFETY: `stream` owns a valid Unix-domain socket descriptor and both output pointers
        // reference initialized storage for the duration of the call.
        let result = unsafe {
            libc::getpeereid(
                stream.as_raw_fd(),
                &mut peer_uid as *mut libc::uid_t,
                &mut peer_gid as *mut libc::gid_t,
            )
        };
        if result != 0 {
            return Err(ConnectionError::PeerInspection);
        }
        // SAFETY: `geteuid` has no preconditions and does not expose credential material.
        let current_uid = unsafe { libc::geteuid() };
        if peer_uid != current_uid {
            return Err(ConnectionError::PeerRejected);
        }
        Ok(())
    }
}

fn handle_connection(
    stream: &mut UnixStream,
    capability: &CapabilityToken,
    peer_validator: &dyn PeerValidator,
    presenter: &dyn BrokerPresenter,
) -> Result<(), ConnectionError> {
    peer_validator.validate(stream)?;
    handle_verified_connection(stream, capability, presenter)
}

fn handle_verified_connection<S: Read + Write>(
    stream: &mut S,
    capability: &CapabilityToken,
    presenter: &dyn BrokerPresenter,
) -> Result<(), ConnectionError> {
    let request = read_request(stream, capability)?;
    let answer = presenter.present(request).unwrap_or(BrokerAnswer::Failed);
    write_reply(stream, answer)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectionError {
    PeerInspection,
    PeerRejected,
    Disconnected,
    OversizedFrame,
    MalformedFrame,
    InvalidCapability,
    InvalidRequest,
    WriteFailed,
}

fn read_request<S: Read>(
    stream: &mut S,
    capability: &CapabilityToken,
) -> Result<AskPassRequest, ConnectionError> {
    let length = read_frame_length(stream, MAX_REQUEST_FRAME_BYTES)?;
    let mut frame = Zeroizing::new(vec![0_u8; length]);
    stream
        .read_exact(frame.as_mut_slice())
        .map_err(|_| ConnectionError::Disconnected)?;
    let mut cursor = FrameCursor::new(frame.as_slice());
    if cursor.byte()? != PROTOCOL_VERSION {
        return Err(ConnectionError::MalformedFrame);
    }
    let kind = match cursor.byte()? {
        REQUEST_SECRET => AskPassPromptKind::Secret,
        REQUEST_CONFIRMATION => AskPassPromptKind::Confirmation,
        _ => return Err(ConnectionError::MalformedFrame),
    };
    let token_length = usize::from(cursor.u16()?);
    let token = cursor.bytes(token_length)?;
    if !capability.matches(token) {
        return Err(ConnectionError::InvalidCapability);
    }
    let prompt_length =
        usize::try_from(cursor.u32()?).map_err(|_| ConnectionError::MalformedFrame)?;
    if prompt_length == 0 || prompt_length > MAX_PROMPT_BYTES {
        return Err(ConnectionError::InvalidRequest);
    }
    let prompt_bytes = cursor.bytes(prompt_length)?;
    if !cursor.is_empty() {
        return Err(ConnectionError::MalformedFrame);
    }
    let prompt = std::str::from_utf8(prompt_bytes)
        .map_err(|_| ConnectionError::InvalidRequest)?
        .to_owned();
    AskPassRequest::new(prompt, kind).map_err(|_| ConnectionError::InvalidRequest)
}

fn write_reply<S: Write>(stream: &mut S, answer: BrokerAnswer) -> Result<(), ConnectionError> {
    match answer {
        BrokerAnswer::Secret(secret) if secret.len() <= MAX_SECRET_BYTES => {
            let body_length = 1_usize
                .checked_add(secret.len())
                .ok_or(ConnectionError::OversizedFrame)?;
            write_frame_length(stream, body_length)?;
            stream
                .write_all(&[REPLY_SECRET])
                .and_then(|()| stream.write_all(secret.as_slice()))
                .map_err(|_| ConnectionError::WriteFailed)
        }
        BrokerAnswer::Secret(_) => write_status_reply(stream, REPLY_FAILED),
        BrokerAnswer::Confirmation(true) => write_status_reply(stream, REPLY_CONFIRMATION_YES),
        BrokerAnswer::Confirmation(false) => write_status_reply(stream, REPLY_CONFIRMATION_NO),
        BrokerAnswer::Cancelled => write_status_reply(stream, REPLY_CANCELLED),
        BrokerAnswer::Failed => write_status_reply(stream, REPLY_FAILED),
    }
}

fn write_status_reply<S: Write>(stream: &mut S, status: u8) -> Result<(), ConnectionError> {
    write_frame_length(stream, 1)?;
    stream
        .write_all(&[status])
        .map_err(|_| ConnectionError::WriteFailed)
}

fn read_frame_length<S: Read>(stream: &mut S, maximum: usize) -> Result<usize, ConnectionError> {
    let mut encoded = [0_u8; 4];
    stream
        .read_exact(&mut encoded)
        .map_err(|_| ConnectionError::Disconnected)?;
    let length = usize::try_from(u32::from_be_bytes(encoded))
        .map_err(|_| ConnectionError::OversizedFrame)?;
    if length == 0 || length > maximum {
        return Err(ConnectionError::OversizedFrame);
    }
    Ok(length)
}

fn write_frame_length<S: Write>(stream: &mut S, length: usize) -> Result<(), ConnectionError> {
    let length = u32::try_from(length).map_err(|_| ConnectionError::OversizedFrame)?;
    stream
        .write_all(&length.to_be_bytes())
        .map_err(|_| ConnectionError::WriteFailed)
}

struct FrameCursor<'a> {
    remaining: &'a [u8],
}

impl<'a> FrameCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { remaining: bytes }
    }

    fn byte(&mut self) -> Result<u8, ConnectionError> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, ConnectionError> {
        let bytes: [u8; 2] = self
            .bytes(2)?
            .try_into()
            .map_err(|_| ConnectionError::MalformedFrame)?;
        Ok(u16::from_be_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, ConnectionError> {
        let bytes: [u8; 4] = self
            .bytes(4)?
            .try_into()
            .map_err(|_| ConnectionError::MalformedFrame)?;
        Ok(u32::from_be_bytes(bytes))
    }

    fn bytes(&mut self, length: usize) -> Result<&'a [u8], ConnectionError> {
        if length > self.remaining.len() {
            return Err(ConnectionError::MalformedFrame);
        }
        let (bytes, remaining) = self.remaining.split_at(length);
        self.remaining = remaining;
        Ok(bytes)
    }

    fn is_empty(&self) -> bool {
        self.remaining.is_empty()
    }
}

pub(crate) fn dispatch_helper_from_environment() -> Option<i32> {
    dispatch_helper_role(std::env::var_os(HELPER_MODE_ENV), |mode| {
        let invocation = HelperInvocation {
            mode,
            socket_path: std::env::var_os(SOCKET_ENV),
            capability: std::env::var_os(CAPABILITY_ENV),
            prompt: std::env::args_os().nth(1),
            prompt_kind: std::env::var_os(SSH_PROMPT_KIND_ENV),
        };
        let mut stdout = io::stdout().lock();
        run_helper(invocation, &UnixHelperConnector, &mut stdout)
    })
}

fn dispatch_helper_role(mode: Option<OsString>, run: impl FnOnce(OsString) -> i32) -> Option<i32> {
    mode.map(run)
}

struct HelperInvocation {
    mode: OsString,
    socket_path: Option<OsString>,
    capability: Option<OsString>,
    prompt: Option<OsString>,
    prompt_kind: Option<OsString>,
}

trait HelperConnector {
    type Stream: Read + Write;

    fn connect(&self, path: &Path) -> io::Result<Self::Stream>;
}

struct UnixHelperConnector;

impl HelperConnector for UnixHelperConnector {
    type Stream = UnixStream;

    fn connect(&self, path: &Path) -> io::Result<Self::Stream> {
        UnixStream::connect(path)
    }
}

fn run_helper<C: HelperConnector, W: Write>(
    invocation: HelperInvocation,
    connector: &C,
    stdout: &mut W,
) -> i32 {
    let Some(request) = helper_request(&invocation) else {
        return HELPER_FAILED;
    };
    let Some(socket_path) = invocation.socket_path else {
        return HELPER_FAILED;
    };
    let Some(capability) = invocation.capability else {
        return HELPER_FAILED;
    };
    let capability = capability.as_os_str().as_bytes();
    if capability.len() != CAPABILITY_TEXT_BYTES {
        return HELPER_FAILED;
    }
    let Ok(mut stream) = connector.connect(Path::new(&socket_path)) else {
        return HELPER_FAILED;
    };
    if write_request(&mut stream, capability, &request).is_err() {
        return HELPER_FAILED;
    }
    let Ok(answer) = read_reply(&mut stream) else {
        return HELPER_FAILED;
    };
    match answer {
        HelperAnswer::Secret(secret) => {
            if stdout
                .write_all(secret.as_slice())
                .and_then(|()| stdout.write_all(b"\n"))
                .and_then(|()| stdout.flush())
                .is_ok()
            {
                HELPER_SUCCESS
            } else {
                HELPER_FAILED
            }
        }
        HelperAnswer::Confirmation(confirmed) => {
            let answer: &[u8] = if confirmed { b"yes\n" } else { b"no\n" };
            if stdout
                .write_all(answer)
                .and_then(|()| stdout.flush())
                .is_ok()
            {
                HELPER_SUCCESS
            } else {
                HELPER_FAILED
            }
        }
        HelperAnswer::Cancelled => HELPER_CANCELLED,
        HelperAnswer::Failed => HELPER_FAILED,
    }
}

fn helper_request(invocation: &HelperInvocation) -> Option<AskPassRequest> {
    if invocation.mode != OsStr::new(HELPER_MODE) {
        return None;
    }
    let prompt = invocation.prompt.as_ref()?;
    let prompt_bytes = prompt.as_os_str().as_bytes();
    if prompt_bytes.is_empty() || prompt_bytes.len() > MAX_PROMPT_BYTES {
        return None;
    }
    let prompt = std::str::from_utf8(prompt_bytes).ok()?.to_owned();
    let kind = if invocation.prompt_kind.as_deref() == Some(OsStr::new("confirm")) {
        AskPassPromptKind::Confirmation
    } else {
        AskPassPromptKind::Secret
    };
    AskPassRequest::new(prompt, kind).ok()
}

fn write_request<S: Write>(
    stream: &mut S,
    capability: &[u8],
    request: &AskPassRequest,
) -> Result<(), ConnectionError> {
    if capability.len() > usize::from(u16::MAX) || request.prompt().len() > MAX_PROMPT_BYTES {
        return Err(ConnectionError::OversizedFrame);
    }
    let body_length = 1_usize
        .checked_add(1)
        .and_then(|length| length.checked_add(2))
        .and_then(|length| length.checked_add(capability.len()))
        .and_then(|length| length.checked_add(4))
        .and_then(|length| length.checked_add(request.prompt().len()))
        .ok_or(ConnectionError::OversizedFrame)?;
    if body_length > MAX_REQUEST_FRAME_BYTES {
        return Err(ConnectionError::OversizedFrame);
    }
    write_frame_length(stream, body_length)?;
    let kind = match request.kind() {
        AskPassPromptKind::Secret => REQUEST_SECRET,
        AskPassPromptKind::Confirmation => REQUEST_CONFIRMATION,
    };
    stream
        .write_all(&[PROTOCOL_VERSION, kind])
        .and_then(|()| {
            stream.write_all(
                &u16::try_from(capability.len())
                    .expect("capability length was bounded")
                    .to_be_bytes(),
            )
        })
        .and_then(|()| stream.write_all(capability))
        .and_then(|()| {
            stream.write_all(
                &u32::try_from(request.prompt().len())
                    .expect("prompt length was bounded")
                    .to_be_bytes(),
            )
        })
        .and_then(|()| stream.write_all(request.prompt().as_bytes()))
        .map_err(|_| ConnectionError::WriteFailed)
}

enum HelperAnswer {
    Secret(Zeroizing<Vec<u8>>),
    Confirmation(bool),
    Cancelled,
    Failed,
}

fn read_reply<S: Read>(stream: &mut S) -> Result<HelperAnswer, ConnectionError> {
    let length = read_frame_length(stream, MAX_REPLY_FRAME_BYTES)?;
    let mut frame = Zeroizing::new(vec![0_u8; length]);
    stream
        .read_exact(frame.as_mut_slice())
        .map_err(|_| ConnectionError::Disconnected)?;
    let status = frame[0];
    match status {
        REPLY_SECRET => {
            frame.remove(0);
            Ok(HelperAnswer::Secret(frame))
        }
        REPLY_CONFIRMATION_YES if frame.len() == 1 => Ok(HelperAnswer::Confirmation(true)),
        REPLY_CONFIRMATION_NO if frame.len() == 1 => Ok(HelperAnswer::Confirmation(false)),
        REPLY_CANCELLED if frame.len() == 1 => Ok(HelperAnswer::Cancelled),
        REPLY_FAILED if frame.len() == 1 => Ok(HelperAnswer::Failed),
        _ => Err(ConnectionError::MalformedFrame),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::Cursor;
    use std::sync::Mutex;

    use super::*;

    struct MemoryStream {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl MemoryStream {
        fn new(input: Vec<u8>) -> Self {
            Self {
                input: Cursor::new(input),
                output: Vec::new(),
            }
        }
    }

    impl Read for MemoryStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.input.read(buffer)
        }
    }

    impl Write for MemoryStream {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct FakePresenter {
        answers: Mutex<VecDeque<BrokerAnswer>>,
        prompts: Mutex<Vec<(String, AskPassPromptKind)>>,
        cancelled: AtomicBool,
    }

    impl FakePresenter {
        fn new(answers: impl IntoIterator<Item = BrokerAnswer>) -> Self {
            Self {
                answers: Mutex::new(answers.into_iter().collect()),
                prompts: Mutex::new(Vec::new()),
                cancelled: AtomicBool::new(false),
            }
        }
    }

    impl BrokerPresenter for FakePresenter {
        fn present(
            &self,
            request: AskPassRequest,
        ) -> Result<BrokerAnswer, BrokerPresentationFailure> {
            self.prompts
                .lock()
                .unwrap()
                .push((request.prompt().to_owned(), request.kind()));
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(BrokerPresentationFailure::Unavailable)
        }

        fn cancel_active(&self) {
            self.cancelled.store(true, Ordering::Release);
        }
    }

    struct FakeConnector {
        stream: Mutex<Option<MemoryStream>>,
    }

    impl HelperConnector for FakeConnector {
        type Stream = MemoryStream;

        fn connect(&self, _path: &Path) -> io::Result<Self::Stream> {
            self.stream
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "missing fake stream"))
        }
    }

    struct RejectPeer;

    impl PeerValidator for RejectPeer {
        fn validate(&self, _stream: &UnixStream) -> Result<(), ConnectionError> {
            Err(ConnectionError::PeerRejected)
        }
    }

    fn token() -> CapabilityToken {
        CapabilityToken::from_random_bytes(&[0x5a; CAPABILITY_BYTES])
    }

    fn request_bytes(capability: &[u8], prompt: &str, kind: AskPassPromptKind) -> Vec<u8> {
        let request = AskPassRequest::new(prompt.to_owned(), kind).unwrap();
        let mut bytes = Vec::new();
        write_request(&mut bytes, capability, &request).unwrap();
        bytes
    }

    fn reply_bytes(answer: BrokerAnswer) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_reply(&mut bytes, answer).unwrap();
        bytes
    }

    fn invocation(prompt_kind: Option<&str>) -> HelperInvocation {
        HelperInvocation {
            mode: OsString::from(HELPER_MODE),
            socket_path: Some(OsString::from("/private/fake.sock")),
            capability: Some(OsString::from(token().as_str())),
            prompt: Some(OsString::from("Password:")),
            prompt_kind: prompt_kind.map(OsString::from),
        }
    }

    #[test]
    fn secret_request_is_presented_and_framed_without_text_conversion() {
        let token = token();
        let presenter = FakePresenter::new([BrokerAnswer::Secret(Zeroizing::new(
            b"correct horse".to_vec(),
        ))]);
        let mut stream = MemoryStream::new(request_bytes(
            token.as_str().as_bytes(),
            "Password:",
            AskPassPromptKind::Secret,
        ));

        handle_verified_connection(&mut stream, &token, &presenter).unwrap();

        let mut reply = Cursor::new(stream.output);
        match read_reply(&mut reply).unwrap() {
            HelperAnswer::Secret(secret) => assert_eq!(secret.as_slice(), b"correct horse"),
            _ => panic!("expected secret reply"),
        }
        assert_eq!(
            presenter.prompts.lock().unwrap().as_slice(),
            &[("Password:".to_owned(), AskPassPromptKind::Secret)]
        );
    }

    #[test]
    fn confirmation_yes_and_no_are_typed_replies() {
        for (confirmed, expected) in [(true, b"yes\n".as_slice()), (false, b"no\n".as_slice())] {
            let connector = FakeConnector {
                stream: Mutex::new(Some(MemoryStream::new(reply_bytes(
                    BrokerAnswer::Confirmation(confirmed),
                )))),
            };
            let mut stdout = Vec::new();
            assert_eq!(
                run_helper(invocation(Some("confirm")), &connector, &mut stdout),
                HELPER_SUCCESS
            );
            assert_eq!(stdout, expected);
        }
    }

    #[test]
    fn helper_writes_only_the_secret_answer_and_required_terminator() {
        let connector = FakeConnector {
            stream: Mutex::new(Some(MemoryStream::new(reply_bytes(BrokerAnswer::Secret(
                Zeroizing::new(b"private bytes".to_vec()),
            ))))),
        };
        let mut stdout = Vec::new();

        assert_eq!(
            run_helper(invocation(None), &connector, &mut stdout),
            HELPER_SUCCESS
        );
        assert_eq!(stdout, b"private bytes\n");
    }

    #[test]
    fn cancellation_exits_nonzero_without_stdout() {
        let connector = FakeConnector {
            stream: Mutex::new(Some(MemoryStream::new(reply_bytes(
                BrokerAnswer::Cancelled,
            )))),
        };
        let mut stdout = Vec::new();
        assert_eq!(
            run_helper(invocation(None), &connector, &mut stdout),
            HELPER_CANCELLED
        );
        assert!(stdout.is_empty());
    }

    #[test]
    fn invalid_capability_never_reaches_presenter() {
        let token = token();
        let presenter = FakePresenter::new([]);
        let mut stream = MemoryStream::new(request_bytes(
            b"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "Password:",
            AskPassPromptKind::Secret,
        ));

        assert_eq!(
            handle_verified_connection(&mut stream, &token, &presenter),
            Err(ConnectionError::InvalidCapability)
        );
        assert!(presenter.prompts.lock().unwrap().is_empty());
        assert!(stream.output.is_empty());
    }

    #[test]
    fn rejected_peer_never_reaches_frame_or_presenter() {
        let token = token();
        let presenter = FakePresenter::new([]);
        let (mut broker_stream, _helper_stream) = UnixStream::pair().unwrap();

        assert_eq!(
            handle_connection(&mut broker_stream, &token, &RejectPeer, &presenter),
            Err(ConnectionError::PeerRejected)
        );
        assert!(presenter.prompts.lock().unwrap().is_empty());
    }

    #[test]
    fn malformed_and_oversized_frames_are_rejected_before_presentation() {
        let token = token();
        let presenter = FakePresenter::new([]);
        let mut malformed = MemoryStream::new(vec![0, 0, 0, 1, 0xff]);
        assert_eq!(
            handle_verified_connection(&mut malformed, &token, &presenter),
            Err(ConnectionError::MalformedFrame)
        );

        let oversized_length = u32::try_from(MAX_REQUEST_FRAME_BYTES + 1)
            .unwrap()
            .to_be_bytes()
            .to_vec();
        let mut oversized = MemoryStream::new(oversized_length);
        assert_eq!(
            handle_verified_connection(&mut oversized, &token, &presenter),
            Err(ConnectionError::OversizedFrame)
        );
        assert!(presenter.prompts.lock().unwrap().is_empty());
    }

    #[test]
    fn disconnect_and_failed_reply_exit_without_stdout() {
        let oversized = u32::try_from(MAX_REPLY_FRAME_BYTES + 1)
            .unwrap()
            .to_be_bytes()
            .to_vec();
        for reply in [Vec::new(), reply_bytes(BrokerAnswer::Failed), oversized] {
            let connector = FakeConnector {
                stream: Mutex::new(Some(MemoryStream::new(reply))),
            };
            let mut stdout = Vec::new();
            assert_eq!(
                run_helper(invocation(None), &connector, &mut stdout),
                HELPER_FAILED
            );
            assert!(stdout.is_empty());
        }
    }

    #[test]
    fn sequential_prompts_are_presented_in_connection_order() {
        let token = token();
        let presenter = FakePresenter::new([
            BrokerAnswer::Confirmation(true),
            BrokerAnswer::Secret(Zeroizing::new(b"second".to_vec())),
        ]);
        let mut first = MemoryStream::new(request_bytes(
            token.as_str().as_bytes(),
            "Continue?",
            AskPassPromptKind::Confirmation,
        ));
        let mut second = MemoryStream::new(request_bytes(
            token.as_str().as_bytes(),
            "Password:",
            AskPassPromptKind::Secret,
        ));

        handle_verified_connection(&mut first, &token, &presenter).unwrap();
        handle_verified_connection(&mut second, &token, &presenter).unwrap();

        assert_eq!(
            presenter.prompts.lock().unwrap().as_slice(),
            &[
                ("Continue?".to_owned(), AskPassPromptKind::Confirmation),
                ("Password:".to_owned(), AskPassPromptKind::Secret),
            ]
        );
    }

    #[test]
    fn helper_mode_is_the_earliest_process_role_gate() {
        let calls = std::cell::Cell::new(0);
        assert_eq!(
            dispatch_helper_role(None, |_| {
                calls.set(calls.get() + 1);
                HELPER_SUCCESS
            }),
            None
        );
        assert_eq!(calls.get(), 0);

        assert_eq!(
            dispatch_helper_role(Some(OsString::from(HELPER_MODE)), |mode| {
                calls.set(calls.get() + 1);
                assert_eq!(mode, OsString::from(HELPER_MODE));
                HELPER_CANCELLED
            }),
            Some(HELPER_CANCELLED)
        );
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn environment_contains_only_the_six_askpass_overlay_entries() {
        let capability = Arc::new(token());
        let environment = AskPassEnvironment {
            helper_path: PathBuf::from("/Applications/SpaceTerm.app/Contents/MacOS/spaceterm"),
            socket_path: PathBuf::from("/private/runtime/broker.sock"),
            capability,
        };
        let entries: Vec<_> = environment
            .entries()
            .map(|(key, value)| (key, value.to_os_string()))
            .collect();
        assert_eq!(entries.len(), 6);
        assert_eq!(entries[1], ("SSH_ASKPASS_REQUIRE", OsString::from("force")));
        assert_eq!(entries[2], ("DISPLAY", OsString::from(DISPLAY_MARKER)));
        assert_eq!(entries[3], (HELPER_MODE_ENV, OsString::from(HELPER_MODE)));
    }
}
