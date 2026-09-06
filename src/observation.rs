use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::terminal::{
    FailureClass, Recoverability, RuntimeLifecycle, RuntimeObservation, RuntimeSample,
    RuntimeTransition,
};

const CHALLENGE_SCHEMA: &str = "spaceterm.acceptance.native-launch-challenge/v5";
const OBSERVATION_SCHEMA: &str = "spaceterm.acceptance.native-launch-proof/v5";
const RUNTIME_SCHEMA: &str = "spaceterm.acceptance.runtime-stream/v1";
const RUNTIME_TICK_SCHEMA: &str = "spaceterm.acceptance.runtime-tick/v1";
const RUNTIME_COMPLETE_SCHEMA: &str = "spaceterm.acceptance.runtime-complete/v1";
const RUNTIME_ACK_SCHEMA: &str = "spaceterm.acceptance.runtime-ack/v1";
const RUNTIME_CLOSED_SCHEMA: &str = "spaceterm.acceptance.runtime-closed/v1";
const FAILURE_ACTION_SCHEMA: &str = "spaceterm.acceptance.failure-action/v1";
const FAILURE_ACTION_RESULT_SCHEMA: &str = "spaceterm.acceptance.failure-action-result/v2";
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const SAMPLE_LATE_TOLERANCE: Duration = Duration::from_millis(250);
const FAILURE_ACTION_POLL_INTERVAL: Duration = Duration::from_millis(10);
const TRANSITION_CAPACITY: usize = 64;
const MAX_FRAME_BYTES: usize = 16 * 1024;
const SOCKET_TIMEOUT: Duration = Duration::from_secs(30);
const FINAL_ACK_TIMEOUT: Duration = Duration::from_secs(5);
const TERMINAL_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ObservationGeometry {
    pub(crate) rows: u16,
    pub(crate) columns: u16,
    pub(crate) logical_width: f32,
    pub(crate) logical_height: f32,
    pub(crate) backing_pixel_width: u32,
    pub(crate) backing_pixel_height: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum AcceptanceObservationError {
    #[error("acceptance observation transport is unavailable")]
    InvalidSocket,
    #[error("acceptance observation challenge is invalid")]
    InvalidChallenge,
    #[error("acceptance observation transport failed")]
    Transport,
    #[error("acceptance launch environment is not clean")]
    InvalidEnvironment,
}

struct ObservationRequest {
    stream: Box<dyn ObservationTransport>,
    package: Box<dyn PackagedExecutable>,
    nonce: String,
    run_id: String,
    app_sha256: String,
    failure_actions_enabled: bool,
    initial: Option<ObservationSelection>,
    runtime: RuntimeObservation,
    failure_action_sender: Option<async_channel::Sender<FailureActionRequest>>,
    failure_action_receiver: Option<async_channel::Receiver<FailureActionRequest>>,
    failure_result_sender: Option<mpsc::SyncSender<FailureActionEvent>>,
    failure_result_receiver: Option<mpsc::Receiver<FailureActionEvent>>,
}

pub(crate) struct PreparedObservation {
    request: ObservationRequest,
    initial: ObservationSelection,
    owner: Weak<ObservationOwner>,
    cleanup: PreparedCleanup,
}

struct PreparedCleanup {
    owner: Weak<ObservationOwner>,
    armed: bool,
}
impl PreparedCleanup {
    fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for PreparedCleanup {
    fn drop(&mut self) {
        if self.armed
            && let Some(owner) = self.owner.upgrade()
        {
            owner.live.store(false, Ordering::Release);
            owner.runtime.fail();
            owner.runtime.revoke_producers();
        }
    }
}

#[derive(Debug)]
pub(crate) struct ClaimedObservation {
    pub(crate) runtime: RuntimeObservation,
    pub(crate) failure_actions: Option<FailureActionController>,
    pub(crate) lease: ObservationLease,
    pub(crate) session: SessionObservationLease,
}

#[derive(Clone, Debug)]
pub(crate) struct FailureActionController {
    commands: async_channel::Receiver<FailureActionRequest>,
    results: mpsc::SyncSender<FailureActionEvent>,
    live: Arc<AtomicBool>,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct FailureActionRequest {
    pub(crate) id: String,
    pub(crate) sequence: u64,
    pub(crate) case: FailureActionCase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailureActionCase {
    PresentationInvalidScale,
    PresentationGlyph,
    RendererImagePreflight,
    RendererResourceBeforeSync,
    RendererResourceAfterStaging,
    PasteboardWrite,
    PtyFatal,
    EmulatorFatal,
    NormalExitControl,
}

impl FailureActionCase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::PresentationInvalidScale => "presentation-invalid-scale",
            Self::PresentationGlyph => "presentation-glyph",
            Self::RendererImagePreflight => "renderer-image-preflight",
            Self::RendererResourceBeforeSync => "renderer-resource-before-sync",
            Self::RendererResourceAfterStaging => "renderer-resource-after-staging",
            Self::PasteboardWrite => "pasteboard-write",
            Self::PtyFatal => "pty-fatal",
            Self::EmulatorFatal => "emulator-fatal",
            Self::NormalExitControl => "normal-exit-control",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "presentation-invalid-scale" => Some(Self::PresentationInvalidScale),
            "presentation-glyph" => Some(Self::PresentationGlyph),
            "renderer-image-preflight" => Some(Self::RendererImagePreflight),
            "renderer-resource-before-sync" => Some(Self::RendererResourceBeforeSync),
            "renderer-resource-after-staging" => Some(Self::RendererResourceAfterStaging),
            "pasteboard-write" => Some(Self::PasteboardWrite),
            "pty-fatal" => Some(Self::PtyFatal),
            "emulator-fatal" => Some(Self::EmulatorFatal),
            "normal-exit-control" => Some(Self::NormalExitControl),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailureActionPhase {
    Armed,
    Injected,
    RetryRequested,
    Completed,
}

impl FailureActionPhase {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Armed => "armed",
            Self::Injected => "injected",
            Self::RetryRequested => "retry-requested",
            Self::Completed => "completed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailureActionResult {
    Accepted,
    FailedState,
    Recovered,
    Closed,
    Exited,
}

impl FailureActionResult {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::FailedState => "failed-state",
            Self::Recovered => "recovered",
            Self::Closed => "closed",
            Self::Exited => "exited",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailurePaneState {
    Running,
    Failed,
    Exited,
}

impl FailurePaneState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Failed => "failed",
            Self::Exited => "exited",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailurePendingRecovery {
    Presentation,
    RendererResources,
    CopySelection,
    None,
}

impl FailurePendingRecovery {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Presentation => "presentation",
            Self::RendererResources => "renderer-resources",
            Self::CopySelection => "copy-selection",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FailureActionEvent {
    pub(crate) request: FailureActionRequest,
    pub(crate) phase: FailureActionPhase,
    pub(crate) result: FailureActionResult,
    pub(crate) pane_identity: u64,
    pub(crate) pane_state: FailurePaneState,
    pub(crate) failure_class: Option<FailureClass>,
    pub(crate) recoverability: Option<Recoverability>,
    pub(crate) failure_operation: Option<&'static str>,
    pub(crate) state_revision: u64,
    pub(crate) latest_generation: u64,
    pub(crate) last_valid_generation: u64,
    pub(crate) visible_generation: Option<u64>,
    pub(crate) pending_recovery: FailurePendingRecovery,
    pub(crate) terminal_input_usable: bool,
    pub(crate) session_attached: bool,
    pub(crate) resource_staged_count: u64,
    pub(crate) resource_staged_bytes: u64,
    pub(crate) resource_rolled_back_count: u64,
    pub(crate) resource_rolled_back_bytes: u64,
}

#[derive(Debug)]
struct RuntimeWriter {
    shutdown: mpsc::Sender<()>,
    thread: JoinHandle<Result<(), RuntimeWriterError>>,
}

struct FailureTransport {
    nonce: String,
    run_id: String,
    app_sha256: String,
    requests: Option<async_channel::Sender<FailureActionRequest>>,
    results: Option<mpsc::Receiver<FailureActionEvent>>,
}

#[derive(Default)]
struct IncomingFrames {
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
enum RuntimeWriterError {
    #[error("runtime observer transport failed")]
    Transport,
    #[error("runtime observer protocol failed")]
    Protocol,
}

#[derive(Debug)]
struct ObservationSelection {
    selected_font: String,
    geometry: ObservationGeometry,
}

#[allow(clippy::type_complexity)]
fn failure_channels(
    enabled: bool,
) -> (
    Option<async_channel::Sender<FailureActionRequest>>,
    Option<async_channel::Receiver<FailureActionRequest>>,
    Option<mpsc::SyncSender<FailureActionEvent>>,
    Option<mpsc::Receiver<FailureActionEvent>>,
) {
    if !enabled {
        return (None, None, None, None);
    }
    let (action_sender, action_receiver) = async_channel::bounded(1);
    let (result_sender, result_receiver) = mpsc::sync_channel(8);
    (
        Some(action_sender),
        Some(action_receiver),
        Some(result_sender),
        Some(result_receiver),
    )
}

impl FailureActionController {
    pub(crate) fn is_active(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }
    #[cfg(test)]
    pub(crate) fn test_revoke(&self) {
        self.live.store(false, Ordering::Release);
    }
    pub(crate) async fn receive(&self) -> Option<FailureActionRequest> {
        let request = self.commands.recv().await.ok()?;
        self.live.load(Ordering::Acquire).then_some(request)
    }

    pub(crate) fn emit(&self, event: FailureActionEvent) -> bool {
        self.live.load(Ordering::Acquire) && self.results.try_send(event).is_ok()
    }

    #[cfg(test)]
    pub(crate) fn test_channel() -> (
        Self,
        async_channel::Sender<FailureActionRequest>,
        mpsc::Receiver<FailureActionEvent>,
    ) {
        let (requests, commands) = async_channel::bounded(1);
        let (results, events) = mpsc::sync_channel(8);
        (
            Self {
                commands,
                results,
                live: Arc::new(AtomicBool::new(true)),
            },
            requests,
            events,
        )
    }
}

#[cfg(test)]
fn spawn_runtime_writer(
    mut stream: Box<dyn ObservationTransport>,
    observation: RuntimeObservation,
    failure: FailureTransport,
) -> Result<RuntimeWriter, AcceptanceObservationError> {
    stream.set_read_timeout(Some(FAILURE_ACTION_POLL_INTERVAL))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))?;
    let (shutdown, receiver) = mpsc::channel();
    let thread = thread::Builder::new()
        .name("spaceterm-acceptance-observer".to_owned())
        .spawn({
            let observation = observation.clone();
            move || run_runtime_writer(stream.as_mut(), &observation, &receiver, failure)
        })
        .map_err(|_| AcceptanceObservationError::Transport)?;
    Ok(RuntimeWriter { shutdown, thread })
}

fn run_runtime_writer(
    stream: &mut dyn ObservationTransport,
    observation: &RuntimeObservation,
    shutdown: &mpsc::Receiver<()>,
    failure: FailureTransport,
) -> Result<(), RuntimeWriterError> {
    let mut sequence = 0_u64;
    let mut cadence = WriterCadence::new(Instant::now());
    let mut started_ns = None;
    let mut event_count = 0_u64;
    let mut expected_action_sequence = 0_u64;
    let mut authority = FailureAuthority::default();
    let mut incoming = IncomingFrames::default();

    let last_periodic_ns = loop {
        if observation.is_failed() {
            return Err(RuntimeWriterError::Protocol);
        }
        let now = Instant::now();
        let due = cadence.poll(now)?;
        if due == Some(true) {
            observation.fail();
        }
        if due.is_some() {
            let transitions = observation.drain_transitions();
            let sample = observation.sample();
            started_ns.get_or_insert(sample.continuous_ns);
            event_count = event_count
                .checked_add(transitions.len() as u64)
                .ok_or(RuntimeWriterError::Protocol)?;
            write_frame(
                stream,
                format_runtime_tick(sequence, sample, &transitions).as_bytes(),
            )
            .map_err(|_| RuntimeWriterError::Transport)?;
            sequence = sequence
                .checked_add(1)
                .ok_or(RuntimeWriterError::Protocol)?;
        }

        let frames = incoming
            .read_available(stream)
            .map_err(|error| match error.kind() {
                io::ErrorKind::InvalidData => RuntimeWriterError::Protocol,
                _ => RuntimeWriterError::Transport,
            })?;
        if failure.requests.is_none() && !frames.is_empty() {
            return Err(RuntimeWriterError::Protocol);
        }
        for frame in frames {
            let request = parse_failure_action(
                &frame,
                &failure.nonce,
                &failure.run_id,
                &failure.app_sha256,
                expected_action_sequence,
            )
            .map_err(|_| RuntimeWriterError::Protocol)?;
            authority.request(request.clone())?;
            failure
                .requests
                .as_ref()
                .ok_or(RuntimeWriterError::Protocol)?
                .try_send(request)
                .map_err(|_| RuntimeWriterError::Protocol)?;
            expected_action_sequence = expected_action_sequence
                .checked_add(1)
                .ok_or(RuntimeWriterError::Protocol)?;
        }
        if let Some(results) = &failure.results {
            while let Ok(result) = results.try_recv() {
                authority.event(&result)?;
                write_frame(stream, format_failure_action_result(&result).as_bytes())
                    .map_err(|_| RuntimeWriterError::Transport)?;
            }
        }

        match shutdown.try_recv() {
            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => {
                break observation.sample().continuous_ns;
            }
            Err(mpsc::TryRecvError::Empty) => {
                thread::sleep(
                    FAILURE_ACTION_POLL_INTERVAL
                        .min(cadence.deadline.saturating_duration_since(Instant::now())),
                );
            }
        }
    };

    let lifecycle_deadline = Instant::now() + TERMINAL_LIFECYCLE_TIMEOUT;
    loop {
        let sample = observation.sample();
        if matches!(
            sample.lifecycle,
            RuntimeLifecycle::Exited | RuntimeLifecycle::Failed | RuntimeLifecycle::ObserverFailed
        ) {
            break;
        }
        if Instant::now() >= lifecycle_deadline {
            observation.fail();
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    if let Some(results) = &failure.results {
        while let Ok(result) = results.try_recv() {
            authority.event(&result)?;
            write_frame(stream, format_failure_action_result(&result).as_bytes())
                .map_err(|_| RuntimeWriterError::Transport)?;
        }
    }
    if authority.pending.is_some() {
        observation.fail();
    }
    let transitions = observation.seal_and_drain_transitions();
    let sample = observation.sample();
    let last_ns = sample.continuous_ns.max(last_periodic_ns);
    event_count = event_count
        .checked_add(transitions.len() as u64)
        .ok_or(RuntimeWriterError::Protocol)?;
    write_frame(
        stream,
        format_runtime_tick(sequence, sample, &transitions).as_bytes(),
    )
    .map_err(|_| RuntimeWriterError::Transport)?;
    sequence = sequence
        .checked_add(1)
        .ok_or(RuntimeWriterError::Protocol)?;
    let complete = format!(
        "schema\t{RUNTIME_COMPLETE_SCHEMA}\nobserver.started_continuous_ns\t{}\nobserver.ended_continuous_ns\t{last_ns}\nobserver.sample_count\t{sequence}\nobserver.event_count\t{event_count}\nobserver.status\t{}\n",
        started_ns.unwrap_or(last_ns),
        if observation.is_failed() {
            "not-run"
        } else {
            "complete"
        },
    );
    write_frame(stream, complete.as_bytes()).map_err(|_| RuntimeWriterError::Transport)?;
    stream
        .set_read_timeout(Some(FINAL_ACK_TIMEOUT))
        .map_err(|_| RuntimeWriterError::Transport)?;
    let ack = read_frame_before(stream, Instant::now() + FINAL_ACK_TIMEOUT)
        .map_err(|_| RuntimeWriterError::Transport)?;
    if ack != format!("schema\t{RUNTIME_ACK_SCHEMA}\nstatus\taccepted\n").as_bytes() {
        return Err(RuntimeWriterError::Protocol);
    }
    write_frame(
        stream,
        format!("schema\t{RUNTIME_CLOSED_SCHEMA}\nstatus\tconfirmed\n").as_bytes(),
    )
    .map_err(|_| RuntimeWriterError::Transport)?;
    stream.close().map_err(|_| RuntimeWriterError::Transport)?;
    if observation.is_failed() {
        Err(RuntimeWriterError::Protocol)
    } else {
        Ok(())
    }
}

fn format_runtime_tick(
    sequence: u64,
    sample: RuntimeSample,
    transitions: &[RuntimeTransition],
) -> String {
    let mut output = String::with_capacity(2048);
    let _ = writeln!(output, "schema\t{RUNTIME_TICK_SCHEMA}");
    let _ = writeln!(output, "sequence\t{sequence}");
    let _ = writeln!(output, "event_count\t{}", transitions.len());
    format_sample_records(&mut output, sample);
    for transition in transitions {
        let _ = writeln!(
            output,
            "event\t{}\t{}\t{}\t{}\t{}\t{}",
            transition.sequence,
            transition.continuous_ns,
            transition.kind.as_str(),
            transition.generation,
            transition.aux0,
            transition.aux1,
        );
    }
    output
}

fn format_sample_records(output: &mut String, sample: RuntimeSample) {
    let values = [
        sample.continuous_ns.to_string(),
        sample.worker_generation.to_string(),
        sample.screens_published.to_string(),
        sample.screens_enqueued.to_string(),
        sample.screens_superseded.to_string(),
        sample.event_queue_length.to_string(),
        sample.event_queue_high_water.to_string(),
        sample.ui_dispatches.to_string(),
        sample.ui_screen_events.to_string(),
        sample.ui_drain_high_water.to_string(),
        sample.ui_latest_generation.to_string(),
        sample.render_latest_generation.to_string(),
        sample.next_frame_generation.to_string(),
        sample.next_frame_count.to_string(),
        bool_digit(sample.presentable),
        bool_digit(sample.minimized),
        bool_digit(sample.occluded),
        bool_digit(sample.workspace_visible),
        bool_digit(sample.pane_visible),
        bool_digit(sample.live_resize),
        sample.viewport_total_rows.to_string(),
        sample.viewport_visible_rows.to_string(),
        sample.viewport_offset_rows.to_string(),
        bool_digit(sample.selection_present),
        sample.resize_requests.to_string(),
        sample.resize_notifications.to_string(),
        sample.resize_applied.to_string(),
        sample.resize_coalesced.to_string(),
        sample.pty_rows.to_string(),
        sample.pty_columns.to_string(),
        sample.pty_pixel_width.to_string(),
        sample.pty_pixel_height.to_string(),
        sample.terminal_inputs_accepted.to_string(),
        sample.lifecycle.as_str().to_owned(),
        sample.observer_drops.to_string(),
    ];
    let _ = writeln!(output, "sample\t{}", values.join("\t"));
}

fn bool_digit(value: bool) -> String {
    u8::from(value).to_string()
}

const ACCEPTANCE_ENVIRONMENT_KEYS: &[&str] = &[
    "USER",
    "LOGNAME",
    "SHELL",
    "PATH",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "TMPDIR",
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
];

pub(crate) fn clean_acceptance_environment(
    filesystem: &dyn PrivateEnvironment,
    variables: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<BTreeMap<OsString, OsString>, AcceptanceObservationError> {
    let mut result = BTreeMap::new();
    for (key, value) in variables {
        let Some(key_text) = key.to_str() else {
            continue;
        };
        if ACCEPTANCE_ENVIRONMENT_KEYS.contains(&key_text) {
            result.insert(key, value);
        }
    }
    let home = result
        .get(&OsString::from("HOME"))
        .map(PathBuf::from)
        .ok_or(AcceptanceObservationError::InvalidEnvironment)?;
    if !home.is_absolute() {
        return Err(AcceptanceObservationError::InvalidEnvironment);
    }
    let home = filesystem.validate_directory(&home)?;
    for key in [
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
    ] {
        let path = result
            .get(&OsString::from(key))
            .map(PathBuf::from)
            .ok_or(AcceptanceObservationError::InvalidEnvironment)?;
        let path = filesystem.validate_directory(&path)?;
        if !path.starts_with(&home) {
            return Err(AcceptanceObservationError::InvalidEnvironment);
        }
    }
    if !result.contains_key(&OsString::from("PATH")) {
        return Err(AcceptanceObservationError::InvalidEnvironment);
    }
    Ok(result)
}

pub(crate) fn read_frame(
    stream: &mut dyn ObservationTransport,
) -> Result<Vec<u8>, AcceptanceObservationError> {
    read_frame_before(stream, Instant::now() + SOCKET_TIMEOUT)
}
fn read_frame_before(
    stream: &mut dyn ObservationTransport,
    deadline: Instant,
) -> Result<Vec<u8>, AcceptanceObservationError> {
    fn read_exact_before(
        stream: &mut dyn ObservationTransport,
        mut output: &mut [u8],
        deadline: Instant,
    ) -> Result<(), AcceptanceObservationError> {
        while !output.is_empty() {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|duration| !duration.is_zero())
                .ok_or(AcceptanceObservationError::Transport)?;
            stream.set_read_timeout(Some(remaining))?;
            match stream.read(output) {
                Ok(0) => return Err(AcceptanceObservationError::Transport),
                Ok(count) => output = &mut output[count..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(AcceptanceObservationError::Transport),
            }
        }
        Ok(())
    }
    let mut length = [0_u8; 4];
    read_exact_before(stream, &mut length, deadline)?;
    let length = u32::from_be_bytes(length) as usize;
    if !(1..=MAX_FRAME_BYTES).contains(&length) {
        return Err(AcceptanceObservationError::InvalidChallenge);
    }
    let mut frame = vec![0_u8; length];
    read_exact_before(stream, &mut frame, deadline)?;
    Ok(frame)
}
pub(crate) fn write_frame(stream: &mut dyn ObservationTransport, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len())
        .ok()
        .filter(|length| (1..=MAX_FRAME_BYTES as u32).contains(length))
        .ok_or(io::ErrorKind::InvalidData)?;
    let deadline = Instant::now() + SOCKET_TIMEOUT;
    for mut bytes in [length.to_be_bytes().as_slice(), payload] {
        while !bytes.is_empty() {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|duration| !duration.is_zero())
                .ok_or(io::ErrorKind::TimedOut)?;
            stream
                .set_write_timeout(Some(remaining))
                .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
            match stream.write(bytes) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => bytes = &bytes[count..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }
    stream.flush()
}

impl IncomingFrames {
    fn read_available(
        &mut self,
        stream: &mut dyn ObservationTransport,
    ) -> io::Result<Vec<Vec<u8>>> {
        let mut chunk = [0_u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "acceptance action stream closed",
                    ));
                }
                Ok(length) => {
                    self.bytes.extend_from_slice(&chunk[..length]);
                    if self.bytes.len() > MAX_FRAME_BYTES + 4 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "acceptance action frame exceeds the bound",
                        ));
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }

        let mut frames = Vec::new();
        loop {
            if self.bytes.len() < 4 {
                break;
            }
            let length = u32::from_be_bytes(
                self.bytes[..4]
                    .try_into()
                    .expect("a four-byte frame prefix was checked"),
            ) as usize;
            if !(1..=MAX_FRAME_BYTES).contains(&length) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid acceptance action frame length",
                ));
            }
            if self.bytes.len() < length + 4 {
                break;
            }
            frames.push(self.bytes[4..length + 4].to_vec());
            self.bytes.drain(..length + 4);
        }
        Ok(frames)
    }
}

fn parse_failure_action(
    frame: &[u8],
    expected_nonce: &str,
    expected_run_id: &str,
    expected_app_sha256: &str,
    expected_sequence: u64,
) -> Result<FailureActionRequest, AcceptanceObservationError> {
    let frame =
        std::str::from_utf8(frame).map_err(|_| AcceptanceObservationError::InvalidChallenge)?;
    let records = parse_records(frame)?;
    let [
        schema,
        nonce,
        run_id,
        app_sha256,
        request_id,
        sequence,
        case,
        once,
    ] = records.as_slice()
    else {
        return Err(AcceptanceObservationError::InvalidChallenge);
    };
    let parsed_sequence = sequence
        .1
        .parse::<u64>()
        .map_err(|_| AcceptanceObservationError::InvalidChallenge)?;
    if case.0 != "case.id" {
        return Err(AcceptanceObservationError::InvalidChallenge);
    }
    let case =
        FailureActionCase::parse(&case.1).ok_or(AcceptanceObservationError::InvalidChallenge)?;
    if schema != &("schema", FAILURE_ACTION_SCHEMA.to_owned())
        || nonce != &("launch.nonce", expected_nonce.to_owned())
        || run_id != &("run.id", expected_run_id.to_owned())
        || app_sha256 != &("package.app.sha256", expected_app_sha256.to_owned())
        || request_id.0 != "request.id"
        || !is_lower_hex(&request_id.1, 64)
        || sequence.0 != "sequence"
        || parsed_sequence != expected_sequence
        || once != &("request.once", "true".to_owned())
    {
        return Err(AcceptanceObservationError::InvalidChallenge);
    }
    Ok(FailureActionRequest {
        id: request_id.1.clone(),
        sequence: parsed_sequence,
        case,
    })
}

fn format_failure_action_result(event: &FailureActionEvent) -> String {
    let failure = event.failure_class.map_or("none", failure_class_name);
    let recoverability = event.recoverability.map_or("none", recoverability_name);
    let operation = event.failure_operation.unwrap_or("none");
    let visible = event
        .visible_generation
        .map_or_else(|| "unavailable".to_owned(), |value| value.to_string());
    format!(
        concat!(
            "schema\t{}\n",
            "request.id\t{}\n",
            "sequence\t{}\n",
            "case.id\t{}\n",
            "action\t{}\n",
            "result\t{}\n",
            "pane.id\t{}\n",
            "pane.state\t{}\n",
            "failure.class\t{}\n",
            "failure.recoverability\t{}\n",
            "failure.operation\t{}\n",
            "state.revision\t{}\n",
            "latest.generation\t{}\n",
            "last_valid.generation\t{}\n",
            "visible.generation\t{}\n",
            "pending_recovery\t{}\n",
            "terminal_input_usable\t{}\n",
            "session_attached\t{}\n",
            "resource.staged_count\t{}\n",
            "resource.staged_bytes\t{}\n",
            "resource.rolled_back_count\t{}\n",
            "resource.rolled_back_bytes\t{}\n",
        ),
        FAILURE_ACTION_RESULT_SCHEMA,
        event.request.id,
        event.request.sequence,
        event.request.case.as_str(),
        event.phase.as_str(),
        event.result.as_str(),
        event.pane_identity,
        event.pane_state.as_str(),
        failure,
        recoverability,
        operation,
        event.state_revision,
        event.latest_generation,
        event.last_valid_generation,
        visible,
        event.pending_recovery.as_str(),
        bool_digit(event.terminal_input_usable),
        bool_digit(event.session_attached),
        event.resource_staged_count,
        event.resource_staged_bytes,
        event.resource_rolled_back_count,
        event.resource_rolled_back_bytes,
    )
}

const fn failure_class_name(class: FailureClass) -> &'static str {
    match class {
        FailureClass::Pty => "pty",
        FailureClass::Emulator => "emulator",
        FailureClass::Presentation => "presentation",
        FailureClass::Platform => "platform",
        FailureClass::Resource => "resource",
    }
}

const fn recoverability_name(recoverability: Recoverability) -> &'static str {
    match recoverability {
        Recoverability::Recoverable => "recoverable",
        Recoverability::Fatal => "fatal",
    }
}

pub(crate) fn parse_challenge(
    challenge: &[u8],
) -> Result<LaunchAuthentication, AcceptanceObservationError> {
    let challenge =
        std::str::from_utf8(challenge).map_err(|_| AcceptanceObservationError::InvalidChallenge)?;
    let records = parse_records(challenge)?;
    let [
        schema,
        nonce,
        run_id,
        app_sha256,
        runtime_schema,
        sample_interval,
        transition_capacity,
        failure_action_schema,
        failure_action_enabled,
    ] = records.as_slice()
    else {
        return Err(AcceptanceObservationError::InvalidChallenge);
    };
    if schema != &("schema", CHALLENGE_SCHEMA.to_owned())
        || nonce.0 != "launch.nonce"
        || !is_lower_hex(&nonce.1, 64)
        || run_id.0 != "run.id"
        || !is_run_id(&run_id.1)
        || app_sha256.0 != "package.app.sha256"
        || !is_lower_hex(&app_sha256.1, 64)
        || runtime_schema != &("runtime.schema", RUNTIME_SCHEMA.to_owned())
        || sample_interval
            != &(
                "runtime.sample_interval_ms",
                SAMPLE_INTERVAL.as_millis().to_string(),
            )
        || transition_capacity
            != &(
                "runtime.transition_capacity",
                TRANSITION_CAPACITY.to_string(),
            )
        || failure_action_schema != &("failure.action.schema", FAILURE_ACTION_SCHEMA.to_owned())
        || failure_action_enabled.0 != "failure.action.enabled"
        || !matches!(failure_action_enabled.1.as_str(), "true" | "false")
    {
        return Err(AcceptanceObservationError::InvalidChallenge);
    }
    Ok(LaunchAuthentication {
        nonce: nonce.1.clone(),
        run_id: run_id.1.clone(),
        app_sha256: app_sha256.1.clone(),
        failure_actions_enabled: failure_action_enabled.1 == "true",
    })
}

pub(crate) fn parse_records(
    value: &str,
) -> Result<Vec<(&str, String)>, AcceptanceObservationError> {
    if value.len() > MAX_FRAME_BYTES || !value.ends_with('\n') {
        return Err(AcceptanceObservationError::InvalidChallenge);
    }
    value
        .strip_suffix('\n')
        .expect("trailing newline was checked")
        .split('\n')
        .map(|line| {
            let (key, value) = line
                .split_once('\t')
                .ok_or(AcceptanceObservationError::InvalidChallenge)?;
            if key.is_empty() || value.contains('\t') {
                return Err(AcceptanceObservationError::InvalidChallenge);
            }
            Ok((key, decode_value(value)?))
        })
        .collect()
}

fn format_observation(
    request: &ObservationRequest,
    selected_font: &str,
    geometry: ObservationGeometry,
) -> LaunchProof {
    let mut prefix = String::new();
    let mut output = String::new();
    for (key, value) in [
        ("schema", OBSERVATION_SCHEMA.to_owned()),
        ("observation.source", "production-app".to_owned()),
        ("launch.nonce", request.nonce.clone()),
        ("run.id", request.run_id.clone()),
        ("package.app.sha256", request.app_sha256.clone()),
        ("runtime.schema", RUNTIME_SCHEMA.to_owned()),
        (
            "runtime.sample_interval_ms",
            SAMPLE_INTERVAL.as_millis().to_string(),
        ),
        (
            "runtime.transition_capacity",
            TRANSITION_CAPACITY.to_string(),
        ),
        ("failure.action.schema", FAILURE_ACTION_SCHEMA.to_owned()),
        (
            "failure.action.enabled",
            request.failure_actions_enabled.to_string(),
        ),
        ("terminal_font_selected", selected_font.to_owned()),
        ("initial_grid.rows", geometry.rows.to_string()),
        ("initial_grid.columns", geometry.columns.to_string()),
        (
            "initial_grid.logical_width",
            decimal(geometry.logical_width),
        ),
        (
            "initial_grid.logical_height",
            decimal(geometry.logical_height),
        ),
        (
            "initial_grid.backing_pixel_width",
            geometry.backing_pixel_width.to_string(),
        ),
        (
            "initial_grid.backing_pixel_height",
            geometry.backing_pixel_height.to_string(),
        ),
        ("observation.complete", "true".to_owned()),
    ] {
        if key == "terminal_font_selected" {
            prefix = std::mem::take(&mut output);
        }
        output.push_str(key);
        output.push('\t');
        output.push_str(&encode_value(&value));
        output.push('\n');
    }
    LaunchProof {
        prefix,
        suffix: output,
    }
}

pub(crate) fn encode_value(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\t', "%09")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

fn decode_value(value: &str) -> Result<String, AcceptanceObservationError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return Err(AcceptanceObservationError::InvalidChallenge);
        }
        let byte = match &bytes[index + 1..=index + 2] {
            b"25" => b'%',
            b"09" => b'\t',
            b"0D" => b'\r',
            b"0A" => b'\n',
            _ => return Err(AcceptanceObservationError::InvalidChallenge),
        };
        decoded.push(byte);
        index += 3;
    }
    String::from_utf8(decoded).map_err(|_| AcceptanceObservationError::InvalidChallenge)
}

fn decimal(value: f32) -> String {
    let formatted = format!("{value:.6}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_run_id(value: &str) -> bool {
    (1..=80).contains(&value.len())
        && value.bytes().enumerate().all(|(index, byte)| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => true,
            b'.' | b'_' | b'-' => index > 0,
            _ => false,
        })
}

/// Sleep-inclusive nanoseconds on the external observer's monotonic clock.
pub(crate) trait ContinuousClock: Send + Sync + std::fmt::Debug {
    fn now_ns(&self) -> Option<u64>;
}

/// Byte transport only. Implementations discard native errors at this boundary.
pub(crate) trait ObservationTransport: Read + Write + Send {
    fn set_read_timeout(&self, timeout: Option<Duration>)
    -> Result<(), AcceptanceObservationError>;
    fn set_write_timeout(
        &self,
        timeout: Option<Duration>,
    ) -> Result<(), AcceptanceObservationError>;
    fn close(&self) -> Result<(), AcceptanceObservationError>;
}

/// Validated content-private authentication facts with no native identity representation.
pub(crate) struct LaunchAuthentication {
    nonce: String,
    run_id: String,
    app_sha256: String,
    failure_actions_enabled: bool,
}
/// Portable launch proof sections surrounding the legacy package identity envelope.
pub(crate) struct LaunchProof {
    prefix: String,
    suffix: String,
}
impl LaunchProof {
    pub(crate) fn prefix(&self) -> &str {
        &self.prefix
    }
    pub(crate) fn suffix(&self) -> &str {
        &self.suffix
    }
}
/// Exact packaged-executable proof publication, independent of transport and timing.
/// The selected capability retains and encodes its native identity privately.
pub(crate) trait PackagedExecutable: Send {
    fn publish(
        &self,
        stream: &mut dyn ObservationTransport,
        proof: LaunchProof,
    ) -> Result<(), AcceptanceObservationError>;
}

/// Private-owner directory facts needed by acceptance environment policy.
pub(crate) trait PrivateEnvironment {
    fn validate_directory(&self, path: &Path) -> Result<PathBuf, AcceptanceObservationError>;
}

impl From<io::Error> for AcceptanceObservationError {
    fn from(_: io::Error) -> Self {
        Self::Transport
    }
}

#[derive(Clone)]
pub(crate) struct AuthenticatedObservation(Arc<ObservationOwner>);
struct ObservationOwner {
    request: Mutex<Option<ObservationRequest>>,
    writer: Mutex<Option<RuntimeWriter>>,
    runtime: RuntimeObservation,
    live: Arc<AtomicBool>,
    finishing: AtomicBool,
    completion: Mutex<Option<Result<(), AcceptanceObservationError>>>,
}
impl std::fmt::Debug for AuthenticatedObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthenticatedObservation")
    }
}
#[derive(Debug)]
pub(crate) struct ObservationLease {
    owner: Weak<ObservationOwner>,
    runtime: RuntimeObservation,
}
#[derive(Debug)]
pub(crate) struct SessionObservationLease {
    runtime: Option<RuntimeObservation>,
    live: Arc<AtomicBool>,
}
impl SessionObservationLease {
    pub(crate) fn consume(mut self) -> Option<RuntimeObservation> {
        self.live
            .load(Ordering::Acquire)
            .then(|| self.runtime.take())
            .flatten()
    }
}
impl Drop for SessionObservationLease {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            self.live.store(false, Ordering::Release);
            runtime.fail();
        }
    }
}
impl std::fmt::Debug for ObservationOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ObservationOwner")
    }
}
impl AuthenticatedObservation {
    pub(crate) fn configure(
        stream: Box<dyn ObservationTransport>,
        authentication: LaunchAuthentication,
        package: Box<dyn PackagedExecutable>,
        clock: Arc<dyn ContinuousClock>,
    ) -> Result<Self, AcceptanceObservationError> {
        let LaunchAuthentication {
            nonce,
            run_id,
            app_sha256,
            failure_actions_enabled,
        } = authentication;
        let (
            failure_action_sender,
            failure_action_receiver,
            failure_result_sender,
            failure_result_receiver,
        ) = failure_channels(failure_actions_enabled);
        let runtime = RuntimeObservation::with_clock(clock);
        Ok(Self(Arc::new(ObservationOwner {
            request: Mutex::new(Some(ObservationRequest {
                stream,
                package,
                nonce,
                run_id,
                app_sha256,
                failure_actions_enabled,
                initial: None,
                runtime: runtime.clone(),
                failure_action_sender,
                failure_action_receiver,
                failure_result_sender,
                failure_result_receiver,
            })),
            writer: Mutex::new(None),
            runtime,
            live: Arc::new(AtomicBool::new(true)),
            finishing: AtomicBool::new(false),
            completion: Mutex::new(None),
        })))
    }
    pub(crate) fn claim_session(
        &self,
        selected_font: &str,
        geometry: ObservationGeometry,
    ) -> Option<ClaimedObservation> {
        if !self.0.live.load(Ordering::Acquire) || selected_font.len() > 256 {
            return None;
        }
        let mut slot = self.0.request.lock().ok()?;
        let request = slot.as_mut()?;
        if request.initial.is_some() {
            return None;
        }
        request.initial = Some(ObservationSelection {
            selected_font: selected_font.to_owned(),
            geometry,
        });
        let pane_runtime = request.runtime.scoped();
        Some(ClaimedObservation {
            runtime: pane_runtime.clone(),
            failure_actions: request
                .failure_action_receiver
                .take()
                .zip(request.failure_result_sender.take())
                .map(|(commands, results)| FailureActionController {
                    commands,
                    results,
                    live: Arc::clone(&self.0.live),
                }),
            lease: ObservationLease {
                owner: Arc::downgrade(&self.0),
                runtime: pane_runtime,
            },
            session: SessionObservationLease {
                runtime: Some(request.runtime.scoped()),
                live: Arc::clone(&self.0.live),
            },
        })
    }
    /// Called only on a background executor or after the event loop, never on GPUI's foreground.
    pub(crate) fn finish(&self) -> Result<(), AcceptanceObservationError> {
        let mut completion = self
            .0
            .completion
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(result) = *completion {
            return result;
        }
        self.0.finishing.store(true, Ordering::Release);
        self.0.live.store(false, Ordering::Release);
        // Serialize finalization, including the post-event-loop fallback, through writer ownership.
        let mut writer = self
            .0
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.0
            .request
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let result = if let Some(writer) = writer.take() {
            finish_runtime_writer(writer)
        } else {
            self.0.runtime.fail();
            self.0.runtime.seal_and_drain_transitions();
            Err(AcceptanceObservationError::InvalidChallenge)
        };
        *completion = Some(result);
        result
    }
}
impl Drop for ObservationOwner {
    fn drop(&mut self) {
        self.live.store(false, Ordering::Release);
        if !self
            .completion
            .get_mut()
            .is_ok_and(|completion| matches!(completion, Some(Ok(()))))
        {
            self.runtime.fail();
        }
        self.runtime.revoke_producers();
        // Dropping the shutdown sender transfers bounded cleanup to the existing writer thread.
    }
}
impl ObservationLease {
    pub(crate) fn update_geometry(&self, geometry: ObservationGeometry) {
        let Some(owner) = self
            .owner
            .upgrade()
            .filter(|owner| owner.live.load(Ordering::Acquire))
        else {
            return;
        };
        if let Ok(mut request) = owner.request.lock()
            && let Some(initial) = request
                .as_mut()
                .and_then(|request| request.initial.as_mut())
        {
            initial.geometry = geometry;
        }
    }
    pub(crate) fn prepare_once(&self, rows: u16, columns: u16) -> Option<PreparedObservation> {
        let owner = self
            .owner
            .upgrade()
            .filter(|owner| owner.live.load(Ordering::Acquire))?;
        let mut slot = owner.request.lock().ok()?;
        let initial = slot.as_ref()?.initial.as_ref()?;
        if initial.geometry.rows != rows || initial.geometry.columns != columns {
            return None;
        }
        let mut request = slot.take()?;
        let initial = request.initial.take()?;
        Some(PreparedObservation {
            request,
            initial,
            owner: Arc::downgrade(&owner),
            cleanup: PreparedCleanup {
                owner: Arc::downgrade(&owner),
                armed: true,
            },
        })
    }
}
impl Drop for ObservationLease {
    fn drop(&mut self) {
        self.runtime.revoke_handle();
        if let Some(owner) = self.owner.upgrade() {
            owner.live.store(false, Ordering::Release);
        }
    }
}
impl PreparedObservation {
    pub(crate) fn emit(self) -> Result<(), AcceptanceObservationError> {
        self.emit_with(|work| {
            thread::Builder::new()
                .name("spaceterm-acceptance-observer".to_owned())
                .spawn(work)
        })
    }
    fn emit_with(
        self,
        spawn: impl FnOnce(
            Box<dyn FnOnce() -> Result<(), RuntimeWriterError> + Send>,
        ) -> io::Result<JoinHandle<Result<(), RuntimeWriterError>>>,
    ) -> Result<(), AcceptanceObservationError> {
        let owner = self
            .owner
            .upgrade()
            .ok_or(AcceptanceObservationError::InvalidChallenge)?;
        let mut slot = owner
            .writer
            .try_lock()
            .map_err(|_| AcceptanceObservationError::InvalidChallenge)?;
        if owner.finishing.load(Ordering::Acquire)
            || !owner.live.load(Ordering::Acquire)
            || slot.is_some()
        {
            return Err(AcceptanceObservationError::InvalidChallenge);
        }
        let PreparedObservation {
            mut request,
            initial,
            mut cleanup,
            ..
        } = self;
        let (shutdown, receiver) = mpsc::channel();
        let observation = request.runtime.clone();
        let thread_observation = observation.clone();
        let live = Arc::clone(&owner.live);
        let thread = spawn(Box::new(move || {
            let result = (|| {
                let record = format_observation(&request, &initial.selected_font, initial.geometry);
                request
                    .stream
                    .set_read_timeout(Some(FAILURE_ACTION_POLL_INTERVAL))
                    .map_err(|_| RuntimeWriterError::Transport)?;
                request
                    .stream
                    .set_write_timeout(Some(SOCKET_TIMEOUT))
                    .map_err(|_| RuntimeWriterError::Transport)?;
                request
                    .package
                    .publish(request.stream.as_mut(), record)
                    .map_err(|_| RuntimeWriterError::Transport)?;
                run_runtime_writer(
                    request.stream.as_mut(),
                    &thread_observation,
                    &receiver,
                    FailureTransport {
                        nonce: request.nonce,
                        run_id: request.run_id,
                        app_sha256: request.app_sha256,
                        requests: request.failure_action_sender,
                        results: request.failure_result_receiver,
                    },
                )
            })();
            live.store(false, Ordering::Release);
            if result.is_err() {
                thread_observation.fail();
            }
            thread_observation.revoke_producers();
            cleanup.disarm();
            result
        }))
        .map_err(|_| AcceptanceObservationError::Transport)?;
        *slot = Some(RuntimeWriter { shutdown, thread });
        Ok(())
    }
}
fn finish_runtime_writer(writer: RuntimeWriter) -> Result<(), AcceptanceObservationError> {
    let _ = writer.shutdown.send(());
    match writer.thread.join() {
        Ok(Ok(())) => Ok(()),
        _ => Err(AcceptanceObservationError::Transport),
    }
}
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct TestClock(std::sync::atomic::AtomicU64);
#[cfg(test)]
impl ContinuousClock for TestClock {
    fn now_ns(&self) -> Option<u64> {
        Some(self.0.fetch_add(1, Ordering::Relaxed))
    }
}

#[cfg(test)]
pub(crate) mod tests;

impl std::fmt::Debug for FailureActionRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FailureActionRequest")
            .field("sequence", &self.sequence)
            .field("case", &self.case)
            .finish_non_exhaustive()
    }
}
#[derive(Default)]
struct FailureAuthority {
    pending: Option<PendingFailure>,
}
struct PendingFailure {
    request: FailureActionRequest,
    previous: Option<FailureActionEvent>,
    injected: Option<FailureActionEvent>,
}
impl FailureAuthority {
    fn request(&mut self, request: FailureActionRequest) -> Result<(), RuntimeWriterError> {
        if self.pending.is_some() {
            return Err(RuntimeWriterError::Protocol);
        }
        self.pending = Some(PendingFailure {
            request,
            previous: None,
            injected: None,
        });
        Ok(())
    }
    fn event(&mut self, event: &FailureActionEvent) -> Result<(), RuntimeWriterError> {
        let pending = self.pending.as_mut().ok_or(RuntimeWriterError::Protocol)?;
        if pending.request != event.request
            || pending.previous.as_ref().is_some_and(|previous| {
                previous.pane_identity != event.pane_identity
                    || event.state_revision < previous.state_revision
            })
            || !event.valid_evidence(pending.injected.as_ref())
        {
            return Err(RuntimeWriterError::Protocol);
        }
        let fatal = event.request.case.is_fatal();
        let normal = event.request.case == FailureActionCase::NormalExitControl;
        let valid = match (
            pending.previous.as_ref().map(|event| event.phase),
            event.phase,
            event.result,
        ) {
            (None, FailureActionPhase::Armed, FailureActionResult::Accepted) => true,
            (
                Some(FailureActionPhase::Armed),
                FailureActionPhase::Injected,
                FailureActionResult::FailedState,
            ) => !normal,
            (
                Some(FailureActionPhase::Injected),
                FailureActionPhase::RetryRequested,
                FailureActionResult::Accepted,
            ) => !normal && !fatal,
            (
                Some(FailureActionPhase::RetryRequested),
                FailureActionPhase::Completed,
                FailureActionResult::Recovered,
            ) => !normal && !fatal,
            (
                Some(FailureActionPhase::Injected),
                FailureActionPhase::Completed,
                FailureActionResult::Closed,
            ) => fatal,
            (
                Some(FailureActionPhase::Armed),
                FailureActionPhase::Completed,
                FailureActionResult::Exited,
            ) => normal,
            _ => false,
        };
        if !valid {
            return Err(RuntimeWriterError::Protocol);
        }
        if event.phase == FailureActionPhase::Injected {
            pending.injected = Some(event.clone());
        }
        pending.previous = Some(event.clone());
        if event.phase == FailureActionPhase::Completed {
            self.pending = None;
        }
        Ok(())
    }
}
impl FailureActionCase {
    const fn is_fatal(self) -> bool {
        matches!(self, Self::PtyFatal | Self::EmulatorFatal)
    }
    const fn failure_evidence(
        self,
    ) -> Option<(
        FailureClass,
        Recoverability,
        &'static str,
        FailurePendingRecovery,
    )> {
        Some(match self {
            Self::PresentationInvalidScale => (
                FailureClass::Presentation,
                Recoverability::Recoverable,
                "update-backing-scale",
                FailurePendingRecovery::Presentation,
            ),
            Self::PresentationGlyph => (
                FailureClass::Presentation,
                Recoverability::Recoverable,
                "paint-terminal-presentation",
                FailurePendingRecovery::Presentation,
            ),
            Self::RendererImagePreflight => (
                FailureClass::Resource,
                Recoverability::Recoverable,
                "paint-terminal-graphics",
                FailurePendingRecovery::RendererResources,
            ),
            Self::RendererResourceBeforeSync | Self::RendererResourceAfterStaging => (
                FailureClass::Resource,
                Recoverability::Recoverable,
                "prepare-terminal-graphics",
                FailurePendingRecovery::RendererResources,
            ),
            Self::PasteboardWrite => (
                FailureClass::Platform,
                Recoverability::Recoverable,
                "write-selection-pasteboard",
                FailurePendingRecovery::CopySelection,
            ),
            Self::PtyFatal => (
                FailureClass::Pty,
                Recoverability::Fatal,
                "read-shell-output",
                FailurePendingRecovery::None,
            ),
            Self::EmulatorFatal => (
                FailureClass::Emulator,
                Recoverability::Fatal,
                "session-runtime",
                FailurePendingRecovery::None,
            ),
            Self::NormalExitControl => return None,
        })
    }
}
impl FailureActionEvent {
    fn valid_evidence(&self, injected: Option<&Self>) -> bool {
        let case = self.request.case;
        let fatal = case.is_fatal();
        let armed = self.phase == FailureActionPhase::Armed;
        let completed = self.phase == FailureActionPhase::Completed;
        let pasteboard = case == FailureActionCase::PasteboardWrite;
        if self.latest_generation < self.last_valid_generation
            || self
                .visible_generation
                .is_some_and(|visible| visible > self.latest_generation)
            || self.resource_staged_count > 65536
            || self.resource_rolled_back_count > 65536
            || self.resource_staged_bytes > 402653184
            || self.resource_rolled_back_bytes > 402653184
        {
            return false;
        }
        let expected = if armed || (completed && !fatal) {
            None
        } else {
            case.failure_evidence()
        };
        let typed = match expected {
            None => {
                self.failure_class.is_none()
                    && self.recoverability.is_none()
                    && self.failure_operation.is_none()
                    && self.pending_recovery == FailurePendingRecovery::None
            }
            Some((class, recoverability, operation, pending)) => {
                self.failure_class == Some(class)
                    && self.recoverability == Some(recoverability)
                    && self.failure_operation == Some(operation)
                    && self.pending_recovery == pending
            }
        };
        if !typed {
            return false;
        }
        let state = match (self.phase, self.result) {
            (FailureActionPhase::Armed, FailureActionResult::Accepted)
            | (FailureActionPhase::Completed, FailureActionResult::Recovered) => {
                self.pane_state == FailurePaneState::Running && self.session_attached
            }
            (FailureActionPhase::Injected, FailureActionResult::FailedState) => {
                self.pane_state == FailurePaneState::Failed
                    && self.session_attached
                    && (!fatal || !self.terminal_input_usable)
                    && (!pasteboard || self.terminal_input_usable)
            }
            (FailureActionPhase::RetryRequested, FailureActionResult::Accepted) => {
                self.pane_state == FailurePaneState::Failed && self.session_attached
            }
            (FailureActionPhase::Completed, FailureActionResult::Closed) => {
                self.pane_state == FailurePaneState::Failed
                    && !self.session_attached
                    && !self.terminal_input_usable
            }
            (FailureActionPhase::Completed, FailureActionResult::Exited) => {
                self.pane_state == FailurePaneState::Exited && self.session_attached
            }
            _ => false,
        };
        if !state {
            return false;
        }
        if self.phase == FailureActionPhase::Injected
            && !fatal
            && self.visible_generation != Some(self.last_valid_generation)
        {
            return false;
        }
        if case != FailureActionCase::RendererResourceAfterStaging || armed {
            if self.resource_staged_count != 0
                || self.resource_staged_bytes != 0
                || self.resource_rolled_back_count != 0
                || self.resource_rolled_back_bytes != 0
            {
                return false;
            }
        } else if self.resource_staged_count == 0
            || self.resource_staged_bytes == 0
            || self.resource_staged_count != self.resource_rolled_back_count
            || self.resource_staged_bytes != self.resource_rolled_back_bytes
            || injected.is_some_and(|injected| {
                self.resource_staged_count != injected.resource_staged_count
                    || self.resource_staged_bytes != injected.resource_staged_bytes
            })
        {
            return false;
        }
        let retry = self.phase == FailureActionPhase::RetryRequested;
        let recovered = completed && self.result == FailureActionResult::Recovered;
        if retry || recovered {
            let Some(injected) = injected else {
                return false;
            };
            let Some(visible) = self.visible_generation else {
                return false;
            };
            if pasteboard {
                return self.latest_generation >= injected.latest_generation
                    && self.last_valid_generation >= injected.last_valid_generation
                    && injected
                        .visible_generation
                        .is_some_and(|old| visible >= old)
                    && visible == self.last_valid_generation
                    && self.terminal_input_usable
                    && self.session_attached;
            }
            if self.latest_generation != injected.latest_generation {
                return false;
            }
            if retry {
                return self.last_valid_generation == injected.last_valid_generation
                    && Some(visible) == injected.visible_generation;
            }
            return self.last_valid_generation == self.latest_generation
                && visible == self.latest_generation;
        }
        true
    }
}

struct WriterCadence {
    deadline: Instant,
    previous: Instant,
}
impl WriterCadence {
    fn new(now: Instant) -> Self {
        Self {
            deadline: now,
            previous: now,
        }
    }
    /// Returns whether a due sample missed its allowed lateness, preserving absolute cadence.
    fn poll(&mut self, now: Instant) -> Result<Option<bool>, RuntimeWriterError> {
        if now < self.previous {
            return Err(RuntimeWriterError::Protocol);
        }
        self.previous = now;
        if now < self.deadline {
            return Ok(None);
        }
        let late = now.duration_since(self.deadline) > SAMPLE_LATE_TOLERANCE;
        self.deadline = if late { now } else { self.deadline }
            .checked_add(SAMPLE_INTERVAL)
            .ok_or(RuntimeWriterError::Protocol)?;
        Ok(Some(late))
    }
}
