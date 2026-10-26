//! Explicit, bounded native latency captures. Set `SPACETERM_NATIVE_LATENCY_CAPTURE` to an
//! existing absolute directory before launching the installed process. Each process exclusively
//! creates a private `latency-PID.jsonl` there. Nothing is captured or written without this opt-in.
//!
//! Times are nanoseconds since this process's capture began. Clocks from different processes
//! cannot be subtracted. Scene submission is CPU work, not GPU completion or display visibility.
//! Require the final summary's `complete` field before causal joins. These events do not
//! identify which screen contains an input's output; use a controlled fixture and pixel witness.
use serde::Serialize;
use std::{
    cell::Cell,
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};

const CAPACITY: usize = 4096;
const MAX_RECORDS: u64 = 131_072;
const MAX_BYTES: usize = 32 * 1024 * 1024;
const LIFETIME: Duration = Duration::from_secs(300);
static CAPTURE: OnceLock<Option<Capture>> = OnceLock::new();
static NEXT_THREAD: AtomicU64 = AtomicU64::new(1);
static NEXT_SCOPE: AtomicU64 = AtomicU64::new(1);
thread_local! { static THREAD: Cell<u64> = const { Cell::new(0) }; }

/// Request categories carry no operation payload or authority.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Key,
    TextCommit,
    Keys,
    Resize,
    Poll,
    Effects,
    Graphics,
    Accessibility,
    Other,
}

/// Only fixed classifications, process-local view IDs, generations and counts can be recorded.
#[derive(Clone, Copy, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    NativeKey {
        view: u64,
    },
    NativeTextCommit {
        view: u64,
    },
    InputAdmission {
        accepted: bool,
        operation: Operation,
    },
    InputDequeued {
        operation: Operation,
    },
    RpcQueued {
        connection: u64,
        operation: Operation,
    },
    RpcAdmitted {
        connection: u64,
        sequence: u64,
        operation: Operation,
    },
    RequestWritten {
        connection: u64,
        sequence: u64,
        operation: Operation,
    },
    ReplyRead {
        connection: u64,
        sequence: u64,
        operation: Operation,
    },
    ServerRequestRead {
        connection: u64,
        sequence: u64,
        operation: Operation,
    },
    ServerAdmitted {
        connection: u64,
        sequence: u64,
        operation: Operation,
    },
    ServerCompleted {
        connection: u64,
        sequence: u64,
        operation: Operation,
    },
    ServerReplyWritten {
        connection: u64,
        sequence: u64,
        operation: Operation,
    },
    BridgeRead {
        bytes: u64,
    },
    WebsocketFlushed,
    WebsocketRead {
        bytes: u64,
    },
    BridgeWritten {
        bytes: u64,
    },
    BridgeWoke {
        idle_sleeps: u64,
    },
    KeyQueueStarted,
    KeyProcessed,
    PtyInputWritten {
        bytes: u64,
    },
    PtyOutputReceived,
    EmulatorOutputApplied,
    ScreenPublished {
        generation: u64,
    },
    ScreenEncoded {
        generation: u64,
    },
    ScreenDecoded {
        generation: u64,
        wire_generation: u64,
    },
    ScreenAccepted {
        view: u64,
        generation: u64,
    },
    SceneSubmitted {
        view: u64,
        generation: u64,
    },
}

impl Operation {
    /// Classifies a request without retaining any of its fields.
    pub fn of(request: &crate::pane::Request) -> Self {
        use crate::pane::Request;
        match request {
            Request::Input {
                input: crate::pane::InteractiveInput::Key(_),
                ..
            } => Self::Key,
            Request::Keys { .. } => Self::Keys,
            Request::Resize { .. } => Self::Resize,
            Request::Poll { .. } | Request::PollChanges { .. } => Self::Poll,
            Request::NativeEffects { .. } => Self::Effects,
            Request::GraphicsChunk { .. } => Self::Graphics,
            Request::Accessibility { .. } => Self::Accessibility,
            _ => Self::Other,
        }
    }
}

#[derive(Serialize)]
struct Record {
    ns: u64,
    thread: u64,
    #[serde(flatten)]
    event: Event,
}
struct Capture {
    start: Instant,
    sender: SyncSender<Record>,
    state: Arc<State>,
}
#[derive(Default)]
struct State {
    stopped: AtomicBool,
    in_flight: AtomicU64,
    attempted: AtomicU64,
    dropped: AtomicU64,
}

/// Initializes the opt-in before a measured interval. Invalid destinations disable capture.
/// Capture failures never change terminal behavior or expose native errors.
pub fn initialize() {
    CAPTURE.get_or_init(|| {
        let directory = std::env::var_os("SPACETERM_NATIVE_LATENCY_CAPTURE")?;
        let file = create_file(Path::new(&directory)).ok()?;
        let (sender, receiver) = mpsc::sync_channel(CAPACITY);
        let state = Arc::new(State::default());
        let worker_state = Arc::clone(&state);
        let start = Instant::now();
        std::thread::Builder::new()
            .name("spaceterm-measurement".into())
            .spawn(move || {
                let _ = write_capture(file, receiver, &worker_state, start);
                worker_state.stopped.store(true, Ordering::Release);
            })
            .ok()?;
        Some(Capture {
            start,
            sender,
            state,
        })
    });
}

/// Allocates a process-local label only while an explicit capture is running.
pub fn scope() -> u64 {
    if CAPTURE
        .get()
        .and_then(Option::as_ref)
        .is_some_and(|capture| !capture.state.stopped.load(Ordering::Acquire))
    {
        NEXT_SCOPE.fetch_add(1, Ordering::Relaxed)
    } else {
        0
    }
}

#[derive(Serialize)]
struct ClockCalibration {
    capture_ns_before: u64,
    capture_ns_after: u64,
    unix_ns: u64,
    host_monotonic_ns: u64,
}

fn calibration(start: Instant) -> ClockCalibration {
    let before = start.elapsed();
    let unix_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // SAFETY: clock_gettime receives a valid writable timespec. Values are numeric clocks only.
    let mut host = std::mem::MaybeUninit::<libc::timespec>::uninit();
    let host_monotonic_ns =
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, host.as_mut_ptr()) } == 0 {
            // SAFETY: the successful call initialized the timespec.
            let host = unsafe { host.assume_init() };
            (host.tv_sec as u128 * 1_000_000_000 + host.tv_nsec as u128).min(u64::MAX as u128)
                as u64
        } else {
            0
        };
    ClockCalibration {
        capture_ns_before: before.as_nanos().min(u64::MAX as u128) as u64,
        capture_ns_after: start.elapsed().as_nanos().min(u64::MAX as u128) as u64,
        unix_ns: unix_ns.min(u64::MAX as u128) as u64,
        host_monotonic_ns,
    }
}

fn create_file(directory: &Path) -> std::io::Result<File> {
    if !directory.is_absolute() || !directory.is_dir() {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.join(format!("latency-{}.jsonl", std::process::id())))
}

/// Records without waiting for disk or queue capacity. Disabled calls read no clock.
pub fn record(event: Event) {
    let Some(Some(capture)) = CAPTURE.get() else {
        return;
    };
    capture.record(event);
}

impl Capture {
    fn record(&self, event: Event) {
        self.state.in_flight.fetch_add(1, Ordering::AcqRel);
        let _recording = Recording(&self.state);
        if self.state.stopped.load(Ordering::Acquire) {
            return;
        }
        let elapsed = self.start.elapsed();
        if elapsed >= LIFETIME
            || self.state.attempted.fetch_add(1, Ordering::Relaxed) >= MAX_RECORDS
        {
            self.state.stopped.store(true, Ordering::Release);
            return;
        }
        let thread = THREAD.with(|id| {
            if id.get() == 0 {
                id.set(NEXT_THREAD.fetch_add(1, Ordering::Relaxed));
            }
            id.get()
        });
        let record = Record {
            ns: elapsed.as_nanos().min(u64::MAX as u128) as u64,
            thread,
            event,
        };
        if self.sender.try_send(record).is_err() {
            self.state.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

struct Recording<'a>(&'a State);
impl Drop for Recording<'_> {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

fn stop(state: &State) {
    state.stopped.store(true, Ordering::Release);
    // A producer already inside record() may still enqueue. Close admission before draining
    // or sampling loss counters. An unusually stalled producer makes the summary incomplete.
    let until = Instant::now() + Duration::from_millis(100);
    while state.in_flight.load(Ordering::Acquire) != 0 && Instant::now() < until {
        std::thread::yield_now();
    }
}

fn write_capture(
    file: File,
    receiver: Receiver<Record>,
    state: &State,
    start: Instant,
) -> std::io::Result<()> {
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(
        &mut writer,
        &serde_json::json!({
            "schema": 1, "clock": "process_monotonic_ns", "host_clock": "CLOCK_MONOTONIC",
            "endpoint": "gpui_scene_submission_not_display", "calibration": calibration(start),
            "max_records": MAX_RECORDS, "lifetime_ms": LIFETIME.as_millis(),
        }),
    )?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    let mut bytes = 0;
    let mut records = 0;
    let mut limited = false;
    loop {
        let stopped = state.stopped.load(Ordering::Acquire) || start.elapsed() >= LIFETIME;
        if stopped {
            stop(state);
        }
        let received = if stopped {
            receiver
                .try_recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        } else {
            receiver.recv_timeout(Duration::from_millis(100))
        };
        match received {
            Ok(record) => {
                let line = serde_json::to_vec(&record)?;
                if records >= MAX_RECORDS || line.len() + 1 > MAX_BYTES - bytes {
                    limited = true;
                    state.stopped.store(true, Ordering::Release);
                    break;
                }
                writer.write_all(&line)?;
                writer.write_all(b"\n")?;
                bytes += line.len() + 1;
                records += 1;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => writer.flush()?,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    stop(state);
    let dropped = state.dropped.load(Ordering::Acquire);
    let in_flight = state.in_flight.load(Ordering::Acquire);
    let attempted = state.attempted.load(Ordering::Acquire);
    let limit_reached = limited || attempted >= MAX_RECORDS;
    serde_json::to_writer(
        &mut writer,
        &serde_json::json!({
            "summary": true, "records": records,
            "attempted": attempted, "dropped": dropped, "in_flight": in_flight,
            "limit_reached": limit_reached,
            "complete": !limit_reached && dropped == 0 && in_flight == 0 && attempted == records,
            "calibration_end": calibration(start),
        }),
    )?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_full_observer_drops_metadata_instead_of_waiting_for_the_terminal() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let capture = Capture {
            start: Instant::now(),
            sender,
            state: Arc::default(),
        };
        capture.record(Event::KeyProcessed);
        capture.record(Event::KeyProcessed);
        assert_eq!(capture.state.dropped.load(Ordering::Relaxed), 1);
        assert!(matches!(
            receiver.try_recv().unwrap().event,
            Event::KeyProcessed
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn capture_expires_without_admitting_more_records() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let capture = Capture {
            start: Instant::now() - LIFETIME,
            sender,
            state: Arc::default(),
        };
        capture.record(Event::KeyProcessed);
        assert!(capture.state.stopped.load(Ordering::Acquire));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn the_record_limit_stops_a_capture_before_another_enqueue() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let capture = Capture {
            start: Instant::now(),
            sender,
            state: Arc::default(),
        };
        capture
            .state
            .attempted
            .store(MAX_RECORDS, Ordering::Relaxed);
        capture.record(Event::KeyProcessed);
        assert!(capture.state.stopped.load(Ordering::Acquire));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn capture_refuses_replacement_and_creates_only_owner_readable_metadata() {
        let directory =
            std::env::temp_dir().join(format!("spaceterm-measurement-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let file = create_file(&directory).unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        assert!(create_file(&directory).is_err());
        let (sender, receiver) = mpsc::sync_channel(1);
        sender
            .send(Record {
                ns: 123,
                thread: 1,
                event: Event::SceneSubmitted {
                    view: 2,
                    generation: 3,
                },
            })
            .unwrap();
        drop(sender);
        let state = State::default();
        state.attempted.store(1, Ordering::Relaxed);
        write_capture(file, receiver, &state, Instant::now()).unwrap();
        let contents = std::fs::read_to_string(
            directory.join(format!("latency-{}.jsonl", std::process::id())),
        )
        .unwrap();
        let lines: Vec<serde_json::Value> = contents
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            lines[1],
            serde_json::json!({ "ns":123, "thread":1, "event":"scene_submitted", "view":2, "generation":3 })
        );
        assert_eq!(lines[2]["records"], 1);
        assert_eq!(lines[2]["complete"], true);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
