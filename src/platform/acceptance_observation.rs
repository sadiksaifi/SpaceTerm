use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs::{File, Metadata};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::terminal::{
    FailureClass, Recoverability, RuntimeLifecycle, RuntimeObservation, RuntimeSample,
    RuntimeTransition,
};

const SOCKET_ENV: &str = "SPACETERM_ACCEPTANCE_SOCKET";
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
const NORMAL_QUIT_CLASS_CODE: u64 = 4;

static REQUEST: OnceLock<Mutex<Option<ObservationRequest>>> = OnceLock::new();
static RUNTIME_WRITER: OnceLock<Mutex<Option<RuntimeWriter>>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ObservationGeometry {
    pub(crate) rows: u16,
    pub(crate) columns: u16,
    pub(crate) logical_width: f32,
    pub(crate) logical_height: f32,
    pub(crate) backing_pixel_width: u32,
    pub(crate) backing_pixel_height: u32,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AcceptanceObservationError {
    #[error("acceptance observation socket is not an absolute private Unix socket")]
    InvalidSocket,
    #[error("acceptance observation challenge is invalid")]
    InvalidChallenge,
    #[error("acceptance observation was configured more than once")]
    ConfiguredTwice,
    #[error("acceptance observation failed: {0}")]
    Io(#[from] io::Error),
    #[error("acceptance launch environment is not clean")]
    InvalidEnvironment,
}

#[derive(Debug)]
struct ObservationRequest {
    stream: UnixStream,
    nonce: String,
    run_id: String,
    app_sha256: String,
    failure_actions_enabled: bool,
    initial: Option<ObservationSelection>,
    runtime: RuntimeObservation,
    runtime_session_pending: bool,
    failure_action_sender: Option<async_channel::Sender<FailureActionRequest>>,
    failure_action_receiver: Option<async_channel::Receiver<FailureActionRequest>>,
    failure_result_sender: Option<mpsc::Sender<FailureActionEvent>>,
    failure_result_receiver: Option<mpsc::Receiver<FailureActionEvent>>,
}

#[derive(Debug)]
pub(crate) struct PreparedObservation {
    request: ObservationRequest,
    initial: ObservationSelection,
}

#[derive(Debug)]
pub(crate) struct ClaimedObservation {
    pub(crate) runtime: RuntimeObservation,
    pub(crate) failure_actions: Option<FailureActionController>,
}

#[derive(Clone, Debug)]
pub(crate) struct FailureActionController {
    commands: async_channel::Receiver<FailureActionRequest>,
    results: mpsc::Sender<FailureActionEvent>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
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
    observation: RuntimeObservation,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PackagedExecutableIdentity {
    device: u64,
    inode: u64,
}

pub(crate) fn configure_from_environment() -> Result<(), AcceptanceObservationError> {
    let socket_path = take_socket_environment()?;
    let Some(socket_path) = socket_path else {
        return Ok(());
    };
    sanitize_acceptance_process_environment()?;
    let packaged_executable = packaged_executable_identity()?;
    let mut stream = connect_private_socket(&socket_path)?;
    let challenge = read_frame(&mut stream)?;
    let (nonce, run_id, app_sha256, failure_actions_enabled) =
        parse_challenge(&challenge, packaged_executable)?;
    let (
        failure_action_sender,
        failure_action_receiver,
        failure_result_sender,
        failure_result_receiver,
    ) = failure_channels(failure_actions_enabled);
    REQUEST
        .set(Mutex::new(Some(ObservationRequest {
            stream,
            nonce,
            run_id,
            app_sha256,
            failure_actions_enabled,
            initial: None,
            runtime: RuntimeObservation::new(),
            runtime_session_pending: false,
            failure_action_sender,
            failure_action_receiver,
            failure_result_sender,
            failure_result_receiver,
        })))
        .map_err(|_| AcceptanceObservationError::ConfiguredTwice)
}

#[allow(clippy::type_complexity)]
fn failure_channels(
    enabled: bool,
) -> (
    Option<async_channel::Sender<FailureActionRequest>>,
    Option<async_channel::Receiver<FailureActionRequest>>,
    Option<mpsc::Sender<FailureActionEvent>>,
    Option<mpsc::Receiver<FailureActionEvent>>,
) {
    if !enabled {
        return (None, None, None, None);
    }
    let (action_sender, action_receiver) = async_channel::bounded(1);
    let (result_sender, result_receiver) = mpsc::channel();
    (
        Some(action_sender),
        Some(action_receiver),
        Some(result_sender),
        Some(result_receiver),
    )
}

pub(crate) fn take_runtime_session_observation() -> Option<RuntimeObservation> {
    let request = REQUEST.get()?;
    let mut request = request
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let request = request.as_mut()?;
    if !request.runtime_session_pending {
        return None;
    }
    request.runtime_session_pending = false;
    Some(request.runtime.clone())
}

pub(crate) fn claim_session(
    selected_font: &str,
    geometry: ObservationGeometry,
) -> Option<ClaimedObservation> {
    let request = REQUEST.get()?;
    let mut request = request
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let request = request.as_mut()?;
    if request.initial.is_some() {
        return None;
    }
    request.initial = Some(ObservationSelection {
        selected_font: selected_font.to_owned(),
        geometry,
    });
    request.runtime_session_pending = true;
    Some(ClaimedObservation {
        runtime: request.runtime.clone(),
        failure_actions: request
            .failure_action_receiver
            .take()
            .zip(request.failure_result_sender.clone())
            .map(|(commands, results)| FailureActionController { commands, results }),
    })
}

impl FailureActionController {
    pub(crate) async fn receive(&self) -> Option<FailureActionRequest> {
        self.commands.recv().await.ok()
    }

    pub(crate) fn emit(&self, event: FailureActionEvent) -> bool {
        self.results.send(event).is_ok()
    }

    #[cfg(test)]
    pub(crate) fn test_channel() -> (
        Self,
        async_channel::Sender<FailureActionRequest>,
        mpsc::Receiver<FailureActionEvent>,
    ) {
        let (requests, commands) = async_channel::bounded(1);
        let (results, events) = mpsc::channel();
        (Self { commands, results }, requests, events)
    }
}

pub(crate) fn update_geometry(geometry: ObservationGeometry) {
    let Some(request) = REQUEST.get() else {
        return;
    };
    let mut request = request
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(initial) = request
        .as_mut()
        .and_then(|request| request.initial.as_mut())
    else {
        return;
    };
    initial.geometry = geometry;
}

pub(crate) fn prepare_once(rows: u16, columns: u16) -> Option<PreparedObservation> {
    let request = REQUEST.get()?;
    let mut request = request
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let initial = request.as_ref()?.initial.as_ref()?;
    if initial.geometry.rows != rows || initial.geometry.columns != columns {
        return None;
    }
    let mut request = request.take()?;
    let initial = request.initial.take()?;
    Some(PreparedObservation { request, initial })
}

impl PreparedObservation {
    pub(crate) fn emit(mut self) -> Result<(), AcceptanceObservationError> {
        let executable = env::current_exe()?.canonicalize()?;
        let executable_metadata = executable.metadata()?;
        let record = format_observation(
            &self.request,
            &self.initial.selected_font,
            self.initial.geometry,
            &executable,
            executable_metadata.dev(),
            executable_metadata.ino(),
        );
        write_frame(&mut self.request.stream, record.as_bytes())?;
        let failure_action_sender = self.request.failure_action_sender.clone();
        let failure_result_receiver = self.request.failure_result_receiver.take();
        start_runtime_writer(
            self.request.stream,
            self.request.runtime.clone(),
            FailureTransport {
                nonce: self.request.nonce,
                run_id: self.request.run_id,
                app_sha256: self.request.app_sha256,
                requests: failure_action_sender,
                results: failure_result_receiver,
            },
        )?;
        Ok(())
    }
}

pub(crate) fn finish_runtime_observation() -> Result<(), AcceptanceObservationError> {
    let Some(writer) = RUNTIME_WRITER.get() else {
        return Ok(());
    };
    finish_runtime_writer_slot(writer)
}

fn finish_runtime_writer_slot(
    slot: &Mutex<Option<RuntimeWriter>>,
) -> Result<(), AcceptanceObservationError> {
    let Some(writer) = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    else {
        return Ok(());
    };
    writer.observation.session_exited(NORMAL_QUIT_CLASS_CODE);
    let _ = writer.shutdown.send(());
    match writer.thread.join() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) | Err(_) => Err(AcceptanceObservationError::Io(io::Error::other(
            "runtime observation did not complete",
        ))),
    }
}

fn start_runtime_writer(
    stream: UnixStream,
    observation: RuntimeObservation,
    failure: FailureTransport,
) -> Result<(), AcceptanceObservationError> {
    let writer = spawn_runtime_writer(stream, observation, failure)?;
    RUNTIME_WRITER
        .set(Mutex::new(Some(writer)))
        .map_err(|_| AcceptanceObservationError::ConfiguredTwice)
}

fn spawn_runtime_writer(
    mut stream: UnixStream,
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
            move || run_runtime_writer(&mut stream, &observation, &receiver, failure)
        })?;
    Ok(RuntimeWriter {
        observation,
        shutdown,
        thread,
    })
}

fn run_runtime_writer(
    stream: &mut UnixStream,
    observation: &RuntimeObservation,
    shutdown: &mpsc::Receiver<()>,
    failure: FailureTransport,
) -> Result<(), RuntimeWriterError> {
    let mut sequence = 0_u64;
    let mut deadline = Instant::now();
    let mut started_ns = None;
    let mut event_count = 0_u64;
    let mut expected_action_sequence = 0_u64;
    let mut incoming = IncomingFrames::default();

    let last_periodic_ns = loop {
        let now = Instant::now();
        if now > deadline + SAMPLE_LATE_TOLERANCE {
            observation.fail();
            deadline = now;
        }
        if now >= deadline {
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
            deadline += SAMPLE_INTERVAL;
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
                        .min(deadline.saturating_duration_since(Instant::now())),
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
            write_frame(stream, format_failure_action_result(&result).as_bytes())
                .map_err(|_| RuntimeWriterError::Transport)?;
        }
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
    let ack = read_frame(stream).map_err(|_| RuntimeWriterError::Transport)?;
    if ack != format!("schema\t{RUNTIME_ACK_SCHEMA}\nstatus\taccepted\n").as_bytes() {
        return Err(RuntimeWriterError::Protocol);
    }
    write_frame(
        stream,
        format!("schema\t{RUNTIME_CLOSED_SCHEMA}\nstatus\tconfirmed\n").as_bytes(),
    )
    .map_err(|_| RuntimeWriterError::Transport)?;
    stream
        .shutdown(std::net::Shutdown::Both)
        .map_err(|_| RuntimeWriterError::Transport)
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

fn clean_acceptance_environment(
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
    let home_source_metadata = home
        .symlink_metadata()
        .map_err(|_| AcceptanceObservationError::InvalidEnvironment)?;
    if home_source_metadata.file_type().is_symlink() {
        return Err(AcceptanceObservationError::InvalidEnvironment);
    }
    let home = home
        .canonicalize()
        .map_err(|_| AcceptanceObservationError::InvalidEnvironment)?;
    let home_metadata = home
        .symlink_metadata()
        .map_err(|_| AcceptanceObservationError::InvalidEnvironment)?;
    if !home_metadata.is_dir()
        || home_metadata.file_type().is_symlink()
        || !is_private_owner(&home_metadata)
    {
        return Err(AcceptanceObservationError::InvalidEnvironment);
    }
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
        let source_metadata = path
            .symlink_metadata()
            .map_err(|_| AcceptanceObservationError::InvalidEnvironment)?;
        if source_metadata.file_type().is_symlink() {
            return Err(AcceptanceObservationError::InvalidEnvironment);
        }
        let path = path
            .canonicalize()
            .map_err(|_| AcceptanceObservationError::InvalidEnvironment)?;
        let metadata = path
            .symlink_metadata()
            .map_err(|_| AcceptanceObservationError::InvalidEnvironment)?;
        if !path.starts_with(&home)
            || !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || !is_private_owner(&metadata)
        {
            return Err(AcceptanceObservationError::InvalidEnvironment);
        }
    }
    if !result.contains_key(&OsString::from("PATH")) {
        return Err(AcceptanceObservationError::InvalidEnvironment);
    }
    Ok(result)
}

fn sanitize_acceptance_process_environment() -> Result<(), AcceptanceObservationError> {
    let current = env::vars_os().collect::<Vec<_>>();
    let clean = clean_acceptance_environment(current.iter().cloned())?;
    // SAFETY: configure_from_environment runs at the first instruction in main, before GPUI or any
    // application worker thread exists. Replacing the environment here cannot race another thread.
    unsafe {
        for (key, _) in &current {
            env::remove_var(key);
        }
        for (key, value) in clean {
            env::set_var(key, value);
        }
    }
    Ok(())
}

fn take_socket_environment() -> Result<Option<PathBuf>, AcceptanceObservationError> {
    let value = env::var_os(SOCKET_ENV);
    // This runs at the beginning of main, before GPUI creates worker threads. The socket name is
    // never inherited by the Shell Process, and the connected descriptor is close-on-exec.
    unsafe {
        env::remove_var(SOCKET_ENV);
    }
    value
        .map(|value| {
            value
                .into_string()
                .map(PathBuf::from)
                .map_err(|_| AcceptanceObservationError::InvalidSocket)
        })
        .transpose()
}

fn packaged_executable_identity() -> Result<PackagedExecutableIdentity, AcceptanceObservationError>
{
    let executable = env::current_exe()?.canonicalize()?;
    if !executable.ends_with(Path::new("SpaceTerm.app/Contents/MacOS/SpaceTerm")) {
        return Err(AcceptanceObservationError::InvalidChallenge);
    }
    let file = File::open(&executable)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(AcceptanceObservationError::InvalidChallenge);
    }
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    if unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    let filesystem = unsafe { filesystem.assume_init() };
    if filesystem.f_flags & u32::try_from(libc::MNT_RDONLY).unwrap_or_default() == 0 {
        return Err(AcceptanceObservationError::InvalidChallenge);
    }
    Ok(PackagedExecutableIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn connect_private_socket(path: &Path) -> Result<UnixStream, AcceptanceObservationError> {
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
    Ok(stream)
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

fn read_frame(stream: &mut UnixStream) -> Result<Vec<u8>, AcceptanceObservationError> {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if !(1..=MAX_FRAME_BYTES).contains(&length) {
        return Err(AcceptanceObservationError::InvalidChallenge);
    }
    let mut frame = vec![0_u8; length];
    stream.read_exact(&mut frame)?;
    Ok(frame)
}

fn write_frame(stream: &mut UnixStream, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len())
        .ok()
        .filter(|length| (1..=MAX_FRAME_BYTES as u32).contains(length))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid frame length"))?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(payload)?;
    stream.flush()
}

impl IncomingFrames {
    fn read_available(&mut self, stream: &mut UnixStream) -> io::Result<Vec<Vec<u8>>> {
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
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
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

fn parse_challenge(
    challenge: &[u8],
    packaged_executable: PackagedExecutableIdentity,
) -> Result<(String, String, String, bool), AcceptanceObservationError> {
    let challenge =
        std::str::from_utf8(challenge).map_err(|_| AcceptanceObservationError::InvalidChallenge)?;
    let records = parse_records(challenge)?;
    let [
        schema,
        nonce,
        run_id,
        app_sha256,
        executable_device,
        executable_inode,
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
        || executable_device
            != &(
                "package.app.executable.device",
                packaged_executable.device.to_string(),
            )
        || executable_inode
            != &(
                "package.app.executable.inode",
                packaged_executable.inode.to_string(),
            )
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
    Ok((
        nonce.1.clone(),
        run_id.1.clone(),
        app_sha256.1.clone(),
        failure_action_enabled.1 == "true",
    ))
}

fn parse_records(value: &str) -> Result<Vec<(&str, String)>, AcceptanceObservationError> {
    if !value.ends_with('\n') {
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
    executable: &Path,
    executable_device: u64,
    executable_inode: u64,
) -> String {
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
        ("process.pid", std::process::id().to_string()),
        (
            "process.executable.path",
            executable.to_string_lossy().into_owned(),
        ),
        ("process.executable.device", executable_device.to_string()),
        ("process.executable.inode", executable_inode.to_string()),
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
        output.push_str(key);
        output.push('\t');
        output.push_str(&encode_value(&value));
        output.push('\n');
    }
    output
}

fn encode_value(value: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::geometry::{
        BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry,
    };
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn request(stream: UnixStream) -> ObservationRequest {
        let (failure_action_sender, failure_action_receiver) = async_channel::bounded(1);
        let (failure_result_sender, failure_result_receiver) = mpsc::channel();
        ObservationRequest {
            stream,
            nonce: "a".repeat(64),
            run_id: "i43-proof".to_owned(),
            app_sha256: "b".repeat(64),
            initial: None,
            runtime: RuntimeObservation::new(),
            runtime_session_pending: false,
            failure_actions_enabled: true,
            failure_action_sender: Some(failure_action_sender),
            failure_action_receiver: Some(failure_action_receiver),
            failure_result_sender: Some(failure_result_sender),
            failure_result_receiver: Some(failure_result_receiver),
        }
    }

    fn running_observation() -> RuntimeObservation {
        let observation = RuntimeObservation::new();
        observation.worker_started(TerminalGeometry::from_grid(
            CellGridSize::new(80, 24),
            LogicalCellSize::new(10.0, 20.0),
            BackingScale::new(2.0).unwrap(),
        ));
        observation
    }

    fn runtime_writer_fixture(
        observation: RuntimeObservation,
    ) -> (Arc<Mutex<Option<RuntimeWriter>>>, UnixStream) {
        let (writer_stream, peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        peer.set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let writer = spawn_runtime_writer(
            writer_stream,
            observation,
            FailureTransport {
                nonce: "a".repeat(64),
                run_id: "i43-proof".to_owned(),
                app_sha256: "b".repeat(64),
                requests: None,
                results: None,
            },
        )
        .unwrap();
        (Arc::new(Mutex::new(Some(writer))), peer)
    }

    fn read_text_frame(peer: &mut UnixStream) -> String {
        String::from_utf8(read_frame(peer).unwrap()).unwrap()
    }

    fn accept_runtime_closure(peer: &mut UnixStream) -> Vec<String> {
        let mut frames = Vec::new();
        loop {
            let frame = read_text_frame(peer);
            let complete = frame.starts_with(&format!("schema\t{RUNTIME_COMPLETE_SCHEMA}\n"));
            frames.push(frame);
            if complete {
                break;
            }
        }
        write_frame(
            peer,
            format!("schema\t{RUNTIME_ACK_SCHEMA}\nstatus\taccepted\n").as_bytes(),
        )
        .unwrap();
        assert_eq!(
            read_text_frame(peer),
            format!("schema\t{RUNTIME_CLOSED_SCHEMA}\nstatus\tconfirmed\n")
        );
        frames
    }

    #[test]
    fn clean_environment_should_isolate_real_zsh_and_bash_from_hostile_startup_overrides() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "spaceterm-acceptance-environment-{}-{unique}",
            std::process::id()
        ));
        let home = root.join("home");
        let config = home.join(".xdg/config");
        let data = home.join(".xdg/data");
        let state = home.join(".xdg/state");
        let cache = home.join(".xdg/cache");
        let hostile = root.join("permanent-sentinel");
        for path in [&home, &config, &data, &state, &cache, &hostile] {
            std::fs::create_dir_all(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let marker = hostile.join("startup-was-read");
        std::fs::write(
            hostile.join(".zshenv"),
            format!(
                "print -r -- PERMANENT_SENTINEL\nprint -r -- TRUST_PROMPT\n: > {}\n",
                marker.display()
            ),
        )
        .unwrap();
        let bash_environment = hostile.join("bash-env");
        std::fs::write(
            &bash_environment,
            format!(
                "printf 'PERMANENT_SENTINEL\\nTRUST_PROMPT\\n'\n: > {}\n",
                marker.display()
            ),
        )
        .unwrap();

        let variables = [
            ("USER", "fixture-user".to_owned()),
            ("LOGNAME", "fixture-user".to_owned()),
            ("SHELL", "/bin/zsh".to_owned()),
            ("PATH", "/usr/bin:/bin".to_owned()),
            ("LANG", "C".to_owned()),
            ("LC_ALL", "C".to_owned()),
            ("TMPDIR", root.to_string_lossy().into_owned()),
            ("HOME", home.to_string_lossy().into_owned()),
            ("XDG_CONFIG_HOME", config.to_string_lossy().into_owned()),
            ("XDG_DATA_HOME", data.to_string_lossy().into_owned()),
            ("XDG_STATE_HOME", state.to_string_lossy().into_owned()),
            ("XDG_CACHE_HOME", cache.to_string_lossy().into_owned()),
            ("ZDOTDIR", hostile.to_string_lossy().into_owned()),
            ("BASH_ENV", bash_environment.to_string_lossy().into_owned()),
            ("ENV", bash_environment.to_string_lossy().into_owned()),
            ("INPUTRC", bash_environment.to_string_lossy().into_owned()),
            ("HISTFILE", marker.to_string_lossy().into_owned()),
            ("MISE_CONFIG_DIR", hostile.to_string_lossy().into_owned()),
            (
                "MISE_TRUSTED_CONFIG_PATHS",
                hostile.to_string_lossy().into_owned(),
            ),
        ]
        .into_iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)));
        let clean = clean_acceptance_environment(variables).unwrap();

        let zsh = Command::new("/bin/zsh")
            .arg("-c")
            .arg("printf ZSH_CLEAN; : > \"$XDG_STATE_HOME/zsh-write\"")
            .env_clear()
            .envs(&clean)
            .output()
            .unwrap();
        let bash = Command::new("/bin/bash")
            .arg("-c")
            .arg("printf BASH_CLEAN; : > \"$XDG_STATE_HOME/bash-write\"")
            .env_clear()
            .envs(&clean)
            .output()
            .unwrap();
        assert_eq!(zsh.stdout, b"ZSH_CLEAN");
        assert!(zsh.stderr.is_empty());
        assert_eq!(bash.stdout, b"BASH_CLEAN");
        assert!(bash.stderr.is_empty());
        assert!(!marker.exists());
        assert!(state.join("zsh-write").is_file());
        assert!(state.join("bash-write").is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn normal_app_quit_should_close_the_runtime_stream_before_process_teardown() {
        let (slot, mut peer) = runtime_writer_fixture(running_observation());
        let initial = read_text_frame(&mut peer);
        assert!(initial.contains("\trunning\t0\n"));

        let finalizer_slot = Arc::clone(&slot);
        let finalizer = thread::spawn(move || finish_runtime_writer_slot(&finalizer_slot));
        let frames = accept_runtime_closure(&mut peer);
        let final_tick = frames
            .iter()
            .find(|frame| frame.starts_with(&format!("schema\t{RUNTIME_TICK_SCHEMA}\n")))
            .expect("normal quit must emit a final tick");
        assert!(final_tick.contains("\texited\t0\n"));
        assert!(final_tick.contains("\tsession-exited\t"));
        let complete = frames.last().unwrap();
        assert_eq!(complete.lines().count(), 6);
        assert!(complete.contains("observer.status\tcomplete\n"));
        assert!(finalizer.join().unwrap().is_ok());

        let mut trailing = [0_u8; 1];
        assert_eq!(peer.read(&mut trailing).unwrap(), 0);
    }

    #[test]
    fn duplicate_runtime_finalization_should_be_inert() {
        let (slot, mut peer) = runtime_writer_fixture(running_observation());
        let _ = read_text_frame(&mut peer);
        let finalizer_slot = Arc::clone(&slot);
        let finalizer = thread::spawn(move || finish_runtime_writer_slot(&finalizer_slot));
        let _ = accept_runtime_closure(&mut peer);
        assert!(finalizer.join().unwrap().is_ok());
        assert!(finish_runtime_writer_slot(&slot).is_ok());
    }

    #[test]
    fn forced_terminal_exit_should_not_be_reclassified_by_app_quit() {
        let observation = running_observation();
        let (slot, mut peer) = runtime_writer_fixture(observation.clone());
        let _ = read_text_frame(&mut peer);
        observation.session_exited(5);

        let finalizer_slot = Arc::clone(&slot);
        let finalizer = thread::spawn(move || finish_runtime_writer_slot(&finalizer_slot));
        let frames = accept_runtime_closure(&mut peer);
        let final_tick = frames
            .iter()
            .find(|frame| frame.starts_with(&format!("schema\t{RUNTIME_TICK_SCHEMA}\n")))
            .expect("forced exit must emit a final tick");
        assert!(final_tick.contains("\texited\t0\n"));
        assert!(final_tick.contains("\tsession-exited\t"));
        assert!(final_tick.lines().any(|line| line.ends_with("\t5\t0")));
        assert!(finalizer.join().unwrap().is_ok());
    }

    #[test]
    fn disconnected_verifier_should_fail_finalization_without_a_duplicate_attempt() {
        let (slot, mut peer) = runtime_writer_fixture(running_observation());
        let _ = read_text_frame(&mut peer);
        drop(peer);

        assert!(finish_runtime_writer_slot(&slot).is_err());
        assert!(finish_runtime_writer_slot(&slot).is_ok());
    }

    #[test]
    fn challenge_should_be_exact_and_bounded() {
        let packaged_executable = PackagedExecutableIdentity {
            device: 17,
            inode: 19,
        };
        let challenge = format!(
            "schema\t{CHALLENGE_SCHEMA}\nlaunch.nonce\t{}\nrun.id\ti43-proof\npackage.app.sha256\t{}\npackage.app.executable.device\t17\npackage.app.executable.inode\t19\nruntime.schema\t{RUNTIME_SCHEMA}\nruntime.sample_interval_ms\t1000\nruntime.transition_capacity\t64\nfailure.action.schema\t{FAILURE_ACTION_SCHEMA}\nfailure.action.enabled\ttrue\n",
            "a".repeat(64),
            "b".repeat(64),
        );
        let (_, run_id, _, enabled) =
            parse_challenge(challenge.as_bytes(), packaged_executable).unwrap();
        assert_eq!(run_id, "i43-proof");
        assert!(enabled);
        let disabled = challenge.replace(
            "failure.action.enabled\ttrue",
            "failure.action.enabled\tfalse",
        );
        assert!(
            !parse_challenge(disabled.as_bytes(), packaged_executable)
                .unwrap()
                .3
        );
        let disabled_channels = failure_channels(false);
        assert!(disabled_channels.0.is_none());
        assert!(disabled_channels.1.is_none());
        assert!(disabled_channels.2.is_none());
        assert!(disabled_channels.3.is_none());

        assert!(
            parse_challenge(
                format!("{challenge}extra\ttrue\n").as_bytes(),
                packaged_executable
            )
            .is_err()
        );
        assert!(parse_challenge(challenge.trim_end().as_bytes(), packaged_executable).is_err());
        assert!(
            parse_challenge(
                challenge.replace("device\t17", "device\t18").as_bytes(),
                packaged_executable
            )
            .is_err()
        );
    }

    #[test]
    fn observation_should_bind_runtime_facts_without_terminal_content() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let record = format_observation(
            &request(stream),
            "JetBrainsMono Nerd Font",
            ObservationGeometry {
                rows: 24,
                columns: 80,
                logical_width: 800.5,
                logical_height: 480.0,
                backing_pixel_width: 1601,
                backing_pixel_height: 960,
            },
            Path::new("/Volumes/SpaceTerm/SpaceTerm.app/Contents/MacOS/SpaceTerm"),
            7,
            11,
        );

        assert!(record.contains("observation.source\tproduction-app\n"));
        assert!(record.contains("initial_grid.logical_width\t800.5\n"));
        assert!(record.contains("process.executable.inode\t11\n"));
        assert!(record.contains("failure.action.schema\tspaceterm.acceptance.failure-action/v1\n"));
        assert!(record.contains("observation.complete\ttrue\n"));
        assert!(!record.contains("terminal.content"));
    }

    #[test]
    fn runtime_tick_should_have_the_exact_content_free_schema() {
        let sample = RuntimeSample {
            continuous_ns: 1,
            worker_generation: 2,
            screens_published: 3,
            screens_enqueued: 4,
            screens_superseded: 5,
            event_queue_length: 1,
            event_queue_high_water: 2,
            ui_dispatches: 6,
            ui_screen_events: 7,
            ui_drain_high_water: 2,
            ui_latest_generation: 8,
            render_latest_generation: 9,
            next_frame_generation: 10,
            next_frame_count: 11,
            presentable: true,
            minimized: false,
            occluded: false,
            workspace_visible: true,
            pane_visible: true,
            live_resize: false,
            viewport_total_rows: 12,
            viewport_visible_rows: 13,
            viewport_offset_rows: 14,
            selection_present: true,
            resize_requests: 15,
            resize_notifications: 16,
            resize_applied: 17,
            resize_coalesced: 18,
            pty_rows: 19,
            pty_columns: 20,
            pty_pixel_width: 21,
            pty_pixel_height: 22,
            terminal_inputs_accepted: 23,
            lifecycle: RuntimeLifecycle::Running,
            observer_drops: 0,
        };
        let transition = RuntimeTransition {
            sequence: 0,
            continuous_ns: 24,
            kind: crate::terminal::RuntimeEventKind::VisibilityRestored,
            generation: 25,
            aux0: 0,
            aux1: 0,
        };

        assert_eq!(
            format_runtime_tick(0, sample, &[transition]),
            "schema\tspaceterm.acceptance.runtime-tick/v1\nsequence\t0\nevent_count\t1\nsample\t1\t2\t3\t4\t5\t1\t2\t6\t7\t2\t8\t9\t10\t11\t1\t0\t0\t1\t1\t0\t12\t13\t14\t1\t15\t16\t17\t18\t19\t20\t21\t22\t23\trunning\t0\nevent\t0\t24\tvisibility-restored\t25\t0\t0\n"
        );
    }

    #[test]
    fn runtime_tick_types_cannot_carry_terminal_strings() {
        assert!(std::mem::size_of::<RuntimeSample>() < 512);
        assert!(std::mem::size_of::<RuntimeTransition>() < 128);
        let observation = RuntimeObservation::new();
        let sample = observation.sample();
        let canaries = [
            "terminal canary",
            "title canary",
            "/private/path/canary",
            "clipboard canary",
            "key canary",
            "https://canary.invalid",
        ];
        let tick = format_runtime_tick(0, sample, &[]);
        for canary in canaries {
            assert!(!tick.contains(canary));
        }
    }

    #[test]
    fn value_encoding_should_reject_noncanonical_escapes() {
        assert_eq!(decode_value("100%25%09ok").unwrap(), "100%\tok");
        assert!(decode_value("%2f").is_err());
        assert!(decode_value("%").is_err());
    }

    #[test]
    fn failure_action_should_require_exact_authentication_order_and_one_shot_sequence() {
        let nonce = "a".repeat(64);
        let app_sha256 = "b".repeat(64);
        let request_id = "c".repeat(64);
        let frame = format!(
            "schema\t{FAILURE_ACTION_SCHEMA}\nlaunch.nonce\t{nonce}\nrun.id\ti43-proof\npackage.app.sha256\t{app_sha256}\nrequest.id\t{request_id}\nsequence\t0\ncase.id\tpresentation-glyph\nrequest.once\ttrue\n"
        );
        let request =
            parse_failure_action(frame.as_bytes(), &nonce, "i43-proof", &app_sha256, 0).unwrap();
        assert_eq!(request.case, FailureActionCase::PresentationGlyph);
        assert!(
            parse_failure_action(frame.as_bytes(), &nonce, "i43-proof", &app_sha256, 1).is_err()
        );
        assert!(
            parse_failure_action(
                frame
                    .replace("request.once\ttrue", "request.once\tfalse")
                    .as_bytes(),
                &nonce,
                "i43-proof",
                &app_sha256,
                0,
            )
            .is_err()
        );
        assert!(
            parse_failure_action(
                frame
                    .replace("presentation-glyph", "arbitrary-failure")
                    .as_bytes(),
                &nonce,
                "i43-proof",
                &app_sha256,
                0,
            )
            .is_err()
        );
    }

    #[test]
    fn failure_result_schema_should_be_content_free_and_closed() {
        let event = FailureActionEvent {
            request: FailureActionRequest {
                id: "c".repeat(64),
                sequence: 0,
                case: FailureActionCase::PasteboardWrite,
            },
            phase: FailureActionPhase::Injected,
            result: FailureActionResult::FailedState,
            pane_identity: 7,
            pane_state: FailurePaneState::Failed,
            failure_class: Some(FailureClass::Platform),
            recoverability: Some(Recoverability::Recoverable),
            failure_operation: Some("write-selection-pasteboard"),
            state_revision: 2,
            latest_generation: 9,
            last_valid_generation: 8,
            visible_generation: Some(8),
            pending_recovery: FailurePendingRecovery::CopySelection,
            terminal_input_usable: true,
            session_attached: true,
            resource_staged_count: 0,
            resource_staged_bytes: 0,
            resource_rolled_back_count: 0,
            resource_rolled_back_bytes: 0,
        };
        let result = format_failure_action_result(&event);
        assert!(result.starts_with("schema\tspaceterm.acceptance.failure-action-result/v2\n"));
        assert!(result.contains("failure.class\tplatform\n"));
        for canary in [
            "terminal canary",
            "clipboard canary",
            "/private/path/canary",
            "environment canary",
        ] {
            assert!(!result.contains(canary));
        }
    }
}
