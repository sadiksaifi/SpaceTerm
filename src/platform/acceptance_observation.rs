use std::env;
use std::fmt::Write as _;
use std::fs::Metadata;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::terminal::{RuntimeLifecycle, RuntimeObservation, RuntimeSample, RuntimeTransition};

const SOCKET_ENV: &str = "SPACETERM_ACCEPTANCE_SOCKET";
const CHALLENGE_SCHEMA: &str = "spaceterm.acceptance.native-launch-challenge/v2";
const OBSERVATION_SCHEMA: &str = "spaceterm.acceptance.native-launch-proof/v3";
const RUNTIME_SCHEMA: &str = "spaceterm.acceptance.runtime-stream/v1";
const RUNTIME_TICK_SCHEMA: &str = "spaceterm.acceptance.runtime-tick/v1";
const RUNTIME_COMPLETE_SCHEMA: &str = "spaceterm.acceptance.runtime-complete/v1";
const RUNTIME_ACK_SCHEMA: &str = "spaceterm.acceptance.runtime-ack/v1";
const RUNTIME_CLOSED_SCHEMA: &str = "spaceterm.acceptance.runtime-closed/v1";
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const SAMPLE_LATE_TOLERANCE: Duration = Duration::from_millis(250);
const TRANSITION_CAPACITY: usize = 64;
const MAX_FRAME_BYTES: usize = 16 * 1024;
const SOCKET_TIMEOUT: Duration = Duration::from_secs(30);
const FINAL_ACK_TIMEOUT: Duration = Duration::from_secs(5);
const TERMINAL_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(5);

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
}

#[derive(Debug)]
struct ObservationRequest {
    stream: UnixStream,
    nonce: String,
    run_id: String,
    app_sha256: String,
    initial: Option<ObservationSelection>,
    runtime: RuntimeObservation,
    runtime_session_pending: bool,
}

#[derive(Debug)]
pub(crate) struct PreparedObservation {
    request: ObservationRequest,
    initial: ObservationSelection,
}

#[derive(Debug)]
struct RuntimeWriter {
    shutdown: mpsc::Sender<()>,
    thread: JoinHandle<Result<(), RuntimeWriterError>>,
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

pub(crate) fn configure_from_environment() -> Result<(), AcceptanceObservationError> {
    let socket_path = take_socket_environment()?;
    let Some(socket_path) = socket_path else {
        return Ok(());
    };
    let mut stream = connect_private_socket(&socket_path)?;
    let challenge = read_frame(&mut stream)?;
    let (nonce, run_id, app_sha256) = parse_challenge(&challenge)?;
    REQUEST
        .set(Mutex::new(Some(ObservationRequest {
            stream,
            nonce,
            run_id,
            app_sha256,
            initial: None,
            runtime: RuntimeObservation::new(),
            runtime_session_pending: false,
        })))
        .map_err(|_| AcceptanceObservationError::ConfiguredTwice)
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
) -> Option<RuntimeObservation> {
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
    Some(request.runtime.clone())
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
        start_runtime_writer(self.request.stream, self.request.runtime.clone())?;
        Ok(())
    }
}

pub(crate) fn finish_runtime_observation() -> Result<(), AcceptanceObservationError> {
    let Some(writer) = RUNTIME_WRITER.get() else {
        return Ok(());
    };
    let mut writer = writer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(writer) = writer.take() else {
        return Ok(());
    };
    let _ = writer.shutdown.send(());
    match writer.thread.join() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) | Err(_) => Err(AcceptanceObservationError::Io(io::Error::other(
            "runtime observation did not complete",
        ))),
    }
}

fn start_runtime_writer(
    mut stream: UnixStream,
    observation: RuntimeObservation,
) -> Result<(), AcceptanceObservationError> {
    stream.set_read_timeout(Some(FINAL_ACK_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))?;
    let (shutdown, receiver) = mpsc::channel();
    let thread = thread::Builder::new()
        .name("spaceterm-acceptance-observer".to_owned())
        .spawn(move || run_runtime_writer(&mut stream, &observation, &receiver))?;
    RUNTIME_WRITER
        .set(Mutex::new(Some(RuntimeWriter { shutdown, thread })))
        .map_err(|_| AcceptanceObservationError::ConfiguredTwice)
}

fn run_runtime_writer(
    stream: &mut UnixStream,
    observation: &RuntimeObservation,
    shutdown: &mpsc::Receiver<()>,
) -> Result<(), RuntimeWriterError> {
    let mut sequence = 0_u64;
    let mut deadline = Instant::now();
    let mut started_ns = None;
    let mut event_count = 0_u64;

    let last_periodic_ns = loop {
        let now = Instant::now();
        if now > deadline + SAMPLE_LATE_TOLERANCE {
            observation.fail();
            deadline = now;
        }
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

        match shutdown.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break sample.continuous_ns,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
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

fn parse_challenge(
    challenge: &[u8],
) -> Result<(String, String, String), AcceptanceObservationError> {
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
    {
        return Err(AcceptanceObservationError::InvalidChallenge);
    }
    Ok((nonce.1.clone(), run_id.1.clone(), app_sha256.1.clone()))
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

    fn request(stream: UnixStream) -> ObservationRequest {
        ObservationRequest {
            stream,
            nonce: "a".repeat(64),
            run_id: "i43-proof".to_owned(),
            app_sha256: "b".repeat(64),
            initial: None,
            runtime: RuntimeObservation::new(),
            runtime_session_pending: false,
        }
    }

    #[test]
    fn challenge_should_be_exact_and_bounded() {
        let challenge = format!(
            "schema\t{CHALLENGE_SCHEMA}\nlaunch.nonce\t{}\nrun.id\ti43-proof\npackage.app.sha256\t{}\nruntime.schema\t{RUNTIME_SCHEMA}\nruntime.sample_interval_ms\t1000\nruntime.transition_capacity\t64\n",
            "a".repeat(64),
            "b".repeat(64),
        );
        let (_, run_id, _) = parse_challenge(challenge.as_bytes()).unwrap();
        assert_eq!(run_id, "i43-proof");

        assert!(parse_challenge(format!("{challenge}extra\ttrue\n").as_bytes()).is_err());
        assert!(parse_challenge(challenge.trim_end().as_bytes()).is_err());
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
}
