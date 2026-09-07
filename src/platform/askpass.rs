//! Portable AskPass protocol, presentation coordination, and lifetime authority.
use super::app_paths::AppPaths;
use crate::ui::ssh_askpass_dialog::GpuiAskPassPresenter;
use gpui::{App, AppContext, Window};
use std::cell::RefCell;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};
use std::thread::{self, JoinHandle};
use zeroize::Zeroizing;

use super::ssh_askpass::{AskPassPromptKind, AskPassRequest, AskPassSecret};

pub(super) const CAPABILITY_TEXT_BYTES: usize = 64;
pub(super) const MAX_PROMPT_BYTES: usize = 4 * 1024;
pub(super) const MAX_REQUEST_FRAME_BYTES: usize = CAPABILITY_TEXT_BYTES + MAX_PROMPT_BYTES + 8;
pub(super) const MAX_REPLY_FRAME_BYTES: usize = 16 * 1024 + 1;
pub(super) const HELPER_MODE_ENV: &str = "SPACETERM_SSH_ASKPASS_MODE";
pub(super) const ENDPOINT_ENV: &str = "SPACETERM_SSH_ASKPASS_SOCKET";
pub(super) const CAPABILITY_ENV: &str = "SPACETERM_SSH_ASKPASS_CAPABILITY";
pub(super) const HELPER_MODE: &str = "broker-v1";
pub(super) const DISPLAY_MARKER: &str = "spaceterm-askpass";
const SSH_PROMPT_KIND_ENV: &str = "SSH_ASKPASS_PROMPT";
const HELPER_SUCCESS: i32 = 0;
const HELPER_CANCELLED: i32 = 1;
const HELPER_FAILED: i32 = 2;
const PRESENTATION_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(15);

const PROTOCOL_VERSION: u8 = 1;
const CAPABILITY_BYTES: usize = CAPABILITY_TEXT_BYTES / 2;
const REQUEST_SECRET: u8 = 1;
const REQUEST_CONFIRMATION: u8 = 2;
const REPLY_SECRET: u8 = 1;
const REPLY_CONFIRMATION_YES: u8 = 2;
const REPLY_CONFIRMATION_NO: u8 = 3;
const REPLY_CANCELLED: u8 = 4;
const REPLY_FAILED: u8 = 5;

/// Non-clone, non-Debug capability owner. Generation and validation are portable policy.
pub(super) struct AskPassCapability {
    text: Zeroizing<String>,
}

/// One non-clone, non-Debug capability copy owned only across an SSH spawn boundary.
pub(crate) struct AskPassCapabilityCopy {
    bytes: Zeroizing<Vec<u8>>,
}

impl AskPassCapabilityCopy {
    pub(crate) fn environment_name(&self) -> &'static OsStr {
        OsStr::new(CAPABILITY_ENV)
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.bytes.as_slice()
    }

    #[cfg(all(test, feature = "macos-native-tests"))]
    pub(crate) fn from_test_bytes(bytes: &[u8]) -> Self {
        Self {
            bytes: Zeroizing::new(bytes.to_vec()),
        }
    }
}

impl AskPassCapability {
    pub(super) fn generate() -> Result<Self, AskPassUnavailable> {
        let mut bytes = Zeroizing::new([0_u8; CAPABILITY_BYTES]);
        getrandom::fill(bytes.as_mut_slice()).map_err(|_| AskPassUnavailable)?;
        Ok(Self::encode(bytes.as_slice()))
    }

    #[cfg(test)]
    pub(super) fn from_random_bytes(bytes: &[u8]) -> Self {
        Self::encode(bytes)
    }

    fn encode(bytes: &[u8]) -> Self {
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

    pub(super) fn as_str(&self) -> &str {
        self.text.as_str()
    }

    /// Compares all expected bytes even when the candidate length is wrong.
    pub(super) fn matches(&self, candidate: &[u8]) -> bool {
        let expected = self.text.as_bytes();
        let mut difference = expected.len() ^ candidate.len();
        for (index, expected_byte) in expected.iter().copied().enumerate() {
            difference |= usize::from(expected_byte ^ candidate.get(index).copied().unwrap_or(0));
        }
        difference == 0
    }
}

/// Validated presentation result before bounded protocol encoding.
pub(super) enum AskPassProtocolReply {
    Secret(AskPassSecret),
    Confirmation(bool),
    Cancelled,
    Failed,
}

/// Validated helper-side reply. Secret bytes are zeroized on every exit path.
pub(super) enum AskPassHelperReply {
    Secret(Zeroizing<Vec<u8>>),
    Confirmation(bool),
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AskPassProtocolError {
    Disconnected,
    OversizedFrame,
    MalformedFrame,
    InvalidCapability,
    InvalidRequest,
    WriteFailed,
}

pub(super) fn read_request<S: Read + ?Sized>(
    stream: &mut S,
    capability: &AskPassCapability,
) -> Result<AskPassRequest, AskPassProtocolError> {
    let length = read_frame_length(stream, MAX_REQUEST_FRAME_BYTES)?;
    let mut frame = Zeroizing::new(vec![0_u8; length]);
    stream
        .read_exact(frame.as_mut_slice())
        .map_err(|_| AskPassProtocolError::Disconnected)?;
    let mut cursor = FrameCursor::new(frame.as_slice());
    if cursor.byte()? != PROTOCOL_VERSION {
        return Err(AskPassProtocolError::MalformedFrame);
    }
    let kind = match cursor.byte()? {
        REQUEST_SECRET => AskPassPromptKind::Secret,
        REQUEST_CONFIRMATION => AskPassPromptKind::Confirmation,
        _ => return Err(AskPassProtocolError::MalformedFrame),
    };
    let token_length = usize::from(cursor.u16()?);
    if !capability.matches(cursor.bytes(token_length)?) {
        return Err(AskPassProtocolError::InvalidCapability);
    }
    let prompt_length =
        usize::try_from(cursor.u32()?).map_err(|_| AskPassProtocolError::MalformedFrame)?;
    if prompt_length == 0 || prompt_length > MAX_PROMPT_BYTES {
        return Err(AskPassProtocolError::InvalidRequest);
    }
    let prompt_bytes = cursor.bytes(prompt_length)?;
    if !cursor.is_empty() {
        return Err(AskPassProtocolError::MalformedFrame);
    }
    let prompt = std::str::from_utf8(prompt_bytes)
        .map_err(|_| AskPassProtocolError::InvalidRequest)?
        .to_owned();
    AskPassRequest::new(prompt, kind).map_err(|_| AskPassProtocolError::InvalidRequest)
}

pub(super) fn write_request<S: Write + ?Sized>(
    stream: &mut S,
    capability: &[u8],
    request: &AskPassRequest,
) -> Result<(), AskPassProtocolError> {
    let capability_length =
        u16::try_from(capability.len()).map_err(|_| AskPassProtocolError::OversizedFrame)?;
    let prompt_length =
        u32::try_from(request.prompt().len()).map_err(|_| AskPassProtocolError::OversizedFrame)?;
    if request.prompt().len() > MAX_PROMPT_BYTES {
        return Err(AskPassProtocolError::OversizedFrame);
    }
    let body_length = 8_usize
        .checked_add(capability.len())
        .and_then(|length| length.checked_add(request.prompt().len()))
        .filter(|length| *length <= MAX_REQUEST_FRAME_BYTES)
        .ok_or(AskPassProtocolError::OversizedFrame)?;
    let kind = match request.kind() {
        AskPassPromptKind::Secret => REQUEST_SECRET,
        AskPassPromptKind::Confirmation => REQUEST_CONFIRMATION,
    };
    write_frame_length(stream, body_length)?;
    stream
        .write_all(&[PROTOCOL_VERSION, kind])
        .and_then(|()| stream.write_all(&capability_length.to_be_bytes()))
        .and_then(|()| stream.write_all(capability))
        .and_then(|()| stream.write_all(&prompt_length.to_be_bytes()))
        .and_then(|()| stream.write_all(request.prompt().as_bytes()))
        .map_err(|_| AskPassProtocolError::WriteFailed)
}

pub(super) fn write_reply<S: Write + ?Sized>(
    stream: &mut S,
    answer: AskPassProtocolReply,
) -> Result<(), AskPassProtocolError> {
    match answer {
        AskPassProtocolReply::Secret(secret) if secret.as_bytes().len() < MAX_REPLY_FRAME_BYTES => {
            let body_length = 1_usize
                .checked_add(secret.as_bytes().len())
                .ok_or(AskPassProtocolError::OversizedFrame)?;
            write_frame_length(stream, body_length)?;
            stream
                .write_all(&[REPLY_SECRET])
                .and_then(|()| stream.write_all(secret.as_bytes()))
                .map_err(|_| AskPassProtocolError::WriteFailed)
        }
        AskPassProtocolReply::Secret(_) => write_status_reply(stream, REPLY_FAILED),
        AskPassProtocolReply::Confirmation(true) => {
            write_status_reply(stream, REPLY_CONFIRMATION_YES)
        }
        AskPassProtocolReply::Confirmation(false) => {
            write_status_reply(stream, REPLY_CONFIRMATION_NO)
        }
        AskPassProtocolReply::Cancelled => write_status_reply(stream, REPLY_CANCELLED),
        AskPassProtocolReply::Failed => write_status_reply(stream, REPLY_FAILED),
    }
}

pub(super) fn read_reply<S: Read + ?Sized>(
    stream: &mut S,
) -> Result<AskPassHelperReply, AskPassProtocolError> {
    let length = read_frame_length(stream, MAX_REPLY_FRAME_BYTES)?;
    let mut frame = Zeroizing::new(vec![0_u8; length]);
    stream
        .read_exact(frame.as_mut_slice())
        .map_err(|_| AskPassProtocolError::Disconnected)?;
    match frame[0] {
        REPLY_SECRET => {
            frame.remove(0);
            Ok(AskPassHelperReply::Secret(frame))
        }
        REPLY_CONFIRMATION_YES if frame.len() == 1 => Ok(AskPassHelperReply::Confirmation(true)),
        REPLY_CONFIRMATION_NO if frame.len() == 1 => Ok(AskPassHelperReply::Confirmation(false)),
        REPLY_CANCELLED if frame.len() == 1 => Ok(AskPassHelperReply::Cancelled),
        REPLY_FAILED if frame.len() == 1 => Ok(AskPassHelperReply::Failed),
        _ => Err(AskPassProtocolError::MalformedFrame),
    }
}

fn read_frame_length<S: Read + ?Sized>(
    stream: &mut S,
    maximum: usize,
) -> Result<usize, AskPassProtocolError> {
    let mut encoded = [0_u8; 4];
    stream
        .read_exact(&mut encoded)
        .map_err(|_| AskPassProtocolError::Disconnected)?;
    let length = usize::try_from(u32::from_be_bytes(encoded))
        .map_err(|_| AskPassProtocolError::OversizedFrame)?;
    if length == 0 || length > maximum {
        return Err(AskPassProtocolError::OversizedFrame);
    }
    Ok(length)
}

fn write_frame_length<S: Write + ?Sized>(
    stream: &mut S,
    length: usize,
) -> Result<(), AskPassProtocolError> {
    let length = u32::try_from(length).map_err(|_| AskPassProtocolError::OversizedFrame)?;
    stream
        .write_all(&length.to_be_bytes())
        .map_err(|_| AskPassProtocolError::WriteFailed)
}

fn write_status_reply<S: Write + ?Sized>(
    stream: &mut S,
    status: u8,
) -> Result<(), AskPassProtocolError> {
    write_frame_length(stream, 1)?;
    stream
        .write_all(&[status])
        .map_err(|_| AskPassProtocolError::WriteFailed)
}

struct FrameCursor<'a> {
    remaining: &'a [u8],
}

impl<'a> FrameCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { remaining: bytes }
    }

    fn byte(&mut self) -> Result<u8, AskPassProtocolError> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, AskPassProtocolError> {
        let bytes = self
            .bytes(2)?
            .try_into()
            .map_err(|_| AskPassProtocolError::MalformedFrame)?;
        Ok(u16::from_be_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, AskPassProtocolError> {
        let bytes = self
            .bytes(4)?
            .try_into()
            .map_err(|_| AskPassProtocolError::MalformedFrame)?;
        Ok(u32::from_be_bytes(bytes))
    }

    fn bytes(&mut self, length: usize) -> Result<&'a [u8], AskPassProtocolError> {
        if length > self.remaining.len() {
            return Err(AskPassProtocolError::MalformedFrame);
        }
        let (bytes, remaining) = self.remaining.split_at(length);
        self.remaining = remaining;
        Ok(bytes)
    }

    fn is_empty(&self) -> bool {
        self.remaining.is_empty()
    }
}

/// Only the native endpoint connection mechanic is injected into portable helper policy.
///
/// Implementations must authenticate the expected broker process before returning a stream. The
/// portable helper writes its capability and prompt immediately after this method succeeds.
pub(super) trait AskPassHelperConnector {
    type Stream: Read + Write;

    fn connect(&self, endpoint: &OsStr) -> Result<Self::Stream, AskPassUnavailable>;
}

struct HelperInvocation {
    mode: OsString,
    endpoint: Option<OsString>,
    capability: Option<Zeroizing<Vec<u8>>>,
    prompt: Option<OsString>,
    prompt_kind: Option<OsString>,
}

/// Dispatches the helper process role without constructing or entering GPUI.
pub(super) fn dispatch_helper_from_environment(
    connector: &impl AskPassHelperConnector,
) -> Option<i32> {
    dispatch_helper_role(std::env::var_os(HELPER_MODE_ENV), |mode| {
        let invocation = HelperInvocation {
            mode,
            endpoint: std::env::var_os(ENDPOINT_ENV),
            capability: std::env::var_os(CAPABILITY_ENV)
                .map(OsString::into_encoded_bytes)
                .map(Zeroizing::new),
            prompt: std::env::args_os().nth(1),
            prompt_kind: std::env::var_os(SSH_PROMPT_KIND_ENV),
        };
        let mut stdout = std::io::stdout().lock();
        run_helper(invocation, connector, &mut stdout)
    })
}

fn dispatch_helper_role(mode: Option<OsString>, run: impl FnOnce(OsString) -> i32) -> Option<i32> {
    mode.map(run)
}

fn run_helper<C: AskPassHelperConnector, W: Write>(
    invocation: HelperInvocation,
    connector: &C,
    stdout: &mut W,
) -> i32 {
    let Some(request) = helper_request(&invocation) else {
        return HELPER_FAILED;
    };
    let (Some(endpoint), Some(capability)) = (invocation.endpoint, invocation.capability) else {
        return HELPER_FAILED;
    };
    if capability.len() != CAPABILITY_TEXT_BYTES {
        return HELPER_FAILED;
    }
    let Ok(mut stream) = connector.connect(&endpoint) else {
        return HELPER_FAILED;
    };
    if write_request(&mut stream, capability.as_slice(), &request).is_err() {
        return HELPER_FAILED;
    }
    let Ok(answer) = read_reply(&mut stream) else {
        return HELPER_FAILED;
    };
    match answer {
        AskPassHelperReply::Secret(secret) => {
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
        AskPassHelperReply::Confirmation(confirmed) => {
            let reply: &[u8] = if confirmed { b"yes\n" } else { b"no\n" };
            if stdout
                .write_all(reply)
                .and_then(|()| stdout.flush())
                .is_ok()
            {
                HELPER_SUCCESS
            } else {
                HELPER_FAILED
            }
        }
        AskPassHelperReply::Cancelled => HELPER_CANCELLED,
        AskPassHelperReply::Failed => HELPER_FAILED,
    }
}

fn helper_request(invocation: &HelperInvocation) -> Option<AskPassRequest> {
    if invocation.mode != OsStr::new(HELPER_MODE) {
        return None;
    }
    let prompt = invocation.prompt.as_ref()?.to_str()?;
    if prompt.is_empty() || prompt.len() > MAX_PROMPT_BYTES {
        return None;
    }
    let kind = if invocation.prompt_kind.as_deref() == Some(OsStr::new("confirm")) {
        AskPassPromptKind::Confirmation
    } else {
        AskPassPromptKind::Secret
    };
    AskPassRequest::new(prompt.to_owned(), kind).ok()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AskPassPresentationFailure {
    Unavailable,
    Rejected,
}

pub(super) trait AskPassPresenter: Send + Sync {
    fn present(
        &self,
        request: AskPassRequest,
        stop: &AtomicBool,
    ) -> Result<AskPassProtocolReply, AskPassPresentationFailure>;
    fn cancel_active(&self);
}

pub(super) struct ObservedAskPassPresenter {
    inner: Arc<dyn AskPassPresenter>,
    observation: AskPassAttemptObservation,
}

impl ObservedAskPassPresenter {
    pub(super) fn new(
        inner: Arc<dyn AskPassPresenter>,
        observation: AskPassAttemptObservation,
    ) -> Self {
        Self { inner, observation }
    }
}

struct PromptActivity<'a>(&'a AtomicBool);

impl PromptActivity<'_> {
    fn begin(active: &AtomicBool) -> PromptActivity<'_> {
        active.store(true, Ordering::Release);
        PromptActivity(active)
    }
}

impl Drop for PromptActivity<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl AskPassPresenter for ObservedAskPassPresenter {
    fn present(
        &self,
        request: AskPassRequest,
        stop: &AtomicBool,
    ) -> Result<AskPassProtocolReply, AskPassPresentationFailure> {
        self.observation
            .state
            .prompt_started
            .store(true, Ordering::Release);
        let activity = PromptActivity::begin(&self.observation.state.prompt_active);
        let answer = self.inner.present(request, stop);
        drop(activity);
        if matches!(answer, Ok(AskPassProtocolReply::Cancelled)) {
            self.observation
                .state
                .cancelled
                .store(true, Ordering::Release);
        }
        answer
    }

    fn cancel_active(&self) {
        self.observation
            .state
            .cancelled
            .store(true, Ordering::Release);
        self.inner.cancel_active();
    }
}

struct PresentationJob {
    request: AskPassRequest,
    response: mpsc::SyncSender<Result<AskPassProtocolReply, AskPassPresentationFailure>>,
    cancelled: Arc<AtomicBool>,
}

enum AskPassUiCommand {
    Present { owner: u64, job: PresentationJob },
    Cancel { owner: u64 },
}

pub(super) struct GpuiAskPassBridge {
    commands: async_channel::Sender<AskPassUiCommand>,
    next_owner: Arc<AtomicU64>,
}

impl GpuiAskPassBridge {
    pub(super) fn new(window: &Window, cx: &mut App) -> Self {
        let presenter = cx.new(|_| GpuiAskPassPresenter::default());
        let window_handle = window.window_handle();
        let (commands, receiver) = async_channel::bounded(2);
        cx.spawn(async move |cx| {
            while let Ok(command) = receiver.recv().await {
                match command {
                    AskPassUiCommand::Present { owner, job } => {
                        if job.cancelled.load(Ordering::Acquire) {
                            let _ = job.response.send(Ok(AskPassProtocolReply::Cancelled));
                            continue;
                        }
                        let response = Rc::new(RefCell::new(Some(job.response)));
                        let completion_response = Rc::clone(&response);
                        let result = window_handle.update(cx, |_, window, cx| {
                            presenter.update(cx, |presenter, cx| {
                                presenter.present(
                                    owner,
                                    job.request,
                                    Box::new(move |result| {
                                        if let Some(response) =
                                            completion_response.borrow_mut().take()
                                        {
                                            let _ =
                                                response.send(Ok(map_presentation_result(result)));
                                        }
                                    }),
                                    window,
                                    cx,
                                )
                            })
                        });
                        if !matches!(result, Ok(Ok(())))
                            && let Some(response) = response.borrow_mut().take()
                        {
                            let _ = response.send(Err(AskPassPresentationFailure::Rejected));
                        }
                    }
                    AskPassUiCommand::Cancel { owner } => {
                        let _ = window_handle.update(cx, |_, window, cx| {
                            presenter.update(cx, |presenter, cx| {
                                presenter.cancel_owner(owner, window, cx);
                            });
                        });
                    }
                }
            }
        })
        .detach();
        Self {
            commands,
            next_owner: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(super) fn presenter(&self) -> Arc<dyn AskPassPresenter> {
        let owner = self
            .next_owner
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        Arc::new(ChannelAskPassPresenter {
            commands: self.commands.clone(),
            owner,
            state: Arc::new(Mutex::new(ChannelPresentationState::default())),
            cancelled: Arc::new(AtomicBool::new(false)),
        })
    }
}

#[derive(Default)]
struct ChannelPresentationState {
    closed: bool,
}

struct ChannelAskPassPresenter {
    commands: async_channel::Sender<AskPassUiCommand>,
    owner: u64,
    state: Arc<Mutex<ChannelPresentationState>>,
    cancelled: Arc<AtomicBool>,
}

impl AskPassPresenter for ChannelAskPassPresenter {
    fn present(
        &self,
        request: AskPassRequest,
        stop: &AtomicBool,
    ) -> Result<AskPassProtocolReply, AskPassPresentationFailure> {
        let state = self
            .state
            .lock()
            .map_err(|_| AskPassPresentationFailure::Unavailable)?;
        if state.closed {
            return Err(AskPassPresentationFailure::Unavailable);
        }
        let (response, receiver) = mpsc::sync_channel(1);
        self.commands
            .send_blocking(AskPassUiCommand::Present {
                owner: self.owner,
                job: PresentationJob {
                    request,
                    response,
                    cancelled: Arc::clone(&self.cancelled),
                },
            })
            .map_err(|_| AskPassPresentationFailure::Unavailable)?;
        drop(state);
        loop {
            match receiver.recv_timeout(PRESENTATION_POLL_INTERVAL) {
                Ok(answer) => return answer,
                Err(mpsc::RecvTimeoutError::Timeout) if stop.load(Ordering::Acquire) => {
                    return Err(AskPassPresentationFailure::Unavailable);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(AskPassPresentationFailure::Unavailable);
                }
            }
        }
    }

    fn cancel_active(&self) {
        self.cancelled.store(true, Ordering::Release);
        match self.state.try_lock() {
            Ok(mut state) => {
                close_presentation_channel(&mut state, self.commands.clone(), self.owner)
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                let state = Arc::clone(&self.state);
                let commands = self.commands.clone();
                let owner = self.owner;
                let _ = thread::Builder::new()
                    .name("spaceterm-askpass-cancel-order".to_owned())
                    .spawn(move || {
                        if let Ok(mut state) = state.lock() {
                            close_presentation_channel(&mut state, commands, owner);
                        }
                    });
            }
            Err(std::sync::TryLockError::Poisoned(_)) => {}
        }
    }
}

fn close_presentation_channel(
    state: &mut ChannelPresentationState,
    commands: async_channel::Sender<AskPassUiCommand>,
    owner: u64,
) {
    if state.closed {
        return;
    }
    state.closed = true;
    enqueue_cancel_without_blocking(commands, owner);
}

fn enqueue_cancel_without_blocking(commands: async_channel::Sender<AskPassUiCommand>, owner: u64) {
    let command = AskPassUiCommand::Cancel { owner };
    match commands.try_send(command) {
        Ok(()) | Err(async_channel::TrySendError::Closed(_)) => {}
        Err(async_channel::TrySendError::Full(command)) => {
            let _ = thread::Builder::new()
                .name("spaceterm-askpass-cancel".to_owned())
                .spawn(move || {
                    let _ = commands.send_blocking(command);
                });
        }
    }
}

fn map_presentation_result(result: super::ssh_askpass::AskPassResult) -> AskPassProtocolReply {
    match result {
        super::ssh_askpass::AskPassResult::Secret(secret) => AskPassProtocolReply::Secret(secret),
        super::ssh_askpass::AskPassResult::Confirmation(confirmed) => {
            AskPassProtocolReply::Confirmation(confirmed)
        }
        super::ssh_askpass::AskPassResult::Cancelled => AskPassProtocolReply::Cancelled,
        super::ssh_askpass::AskPassResult::Failed(_) => AskPassProtocolReply::Failed,
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AskPassUnavailable;
pub(crate) trait AskPassWindowFactory: Send + Sync {
    fn create(
        &self,
        window: &Window,
        cx: &mut App,
    ) -> Result<Arc<dyn AskPassAttemptFactory>, AskPassUnavailable>;
}
pub(crate) trait AskPassAttemptFactory: Send + Sync {
    fn start_attempt(&self, paths: &AppPaths) -> Result<AskPassAttempt, AskPassUnavailable>;
}
pub(crate) struct AskPassAttempt {
    pub(crate) lease: AskPassBrokerLease,
    pub(crate) observation: AskPassAttemptObservation,
}

/// Portable, exactly-once AskPass teardown ordering.
///
/// Cancellation becomes visible first, then the active presentation is cancelled. Worker joining
/// and exact endpoint cleanup transfer to retained background ownership so GPUI never waits.
pub(super) struct AskPassTeardown {
    stop: Arc<AtomicBool>,
    cancel_presentation: Arc<dyn Fn() + Send + Sync>,
    closed: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl AskPassTeardown {
    pub(super) fn new(
        stop: Arc<AtomicBool>,
        cancel_presentation: Arc<dyn Fn() + Send + Sync>,
        worker: JoinHandle<()>,
    ) -> Self {
        Self {
            stop,
            cancel_presentation,
            closed: AtomicBool::new(false),
            worker: Mutex::new(Some(worker)),
        }
    }

    pub(super) fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stop.store(true, Ordering::Release);
        (self.cancel_presentation)();
        let worker = self.worker.lock().ok().and_then(|mut worker| worker.take());
        let Some(worker) = worker else {
            return;
        };
        let _ = thread::Builder::new()
            .name("spaceterm-askpass-cleanup".to_owned())
            .spawn(move || {
                let _ = worker.join();
            });
    }
}

impl Drop for AskPassTeardown {
    fn drop(&mut self) {
        self.close();
    }
}

pub(super) trait AskPassLocalStream: Read + Write + Send {}

impl<T: Read + Write + Send> AskPassLocalStream for T {}

/// Result of one nonblocking, peer-authenticated local endpoint poll.
pub(super) enum AskPassLocalAccept {
    Connected(Box<dyn AskPassLocalStream>),
    Pending,
    Rejected,
    Failed,
}

/// Listener mechanics and exact endpoint resources. Its implementation authenticates a peer
/// before returning `Connected`, and its drop removes only the exact registered endpoint.
pub(super) trait AskPassLocalListener: Send {
    fn accept_authenticated(&self) -> AskPassLocalAccept;
}

/// Bound endpoint with its secret address intentionally excluded from `Debug` and errors.
pub(super) struct BoundAskPassEndpoint {
    address: OsString,
    listener: Box<dyn AskPassLocalListener>,
}

impl BoundAskPassEndpoint {
    pub(super) fn new(address: OsString, listener: Box<dyn AskPassLocalListener>) -> Self {
        Self { address, listener }
    }
}

/// Narrow native listener creation mechanism selected by Host Composition.
pub(super) trait AskPassLocalIpc: Send + Sync {
    fn bind(&self, paths: &AppPaths) -> Result<BoundAskPassEndpoint, AskPassUnavailable>;
}

struct AskPassEnvironment {
    helper_path: PathBuf,
    endpoint: OsString,
    capability: Arc<AskPassCapability>,
}

impl AskPassEnvironment {
    fn entries(&self) -> impl Iterator<Item = (&'static str, &OsStr)> {
        [
            ("SSH_ASKPASS", self.helper_path.as_os_str()),
            ("SSH_ASKPASS_REQUIRE", OsStr::new("force")),
            ("DISPLAY", OsStr::new(DISPLAY_MARKER)),
            (HELPER_MODE_ENV, OsStr::new(HELPER_MODE)),
            (ENDPOINT_ENV, self.endpoint.as_os_str()),
            (CAPABILITY_ENV, OsStr::new(self.capability.as_str())),
        ]
        .into_iter()
    }
}

/// Portable per-window factory for isolated AskPass attempts.
pub(super) struct GpuiAskPassBrokerFactory {
    helper_path: PathBuf,
    bridge: GpuiAskPassBridge,
    local_ipc: Arc<dyn AskPassLocalIpc>,
}

impl GpuiAskPassBrokerFactory {
    pub(super) fn new(
        window: &Window,
        cx: &mut App,
        local_ipc: Arc<dyn AskPassLocalIpc>,
        helper_path: PathBuf,
    ) -> Result<Self, AskPassUnavailable> {
        if !helper_path.is_absolute() {
            return Err(AskPassUnavailable);
        }
        Ok(Self {
            helper_path,
            bridge: GpuiAskPassBridge::new(window, cx),
            local_ipc,
        })
    }
}

impl AskPassAttemptFactory for GpuiAskPassBrokerFactory {
    fn start_attempt(&self, paths: &AppPaths) -> Result<AskPassAttempt, AskPassUnavailable> {
        start_attempt_with_presenter(
            paths,
            self.helper_path.clone(),
            self.local_ipc.as_ref(),
            self.bridge.presenter(),
        )
    }
}

pub(super) fn start_attempt_with_presenter(
    paths: &AppPaths,
    helper_path: PathBuf,
    local_ipc: &dyn AskPassLocalIpc,
    presenter: Arc<dyn AskPassPresenter>,
) -> Result<AskPassAttempt, AskPassUnavailable> {
    if !helper_path.is_absolute() {
        return Err(AskPassUnavailable);
    }
    let observation = AskPassAttemptObservation::default();
    let presenter = Arc::new(ObservedAskPassPresenter::new(
        presenter,
        observation.clone(),
    ));
    let capability = Arc::new(AskPassCapability::generate()?);
    let endpoint = local_ipc.bind(paths)?;
    let environment = Arc::new(AskPassEnvironment {
        helper_path,
        endpoint: endpoint.address.clone(),
        capability: Arc::clone(&capability),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let worker_presenter = Arc::clone(&presenter) as Arc<dyn AskPassPresenter>;
    let worker = thread::Builder::new()
        .name("spaceterm-askpass-broker".to_owned())
        .spawn(move || run_broker(endpoint, capability, worker_presenter, worker_stop))
        .map_err(|_| AskPassUnavailable)?;
    let cancel_presenter = Arc::clone(&presenter);
    let cancel_presentation: Arc<dyn Fn() + Send + Sync> =
        Arc::new(move || cancel_presenter.cancel_active());
    let lifetime = Arc::new(AskPassBrokerLifetime {
        environment,
        teardown: AskPassTeardown::new(stop, cancel_presentation, worker),
    });
    Ok(AskPassAttempt {
        lease: AskPassBrokerLease::new(lifetime),
        observation,
    })
}

struct AskPassBrokerLifetime {
    environment: Arc<AskPassEnvironment>,
    teardown: AskPassTeardown,
}

impl AskPassLease for AskPassBrokerLifetime {
    fn entries(&self) -> Vec<(&'static str, &OsStr)> {
        self.environment.entries().collect()
    }

    fn cancel(&self) {
        self.teardown.close();
    }

    fn capability(&self) -> &[u8] {
        self.environment.capability.as_str().as_bytes()
    }
}

impl Drop for AskPassBrokerLifetime {
    fn drop(&mut self) {
        self.teardown.close();
    }
}

fn run_broker(
    endpoint: BoundAskPassEndpoint,
    capability: Arc<AskPassCapability>,
    presenter: Arc<dyn AskPassPresenter>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Acquire) {
        match endpoint.listener.accept_authenticated() {
            AskPassLocalAccept::Connected(mut stream) => {
                let _ = handle_verified_connection(
                    stream.as_mut(),
                    capability.as_ref(),
                    presenter.as_ref(),
                    stop.as_ref(),
                );
            }
            AskPassLocalAccept::Pending | AskPassLocalAccept::Rejected => {
                thread::sleep(PRESENTATION_POLL_INTERVAL);
            }
            AskPassLocalAccept::Failed => break,
        }
    }
}

fn handle_verified_connection<S: Read + Write + ?Sized>(
    stream: &mut S,
    capability: &AskPassCapability,
    presenter: &dyn AskPassPresenter,
    stop: &AtomicBool,
) -> Result<(), AskPassProtocolError> {
    let request = read_request(stream, capability)?;
    let answer = presenter
        .present(request, stop)
        .unwrap_or(AskPassProtocolReply::Failed);
    write_reply(stream, answer)
}
pub(crate) trait AskPassLease: Send + Sync {
    fn entries(&self) -> Vec<(&'static str, &OsStr)>;
    fn cancel(&self);
    fn capability(&self) -> &[u8];
}
#[derive(Clone)]
pub(crate) struct AskPassBrokerLease(Arc<dyn AskPassLease>);
impl AskPassBrokerLease {
    pub(crate) fn new(lease: Arc<dyn AskPassLease>) -> Self {
        Self(lease)
    }
    #[cfg(all(test, feature = "macos-native-tests"))]
    pub(crate) fn entries(&self) -> impl Iterator<Item = (&'static str, &OsStr)> {
        self.0.entries().into_iter()
    }

    pub(crate) fn ordinary_spawn_entries(&self) -> impl Iterator<Item = (&'static str, &OsStr)> {
        self.0
            .entries()
            .into_iter()
            .filter(|(name, _)| *name != CAPABILITY_ENV)
    }

    pub(crate) fn capability_copy_for_spawn(&self) -> AskPassCapabilityCopy {
        AskPassCapabilityCopy {
            bytes: Zeroizing::new(self.0.capability().to_vec()),
        }
    }
    pub(crate) fn cancel(&self) {
        self.0.cancel();
    }
}
#[derive(Clone, Default)]
/// Content-free observation of authentication prompt activity and user cancellation.
///
/// It is scoped to one connection attempt and carries no prompt or response bytes.
pub(crate) struct AskPassAttemptObservation {
    pub(super) state: Arc<AskPassAttemptObservationState>,
}

#[derive(Default)]
pub(super) struct AskPassAttemptObservationState {
    pub(super) prompt_started: AtomicBool,
    pub(super) prompt_active: AtomicBool,
    pub(super) cancelled: Arc<AtomicBool>,
}

impl AskPassAttemptObservation {
    /// Reports whether any prompt in this attempt reached the presenter.
    #[cfg(test)]
    pub(crate) fn prompt_started(&self) -> bool {
        self.state.prompt_started.load(Ordering::Acquire)
    }

    /// Reports whether an authentication prompt is currently active for this attempt.
    pub(crate) fn prompt_active(&self) -> bool {
        self.state.prompt_active.load(Ordering::Acquire)
    }

    /// Reports whether any prompt in this attempt was cancelled.
    pub(crate) fn cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn cancellation_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.state.cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Result as IoResult};
    use std::sync::Mutex;

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
        fn read(&mut self, buffer: &mut [u8]) -> IoResult<usize> {
            self.input.read(buffer)
        }
    }

    impl Write for MemoryStream {
        fn write(&mut self, buffer: &[u8]) -> IoResult<usize> {
            self.output.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> IoResult<()> {
            Ok(())
        }
    }

    struct MemoryConnector {
        stream: Mutex<Option<MemoryStream>>,
    }

    impl AskPassHelperConnector for MemoryConnector {
        type Stream = MemoryStream;

        fn connect(&self, _endpoint: &OsStr) -> Result<Self::Stream, AskPassUnavailable> {
            self.stream
                .lock()
                .map_err(|_| AskPassUnavailable)?
                .take()
                .ok_or(AskPassUnavailable)
        }
    }

    fn token() -> AskPassCapability {
        AskPassCapability::from_random_bytes(&[0x5a; CAPABILITY_BYTES])
    }

    fn request_bytes(capability: &[u8], prompt: &[u8], kind: u8) -> Vec<u8> {
        let length = 8 + capability.len() + prompt.len();
        let mut frame = Vec::with_capacity(4 + length);
        frame.extend_from_slice(&(u32::try_from(length).unwrap()).to_be_bytes());
        frame.extend_from_slice(&[PROTOCOL_VERSION, kind]);
        frame.extend_from_slice(&(u16::try_from(capability.len()).unwrap()).to_be_bytes());
        frame.extend_from_slice(capability);
        frame.extend_from_slice(&(u32::try_from(prompt.len()).unwrap()).to_be_bytes());
        frame.extend_from_slice(prompt);
        frame
    }

    fn invocation(mode: &str) -> HelperInvocation {
        HelperInvocation {
            mode: OsString::from(mode),
            endpoint: Some(OsString::from("private-endpoint")),
            capability: Some(Zeroizing::new(token().as_str().as_bytes().to_vec())),
            prompt: Some(OsString::from("Password:")),
            prompt_kind: None,
        }
    }

    #[test]
    fn protocol_accepts_split_frames() {
        struct OneByteReader(Cursor<Vec<u8>>);
        impl Read for OneByteReader {
            fn read(&mut self, buffer: &mut [u8]) -> IoResult<usize> {
                let length = buffer.len().min(1);
                self.0.read(&mut buffer[..length])
            }
        }
        let token = token();
        let bytes = request_bytes(token.as_str().as_bytes(), b"Password:", REQUEST_SECRET);
        assert!(read_request(&mut OneByteReader(Cursor::new(bytes)), &token).is_ok());
    }

    #[test]
    fn protocol_rejects_oversized_malformed_and_unknown_frames() {
        let token = token();
        let oversized = (u32::try_from(MAX_REQUEST_FRAME_BYTES + 1).unwrap())
            .to_be_bytes()
            .to_vec();
        assert_eq!(
            read_request(&mut Cursor::new(oversized), &token).err(),
            Some(AskPassProtocolError::OversizedFrame)
        );
        let unknown = request_bytes(token.as_str().as_bytes(), b"Password:", 0xff);
        assert_eq!(
            read_request(&mut Cursor::new(unknown), &token).err(),
            Some(AskPassProtocolError::MalformedFrame)
        );
        let malformed = request_bytes(token.as_str().as_bytes(), &[0xff], REQUEST_SECRET);
        assert_eq!(
            read_request(&mut Cursor::new(malformed), &token).err(),
            Some(AskPassProtocolError::InvalidRequest)
        );
    }

    #[test]
    fn invalid_capability_is_rejected_before_prompt_decoding() {
        let token = token();
        let bytes = request_bytes(
            b"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            &[0xff],
            REQUEST_SECRET,
        );
        assert_eq!(
            read_request(&mut Cursor::new(bytes), &token).err(),
            Some(AskPassProtocolError::InvalidCapability)
        );
    }

    #[test]
    fn capability_comparison_rejects_short_long_and_changed_values() {
        let token = token();
        assert!(token.matches(token.as_str().as_bytes()));
        assert!(!token.matches(&token.as_str().as_bytes()[..CAPABILITY_TEXT_BYTES - 1]));
        let mut long = token.as_str().as_bytes().to_vec();
        long.push(b'0');
        assert!(!token.matches(&long));
        let mut changed = token.as_str().as_bytes().to_vec();
        changed[CAPABILITY_TEXT_BYTES - 1] ^= 1;
        assert!(!token.matches(&changed));
    }

    #[test]
    fn helper_rejects_unknown_reply_without_stdout() {
        let connector = MemoryConnector {
            stream: Mutex::new(Some(MemoryStream::new(vec![0, 0, 0, 1, 0xff]))),
        };
        let mut stdout = Vec::new();
        assert_eq!(
            run_helper(invocation(HELPER_MODE), &connector, &mut stdout),
            HELPER_FAILED
        );
        assert!(stdout.is_empty());
    }

    #[test]
    fn helper_classifies_secret_confirmation_and_cancellation_replies() {
        let cases = [
            (
                AskPassProtocolReply::Secret(AskPassSecret::new(b"private".to_vec()).unwrap()),
                HELPER_SUCCESS,
                b"private\n".as_slice(),
            ),
            (
                AskPassProtocolReply::Confirmation(true),
                HELPER_SUCCESS,
                b"yes\n".as_slice(),
            ),
            (
                AskPassProtocolReply::Cancelled,
                HELPER_CANCELLED,
                b"".as_slice(),
            ),
        ];
        for (reply, expected_exit, expected_stdout) in cases {
            let mut encoded = Vec::new();
            write_reply(&mut encoded, reply).unwrap();
            let connector = MemoryConnector {
                stream: Mutex::new(Some(MemoryStream::new(encoded))),
            };
            let mut stdout = Vec::new();
            assert_eq!(
                run_helper(invocation(HELPER_MODE), &connector, &mut stdout),
                expected_exit
            );
            assert_eq!(stdout, expected_stdout);
        }
    }

    #[test]
    fn helper_dispatch_gate_does_not_run_for_the_gui_role() {
        let calls = std::cell::Cell::new(0);
        assert_eq!(
            dispatch_helper_role(None, |_| {
                calls.set(calls.get() + 1);
                HELPER_SUCCESS
            }),
            None
        );
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn fixed_environment_overlay_names_are_bounded_and_complete() {
        let environment = AskPassEnvironment {
            helper_path: PathBuf::from("/application/helper"),
            endpoint: OsString::from("private-endpoint"),
            capability: Arc::new(token()),
        };
        assert_eq!(
            environment
                .entries()
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            vec![
                "SSH_ASKPASS",
                "SSH_ASKPASS_REQUIRE",
                "DISPLAY",
                HELPER_MODE_ENV,
                ENDPOINT_ENV,
                CAPABILITY_ENV,
            ]
        );
    }

    struct CancelPresenter;

    impl AskPassPresenter for CancelPresenter {
        fn present(
            &self,
            _request: AskPassRequest,
            _stop: &AtomicBool,
        ) -> Result<AskPassProtocolReply, AskPassPresentationFailure> {
            Ok(AskPassProtocolReply::Cancelled)
        }

        fn cancel_active(&self) {}
    }

    #[test]
    fn observation_tracks_one_prompt_and_clears_activity_after_settlement() {
        let observation = AskPassAttemptObservation::default();
        let presenter =
            ObservedAskPassPresenter::new(Arc::new(CancelPresenter), observation.clone());
        let request =
            AskPassRequest::new("Password:".to_owned(), AskPassPromptKind::Secret).unwrap();
        assert!(matches!(
            presenter.present(request, &AtomicBool::new(false)),
            Ok(AskPassProtocolReply::Cancelled)
        ));
        assert!(observation.prompt_started());
        assert!(!observation.prompt_active());
        assert!(observation.cancelled());
    }

    #[test]
    fn cancellation_is_enqueued_after_an_admitted_presentation() {
        let (commands, receiver) = async_channel::bounded(2);
        let presenter = Arc::new(ChannelAskPassPresenter {
            commands,
            owner: 41,
            state: Arc::new(Mutex::new(ChannelPresentationState::default())),
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        let worker = thread::spawn({
            let presenter = Arc::clone(&presenter);
            move || {
                presenter.present(
                    AskPassRequest::new("Password:".to_owned(), AskPassPromptKind::Secret).unwrap(),
                    &AtomicBool::new(false),
                )
            }
        });
        let response = match receiver.recv_blocking().unwrap() {
            AskPassUiCommand::Present { owner, job } => {
                assert_eq!(owner, 41);
                job.response
            }
            AskPassUiCommand::Cancel { .. } => panic!("presentation must be ordered first"),
        };
        presenter.cancel_active();
        assert!(matches!(
            receiver.recv_blocking(),
            Ok(AskPassUiCommand::Cancel { owner: 41 })
        ));
        response.send(Ok(AskPassProtocolReply::Cancelled)).unwrap();
        assert!(matches!(
            worker.join(),
            Ok(Ok(AskPassProtocolReply::Cancelled))
        ));
    }

    #[test]
    fn teardown_is_exactly_once_and_cleans_up_only_after_worker_exit() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let (release, released) = mpsc::sync_channel(1);
        let worker_events = Arc::clone(&events);
        let worker = thread::spawn(move || {
            let _ = released.recv();
            worker_events.lock().unwrap().push("worker-exit");
            worker_events.lock().unwrap().push("cleanup");
        });
        let cancel_events = Arc::clone(&events);
        let release = Mutex::new(Some(release));
        let cancel = Arc::new(move || {
            cancel_events.lock().unwrap().push("cancel");
            if let Some(release) = release.lock().unwrap().take() {
                let _ = release.send(());
            }
        });
        let teardown = AskPassTeardown::new(Arc::new(AtomicBool::new(false)), cancel, worker);

        teardown.close();
        teardown.close();
        for _ in 0..100 {
            if events.lock().unwrap().len() == 3 {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            events.lock().unwrap().as_slice(),
            ["cancel", "worker-exit", "cleanup"]
        );
    }
}
