use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, ErrorKind, Read};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use super::*;
use crate::platform::native_pty::{NativePtyAdapter, NativePtyAdapterParts, NativePtyTermination};
use spaceterm_server::pane::{LineMode, LineState};

const PASSWORD: LineMode = LineMode {
    canonical: true,
    echo: false,
    signals: true,
    shell_foreground: false,
};

fn native_terminal_session_factory() -> NativeTerminalSessionFactory {
    NativeTerminalSessionFactory::new(
        Arc::new(RecordingSessionAdapterFactory {
            constructions: mpsc::channel().0,
        }),
        test_launch_planner(),
        LocalFilesystemAuthority::testing(),
        crate::terminal::metadata::LocalMachine::new(None, Some("fixture.test"), None),
    )
}

/// Returns after the worker has run every command sent before this call.
fn worker_round_trip(runtime: &TerminalRuntime) {
    let _ = runtime.set_server_accessibility_selection(None).recv();
}

/// Fixes the worker's publication clock inside a frame of a publication, so later output
/// coalesces instead of publishing at once as output after idle does.
fn continue_output_burst(worker: &mut TerminalWorker) {
    let now = Instant::now();
    worker.schedules.fix_clock(now);
    worker.schedules.request_pty_presentation(now);
    worker.schedules.mark_presented(now);
}

#[test]
fn native_factory_routes_local_launches_through_injected_factory() {
    let (factory, constructions) = recording_native_terminal_session_factory();
    let working_directory = std::env::temp_dir().join(".");
    let plan = LocalTerminalLaunchPlan::new(crate::domain::ValidatedLocalDirectory::new(
        working_directory.clone(),
        LocalFilesystemAuthority::testing()
            .validate_directory(&working_directory)
            .unwrap()
            .identity()
            .clone(),
    ));

    let runtime = factory
        .start_runtime(test_geometry(), plan, test_terminal_appearance_update())
        .unwrap();
    let construction = constructions
        .recv_timeout(Duration::from_secs(1))
        .expect("Local construction should reach the injected factory");
    drop(runtime);

    assert_eq!(construction.working_directory, working_directory);
    assert_eq!(construction.size, pty_size(test_geometry()));
    assert_eq!(construction.executable, OsString::from("/fixture/zsh"));
    assert_eq!(construction.arguments.last(), Some(&OsString::from("-l")));
    assert!(construction.inherit_environment);
    assert!(
        construction
            .environment_removals
            .contains(&OsString::from("TMUX"))
    );
    assert!(
        construction
            .environment
            .contains(&("TERM_PROGRAM".into(), "ghostty".into()))
    );
    assert!(
        construction
            .environment
            .contains(&("SPACETERM".into(), "1".into()))
    );
    let prompt_owner = construction
        .environment
        .iter()
        .find(|(name, _)| name == "SPACETERM_PROMPT_OWNER")
        .map(|(_, value)| value.to_string_lossy().into_owned())
        .expect("a local shell should receive its Prompt Owner");
    assert_eq!(prompt_owner.len(), 32);
    assert!(prompt_owner.bytes().all(|byte| byte.is_ascii_hexdigit()));
}

use crate::terminal::geometry::{BackingScale, CellGridSize, LogicalCellSize};
use crate::terminal::key::{KeyAction, PhysicalKey};

fn geometry(cols: u16, rows: u16, cell_width: f32, cell_height: f32) -> TerminalGeometry {
    TerminalGeometry::from_grid(
        CellGridSize::new(cols, rows),
        LogicalCellSize::new(cell_width, cell_height),
        BackingScale::ONE,
    )
}

pub(super) fn test_geometry() -> TerminalGeometry {
    geometry(80, 24, 8.0, 20.0)
}

fn text_key(action: KeyAction) -> KeyInput {
    KeyInput {
        action,
        physical_key: PhysicalKey::A,
        native_key_code: Some(0),
        logical_key: "a".to_owned(),
        text: Some("a".to_owned()),
        unshifted_codepoint: Some('a'),
        modifiers: InputModifiers::default(),
        consumed_modifiers: InputModifiers::default(),
        option_as_alt: OptionAsAltPolicy::default(),
    }
}

fn modifier_key(action: KeyAction) -> KeyInput {
    KeyInput {
        action,
        physical_key: PhysicalKey::ShiftLeft,
        native_key_code: Some(56),
        logical_key: "shift".to_owned(),
        text: None,
        unshifted_codepoint: None,
        modifiers: InputModifiers {
            shift: action != KeyAction::Release,
            ..InputModifiers::default()
        },
        consumed_modifiers: InputModifiers::default(),
        option_as_alt: OptionAsAltPolicy::default(),
    }
}

#[test]
fn input_method_commits_never_enter_the_held_key_set() {
    let mut held = HeldKeys::default();

    held.route(&KeyInput::input_method_commit("한"));

    assert!(held.take_releases().is_empty());
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleStep {
    TerminationRequested,
    PtyDropped,
}

#[derive(Clone, Debug, Default)]
struct ScriptedPtyState {
    initial_screen: Option<Arc<ScreenSnapshot>>,
    line_mode: LineMode,
    line_reads: usize,
    take_reader_calls: usize,
    write_attempts: usize,
    write_blocked: bool,
    written: Vec<u8>,
    flushes: usize,
    resizes: Vec<NativePtySize>,
    waits: usize,
    wait_timeouts: Vec<Duration>,
    terminations: usize,
    pty_drops: usize,
    reader_drops: usize,
    terminator_drops: usize,
    lifecycle: Vec<LifecycleStep>,
}

#[derive(Clone, Default)]
struct ScriptedPtyRecords {
    state: Arc<(Mutex<ScriptedPtyState>, Condvar)>,
}

impl ScriptedPtyRecords {
    fn update(&self, update: impl FnOnce(&mut ScriptedPtyState)) {
        let (state, changed) = &*self.state;
        update(&mut state.lock().unwrap());
        changed.notify_all();
    }

    fn snapshot(&self) -> ScriptedPtyState {
        self.state.0.lock().unwrap().clone()
    }

    fn wait_for(
        &self,
        description: &str,
        predicate: impl Fn(&ScriptedPtyState) -> bool,
    ) -> ScriptedPtyState {
        let (state, changed) = &*self.state;
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut state = state.lock().unwrap();
        while !predicate(&state) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "timed out waiting for {description}");
            let (next_state, timeout) = changed.wait_timeout(state, remaining).unwrap();
            state = next_state;
            assert!(
                !timeout.timed_out() || predicate(&state),
                "timed out waiting for {description}"
            );
        }
        state.clone()
    }
}

enum ReaderStep {
    Bytes(Vec<u8>),
    Error(String),
    Eof,
}

struct ScriptedReader {
    steps: mpsc::Receiver<ReaderStep>,
    pending: VecDeque<u8>,
    records: ScriptedPtyRecords,
}

impl Read for ScriptedReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        while self.pending.is_empty() {
            match self.steps.recv() {
                Ok(ReaderStep::Bytes(bytes)) => self.pending.extend(bytes),
                Ok(ReaderStep::Error(message)) => return Err(io::Error::other(message)),
                Ok(ReaderStep::Eof) | Err(_) => return Ok(0),
            }
        }

        let read = buffer.len().min(self.pending.len());
        for destination in &mut buffer[..read] {
            let Some(byte) = self.pending.pop_front() else {
                unreachable!("the scripted reader length was checked before draining")
            };
            *destination = byte;
        }
        Ok(read)
    }
}

impl Drop for ScriptedReader {
    fn drop(&mut self) {
        self.records.update(|state| state.reader_drops += 1);
    }
}

struct ScriptedPtyOptions {
    reader_error: Option<String>,
    resize_error: Option<String>,
    write_error: Option<String>,
    wait_error: Option<String>,
    wait_times_out: bool,
    exit_code: u32,
    termination_error: Option<String>,
    termination_releases_reader: bool,
}

impl Default for ScriptedPtyOptions {
    fn default() -> Self {
        Self {
            reader_error: None,
            resize_error: None,
            write_error: None,
            wait_error: None,
            wait_times_out: false,
            exit_code: 0,
            termination_error: None,
            termination_releases_reader: true,
        }
    }
}

struct ScriptedPty {
    reader: Option<Box<dyn Read + Send>>,
    records: ScriptedPtyRecords,
    reader_error: Option<String>,
    resize_error: Option<String>,
    write_error: Option<String>,
    wait_error: Option<String>,
    wait_times_out: bool,
    exit_code: u32,
}

impl Write for ScriptedPty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.records.update(|state| state.write_attempts += 1);
        if self.records.snapshot().write_blocked {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if let Some(message) = &self.write_error {
            return Err(io::Error::new(ErrorKind::BrokenPipe, message.clone()));
        }
        self.records
            .update(|state| state.written.extend_from_slice(bytes));
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.records.update(|state| state.flushes += 1);
        Ok(())
    }
}

impl NativePtyAdapter for ScriptedPty {
    fn line_mode(&self) -> Result<LineMode, NativePtyOperationFailure> {
        self.records.update(|state| state.line_reads += 1);
        Ok(self.records.snapshot().line_mode)
    }
    fn take_reader(&mut self) -> io::Result<Box<dyn Read + Send>> {
        self.records.update(|state| state.take_reader_calls += 1);
        if let Some(message) = self.reader_error.take() {
            return Err(io::Error::other(message));
        }
        self.reader.take().ok_or_else(|| {
            io::Error::new(ErrorKind::NotFound, "scripted PTY reader was already taken")
        })
    }

    fn resize(&self, size: NativePtySize) -> Result<(), NativePtyOperationFailure> {
        self.records.update(|state| state.resizes.push(size));
        match &self.resize_error {
            Some(_) => Err(NativePtyOperationFailure::new(ErrorKind::Other)),
            None => Ok(()),
        }
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> Result<NativePtyExit, NativePtyWaitFailure> {
        self.records.update(|state| {
            state.waits += 1;
            state.wait_timeouts.push(timeout);
        });
        if self.wait_times_out {
            return Err(NativePtyWaitFailure::new(ErrorKind::TimedOut));
        }
        match &self.wait_error {
            Some(_) => Err(NativePtyWaitFailure::new(ErrorKind::Other)),
            None if self.exit_code == 0 => Ok(NativePtyExit::Success),
            None => Ok(NativePtyExit::ExitCode(self.exit_code)),
        }
    }
}

struct DiscardNativePtyOutput;

impl NativePtyOutputSink for DiscardNativePtyOutput {
    fn publish(&self, _output: NativePtyOutput) -> bool {
        true
    }
}

struct NoopNativePtyTermination;

impl NativePtyTermination for NoopNativePtyTermination {
    fn request_termination(&self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Eq, PartialEq)]
struct RecordedPreparedShellLaunch {
    working_directory: PathBuf,
    executable: OsString,
    arguments: Vec<OsString>,
    inherit_environment: bool,
    environment_removals: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
    size: NativePtySize,
}

struct RecordingSessionAdapterFactory {
    constructions: mpsc::Sender<RecordedPreparedShellLaunch>,
}

impl NativePtyAdapterFactory for RecordingSessionAdapterFactory {
    fn create(
        &self,
        launch: PreparedShellLaunch,
        size: NativePtySize,
    ) -> Result<
        NativePtyAdapterParts,
        crate::platform::native_pty::NativePtyAdapterConstructionFailure,
    > {
        let launch = RecordedPreparedShellLaunch {
            working_directory: launch.working_directory().to_owned(),
            executable: launch.executable().to_owned(),
            arguments: launch.arguments().to_vec(),
            inherit_environment: launch.inherit_environment(),
            environment_removals: launch.environment_removals().to_vec(),
            environment: launch.environment().to_vec(),
            size,
        };
        self.constructions
            .send(launch)
            .expect("construction observation should remain available");
        Ok(NativePtyAdapterParts {
            adapter: Box::new(ScriptedPty {
                reader: Some(Box::new(io::empty())),
                records: ScriptedPtyRecords::default(),
                reader_error: None,
                resize_error: None,
                write_error: None,
                wait_error: None,
                wait_times_out: false,
                exit_code: 0,
            }),
            termination: Arc::new(NoopNativePtyTermination),
        })
    }
}

fn recording_native_terminal_session_factory() -> (
    NativeTerminalSessionFactory,
    mpsc::Receiver<RecordedPreparedShellLaunch>,
) {
    let (constructions, observed) = mpsc::channel();
    (
        NativeTerminalSessionFactory::new(
            Arc::new(RecordingSessionAdapterFactory { constructions }),
            test_launch_planner(),
            LocalFilesystemAuthority::testing(),
            crate::terminal::metadata::LocalMachine::new(None, Some("fixture.test"), None),
        ),
        observed,
    )
}

fn direct_native_pty(records: ScriptedPtyRecords) -> NativePtyOwner {
    let factory = ScriptedSessionAdapterFactory::immediate(NativePtyAdapterParts {
        adapter: Box::new(ScriptedPty {
            reader: Some(Box::new(io::empty())),
            records,
            reader_error: None,
            resize_error: None,
            write_error: None,
            wait_error: None,
            wait_times_out: false,
            exit_code: 0,
        }),
        termination: Arc::new(NoopNativePtyTermination),
    });
    NativePtyOwner::start(
        &factory,
        PreparedShellLaunch::for_test(std::env::temp_dir()),
        pty_size(test_geometry()),
        Arc::new(DiscardNativePtyOutput),
        &NativePtyCloseHandle::default(),
    )
    .expect("scripted Native PTY Owner should start")
}

impl Drop for ScriptedPty {
    fn drop(&mut self) {
        self.records.update(|state| {
            state.pty_drops += 1;
            state.lifecycle.push(LifecycleStep::PtyDropped);
        });
    }
}

struct ScriptedPtyTerminator {
    records: ScriptedPtyRecords,
    reader_steps: mpsc::Sender<ReaderStep>,
    error: Option<String>,
    releases_reader: bool,
}

impl NativePtyTermination for ScriptedPtyTerminator {
    fn request_termination(&self) -> io::Result<()> {
        self.records.update(|state| {
            state.terminations += 1;
            state.lifecycle.push(LifecycleStep::TerminationRequested);
        });
        if self.releases_reader {
            let _ = self.reader_steps.send(ReaderStep::Eof);
        }
        match &self.error {
            Some(message) => Err(io::Error::other(message.clone())),
            None => Ok(()),
        }
    }
}

impl Drop for ScriptedPtyTerminator {
    fn drop(&mut self) {
        self.records.update(|state| state.terminator_drops += 1);
    }
}

struct ScriptedSessionAdapterFactory {
    parts: Mutex<Option<NativePtyAdapterParts>>,
    startup_gate: Option<ScriptedStartupGate>,
}

struct ScriptedStartupGate {
    entered: mpsc::SyncSender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl ScriptedSessionAdapterFactory {
    fn immediate(parts: NativePtyAdapterParts) -> Self {
        Self {
            parts: Mutex::new(Some(parts)),
            startup_gate: None,
        }
    }

    fn gated(
        parts: NativePtyAdapterParts,
        entered: mpsc::SyncSender<()>,
        release: mpsc::Receiver<()>,
    ) -> Self {
        Self {
            parts: Mutex::new(Some(parts)),
            startup_gate: Some(ScriptedStartupGate {
                entered,
                release: Mutex::new(release),
            }),
        }
    }
}

impl NativePtyAdapterFactory for ScriptedSessionAdapterFactory {
    fn create(
        &self,
        launch: PreparedShellLaunch,
        size: NativePtySize,
    ) -> Result<
        NativePtyAdapterParts,
        crate::platform::native_pty::NativePtyAdapterConstructionFailure,
    > {
        let working_directory = launch.working_directory();
        assert_eq!(working_directory, std::env::temp_dir());
        assert_eq!(
            size,
            NativePtySize {
                rows: 24,
                columns: 80,
                pixel_width: 640,
                pixel_height: 480,
            }
        );
        if let Some(gate) = &self.startup_gate {
            gate.entered.send(()).unwrap();
            gate.release.lock().unwrap().recv().unwrap();
        }
        self.parts.lock().unwrap().take().ok_or(
            crate::platform::native_pty::NativePtyAdapterConstructionFailure::ResourceCreationFailed,
        )
    }
}

type ScriptedStart = Result<StartedTerminalSessionParts, TerminalSessionFailure>;

fn start_scripted_session(
    options: ScriptedPtyOptions,
) -> (ScriptedStart, mpsc::Sender<ReaderStep>, ScriptedPtyRecords) {
    let records = ScriptedPtyRecords::default();
    let records_for_pty = records.clone();
    let records_for_terminator = records.clone();
    let (reader_steps, steps) = mpsc::channel();
    let terminator_steps = reader_steps.clone();
    let ScriptedPtyOptions {
        reader_error,
        resize_error,
        write_error,
        wait_error,
        wait_times_out,
        exit_code,
        termination_error,
        termination_releases_reader,
    } = options;

    let adapter_factory = ScriptedSessionAdapterFactory::immediate(NativePtyAdapterParts {
        adapter: Box::new(ScriptedPty {
            reader: Some(Box::new(ScriptedReader {
                steps,
                pending: VecDeque::new(),
                records: records_for_pty.clone(),
            })),
            records: records_for_pty,
            reader_error,
            resize_error,
            write_error,
            wait_error,
            wait_times_out,
            exit_code,
        }),
        termination: Arc::new(ScriptedPtyTerminator {
            records: records_for_terminator,
            reader_steps: terminator_steps,
            error: termination_error,
            releases_reader: termination_releases_reader,
        }),
    });
    let started = TerminalRuntime::start(
        Arc::new(adapter_factory),
        test_launch_planner(),
        test_geometry(),
        &std::env::temp_dir(),
        Some("fixture.test"),
        LocalFilesystemAuthority::testing(),
    )
    .unwrap();
    let startup = receive_event(
        &started.1,
        "the actual asynchronous startup report",
        |event| {
            matches!(
                event,
                TerminalSessionEvent::Screen(_) | TerminalSessionEvent::Failed(_)
            )
        },
    );
    let result = match startup {
        TerminalSessionEvent::Screen(screen) => {
            assert_eq!(
                screen.metadata.current_directory(),
                Some(crate::domain::CurrentDirectory::Local(std::env::temp_dir()))
            );
            assert_eq!(
                screen.appearance_generation,
                test_terminal_appearance_update().generation
            );
            records.update(|state| state.initial_screen = Some(screen));
            Ok(started)
        }
        TerminalSessionEvent::Failed(failure) => {
            drop(started);
            Err(failure)
        }
        _ => unreachable!(),
    };
    (result, reader_steps, records)
}

/// Waits until the subscription's retained line state satisfies `condition`.
fn wait_for_line(
    subscription: &TerminalSubscription,
    description: &str,
    condition: impl Fn(LineState) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while !condition(subscription.line()) {
        assert!(Instant::now() < deadline, "{description} never arrived");
        thread::sleep(Duration::from_millis(1));
    }
}

fn receive_event(
    events: &async_channel::Receiver<TerminalSessionEvent>,
    description: &str,
    predicate: impl Fn(&TerminalSessionEvent) -> bool + Send + 'static,
) -> TerminalSessionEvent {
    let events = events.clone();
    let (matched, result) = mpsc::sync_channel(1);
    let waiter = thread::spawn(move || {
        loop {
            match events.recv_blocking() {
                Ok(event) if predicate(&event) => {
                    let _ = matched.send(Some(event));
                    break;
                }
                Ok(_) => {}
                Err(_) => {
                    let _ = matched.send(None);
                    break;
                }
            }
        }
    });

    match result.recv_timeout(Duration::from_secs(1)) {
        Ok(Some(event)) => {
            waiter.join().unwrap();
            event
        }
        Ok(None) => {
            waiter.join().unwrap();
            panic!("Terminal Session events closed while waiting for {description}")
        }
        Err(error) => {
            drop(waiter);
            panic!("timed out waiting for {description}: {error}")
        }
    }
}

pub(super) fn screen_text(screen: &ScreenSnapshot) -> String {
    screen
        .rows
        .iter()
        .flat_map(|row| row.iter())
        .map(|cell| cell.text.as_str())
        .collect()
}

#[test]
fn dropping_presentation_receivers_keeps_the_owned_runtime_reading_output() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, accessibility) = result.unwrap();
    drop(events);
    drop(accessibility);

    reader_steps
        .send(ReaderStep::Bytes(b"unobserved output\r\n".to_vec()))
        .unwrap();
    reader_steps.send(ReaderStep::Eof).unwrap();
    records.wait_for("the runtime to consume output and exit", |state| {
        state.pty_drops == 1
    });
    session.shutdown_and_join();
}

#[test]
fn a_new_subscription_presents_output_received_while_detached() {
    let (result, reader_steps, _) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut runtime, events, accessibility) = result.unwrap();
    drop(events);
    drop(accessibility);
    reader_steps
        .send(ReaderStep::Bytes(b"before reattach\r\n".to_vec()))
        .unwrap();

    let subscription = runtime.subscribe();
    reader_steps
        .send(ReaderStep::Bytes(b"after reattach".to_vec()))
        .unwrap();
    let event = receive_event(
        &subscription.events,
        "output across re-subscription",
        |event| matches!(event, TerminalSessionEvent::Screen(screen) if screen_text(screen).contains("after reattach")),
    );
    let TerminalSessionEvent::Screen(screen) = event else {
        unreachable!()
    };
    assert!(screen_text(&screen).contains("before reattach"));
    runtime.shutdown_and_join();
}

#[test]
fn a_replacement_subscription_receives_accessibility_without_new_output() {
    fn receive_model(
        receiver: async_channel::Receiver<Arc<TerminalAccessibilityModel>>,
    ) -> Arc<TerminalAccessibilityModel> {
        let (reply, result) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let _ = reply.send(receiver.recv_blocking());
        });
        result
            .recv_timeout(Duration::from_secs(1))
            .expect("accessibility seed must arrive without terminal output")
            .unwrap()
    }
    let (result, _, _) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut runtime, events, accessibility) = result.unwrap();
    let first = receive_model(accessibility);
    drop(events);

    let subscription = runtime.subscribe();
    let second = receive_model(subscription.accessibility);
    assert_eq!(first.text(), second.text());
    runtime.shutdown_and_join();
}

#[test]
fn replacement_subscriptions_seed_unchanged_line_state() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut runtime, events, accessibility) = result.unwrap();
    let original = runtime.subscribe();
    records.update(|state| state.line_mode = PASSWORD);
    runtime.focus(false);
    wait_for_line(&original, "the original hidden-input transition", |line| {
        line.mode.hidden_input()
    });
    drop(events);
    drop(accessibility);
    drop(original);

    let replacement = runtime.subscribe();
    assert!(replacement.line().mode.hidden_input());
    reader_steps
        .send(ReaderStep::Bytes(b"after hidden seed\r\n".repeat(12)))
        .unwrap();
    receive_event(
        &replacement.events,
        "screen output after the hidden-input seed",
        |event| matches!(event, TerminalSessionEvent::Screen(screen) if screen_text(screen).contains("after hidden seed")),
    );
    assert!(replacement.line().mode.hidden_input());
    runtime.shutdown_and_join();
    assert!(!replacement.line().mode.hidden_input());
}

#[test]
fn subscribing_after_shell_exit_replays_final_screen_and_exit() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut runtime, events, accessibility) = result.unwrap();
    drop(events);
    drop(accessibility);
    reader_steps
        .send(ReaderStep::Bytes(b"final detached output".to_vec()))
        .unwrap();
    reader_steps.send(ReaderStep::Eof).unwrap();
    records.wait_for("shell exit while detached", |state| state.pty_drops == 1);

    let subscription = runtime.subscribe();
    let TerminalSessionEvent::Screen(screen) = subscription.events.recv_blocking().unwrap() else {
        panic!("the final screen precedes exit");
    };
    assert!(screen_text(&screen).contains("final detached output"));
    assert!(matches!(
        subscription.events.recv_blocking().unwrap(),
        TerminalSessionEvent::Exited(TerminalSessionExit::Success)
    ));
    assert!(subscription.events.recv_blocking().is_err());
    runtime.shutdown_and_join();
}

#[test]
fn shell_exit_should_preserve_normal_signal_and_shutdown_classifications() {
    assert_eq!(
        classify_native_pty_exit(NativePtyExit::Success),
        TerminalSessionExit::Success
    );
    assert_eq!(
        classify_native_pty_exit(NativePtyExit::ExitCode(17)),
        TerminalSessionExit::ExitCode(17)
    );
    assert_eq!(
        classify_native_pty_exit(NativePtyExit::Signal("Hangup".to_owned())),
        TerminalSessionExit::Signal("Hangup".to_owned())
    );
    assert_eq!(
        classify_native_pty_exit(NativePtyExit::GracefulShutdown),
        TerminalSessionExit::GracefulShutdown
    );
    assert_eq!(
        classify_native_pty_exit(NativePtyExit::ForcedShutdown),
        TerminalSessionExit::ForcedShutdown
    );
}

#[test]
fn scripted_output_and_exit_should_preserve_the_latest_screen_before_the_final_event() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, accessibility) = result.unwrap();

    for index in 0..32 {
        reader_steps
            .send(ReaderStep::Bytes(
                format!("bounded line {index}\r\n").into_bytes(),
            ))
            .unwrap();
    }
    reader_steps.send(ReaderStep::Eof).unwrap();
    records.wait_for("the scripted worker to finish", |state| {
        state.pty_drops == 1
    });

    assert_eq!(events.len(), 2);
    let first = events.try_recv().unwrap();
    let second = events.try_recv().unwrap();
    let result = match (first, second) {
        (TerminalSessionEvent::Screen(screen), TerminalSessionEvent::Exited(status)) => {
            let model = accessibility.try_recv().unwrap();
            assert!(model.text().contains("bounded line 31"));
            assert_eq!(model.generation(), screen.generation);
            (
                screen_text(&screen).contains("bounded line 31"),
                status == TerminalSessionExit::Success,
                events.try_recv().is_err(),
            )
        }
        events => panic!("expected the latest Screen followed by Exited, got {events:?}"),
    };

    assert_eq!(result, (true, true, true));
    session.shutdown();
}

#[test]
fn shell_exit_should_flush_a_pending_synchronized_output_transaction() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();

    reader_steps
        .send(ReaderStep::Bytes(b"\x1b[?2026hfinal output".to_vec()))
        .unwrap();
    reader_steps.send(ReaderStep::Eof).unwrap();
    records.wait_for("the synchronized-output worker to finish", |state| {
        state.pty_drops == 1
    });

    assert_eq!(events.len(), 2);
    let screen = events.try_recv().unwrap();
    let exited = events.try_recv().unwrap();
    assert!(matches!(
        screen,
        TerminalSessionEvent::Screen(screen) if screen_text(&screen).contains("final output")
    ));
    assert!(matches!(
        exited,
        TerminalSessionEvent::Exited(TerminalSessionExit::Success)
    ));
    session.shutdown();
}

/// A resize clears a prompt the shell redraws. The session publishes the resized screen only once
/// the shell has drawn the prompt again, so the prompt never blinks out while a Pane resizes.
#[test]
fn resizing_at_a_redrawable_prompt_should_publish_only_the_redrawn_prompt() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();
    reader_steps
        .send(ReaderStep::Bytes(
            b"QA_RESIZE_MARKER\r\n\x1b]133;A;redraw=1\x07~\r\n> \x1b]133;B\x07".to_vec(),
        ))
        .unwrap();
    receive_event(
        &events,
        "the prompt",
        |event| matches!(event, TerminalSessionEvent::Screen(screen) if screen_text(screen).contains("~")),
    );

    session.resize(geometry(81, 24, 8.0, 20.0));
    records.wait_for("the PTY resize", |state| state.resizes.len() == 1);
    // Captured from zsh after SIGWINCH: it erases from the prompt and prints PS1 again.
    reader_steps
        .send(ReaderStep::Bytes(
            b"\r\r\x1b[A\x1b[0m\x1b[J\x1b]133;A;redraw=1\x07~\r\n> \x1b]133;B\x07".to_vec(),
        ))
        .unwrap();

    let TerminalSessionEvent::Screen(resized) =
        receive_event(&events, "the resized screen", |event| {
            matches!(event, TerminalSessionEvent::Screen(_))
        })
    else {
        unreachable!()
    };
    assert_eq!(
        resized.rows[0].len(),
        81,
        "the next screen must be the resized one"
    );
    assert!(
        screen_text(&resized).contains("~"),
        "the resized screen must show the redrawn prompt, not the cleared one"
    );
    session.shutdown();
}

#[test]
fn session_snapshots_reuse_rows_unchanged_by_later_output() {
    let (result, reader_steps, _records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();

    reader_steps
        .send(ReaderStep::Bytes(b"first row".to_vec()))
        .unwrap();
    let first = receive_event(
        &events,
        "the first row snapshot",
        |event| matches!(event, TerminalSessionEvent::Screen(screen) if screen_text(screen).contains("first row")),
    );
    reader_steps
        .send(ReaderStep::Bytes(b"\r\nsecond row".to_vec()))
        .unwrap();
    let second = receive_event(
        &events,
        "the second row snapshot",
        |event| matches!(event, TerminalSessionEvent::Screen(screen) if screen_text(screen).contains("second row")),
    );

    let (TerminalSessionEvent::Screen(first), TerminalSessionEvent::Screen(second)) =
        (first, second)
    else {
        unreachable!("the event predicates accept only terminal screens")
    };
    assert!(Arc::ptr_eq(&first.rows[0], &second.rows[0]));
    assert!(!Arc::ptr_eq(&first.rows[1], &second.rows[1]));

    session.shutdown();
}

#[test]
fn session_failure_display_should_explain_each_classification() {
    let failures = [
        TerminalSessionFailure::Startup {
            stage: TerminalSessionStartupStage::Pty,
            message: "open unavailable".to_owned(),
        },
        TerminalSessionFailure::Runtime(crate::terminal::TerminalFailure::emulator(
            "session-runtime",
        )),
        TerminalSessionFailure::PtyRead {
            read_error: "read unavailable".to_owned(),
            exit_status: "exit code 7".to_owned(),
        },
        TerminalSessionFailure::ShellWait {
            read_error: Some("read unavailable".to_owned()),
            wait_error: "wait unavailable".to_owned(),
        },
        TerminalSessionFailure::ShellWait {
            read_error: None,
            wait_error: "wait unavailable".to_owned(),
        },
    ];

    let statuses = failures.each_ref().map(ToString::to_string);

    assert_eq!(
        statuses,
        [
            "Terminal Session startup failed during PTY creation: open unavailable",
            "This Terminal Session failed. Close this Pane.",
            "Shell output failed: read unavailable; shell exited (exit code 7)",
            "Shell output failed: read unavailable; waiting for the shell also failed: wait unavailable",
            "Shell output ended, but waiting for the shell failed: wait unavailable",
        ]
    );
    let _: &dyn std::error::Error = &failures[0];
}

#[test]
fn deferred_start_should_return_before_pty_spawn_and_publish_a_typed_failure() {
    let (spawn_entered, entered) = mpsc::sync_channel(1);
    let (release_spawn, release) = mpsc::sync_channel(1);
    let started_at = Instant::now();

    let (mut session, events, _accessibility) = TerminalRuntime::start_deferred_with(
        test_geometry(),
        &std::env::temp_dir(),
        move |size, _output, _close_handle| {
            assert_eq!(size, pty_size(test_geometry()));
            spawn_entered.send(()).unwrap();
            release.recv().unwrap();
            Err(NativePtyStartupFailure::Adapter(
                crate::platform::native_pty::NativePtyAdapterConstructionFailure::ResourceCreationFailed,
            ))
        },
    )
    .unwrap();

    entered.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(
        started_at.elapsed() < Duration::from_millis(250),
        "Terminal Session start waited for PTY spawn"
    );
    release_spawn.send(()).unwrap();
    let event = receive_event(&events, "the deferred PTY spawn failure", |event| {
        matches!(
            event,
            TerminalSessionEvent::Failed(TerminalSessionFailure::Startup {
                stage: TerminalSessionStartupStage::Pty,
                ..
            })
        )
    });

    let TerminalSessionEvent::Failed(TerminalSessionFailure::Startup { message, .. }) = event
    else {
        unreachable!("the event predicate accepts only typed startup failures")
    };
    assert_eq!(message, "Native PTY resources could not be created");
    session.shutdown();
}

#[test]
fn close_before_native_pty_installation_should_terminate_once_after_startup() {
    let records = ScriptedPtyRecords::default();
    let adapter_records = records.clone();
    let termination_records = records.clone();
    let (reader_steps, steps) = mpsc::channel();
    let termination_steps = reader_steps.clone();
    let (startup_entered, entered) = mpsc::sync_channel(1);
    let (release_startup, release) = mpsc::sync_channel(1);

    let adapter_factory = Arc::new(ScriptedSessionAdapterFactory::gated(
        NativePtyAdapterParts {
            adapter: Box::new(ScriptedPty {
                reader: Some(Box::new(ScriptedReader {
                    steps,
                    pending: VecDeque::new(),
                    records: adapter_records.clone(),
                })),
                records: adapter_records,
                reader_error: None,
                resize_error: None,
                write_error: None,
                wait_error: None,
                wait_times_out: false,
                exit_code: 0,
            }),
            termination: Arc::new(ScriptedPtyTerminator {
                records: termination_records,
                reader_steps: termination_steps,
                error: None,
                releases_reader: true,
            }),
        },
        startup_entered,
        release,
    ));
    let (mut session, _events, _accessibility) = TerminalRuntime::start(
        adapter_factory,
        test_launch_planner(),
        test_geometry(),
        &std::env::temp_dir(),
        Some("fixture.test"),
        LocalFilesystemAuthority::testing(),
    )
    .unwrap();

    entered.recv_timeout(Duration::from_secs(1)).unwrap();
    session.shutdown();
    session.shutdown();
    release_startup.send(()).unwrap();

    let state = records.wait_for("the deferred Native PTY Owner cleanup", |state| {
        state.pty_drops == 1 && state.reader_drops == 1
    });
    assert_eq!(
        (
            state.terminations,
            state.waits,
            state.pty_drops,
            state.reader_drops
        ),
        (1, 0, 1, 1)
    );
}

#[test]
fn native_factory_reports_local_launch_directory_failure_through_terminal_session_events() {
    let resources = crate::terminal::testing::ShellResourcesFixture::new();
    let session = native_terminal_session_factory()
        .start_runtime(
            test_geometry(),
            LocalTerminalLaunchPlan::new(crate::domain::ValidatedLocalDirectory::new(
                resources.path().join("missing-session-workspace"),
                crate::domain::LocalDirectoryIdentity::for_test(0),
            )),
            test_terminal_appearance_update(),
        )
        .unwrap();
    let events = session.subscribe().events;

    let event = receive_event(&events, "the native PTY startup failure", |event| {
        matches!(
            event,
            TerminalSessionEvent::Failed(TerminalSessionFailure::Startup {
                stage: TerminalSessionStartupStage::Directory,
                ..
            })
        )
    });

    let TerminalSessionEvent::Failed(TerminalSessionFailure::Startup { message, .. }) = event
    else {
        unreachable!("the event predicate accepts only typed PTY startup failures")
    };
    assert_eq!(
        message,
        "Shell launch directory is unavailable; select an existing directory and retry"
    );
    drop(session);
}

#[test]
fn reader_acquisition_failure_should_fail_startup_and_drop_the_pty_once() {
    let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
        reader_error: Some("native detail: /private/terminal token=secret".to_owned()),
        ..ScriptedPtyOptions::default()
    });

    let error = match result {
        Ok(_) => panic!("a missing PTY reader must fail startup"),
        Err(error) => error,
    };
    let state = records.snapshot();

    assert!(matches!(
        error,
        TerminalSessionFailure::Startup { stage: TerminalSessionStartupStage::Reader, message } if message == "Native PTY reader acquisition failed (Other)"
    ));
    assert_eq!(
        (
            state.take_reader_calls,
            state.pty_drops,
            state.terminations,
            state.terminator_drops,
        ),
        (1, 1, 0, 1)
    );
}

#[test]
fn scripted_output_should_reach_the_terminal_screen() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();

    reader_steps
        .send(ReaderStep::Bytes(b"scripted output".to_vec()))
        .unwrap();
    let event = receive_event(
        &events,
        "scripted terminal output",
        |event| matches!(event, TerminalSessionEvent::Screen(screen) if screen_text(screen).contains("scripted output")),
    );

    let TerminalSessionEvent::Screen(screen) = event else {
        unreachable!("the event predicate accepts only terminal screens")
    };
    assert!(screen_text(&screen).contains("scripted output"));

    session.shutdown();
    let state = records.wait_for(
        "the scripted Terminal Session owners to be released",
        |state| state.pty_drops == 1 && state.reader_drops == 1 && state.terminator_drops == 1,
    );
    assert_eq!(
        (
            state.terminations,
            state.pty_drops,
            state.reader_drops,
            state.terminator_drops,
        ),
        (1, 1, 1, 1)
    );
}

#[test]
fn bounded_output_should_preserve_the_control_lane_and_unblock_the_producer() {
    const CHUNK_SIZE: usize = 16 * 1024;
    let (command_tx, commands) = mpsc::channel();
    let (reader_events, reader_event_rx) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let (attempted, attempts) = mpsc::channel();
    let (completed, completions) = mpsc::channel();
    let output = TerminalSessionNativePtyOutputSink {
        clipboard_authority: Arc::default(),
        commands: command_tx.clone(),
        events: reader_events,
    };

    let producer = thread::spawn(move || {
        for index in 0..=PTY_OUTPUT_QUEUE_CAPACITY {
            attempted.send(index).unwrap();
            let sent = output.publish(NativePtyOutput::Bytes(vec![index as u8; CHUNK_SIZE]));
            completed.send(sent.then_some(CHUNK_SIZE)).unwrap();
            if !sent {
                break;
            }
        }
    });

    let mut queued_bytes = 0;
    for index in 0..PTY_OUTPUT_QUEUE_CAPACITY {
        assert_eq!(attempts.recv().unwrap(), index);
        queued_bytes += completions.recv().unwrap().unwrap();
    }
    assert_eq!(attempts.recv().unwrap(), PTY_OUTPUT_QUEUE_CAPACITY);
    assert!(matches!(
        completions.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert_eq!(queued_bytes, PTY_OUTPUT_QUEUE_CAPACITY * CHUNK_SIZE);

    command_tx.send(Command::Shutdown).unwrap();
    for _ in 0..PTY_OUTPUT_QUEUE_CAPACITY {
        assert!(matches!(commands.recv().unwrap(), Command::ReaderReady(_)));
    }
    assert!(matches!(commands.recv().unwrap(), Command::Shutdown));
    assert!(matches!(
        commands.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    drop(reader_event_rx);
    assert_eq!(completions.recv().unwrap(), None);
    producer.join().unwrap();
}

#[test]
fn held_keys_release_only_unreleased_physical_keys() {
    let mut held = HeldKeys::default();
    held.route(&text_key(KeyAction::Press));
    held.route(&modifier_key(KeyAction::Press));
    held.route(&text_key(KeyAction::Release));

    let releases = held.take_releases();

    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].physical_key, PhysicalKey::ShiftLeft);
    assert_eq!(releases[0].action, KeyAction::Release);
    assert!(held.take_releases().is_empty());
}

#[test]
fn held_keys_suppress_one_stale_release_but_route_a_new_press_release_pair() {
    let mut held = HeldKeys::default();
    let press = text_key(KeyAction::Press);
    let repeat = text_key(KeyAction::Repeat);
    let release = text_key(KeyAction::Release);

    assert!(held.route(&press));
    assert_eq!(held.take_releases().len(), 1);
    assert!(!held.route(&repeat));
    assert!(!held.route(&release));
    assert!(held.route(&release));

    let mut held = HeldKeys::default();
    assert!(held.route(&press));
    assert_eq!(held.take_releases().len(), 1);
    assert!(held.route(&press));
    assert!(held.route(&release));
    assert!(held.take_releases().is_empty());
    assert!(held.suppressed_releases.is_empty());
}

#[test]
fn enabling_focus_reporting_emits_current_state_and_deduplicates_edges() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, _events, _accessibility) = result.unwrap();
    session.focus(false);
    reader_steps
        .send(ReaderStep::Bytes(b"\x1b[?1004h".to_vec()))
        .unwrap();

    let enabled = records.wait_for("the current focus-out report", |state| {
        state.written == b"\x1b[O"
    });
    assert_eq!(enabled.written, b"\x1b[O");

    session.focus(false);
    session.resize(test_geometry());
    records.wait_for("the duplicate focus barrier", |state| {
        !state.resizes.is_empty()
    });
    assert_eq!(records.snapshot().written, b"\x1b[O");

    session.focus(true);
    records.wait_for("the focus-in edge", |state| {
        state.written == b"\x1b[O\x1b[I"
    });
    session.focus(true);
    session.resize(geometry(81, 24, 8.0, 20.0));
    records.wait_for("the duplicate focus-in barrier", |state| {
        state.resizes.len() == 2
    });
    assert_eq!(records.snapshot().written, b"\x1b[O\x1b[I");

    // The status report reply proves the worker parsed the disable. A resize cannot, because the
    // reader may hand the output to the worker after a later command.
    reader_steps
        .send(ReaderStep::Bytes(b"\x1b[?1004l\x1b[5n".to_vec()))
        .unwrap();
    records.wait_for("the focus-reporting disable barrier", |state| {
        state.written == b"\x1b[O\x1b[I\x1b[0n"
    });
    session.focus(false);
    session.resize(geometry(82, 24, 8.0, 20.0));
    records.wait_for("the disabled focus edge barrier", |state| {
        state.resizes.len() == 3
    });
    assert_eq!(records.snapshot().written, b"\x1b[O\x1b[I\x1b[0n");

    session.shutdown();
}

#[test]
fn focus_loss_releases_terminal_held_keys_once_before_focus_out() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, _events, _accessibility) = result.unwrap();
    reader_steps
        .send(ReaderStep::Bytes(b"\x1b[>11u\x1b[?1004h".to_vec()))
        .unwrap();
    records.wait_for("the current focus-in report", |state| {
        state.written == b"\x1b[I"
    });

    session.key(text_key(KeyAction::Press));
    session.focus(false);
    records.wait_for("held release before focus-out", |state| {
        state.written.ends_with(b"\x1b[97;1:3u\x1b[O")
    });
    let once = records.snapshot().written;

    session.focus(false);
    session.resize(test_geometry());
    records.wait_for("the duplicate focus-out barrier", |state| {
        !state.resizes.is_empty()
    });
    assert_eq!(records.snapshot().written, once);

    session.shutdown();
}

#[test]
fn physical_key_up_after_refocus_is_suppressed_after_synthetic_release() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();
    reader_steps
        .send(ReaderStep::Bytes(b"\x1b[>11u\x1b[?1004h".to_vec()))
        .unwrap();
    records.wait_for("the current focus-in report", |state| {
        state.written == b"\x1b[I"
    });

    session.key(text_key(KeyAction::Press));
    reader_steps
        .send(ReaderStep::Bytes(b"output".to_vec()))
        .unwrap();
    receive_event(
        &events,
        "the terminal output after the held key press",
        |event| matches!(event, TerminalSessionEvent::Screen(screen) if screen_text(screen).contains("output")),
    );
    session.focus(false);
    records.wait_for("held release before focus-out", |state| {
        state.written.ends_with(b"\x1b[97;1:3u\x1b[O")
    });
    session.focus(true);
    records.wait_for("focus-in after synthetic release", |state| {
        state.written.ends_with(b"\x1b[I")
    });
    let before_physical_release = records.snapshot().written;

    session.key(text_key(KeyAction::Release));
    session.resize(test_geometry());
    records.wait_for("the suppressed physical release barrier", |state| {
        !state.resizes.is_empty()
    });

    assert_eq!(records.snapshot().written, before_physical_release);
    session.shutdown();
}

#[test]
fn denied_osc52_keeps_prior_focus_reports_before_later_terminal_replies() {
    for operation in [b"\x1b]52;c;?\x07".as_slice(), b"\x1b]52;c;c2VjcmV0\x1b\\"] {
        let (_command_tx, commands) = mpsc::channel();
        let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
        let records = ScriptedPtyRecords::default();
        let (events, _receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
        let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
        let mut worker = TerminalWorker {
            metadata_state: TerminalSessionMetadataState::default(),
            native_pty: direct_native_pty(records.clone()),
            input: PtyInput::default(),
            emulator: TerminalEmulator::new(test_geometry()).unwrap(),
            commands,
            reader_events,
            events: events.into(),
            accessibility,
            pending_command: None,
            terminal_input_focused: true,
            focus_reporting_enabled: false,
            held_keys: HeldKeys::default(),
            schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
            osc52_filter: Osc52Filter::default(),
            permission_requests: PermissionRequestFilter::default(),
            clipboard: WorkerClipboard::default(),
            progress: Default::default(),
        };
        let output = [b"\x1b[?1004h".as_slice(), operation, b"\x1b[5n"].concat();

        assert!(worker.feed_test_output(vec![output]));
        assert_eq!(
            records.snapshot().written,
            if operation.contains(&b'?') {
                b"\x1b[I\x1b]52;c;\x07\x1b[0n".as_slice()
            } else {
                b"\x1b[I\x1b[0n"
            }
        );
        worker.finish();
    }
}

#[test]
fn denied_osc52_reads_reply_empty_and_later_terminal_output_remains_ordered() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(records.clone()),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };

    assert!(worker.feed_test_output(vec![
        b"before\x1b]52;c;?\x07".to_vec(),
        b"\x1b]52;c;c2VjcmV0\x1b\\".to_vec(),
    ]));
    assert_eq!(records.snapshot().written, b"\x1b]52;c;\x07");
    let TerminalSessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
        panic!("denied clipboard operations must only publish the ordinary screen");
    };
    assert!(screen_text(&screen).contains("before"));
    assert!(receiver.try_recv().is_err());

    assert!(worker.feed_test_output(vec![b"after\x1b[5n\r\nlater\x1b[6n".to_vec(),]));
    assert_eq!(
        records.snapshot().written,
        b"\x1b]52;c;\x07\x1b[0n\x1b[2;6R"
    );
    assert!(worker.publish_screen());
    let TerminalSessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
        panic!("ordinary terminal output must continue after denied clipboard operations");
    };
    let text = screen_text(&screen);
    assert!(text.contains("beforeafter"));
    assert!(text.contains("later"));
    assert!(!text.contains("secret"));
    assert!(!text.contains("c2VjcmV0"));
    assert!(receiver.try_recv().is_err());
    worker.finish();
}

/// A worker that reads scripted output, with the receiving end of its events. The channel ends it
/// does not read stay alive with the worker.
struct PermissionRequestWorker {
    worker: TerminalWorker,
    events: async_channel::Receiver<TerminalSessionEvent>,
    records: ScriptedPtyRecords,
    _commands: mpsc::Sender<Command>,
    _reader: mpsc::SyncSender<NativePtyOutput>,
    _accessibility: async_channel::Receiver<Arc<TerminalAccessibilityModel>>,
}

fn permission_request_worker() -> PermissionRequestWorker {
    let (command_tx, commands) = mpsc::channel();
    let (reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, accessibility_receiver) = async_channel::bounded(1);
    let worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(records.clone()),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };
    worker.clipboard.authority.client_focus(Some(true));
    PermissionRequestWorker {
        worker,
        events: receiver,
        records,
        _commands: command_tx,
        _reader: reader_tx,
        _accessibility: accessibility_receiver,
    }
}

/// The client effects a worker relayed to its controlling client, in order.
fn relayed_effects(worker: &TerminalWorker) -> Vec<spaceterm_server::pane::effects::Effect> {
    TerminalSubscription {
        events: async_channel::bounded(1).1,
        accessibility: async_channel::bounded(1).1,
        clipboard: async_channel::bounded(1).1,
        state: worker.events.clone(),
    }
    .take_client_effects()
    .into_iter()
    .map(|request| request.effect)
    .collect()
}

#[test]
fn a_permission_request_is_relayed_and_kept_off_the_screen() {
    let PermissionRequestWorker {
        mut worker,
        events: receiver,
        records,
        ..
    } = permission_request_worker();

    assert!(worker.feed_test_output(vec![
        b"before\x1b]7701;permissions=screen-".to_vec(),
        b"recording\x07after".to_vec(),
    ]));
    assert!(matches!(
        relayed_effects(&worker)[..],
        [spaceterm_server::pane::effects::Effect::PermissionSetup {
            screen_recording: true,
            accessibility: false,
        }]
    ));
    assert!(worker.publish_screen());
    let TerminalSessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
        panic!("terminal output must still publish a screen");
    };
    let text = screen_text(&screen);
    assert!(text.contains("beforeafter"));
    assert!(!text.contains("7701"));
    assert!(records.snapshot().written.is_empty());
    worker.finish();
}

/// A flood of requests in one read takes one slot in the bounded client effect queue, so it
/// cannot push other effects out.
#[test]
fn the_permission_requests_in_one_read_become_one_client_effect() {
    let PermissionRequestWorker { mut worker, .. } = permission_request_worker();
    let mut flood = b"\x1b]7701;permissions=screen-recording\x07".repeat(90);
    flood.extend_from_slice(b"\x1b]7701;permissions=accessibility\x07");

    assert!(worker.feed_test_output(vec![flood]));

    assert!(matches!(
        relayed_effects(&worker)[..],
        [spaceterm_server::pane::effects::Effect::PermissionSetup {
            screen_recording: true,
            accessibility: true,
        }]
    ));
    worker.finish();
}

#[test]
fn consecutive_output_chunks_should_publish_one_ordered_coalesced_screen() {
    let (command_tx, commands) = mpsc::channel();
    let (reader_events, reader_event_rx) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let output = TerminalSessionNativePtyOutputSink {
        clipboard_authority: Arc::default(),
        commands: command_tx,
        events: reader_events,
    };
    assert!(output.publish(NativePtyOutput::Bytes(b"first".to_vec())));
    assert!(output.publish(NativePtyOutput::Bytes(b" second".to_vec())));
    assert!(matches!(commands.recv().unwrap(), Command::ReaderReady(_)));

    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(records),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events: reader_event_rx,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };

    // Output after idle publishes at once, and one batch of reads makes one screen.
    assert!(worker.process_reader_events(Some(3)));
    assert!(worker.pending_command.is_none());
    let TerminalSessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
        panic!("coalesced output must publish a terminal screen")
    };
    assert!(screen_text(&screen).contains("first second"));
    assert!(receiver.try_recv().is_err());
    assert!(matches!(
        worker.commands.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    worker.finish();
}

#[test]
fn a_published_screen_reports_the_input_serial_written_before_its_capture() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(records.clone()),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };

    assert!(worker.write_pty(b"a"));
    assert!(worker.process_command(Command::InputSerial(7)));
    assert_eq!(records.snapshot().written, b"a");
    assert!(worker.feed_test_output(vec![b"a".to_vec()]));

    let TerminalSessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
        panic!("the echo must publish a screen")
    };
    assert_eq!(screen.input_serial, 7);
    worker.finish();
}

#[test]
fn rapid_output_coalesces_before_screen_and_accessibility_construction() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(records.clone()),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };
    continue_output_burst(&mut worker);

    let generation = worker.emulator.presentation_generation();
    for index in 0..64 {
        assert!(worker.feed_test_output(vec![format!("line {index}\r\n").into_bytes()]));
    }
    assert!(worker.feed_test_output(vec![b"\x1b[5n".to_vec()]));

    assert_eq!(worker.emulator.presentation_generation(), generation);
    assert_eq!(records.snapshot().written, b"\x1b[0n");
    assert!(receiver.try_recv().is_err());
    assert!(accessibility_receiver.try_recv().is_err());
    assert!(worker.schedules.take_presentation_barrier());
    assert!(worker.publish_screen());
    let TerminalSessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
        panic!("the coalesced presentation must publish one Screen")
    };
    assert!(screen_text(&screen).contains("line 63"));
    assert!(receiver.try_recv().is_err());
    assert!(accessibility_receiver.try_recv().is_err());
    assert!(worker.process_command(Command::PublishAccessibility));
    for _ in 0..128 {
        if !accessibility_receiver.is_empty() {
            break;
        }
        let command = worker
            .take_accessibility_continuation()
            .expect("accessibility work must finish within the budget");
        assert!(worker.process_command(command));
    }
    let model = accessibility_receiver
        .try_recv()
        .expect("accessibility model must finish within 128 chunks");
    assert!(model.text().contains("line 63"));
    assert_eq!(model.generation(), screen.generation);
    worker.finish();
}

#[test]
fn queued_command_runs_before_accessibility_barrier_uses_the_pending_slot() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, _receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(records),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events: events.into(),
        accessibility,
        pending_command: Some(Command::Key(text_key(KeyAction::Press))),
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };
    worker.schedules.request_presentation();
    worker.schedules.update_accessibility(true);
    for _ in 0..8 {
        worker.schedules.note_normal_command();
    }

    assert!(matches!(
        worker.receive_next_command(),
        Some(Command::Key(_))
    ));
    assert!(matches!(
        worker.receive_next_command(),
        Some(Command::PublishPendingScreen)
    ));
    assert!(matches!(
        worker.pending_command,
        Some(Command::PublishAccessibility)
    ));
    assert!(matches!(
        worker.receive_next_command(),
        Some(Command::PublishAccessibility)
    ));
    worker.finish();
}

#[test]
fn queued_input_runs_before_due_scrollback_compression() {
    let now = Instant::now();
    let (command_tx, commands) = mpsc::channel();
    let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let (events, _receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(ScriptedPtyRecords::default()),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(now + Duration::from_secs(30), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };
    let activity = worker.emulator.compression_activity().unwrap();
    worker
        .schedules
        .observe_compression(now - Duration::from_secs(1), activity, false);
    assert!(worker.schedules.deadline(None).unwrap() < now);
    command_tx
        .send(Command::Key(text_key(KeyAction::Press)))
        .unwrap();

    let first = worker.receive_next_command();
    assert!(matches!(first, Some(Command::Key(_))), "first={first:?}");
    assert!(matches!(
        worker.receive_next_command(),
        Some(Command::CompressScrollback)
    ));
    worker.finish();
}

#[test]
fn accessibility_demand_flushes_a_pending_screen_before_binding_its_model() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, accessibility_receiver) = async_channel::bounded(1);
    let schedule_input = ScheduleInput::default();
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(records),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), schedule_input.clone()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };
    assert!(worker.publish_screen());
    let _ = receiver.try_recv().unwrap();
    worker
        .schedules
        .mark_accessibility_presented(Instant::now(), true);
    worker
        .schedules
        .mark_presented(Instant::now() + Duration::from_secs(1));
    worker.emulator.feed(b"new generation");
    worker.schedules.request_presentation();
    let requested_at = Instant::now();
    assert!(schedule_input.enqueue_accessibility_demand(requested_at));
    worker.schedules.accessibility_demand_received(requested_at);

    assert!(matches!(
        worker.receive_next_command(),
        Some(Command::PublishPendingScreen)
    ));
    assert!(worker.process_command(Command::PublishPendingScreen));
    let TerminalSessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
        panic!("the visual presentation must precede its accessibility model")
    };
    assert!(matches!(
        worker.receive_next_command(),
        Some(Command::PublishAccessibility)
    ));
    assert!(worker.process_command(Command::PublishAccessibility));
    while accessibility_receiver.is_empty() {
        assert!(worker.process_command(Command::AccessibilityContinue));
    }
    let model = accessibility_receiver.try_recv().unwrap();
    assert_eq!(
        model.selection_request(0..0).unwrap().generation,
        screen.generation
    );
    assert!(
        !worker
            .schedules
            .accessibility_presentation_due(Instant::now())
    );
    worker.finish();
}

#[test]
fn hidden_output_builds_one_latest_presentation_only_after_restore() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(records),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };
    worker.schedules.set_presentable(false, Instant::now());

    let generation = worker.emulator.presentation_generation();
    assert!(worker.feed_test_output(vec![b"hidden one\r\n".to_vec()]));
    assert_eq!(worker.emulator.presentation_generation(), generation);
    assert!(worker.feed_test_output(vec![b"hidden latest".to_vec()]));
    assert_eq!(worker.emulator.presentation_generation(), generation);
    assert!(receiver.try_recv().is_err());
    assert!(accessibility_receiver.try_recv().is_err());

    assert!(worker.process_command(Command::SetPresentable(true)));
    let TerminalSessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
        panic!("restoration must publish the latest hidden state")
    };
    assert!(screen_text(&screen).contains("hidden latest"));
    assert!(matches!(
        worker.receive_next_command(),
        Some(Command::PublishAccessibility)
    ));
    assert!(worker.process_command(Command::PublishAccessibility));
    for _ in 0..128 {
        if !accessibility_receiver.is_empty() {
            break;
        }
        let Some(command) = worker.take_accessibility_continuation() else {
            panic!("restored accessibility construction must continue to completion")
        };
        assert!(worker.process_command(command));
    }
    let model = accessibility_receiver
        .try_recv()
        .expect("accessibility model must finish within 128 chunks");
    assert!(model.text().contains("hidden latest"));
    assert_eq!(
        model.generation(),
        worker.emulator.presentation_generation()
    );
    worker.finish();
}

#[test]
fn kitty_animation_publishes_new_pixels_while_the_pty_is_idle() {
    let _guard = crate::terminal::graphics::test_lock();
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, accessibility_receiver) = async_channel::bounded(1);
    drop(accessibility_receiver);
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(ScriptedPtyRecords::default()),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };
    worker
        .schedules
        .update_line(Instant::now(), Ok(LineMode::UNKNOWN));
    worker.emulator.feed(
        b"\x1b_Ga=T,t=d,f=32,i=1,s=1,v=1;AQIDBA==\x1b\\\
          \x1b_Ga=f,t=d,f=32,i=1,s=1,v=1,z=40;BQYHCA==\x1b\\\
          \x1b_Ga=a,i=1,r=1,z=40,s=3\x1b\\",
    );
    assert!(worker.publish_screen());
    let TerminalSessionEvent::Screen(first) = receiver.try_recv().unwrap() else {
        panic!("expected initial screen");
    };
    assert_eq!(first.graphics.images[0].rgba.as_ref(), &[1, 2, 3, 4]);

    let command = worker.receive_next_command().unwrap();
    assert!(matches!(command, Command::GraphicsAnimationTick));
    assert!(worker.process_command(command));
    let TerminalSessionEvent::Screen(next) = receiver.try_recv().unwrap() else {
        panic!("expected animated screen");
    };
    assert_eq!(next.graphics.images[0].rgba.as_ref(), &[5, 6, 7, 8]);
    worker.finish();
}

#[test]
fn kitty_deferred_replacement_retries_when_the_ui_releases_old_pixels_without_output() {
    let _guard = crate::terminal::graphics::test_lock();
    for hidden in [false, true] {
        let (command_tx, commands) = mpsc::channel();
        let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
        let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
        let (accessibility, accessibility_receiver) = async_channel::bounded(1);
        drop(accessibility_receiver);
        let mut emulator = TerminalEmulator::new(test_geometry()).unwrap();
        emulator.set_graphics_budget_wakeup(move || {
            let _ = command_tx.send(Command::GraphicsBudgetAvailable);
        });
        let mut worker = TerminalWorker {
            metadata_state: TerminalSessionMetadataState::default(),
            native_pty: direct_native_pty(ScriptedPtyRecords::default()),
            input: PtyInput::default(),
            emulator,
            commands,
            reader_events,
            events: events.into(),
            accessibility,
            pending_command: None,
            terminal_input_focused: true,
            focus_reporting_enabled: false,
            held_keys: HeldKeys::default(),
            schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
            osc52_filter: Osc52Filter::default(),
            permission_requests: PermissionRequestFilter::default(),
            clipboard: WorkerClipboard::default(),
            progress: Default::default(),
        };
        worker
            .schedules
            .update_line(Instant::now(), Ok(LineMode::UNKNOWN));
        worker
            .emulator
            .feed(b"\x1b_Ga=T,t=d,f=32,i=1,s=1,v=1;AQIDBA==\x1b\\");
        assert!(worker.publish_screen());
        let old_pixels = receiver.try_recv().unwrap();
        let _pressure = crate::terminal::graphics::GraphicsReservation::try_acquire(
            crate::terminal::graphics::APPLICATION_DECODED_LIMIT - 8,
        )
        .unwrap();
        worker
            .emulator
            .feed(b"\x1b_Ga=T,t=d,f=32,i=1,s=1,v=1;BQYHCA==\x1b\\");
        assert!(worker.publish_screen());
        let TerminalSessionEvent::Screen(deferred) = receiver.try_recv().unwrap() else {
            panic!("expected a screen releasing the superseded image");
        };
        assert!(deferred.graphics.images.is_empty());
        assert!(worker.commands.try_recv().is_err());
        if hidden {
            assert!(worker.process_command(Command::SetPresentable(false)));
        }

        drop(old_pixels);

        let command = worker.commands.try_recv().unwrap();
        assert!(matches!(command, Command::GraphicsBudgetAvailable));
        assert!(worker.process_command(command));
        if hidden {
            assert!(receiver.is_empty());
            assert!(worker.process_command(Command::SetPresentable(true)));
        }
        if receiver.is_empty() {
            let command = worker.receive_next_command().unwrap();
            assert!(matches!(command, Command::PublishPendingScreen));
            assert!(worker.process_command(command));
        }
        let TerminalSessionEvent::Screen(replacement) = receiver.try_recv().unwrap() else {
            panic!("expected the deferred image to be published");
        };
        assert_eq!(replacement.graphics.images[0].rgba.as_ref(), &[5, 6, 7, 8]);
        assert!(worker.commands.try_recv().is_err());
        worker.finish();
    }
}

#[test]
fn synchronized_output_between_accessibility_chunks_preserves_the_eager_seed() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(records),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };
    worker.emulator.feed(b"seed");
    assert!(worker.publish_screen());
    let _ = receiver.try_recv().unwrap();
    assert!(worker.process_command(Command::PublishAccessibility));
    assert!(worker.schedules.accessibility_pending());
    assert!(accessibility_receiver.is_empty());

    let started = Instant::now();
    worker.emulator.feed_at(b"\x1b[?2026htransaction", started);
    let continuation = worker.take_accessibility_continuation().unwrap();
    assert!(worker.process_command(continuation));
    assert!(accessibility_receiver.is_empty());

    worker.emulator.feed_at(b" complete\x1b[?2026l", started);
    assert!(worker.publish_screen());
    let due = Instant::now() + Duration::from_millis(100);
    assert!(worker.schedules.accessibility_presentation_due(due));
    assert!(worker.process_command(Command::PublishAccessibility));
    for _ in 0..128 {
        if !accessibility_receiver.is_empty() {
            break;
        }
        let Some(command) = worker.take_accessibility_continuation() else {
            panic!("the eager accessibility seed must survive synchronized output")
        };
        assert!(worker.process_command(command));
    }
    let model = accessibility_receiver
        .try_recv()
        .expect("accessibility model must finish within 128 chunks");
    assert!(model.text().contains("transaction complete"));
    assert_eq!(
        model.generation(),
        worker.emulator.presentation_generation()
    );
    worker.finish();
}

#[test]
fn closed_screen_lane_skips_snapshot_and_accessibility_without_stopping_runtime() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(1);
    let (accessibility, accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(records),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };
    worker.emulator.feed(b"unobserved");
    let generation = worker.emulator.presentation_generation();
    drop(receiver);

    assert!(worker.publish_screen());
    assert_eq!(worker.emulator.presentation_generation(), generation);
    assert!(accessibility_receiver.try_recv().is_err());
    worker.finish();
}

#[test]
fn synchronized_output_deadline_should_publish_only_after_output_stalls() {
    let (command_tx, commands) = mpsc::channel();
    let (_reader_events, reader_event_rx) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(records),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events: reader_event_rx,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };
    assert!(worker.publish_screen());
    let _ = receiver.try_recv().unwrap();

    let started = Instant::now() - Duration::from_millis(1100);
    worker.emulator.feed_at(b"\x1b[?2026hlong", started);
    assert!(worker.publish_screen());
    assert!(receiver.try_recv().is_err());

    let progressed = started + Duration::from_millis(900);
    worker.emulator.feed_at(b" remote redraw", progressed);
    command_tx.send(Command::ReaderReady(None)).unwrap();
    loop {
        let command = worker.receive_next_command().unwrap();
        if matches!(command, Command::ReaderReady(None)) {
            break;
        }
        assert!(worker.process_command(command));
    }
    assert!(receiver.try_recv().is_err());
    assert!(worker.release_synchronized_output_if_due(started + MAX_SYNCHRONIZED_OUTPUT_DURATION));
    assert!(
        receiver.try_recv().is_err(),
        "an active remote redraw must remain atomic after one second of total duration"
    );

    assert!(
        worker.release_synchronized_output_if_due(progressed + MAX_SYNCHRONIZED_OUTPUT_DURATION)
    );
    let TerminalSessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
        panic!("the synchronized-output deadline must publish a screen")
    };
    assert!(screen_text(&screen).contains("long remote redraw"));
    worker.finish();
}

#[test]
fn queued_reader_commands_should_not_extend_the_prompt_redraw_cap() {
    let (command_tx, commands) = mpsc::channel();
    let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, accessibility_receiver) = async_channel::bounded(1);
    let now = Instant::now();
    let resized = now - Duration::from_millis(200);
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(ScriptedPtyRecords::default()),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(resized, ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };
    worker
        .emulator
        .feed_at(b"\x1b]133;A;redraw=last\x07bash> ", resized);
    assert!(worker.publish_screen());
    let _ = receiver.try_recv().unwrap();
    worker.schedules.mark_presented(resized);
    worker
        .emulator
        .resize_for_pty_at(geometry(81, 24, 8.0, 20.0), true, resized)
        .unwrap();
    // The last output is recent, so only the absolute cap can release presentation.
    worker.emulator.feed_at(b"\rbash> background output", now);
    assert!(worker.publish_screen());
    assert!(receiver.try_recv().is_err());
    for _ in 0..8 {
        command_tx.send(Command::ReaderReady(None)).unwrap();
    }

    loop {
        let command = worker.receive_next_command().unwrap();
        if matches!(command, Command::ReaderReady(None)) {
            break;
        }
        assert!(worker.process_command(command));
    }

    let TerminalSessionEvent::Screen(screen) = receiver
        .try_recv()
        .expect("queued readers must not starve the prompt deadline")
    else {
        panic!("the prompt deadline must publish a screen")
    };
    assert!(screen_text(&screen).contains("bash> background output"));
    assert!(worker.process_command(Command::PublishAccessibility));
    while accessibility_receiver.is_empty() {
        assert!(worker.process_command(Command::AccessibilityContinue));
    }
    let model = accessibility_receiver.try_recv().unwrap();
    assert!(model.text().contains("bash> background output"));
    assert_eq!(model.generation(), screen.generation);
    worker.finish();
}

#[test]
fn hidden_worker_should_publish_directory_changes_without_constructing_screens() {
    for remote in [false, true] {
        let (_command_tx, commands) = mpsc::channel();
        let (_reader_events, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
        let (events, receiver) = async_channel::bounded(2);
        let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
        let context = if remote {
            TerminalMetadataContext::Remote(
                crate::terminal::metadata::RemoteTerminalMetadataContext::new(
                    "build".into(),
                    crate::domain::RemoteDirectory::new("~".into()).unwrap(),
                ),
            )
        } else {
            TerminalMetadataContext::local(
                crate::local_path::LocalPathSemantics::Posix,
                "/home/local",
                Default::default(),
            )
        };
        let mut worker = TerminalWorker {
            metadata_state: TerminalSessionMetadataState::default(),
            native_pty: direct_native_pty(ScriptedPtyRecords::default()),
            input: PtyInput::default(),
            emulator: TerminalEmulator::new_with_metadata_context(
                test_geometry(),
                context,
                "shell",
                "xterm",
                Instant::now(),
            )
            .unwrap(),
            commands,
            reader_events,
            events: events.into(),
            accessibility,
            pending_command: None,
            terminal_input_focused: false,
            focus_reporting_enabled: false,
            held_keys: HeldKeys::default(),
            schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
            osc52_filter: Osc52Filter::default(),
            permission_requests: PermissionRequestFilter::default(),
            clipboard: WorkerClipboard::default(),
            progress: Default::default(),
        };
        assert!(worker.publish_screen());
        let _ = receiver.try_recv().unwrap();
        assert!(worker.process_command(Command::SetPresentable(false)));
        let generation = worker.emulator.presentation_generation();
        assert!(worker.feed_test_output(vec![b"\x1b]7;file:///srv/alpha\x07".to_vec()]));
        assert!(worker.feed_test_output(vec![b"\x1b]7;file:///srv/latest\x07".to_vec()]));
        // Overflow the production-sized queue before the UI can consume the directory wake.
        assert!(worker.send_terminal_event(TerminalSessionEvent::PasteUnconfirmed));
        assert!(worker.send_terminal_event(TerminalSessionEvent::SelectionCopyReady));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            TerminalSessionEvent::PasteUnconfirmed
        ));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            TerminalSessionEvent::SelectionCopyReady
        ));
        let expected = if remote {
            crate::domain::CurrentDirectory::Remote(
                crate::domain::RemoteDirectory::new("/srv/latest".into()).unwrap(),
            )
        } else {
            crate::domain::CurrentDirectory::Local(PathBuf::from("/srv/latest"))
        };
        assert_eq!(
            worker
                .metadata_state
                .snapshot()
                .unwrap()
                .current_directory(),
            Some(expected.clone())
        );
        assert_eq!(worker.emulator.presentation_generation(), generation);
        assert!(receiver.try_recv().is_err());
        assert!(worker.feed_test_output(vec![b"ordinary hidden output".to_vec()]));
        assert!(receiver.try_recv().is_err());
        // A hidden Terminal Session's title and progress reach its Tab through the retained facts,
        // once the metadata reporting interval since the directory change has passed.
        worker
            .schedules
            .fix_clock(Instant::now() + Duration::from_millis(100));
        assert!(worker.feed_test_output(vec![b"\x1b]2;agent\x07\x1b]9;4;2\x07".to_vec()]));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            TerminalSessionEvent::MetadataChanged(_)
        ));
        let retained = worker.metadata_state.snapshot().unwrap();
        assert_eq!(retained.title.value.as_ref(), "agent");
        assert_eq!(
            retained.progress,
            crate::terminal::metadata::ProgressMetadata::Error(0)
        );
        worker.emulator.mark_metadata_stale();
        assert!(worker.publish_screen());
        let retained = worker.metadata_state.snapshot().unwrap();
        assert_eq!(retained.current_directory(), None);
        worker.finish();
    }
}

#[test]
fn visible_metadata_screen_does_not_evict_bell_attention() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_events, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let (events, receiver) = async_channel::bounded(2);
    let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(ScriptedPtyRecords::default()),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: false,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };

    worker.clipboard.authority.client_focus(Some(true));
    assert!(worker.feed_test_output(vec![b"\x07\x1b]9;4;1;25\x07".to_vec(),]));
    assert!(matches!(
        relayed_effects(&worker)[..],
        [spaceterm_server::pane::effects::Effect::Bell]
    ));
    assert!(matches!(
        receiver.try_recv().unwrap(),
        TerminalSessionEvent::Screen(_)
    ));
    assert!(receiver.try_recv().is_err());
    worker.finish();
}

#[test]
fn hiding_before_a_throttled_screen_publishes_the_retained_metadata_change() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_events, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(ScriptedPtyRecords::default()),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: false,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };

    assert!(worker.publish_screen());
    let _ = receiver.try_recv().unwrap();
    continue_output_burst(&mut worker);
    assert!(worker.feed_test_output(vec![b"\x1b]2;latest title\x07".to_vec()]));
    assert!(receiver.try_recv().is_err());

    assert!(worker.process_command(Command::SetPresentable(false)));
    assert!(matches!(
        receiver.try_recv().unwrap(),
        TerminalSessionEvent::MetadataChanged(_)
    ));
    assert_eq!(
        worker
            .metadata_state
            .snapshot()
            .unwrap()
            .title
            .value
            .as_ref(),
        "latest title"
    );
    worker.finish();
}

#[test]
fn hidden_metadata_bursts_do_not_evict_bell_attention() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_events, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let (events, receiver) = async_channel::bounded(2);
    let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(ScriptedPtyRecords::default()),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: false,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };

    worker.clipboard.authority.client_focus(Some(true));
    assert!(worker.process_command(Command::SetPresentable(false)));
    assert!(worker.feed_test_output(vec![b"\x07\x1b]2;first\x07".to_vec()]));
    assert!(worker.feed_test_output(vec![b"\x1b]2;latest\x07".to_vec()]));

    assert!(matches!(
        relayed_effects(&worker)[..],
        [spaceterm_server::pane::effects::Effect::Bell]
    ));
    assert!(matches!(
        receiver.try_recv().unwrap(),
        TerminalSessionEvent::MetadataChanged(_)
    ));
    assert!(receiver.try_recv().is_err());
    assert_eq!(
        worker
            .metadata_state
            .snapshot()
            .unwrap()
            .title
            .value
            .as_ref(),
        "latest"
    );
    worker.finish();
}

#[test]
fn hidden_prompt_zone_changes_reach_close_confirmation_facts() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_events, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(ScriptedPtyRecords::default()),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: false,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };

    assert!(worker.publish_screen());
    let _ = receiver.try_recv().unwrap();
    assert!(worker.process_command(Command::SetPresentable(false)));
    assert!(worker.feed_test_output(vec![b"\x1b]133;A\x07".to_vec()]));

    assert!(matches!(
        receiver.try_recv().unwrap(),
        TerminalSessionEvent::MetadataChanged(_)
    ));
    assert_eq!(
        worker.metadata_state.snapshot().unwrap().prompt_zone,
        crate::terminal::metadata::PromptZone::Prompt
    );
    worker.finish();
}

#[test]
fn synchronized_output_expiry_defers_hidden_screen_construction_until_restore() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_events, reader_event_rx) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(records),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events: reader_event_rx,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };
    assert!(worker.publish_screen());
    let _ = receiver.try_recv().unwrap();
    assert!(worker.process_command(Command::SetPresentable(false)));

    let started = Instant::now();
    worker
        .emulator
        .feed_at(b"\x1b[?2026hhidden redraw", started);
    assert!(worker.release_synchronized_output_if_due(started + MAX_SYNCHRONIZED_OUTPUT_DURATION));
    assert!(receiver.try_recv().is_err());

    assert!(worker.process_command(Command::SetPresentable(true)));
    let TerminalSessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
        panic!("restoration must publish the completed synchronized redraw")
    };
    assert!(screen_text(&screen).contains("hidden redraw"));
    assert!(receiver.try_recv().is_err());
    worker.finish();
}

#[test]
fn output_control_output_should_preserve_screen_order_through_the_terminal_session_interface() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();

    reader_steps
        .send(ReaderStep::Bytes(b"first".to_vec()))
        .unwrap();
    let first = receive_event(
        &events,
        "the first output Screen",
        |event| matches!(event, TerminalSessionEvent::Screen(screen) if screen_text(screen).contains("first")),
    );
    session.resize(test_geometry());
    records.wait_for("the control between output chunks", |state| {
        state.resizes.len() == 1
    });
    reader_steps
        .send(ReaderStep::Bytes(b" second".to_vec()))
        .unwrap();
    let second = receive_event(
        &events,
        "the second output Screen",
        |event| matches!(event, TerminalSessionEvent::Screen(screen) if screen_text(screen).contains("first second")),
    );

    let (TerminalSessionEvent::Screen(first), TerminalSessionEvent::Screen(second)) =
        (first, second)
    else {
        unreachable!("the event predicates accept only terminal screens")
    };
    assert_eq!(
        (
            screen_text(&first).contains("second"),
            screen_text(&second).contains("first second"),
            records.snapshot().resizes,
        ),
        (false, true, vec![pty_size(test_geometry())])
    );
    session.shutdown();
}

#[test]
fn command_debug_never_exposes_paste_or_key_contents() {
    let (reply, _receiver) = async_channel::bounded(1);
    assert_eq!(
        format!(
            "{:?}",
            Command::RequestPaste("private paste content".to_owned().into(), reply)
        ),
        "RequestPaste",
    );
    assert_eq!(
        format!("{:?}", Command::Key(text_key(KeyAction::Press))),
        "Key"
    );
}

#[test]
fn resize_should_reach_the_pty_with_pixel_dimensions() {
    let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, _events, _accessibility) = result.unwrap();
    let resized = geometry(100, 30, 9.0, 21.0);

    session.resize(resized);
    let state = records.wait_for("the scripted PTY resize", |state| state.resizes.len() == 1);

    assert_eq!(
        state.resizes,
        vec![NativePtySize {
            rows: 30,
            columns: 100,
            pixel_width: 900,
            pixel_height: 630
        }]
    );
    session.shutdown();
}

#[test]
fn pixel_only_resize_should_reach_the_pty_and_publish_geometry() {
    let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();
    let initial = records
        .snapshot()
        .initial_screen
        .expect("the actual asynchronous startup must publish its initial Screen");
    let grid = test_geometry().grid();
    let resized = geometry(grid.cols, grid.rows, 9.0, 21.0);

    session.resize(resized);
    worker_round_trip(&session);

    assert_eq!(
        records.snapshot().resizes,
        vec![NativePtySize {
            rows: 24,
            columns: 80,
            pixel_width: 720,
            pixel_height: 504,
        }]
    );
    let TerminalSessionEvent::Screen(screen) =
        receive_event(&events, "the resized geometry", |event| {
            matches!(event, TerminalSessionEvent::Screen(_))
        })
    else {
        unreachable!()
    };
    assert_eq!(screen.size, initial.size);
    assert_eq!(screen.cell_pixels, (9, 21));
    assert!(screen.damage.resize);
    assert_eq!(screen.rows, initial.rows);
    assert_eq!(initial.size.cols, grid.cols);
    assert_eq!(initial.size.rows, grid.rows);
    session.shutdown();
}

#[test]
fn unchanged_pty_winsize_does_not_clear_the_idle_prompt() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();
    reader_steps
        .send(ReaderStep::Bytes(
            b"QA_RESIZE_MARKER\r\n\x1b]133;A;redraw=1\x07~\r\n> \x1b]133;B\x07".to_vec(),
        ))
        .unwrap();
    let before = receive_event(
        &events,
        "the idle prompt",
        |event| matches!(event, TerminalSessionEvent::Screen(screen) if screen_text(screen).contains("QA_RESIZE_MARKER") && screen_text(screen).contains("> ")),
    );
    let TerminalSessionEvent::Screen(before) = before else {
        unreachable!()
    };

    session.resize(test_geometry());
    worker_round_trip(&session);
    assert_eq!(records.snapshot().resizes, vec![pty_size(test_geometry())]);
    assert!(events.try_recv().is_err());
    assert!(screen_text(&before).contains("~"));

    session.shutdown();
}

#[test]
fn fractional_backing_geometry_should_export_the_engine_pixel_grid_to_the_pty() {
    let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, _events, _accessibility) = result.unwrap();
    let resized = TerminalGeometry::from_grid(
        CellGridSize::new(10, 2),
        LogicalCellSize::new(7.5, 20.0),
        BackingScale::new(1.5).unwrap(),
    );

    session.resize(resized);
    let state = records.wait_for("the fractional scripted PTY resize", |state| {
        state.resizes.len() == 1
    });

    assert_eq!(resized.backing_grid_size().width, 113);
    assert_eq!(
        state.resizes,
        vec![NativePtySize {
            rows: 2,
            columns: 10,
            pixel_width: 120,
            pixel_height: 60,
        }]
    );
    session.shutdown();
}

#[test]
fn pty_size_should_report_unrepresentable_pixel_extents_as_unavailable() {
    let size = pty_size(geometry(u16::MAX, 2, 2.0, 40.0));
    assert_eq!(size.pixel_width, 0);
    assert_eq!(size.pixel_height, 80);
}

#[test]
fn kitty_auto_sizing_from_pty_pixels_should_agree_with_size_replies_and_not_wrap() {
    let _guard = crate::terminal::graphics::test_lock();
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();
    let resized = geometry(57, 20, 21.6, 40.0);
    assert_eq!(resized.backing_grid_size().width, 1232);
    session.resize(resized);
    let state = records.wait_for("the icat PTY geometry", |state| state.resizes.len() == 1);
    let size = state.resizes[0];
    assert_eq!(
        size,
        NativePtySize {
            rows: 20,
            columns: 57,
            pixel_width: 1254,
            pixel_height: 800,
        },
    );

    reader_steps
        .send(ReaderStep::Bytes(b"\x1b[14t\x1b[16t\x1b[18t".to_vec()))
        .unwrap();
    let state = records.wait_for("size query replies for the icat geometry", |state| {
        state.written.ends_with(b"\x1b[8;20;57t")
    });
    let cell_width = size.pixel_width / size.columns;
    let cell_height = size.pixel_height / size.rows;
    assert_eq!(
        state.written,
        format!(
            "\x1b[4;{};{}t\x1b[6;{cell_height};{cell_width}t\x1b[8;{};{}t",
            size.pixel_height, size.pixel_width, size.rows, size.columns,
        )
        .as_bytes(),
    );

    // icat fits the image to the PTY pixel width, then floors pixel_width/columns
    // to infer cell width.
    let image_columns = size.pixel_width.div_ceil(cell_width);
    assert_eq!(image_columns, 57);
    let mut stream = format!(
        "\x1b_Ga=T,t=d,f=24,i=1,U=1,s={image_columns},v=2,c={image_columns},r=2,q=2;{}\x1b\\",
        "AAAA".repeat(usize::from(image_columns) * 2),
    );
    for row in ['\u{305}', '\u{30d}'] {
        stream.push_str(&format!("\x1b[38;5;1m\u{10eeee}{row}\u{305}"));
        stream.push_str(&"\u{10eeee}".repeat(usize::from(image_columns) - 1));
        stream.push_str("\x1b[0m\r\n");
    }
    reader_steps
        .send(ReaderStep::Bytes(stream.into_bytes()))
        .unwrap();
    let event = receive_event(
        &events,
        "the complete automatic icat image",
        |event| matches!(event, TerminalSessionEvent::Screen(screen) if screen.cursor.position.is_some_and(|position| position.row == 2) && !screen.graphics.images.is_empty()),
    );
    let TerminalSessionEvent::Screen(snapshot) = event else {
        unreachable!()
    };
    assert_eq!(snapshot.graphics.placements.len(), 2);
    assert!(snapshot.graphics.placements.iter().all(|placement| {
        matches!(placement.viewport_row, 0 | 1)
            && placement.viewport_col == 0
            && placement.destination_width == u32::from(size.pixel_width)
    }));
    // Keep the graphics test lock until the worker releases its native pixels.
    session.shutdown_and_join();
}

#[test]
fn rapid_resizes_should_queue_one_notification_and_retain_only_the_latest_geometry() {
    let (commands, receiver) = mpsc::channel();
    let schedule_input = ScheduleInput::default();
    let mut schedules = WorkerSchedules::new(Instant::now(), schedule_input.clone());
    let mut session = TerminalRuntime {
        metadata_state: TerminalSessionMetadataState::default(),
        commands: Some(commands),
        worker: None,
        native_pty_close: None,
        cleanup_confirmation: None,
        schedule_input,
        event_publisher: async_channel::bounded(1).0.into(),
        clipboard_authority: Arc::default(),
    };
    let pixel_only = TerminalGeometry::from_grid(
        CellGridSize::new(80, 24),
        LogicalCellSize::new(7.5, 20.0),
        BackingScale::new(1.5).unwrap(),
    );
    let latest = geometry(100, 30, 9.0, 21.0);

    session.resize(pixel_only);
    session.resize(latest);

    assert_eq!(
        (
            matches!(receiver.try_recv(), Ok(Command::Resize)),
            receiver.try_recv().is_err(),
            schedules.take_resize(),
        ),
        (true, true, Some(latest))
    );
    session.shutdown();
}

#[test]
fn pending_pty_responses_should_precede_later_input_through_the_terminal_session_interface() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();
    let resized = geometry(20, 4, 8.0, 18.0);

    reader_steps
        .send(ReaderStep::Bytes(b"\x1b[?2048hX".to_vec()))
        .unwrap();
    receive_event(
        &events,
        "the mode-setting terminal output",
        |event| matches!(event, TerminalSessionEvent::Screen(screen) if screen_text(screen).contains('X')),
    );
    session.resize(resized);
    records.wait_for("the in-band terminal resize response", |state| {
        state.written == b"\x1b[48;24;80;480;640t\x1b[48;4;20;72;160t"
    });
    assert_eq!(
        session
            .request_paste("later".to_owned().into())
            .recv_blocking()
            .unwrap(),
        Ok(PasteRequestOutcome::Written)
    );
    let state = records.wait_for("the later terminal input", |state| {
        state.written.ends_with(b"later")
    });

    assert_eq!(
        state.written,
        b"\x1b[48;24;80;480;640t\x1b[48;4;20;72;160tlater"
    );
    session.shutdown();
}

#[test]
fn bracketed_multiline_paste_is_written_without_confirmation() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();

    reader_steps
        .send(ReaderStep::Bytes(b"\x1b[?2004hX".to_vec()))
        .unwrap();
    receive_event(
        &events,
        "bracketed-paste mode activation",
        |event| matches!(event, TerminalSessionEvent::Screen(screen) if screen_text(screen).contains('X')),
    );

    assert_eq!(
        session
            .request_paste("one\ntwo".to_owned().into())
            .recv_blocking()
            .unwrap(),
        Ok(PasteRequestOutcome::Written)
    );
    let state = records.wait_for("the bracketed multiline paste", |state| {
        state.written.ends_with(b"\x1b[200~one\ntwo\x1b[201~")
    });
    assert_eq!(state.written, b"\x1b[200~one\ntwo\x1b[201~");
    session.shutdown();
}

#[test]
fn control_bearing_paste_is_sanitized_without_confirmation() {
    let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, _events, _accessibility) = result.unwrap();

    assert_eq!(
        session
            .request_paste("one\x03two".to_owned().into())
            .recv_blocking()
            .unwrap(),
        Ok(PasteRequestOutcome::Written)
    );
    let state = records.wait_for("the sanitized paste", |state| state.written == b"one two");
    assert_eq!(state.written, b"one two");
    session.shutdown();
}

#[test]
fn bracketed_paste_with_a_closing_fence_still_requires_confirmation() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();

    reader_steps
        .send(ReaderStep::Bytes(b"\x1b[?2004hX".to_vec()))
        .unwrap();
    receive_event(
        &events,
        "bracketed-paste mode activation",
        |event| matches!(event, TerminalSessionEvent::Screen(screen) if screen_text(screen).contains('X')),
    );

    let outcome = session
        .request_paste("one\x1b[201~two".to_owned().into())
        .recv_blocking()
        .unwrap()
        .unwrap();
    let PasteRequestOutcome::ConfirmationRequired(confirmation) = outcome else {
        panic!("an embedded closing fence must require confirmation")
    };

    assert!(confirmation.risk.closing_fence);
    assert!(records.snapshot().written.is_empty());
    session.shutdown();
}

#[test]
fn unsafe_paste_is_immutable_until_confirmation_and_uses_exact_unbracketed_bytes() {
    let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, _events, _accessibility) = result.unwrap();
    let mut caller_copy = "one\r\ntw\x03o".to_owned();

    let outcome = session
        .request_paste(caller_copy.clone().into())
        .recv_blocking()
        .unwrap()
        .unwrap();
    caller_copy.clear();
    let PasteRequestOutcome::ConfirmationRequired(confirmation) = outcome else {
        panic!("multiline, control-bearing paste must require confirmation")
    };
    assert!(records.snapshot().written.is_empty());

    assert_eq!(
        session
            .resolve_paste(confirmation.id, PasteDecision::Confirm)
            .recv_blocking()
            .unwrap(),
        Ok(PasteResolution::Written)
    );
    assert_eq!(records.snapshot().written, b"one\rtw o");
    session.shutdown();
}

#[test]
fn cancelled_paste_writes_no_pty_bytes() {
    let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, _events, _accessibility) = result.unwrap();
    let outcome = session
        .request_paste("first\nsecond".to_owned().into())
        .recv_blocking()
        .unwrap()
        .unwrap();
    let PasteRequestOutcome::ConfirmationRequired(confirmation) = outcome else {
        panic!("multiline paste must require confirmation")
    };

    assert_eq!(
        session
            .resolve_paste(confirmation.id, PasteDecision::Cancel)
            .recv_blocking()
            .unwrap(),
        Ok(PasteResolution::Cancelled)
    );
    assert!(records.snapshot().written.is_empty());
    session.shutdown();
}

#[test]
fn focus_loss_invalidates_pending_paste_before_confirmation() {
    let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, _events, _accessibility) = result.unwrap();
    let outcome = session
        .request_paste("first\nsecond".to_owned().into())
        .recv_blocking()
        .unwrap()
        .unwrap();
    let PasteRequestOutcome::ConfirmationRequired(confirmation) = outcome else {
        panic!("multiline paste must require confirmation")
    };

    session.focus(false);
    worker_round_trip(&session);
    assert_eq!(
        session
            .resolve_paste(confirmation.id, PasteDecision::Confirm)
            .recv_blocking()
            .unwrap(),
        Ok(PasteResolution::Stale)
    );
    assert!(records.snapshot().written.is_empty());
    session.shutdown();
}

#[test]
fn only_one_unsafe_paste_can_await_confirmation() {
    let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, _events, _accessibility) = result.unwrap();
    let first = session
        .request_paste("first\ncommand".to_owned().into())
        .recv_blocking()
        .unwrap()
        .unwrap();
    assert!(matches!(
        first,
        PasteRequestOutcome::ConfirmationRequired(_)
    ));

    assert_eq!(
        session
            .request_paste("second\ncommand".to_owned().into())
            .recv_blocking()
            .unwrap(),
        Ok(PasteRequestOutcome::Rejected(
            PasteRejection::ConfirmationPending
        ))
    );
    assert!(records.snapshot().written.is_empty());
    session.shutdown();
}

#[test]
fn write_failure_should_emit_a_runtime_failure_and_stop_the_worker() {
    let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
        write_error: Some("/private/terminal token=secret output=private".to_owned()),
        ..ScriptedPtyOptions::default()
    });
    let (mut session, events, _accessibility) = result.unwrap();

    let _ = session
        .request_paste("input".to_owned().into())
        .recv_blocking();
    let event = receive_event(&events, "the PTY write failure", |event| {
        matches!(event, TerminalSessionEvent::Failed(_))
    });

    let TerminalSessionEvent::Failed(failure) = event else {
        unreachable!("the event predicate accepts only terminal failures")
    };
    let diagnostic = crate::terminal::TerminalFailure::from_session(&failure);
    assert_eq!(diagnostic.class(), crate::terminal::FailureClass::Pty);
    assert_eq!(diagnostic.operation(), "write-shell-input");
    assert_eq!(
        diagnostic.reason(),
        Some(FailureReason::Io(ErrorKind::BrokenPipe))
    );
    assert_eq!(
        failure,
        TerminalSessionFailure::Runtime(
            TerminalFailure::pty("write-shell-input")
                .with_reason(FailureReason::Io(ErrorKind::BrokenPipe))
        )
    );
    let state = records.wait_for("the failed PTY worker to release ownership", |state| {
        state.pty_drops == 1 && state.terminator_drops == 1
    });
    assert_eq!((state.write_attempts, state.written.len()), (1, 0));

    session.shutdown();
    let state = records.snapshot();
    assert_eq!(
        (state.terminations, state.pty_drops, state.terminator_drops),
        (1, 1, 1)
    );
}

#[test]
fn resize_failure_identifies_the_pty_operation_and_stops_the_worker() {
    let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
        resize_error: Some("/private/terminal token=secret".to_owned()),
        ..ScriptedPtyOptions::default()
    });
    let (mut session, events, _accessibility) = result.unwrap();
    session.resize(geometry(100, 30, 9.0, 21.0));
    let event = receive_event(&events, "the PTY resize failure", |event| {
        matches!(event, TerminalSessionEvent::Failed(_))
    });
    let TerminalSessionEvent::Failed(failure) = event else {
        unreachable!("the predicate accepts only failures")
    };
    let diagnostic = TerminalFailure::from_session(&failure);
    assert_eq!(
        diagnostic,
        TerminalFailure::pty("resize-pty").with_reason(FailureReason::Io(ErrorKind::Other))
    );
    records.wait_for("the failed PTY worker to release ownership", |state| {
        state.pty_drops == 1
    });
    session.shutdown();
}

#[test]
fn control_text_input_is_typed_as_keys_and_text_without_a_key_is_discarded() {
    let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();
    // FS has no key. The input is discarded and the Terminal Session keeps running.
    session.key(KeyInput::text_input("token=secret\u{1c}".to_owned()));
    // A line feed is typed as Enter, which the legacy encoding writes as a carriage return.
    session.key(KeyInput::text_input("ls\nok".to_owned()));
    records.wait_for("the typed text and Enter", |state| {
        state.written == b"ls\rok"
    });
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, TerminalSessionEvent::Failed(_)),
            "unencodable input must not fail the Terminal Session"
        );
    }
    session.shutdown();
}

#[test]
fn reader_error_with_successful_wait_should_emit_a_pty_read_failure() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
        exit_code: 7,
        ..ScriptedPtyOptions::default()
    });
    let (mut session, events, _accessibility) = result.unwrap();

    reader_steps
        .send(ReaderStep::Error(
            "native detail: /private/terminal token=secret".to_owned(),
        ))
        .unwrap();
    let event = receive_event(&events, "the PTY read failure", |event| {
        matches!(event, TerminalSessionEvent::Failed(_))
    });

    let TerminalSessionEvent::Failed(TerminalSessionFailure::PtyRead {
        read_error,
        exit_status,
    }) = event
    else {
        panic!("a read error followed by a successful wait must be classified as PtyRead")
    };
    assert_eq!(read_error, "Native PTY read failed (Other)");
    assert_eq!(exit_status, "Shell exited with code 7");
    assert_eq!(records.snapshot().waits, 1);

    session.shutdown();
}

#[test]
fn reader_error_with_wait_failure_should_preserve_both_errors() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
        wait_error: Some("wait unavailable".to_owned()),
        ..ScriptedPtyOptions::default()
    });
    let (mut session, events, _accessibility) = result.unwrap();

    reader_steps
        .send(ReaderStep::Error(
            "native detail: /private/terminal token=secret".to_owned(),
        ))
        .unwrap();
    let event = receive_event(&events, "the PTY read and wait failure", |event| {
        matches!(event, TerminalSessionEvent::Failed(_))
    });

    let TerminalSessionEvent::Failed(failure) = event else {
        unreachable!("the event predicate accepts only terminal failures")
    };
    assert_eq!(
        failure,
        TerminalSessionFailure::ShellWait {
            read_error: Some("Native PTY read failed (Other)".to_owned()),
            wait_error: "Native PTY wait failed (Other)".to_owned(),
        }
    );
    assert_eq!(records.snapshot().waits, 1);

    session.shutdown();
}

#[test]
fn reader_eof_with_wait_failure_should_emit_a_shell_wait_failure() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
        wait_error: Some("wait unavailable".to_owned()),
        ..ScriptedPtyOptions::default()
    });
    let (mut session, events, _accessibility) = result.unwrap();

    reader_steps.send(ReaderStep::Eof).unwrap();
    let event = receive_event(&events, "the shell wait failure", |event| {
        matches!(event, TerminalSessionEvent::Failed(_))
    });

    let TerminalSessionEvent::Failed(failure) = event else {
        unreachable!("the event predicate accepts only terminal failures")
    };
    assert_eq!(
        failure,
        TerminalSessionFailure::ShellWait {
            read_error: None,
            wait_error: "Native PTY wait failed (Other)".to_owned(),
        }
    );
    assert_eq!(records.snapshot().waits, 1);

    session.shutdown();
}

#[test]
fn child_wait_timeout_should_emit_a_shell_wait_failure() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
        wait_times_out: true,
        ..ScriptedPtyOptions::default()
    });
    let (mut session, events, _accessibility) = result.unwrap();

    reader_steps.send(ReaderStep::Eof).unwrap();
    let event = receive_event(&events, "the bounded shell wait failure", |event| {
        matches!(event, TerminalSessionEvent::Failed(_))
    });

    let TerminalSessionEvent::Failed(TerminalSessionFailure::ShellWait {
        read_error,
        wait_error,
    }) = event
    else {
        panic!("a child wait timeout must be classified as ShellWait")
    };
    assert_eq!(read_error, None);
    assert_eq!(wait_error, "Native PTY wait failed (TimedOut)");
    let state = records.wait_for("the timed-out PTY worker to release ownership", |state| {
        state.pty_drops == 1 && state.reader_drops == 1
    });
    assert_eq!(
        (state.waits, state.pty_drops, state.reader_drops),
        (1, 1, 1)
    );

    assert_eq!(
        records.snapshot().wait_timeouts,
        vec![Duration::from_secs(2)]
    );
    session.shutdown();
}

#[test]
fn reader_eof_should_wait_for_the_child_and_emit_exit() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
        exit_code: 7,
        ..ScriptedPtyOptions::default()
    });
    let (mut session, events, _accessibility) = result.unwrap();

    reader_steps.send(ReaderStep::Eof).unwrap();
    let event = receive_event(&events, "the scripted shell exit", |event| {
        matches!(event, TerminalSessionEvent::Exited(_))
    });

    assert!(matches!(
        event,
        TerminalSessionEvent::Exited(TerminalSessionExit::ExitCode(7))
    ));
    let state = records.wait_for("the exited PTY worker to release ownership", |state| {
        state.pty_drops == 1
    });
    assert_eq!((state.waits, state.pty_drops), (1, 1));

    session.shutdown();
}

#[test]
fn repeated_shutdown_should_return_before_a_blocked_reader_finishes() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
        termination_releases_reader: false,
        ..ScriptedPtyOptions::default()
    });
    let (mut session, _events, _accessibility) = result.unwrap();
    let (completed, completion) = mpsc::sync_channel(1);

    let shutdown_thread = thread::spawn(move || {
        session.shutdown();
        session.shutdown();
        completed.send(session).unwrap();
    });
    let session = match completion.recv_timeout(Duration::from_millis(250)) {
        Ok(session) => session,
        Err(error) => {
            reader_steps.send(ReaderStep::Eof).unwrap();
            shutdown_thread.join().unwrap();
            panic!("shutdown waited for the blocked reader thread: {error}");
        }
    };
    shutdown_thread.join().unwrap();
    let state = records.wait_for("the detached worker to drop its PTY", |state| {
        state.pty_drops == 1 && state.terminator_drops == 1
    });
    assert_eq!(
        (
            state.terminations,
            state.pty_drops,
            state.reader_drops,
            state.terminator_drops,
            session.commands.is_none(),
            session.worker.is_none(),
            session.native_pty_close.is_none(),
        ),
        (1, 1, 0, 1, true, true, true)
    );
    assert_eq!(
        state.lifecycle,
        vec![
            LifecycleStep::TerminationRequested,
            LifecycleStep::PtyDropped,
        ]
    );

    reader_steps.send(ReaderStep::Eof).unwrap();
    let state = records.wait_for("the released reader ownership", |state| {
        state.reader_drops == 1
    });
    assert_eq!(
        (
            state.terminations,
            state.pty_drops,
            state.reader_drops,
            state.terminator_drops,
        ),
        (1, 1, 1, 1)
    );
}

#[test]
fn drop_should_return_after_termination_fails_with_a_blocked_reader() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
        termination_error: Some("termination unavailable".to_owned()),
        termination_releases_reader: false,
        ..ScriptedPtyOptions::default()
    });
    let (session, _events, _accessibility) = result.unwrap();
    let (completed, completion) = mpsc::sync_channel(1);

    let drop_thread = thread::spawn(move || {
        drop(session);
        completed.send(()).unwrap();
    });
    if let Err(error) = completion.recv_timeout(Duration::from_millis(250)) {
        reader_steps.send(ReaderStep::Eof).unwrap();
        drop_thread.join().unwrap();
        panic!("Drop waited after termination failed: {error}");
    }
    drop_thread.join().unwrap();
    let state = records.wait_for("the detached worker to drop its PTY", |state| {
        state.pty_drops == 1 && state.terminator_drops == 1
    });
    assert_eq!(
        (
            state.terminations,
            state.pty_drops,
            state.reader_drops,
            state.terminator_drops,
        ),
        (1, 1, 0, 1)
    );

    reader_steps.send(ReaderStep::Eof).unwrap();
    let state = records.wait_for("the reader to release after termination failure", |state| {
        state.reader_drops == 1
    });
    assert_eq!(
        (
            state.terminations,
            state.pty_drops,
            state.reader_drops,
            state.terminator_drops,
        ),
        (1, 1, 1, 1)
    );
}

#[test]
fn accessibility_demand_sender_coalesces_native_queries_onto_the_worker_lane() {
    let (commands, receiver) = mpsc::channel();
    let schedule_input = ScheduleInput::default();
    let session = TerminalRuntime {
        metadata_state: TerminalSessionMetadataState::default(),
        commands: Some(commands),
        worker: None,
        native_pty_close: None,
        cleanup_confirmation: None,
        schedule_input: schedule_input.clone(),
        event_publisher: async_channel::bounded(1).0.into(),
        clipboard_authority: Arc::default(),
    };
    let sender = session.accessibility_demand_sender().unwrap();
    let latest = Instant::now() + Duration::from_millis(1);

    sender.request();
    sender.request_at(latest);

    assert!(matches!(
        receiver.try_recv(),
        Ok(Command::AccessibilityDemand)
    ));
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let mut schedules = WorkerSchedules::new(latest, schedule_input);
    schedules.accessibility_demand_received(latest);
    assert!(schedules.accessibility_presentation_due(latest));
}

#[test]
fn application_mouse_drag_cancellation_releases_once_and_accepts_a_fresh_press() {
    for hidden in [false, true] {
        for (button, code) in [
            (PointerButton::Left, 0),
            (PointerButton::Middle, 1),
            (PointerButton::Right, 2),
        ] {
            let records = ScriptedPtyRecords::default();
            let (_command_tx, commands) = mpsc::channel();
            let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
            let (events, _receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
            let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
            let mut worker = TerminalWorker {
                metadata_state: TerminalSessionMetadataState::default(),
                native_pty: direct_native_pty(records.clone()),
                input: PtyInput::default(),
                emulator: TerminalEmulator::new(test_geometry()).unwrap(),
                commands,
                reader_events,
                events: events.into(),
                accessibility,
                pending_command: None,
                terminal_input_focused: true,
                focus_reporting_enabled: true,
                held_keys: HeldKeys::default(),
                schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
                osc52_filter: Osc52Filter::default(),
                permission_requests: PermissionRequestFilter::default(),
                clipboard: WorkerClipboard::default(),
                progress: Default::default(),
            };
            worker.emulator.feed(b"\x1b[?1002h\x1b[?1006h\x1b[?1004h");
            // The server's pane host sends every mouse event against the screen it published.
            let pointer = |phase, position, modifiers, generation| {
                Command::ApplicationPointer(
                    PointerInput {
                        generation,
                        phase,
                        button: (phase != PointerPhase::Motion).then_some(button),
                        position,
                        modifiers,
                        shift_selection: ShiftSelectionPolicy::default(),
                    },
                    1,
                    1,
                )
            };
            assert!(worker.process_command(pointer(
                PointerPhase::Press,
                SurfacePosition { x: 1.0, y: 1.0 },
                InputModifiers::default(),
                worker.emulator.presentation_generation(),
            )));
            assert!(worker.process_command(pointer(
                PointerPhase::Motion,
                SurfacePosition { x: 17.0, y: 41.0 },
                InputModifiers {
                    control: true,
                    ..InputModifiers::default()
                },
                worker.emulator.presentation_generation(),
            )));
            let mut expected = format!("\x1b[<{code};1;1M\x1b[<{};3;3M", code + 48);
            assert_eq!(records.snapshot().written, expected.as_bytes());

            // Both lifecycle paths must release at the last position before focus-out.
            let release = format!("\x1b[<{};3;3m", code + 16);
            if hidden {
                assert!(worker.process_command(Command::SetPresentable(false)));
                assert_eq!(
                    records.snapshot().written,
                    format!("{expected}{release}").as_bytes()
                );
            }
            assert!(worker.process_command(Command::Focus(false)));
            expected.push_str(&release);
            expected.push_str("\x1b[O");
            assert_eq!(records.snapshot().written, expected.as_bytes(), "{hidden}");

            // Repeated cancellation must not send another release.
            assert!(worker.process_command(Command::SetPresentable(false)));
            assert!(worker.process_command(Command::Focus(false)));
            assert_eq!(records.snapshot().written, expected.as_bytes());
            assert!(worker.process_command(Command::SetPresentable(true)));
            assert!(worker.process_command(Command::Focus(true)));
            assert!(worker.process_command(pointer(
                PointerPhase::Press,
                SurfacePosition { x: 1.0, y: 1.0 },
                InputModifiers::default(),
                worker.emulator.presentation_generation(),
            )));
            expected.push_str(&format!("\x1b[I\x1b[<{code};1;1M"));
            assert_eq!(records.snapshot().written, expected.as_bytes());
            worker.finish();
        }
    }
}

#[test]
fn line_mode_changes_are_reported_on_output_and_focus_during_idle_backoff() {
    let (started, reader, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, _events, _accessibility) = started.unwrap();
    let subscription = session.subscribe();
    records.wait_for("initial hidden-input poll", |state| state.line_reads > 0);
    records.update(|state| state.line_mode = PASSWORD);
    reader
        .send(ReaderStep::Bytes(b"Password: ".to_vec()))
        .unwrap();
    wait_for_line(&subscription, "password prompt", |line| {
        line.mode.hidden_input()
    });
    records.update(|state| state.line_mode = LineMode::UNKNOWN);
    session.focus(false);
    wait_for_line(&subscription, "focus transition", |line| {
        !line.mode.hidden_input()
    });
    records.update(|state| state.line_mode = PASSWORD);
    session.focus(true);
    wait_for_line(&subscription, "focus restoration", |line| {
        line.mode.hidden_input()
    });
    session.shutdown_and_join();
}

/// A direct worker with a fixed clock at `start` that has taken its first line mode read, and
/// the receiver for its Screens.
fn line_worker(
    start: Instant,
) -> (
    TerminalWorker,
    ScriptedPtyRecords,
    async_channel::Receiver<TerminalSessionEvent>,
) {
    let (mut worker, records, _requests, _reader, _commands) = clipboard_worker();
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    worker.events = events.into();
    worker.schedules = WorkerSchedules::new(start, ScheduleInput::default());
    run_due_commands(&mut worker, start);
    assert_eq!(records.snapshot().line_reads, 1);
    (worker, records, receiver)
}

/// Runs every scheduled command due at `now`, as the worker loop does when it wakes.
fn run_due_commands(worker: &mut TerminalWorker, now: Instant) {
    worker.schedules.fix_clock(now);
    while let Some(command) = worker.schedules.take_due(now) {
        assert!(worker.process_command(command));
    }
}

fn count_screens(receiver: &async_channel::Receiver<TerminalSessionEvent>) -> usize {
    std::iter::from_fn(|| receiver.try_recv().ok())
        .filter(|event| matches!(event, TerminalSessionEvent::Screen(_)))
        .count()
}

#[test]
fn an_output_flood_reads_line_mode_only_at_frame_captures_and_one_settle_read() {
    let start = Instant::now();
    let (mut worker, records, receiver) = line_worker(start);
    let mut frames = 0;
    let mut now = start;
    for batch in 0..1000 {
        now += Duration::from_millis(1);
        worker.schedules.fix_clock(now);
        assert!(worker.feed_test_output(vec![format!("y {batch}\r\n").into_bytes()]));
        run_due_commands(&mut worker, now);
        frames += count_screens(&receiver);
    }
    let flood_reads = records.snapshot().line_reads - 1;
    assert!(frames > 0);
    assert!(
        flood_reads <= frames,
        "{flood_reads} line mode reads for {frames} frames"
    );

    // The last coalesced frame publishes once output is quiet, and reads first.
    let settled = now + Duration::from_millis(200);
    run_due_commands(&mut worker, settled - Duration::from_millis(1));
    frames += count_screens(&receiver);
    let reads = records.snapshot().line_reads - 1;
    assert!(
        reads <= frames,
        "{reads} line mode reads for {frames} frames"
    );
    run_due_commands(&mut worker, settled);
    assert_eq!(records.snapshot().line_reads - 1, reads + 1);
    assert_eq!(worker.schedules.deadline(None), None);
    worker.finish();
}

#[test]
fn a_line_mode_change_goes_before_the_frame_captured_with_it() {
    let start = Instant::now();
    let (mut worker, records, _) = line_worker(start);
    let publisher = worker.events.clone();
    let queue: Arc<std::sync::OnceLock<async_channel::Receiver<TerminalSessionEvent>>> =
        Arc::default();
    let notifications = Arc::new(Mutex::new(Vec::new()));
    let notifier: SubscriptionNotifier = {
        let publisher = publisher.clone();
        let queue = Arc::clone(&queue);
        let notifications = Arc::clone(&notifications);
        Arc::new(move || {
            let queued = queue.get().map_or(0, async_channel::Receiver::len);
            notifications
                .lock()
                .unwrap()
                .push((queued, publisher.line().mode));
        })
    };
    let (receiver, _sender) = publisher.subscribe(Some(notifier));
    queue.set(receiver.clone()).unwrap();

    records.update(|state| state.line_mode = PASSWORD);
    assert!(worker.feed_test_output(vec![b"Password: ".to_vec()]));

    let TerminalSessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
        panic!("output after idle must publish a Screen at once");
    };
    assert!(screen_text(&screen).contains("Password: "));
    let notifications = notifications.lock().unwrap();
    let first_frame = notifications
        .iter()
        .find(|(queued, _)| *queued > 0)
        .expect("the Screen must notify the subscriber");
    assert_eq!(first_frame.1, PASSWORD, "{notifications:?}");
    drop(notifications);
    worker.finish();
}

#[test]
fn input_and_focus_read_line_mode_at_once_and_200_ms_later() {
    let start = Instant::now();
    let (mut worker, records, _receiver) = line_worker(start);
    let commands: [fn() -> Command; 2] = [
        || Command::Key(KeyInput::text_input("a".to_owned())),
        || Command::Focus(false),
    ];
    let mut now = start;
    let mut reads = 1;
    for command in commands {
        now += Duration::from_secs(1);
        worker.schedules.fix_clock(now);
        assert!(worker.process_command(command()));
        reads += 1;
        assert_eq!(records.snapshot().line_reads, reads);
        // Nothing else reads until the settle read.
        run_due_commands(&mut worker, now + Duration::from_millis(199));
        assert_eq!(records.snapshot().line_reads, reads);
        run_due_commands(&mut worker, now + Duration::from_millis(200));
        reads += 1;
        assert_eq!(records.snapshot().line_reads, reads);
        assert_eq!(worker.schedules.deadline(None), None);
    }
    worker.finish();
}

#[test]
fn a_hidden_session_reads_line_mode_for_output_at_the_metadata_pace() {
    let start = Instant::now();
    let (mut worker, records, _receiver) = line_worker(start);
    assert!(worker.process_command(Command::SetPresentable(false)));
    let mut now = start;
    let mut read_times = Vec::new();
    for batch in 0..1000 {
        now += Duration::from_millis(1);
        if batch == 500 {
            records.update(|state| state.line_mode = PASSWORD);
        }
        worker.schedules.fix_clock(now);
        assert!(worker.feed_test_output(vec![format!("y {batch}\r\n").into_bytes()]));
        let before = records.snapshot().line_reads;
        run_due_commands(&mut worker, now);
        if records.snapshot().line_reads > before {
            read_times.push(now - start);
        }
    }
    let expected: Vec<_> = (1..=10)
        .map(|step| Duration::from_millis(step * 100))
        .collect();
    assert_eq!(read_times, expected);
    // The change at 501 ms is read at the paced read at 600 ms.
    assert!(worker.events.line().mode.hidden_input());
    worker.finish();
}

#[test]
fn synchronized_output_publishes_a_line_mode_change_within_100_ms() {
    let start = Instant::now();
    let (mut worker, records, receiver) = line_worker(start);
    assert!(worker.publish_screen());
    count_screens(&receiver);
    assert!(worker.feed_test_output(vec![b"\x1b[?2026h".to_vec()]));
    let changed = start + Duration::from_millis(300);
    let mut published = None;
    for tick in 1..=20 {
        let now = start + Duration::from_millis(tick * 50);
        if now == changed {
            records.update(|state| state.line_mode = PASSWORD);
        }
        worker.schedules.fix_clock(now);
        assert!(worker.feed_test_output(vec![b"Password: ".to_vec()]));
        run_due_commands(&mut worker, now);
        if published.is_none() && worker.events.line().mode == PASSWORD {
            published = Some(now);
        }
    }
    assert_eq!(
        count_screens(&receiver),
        0,
        "synchronized output holds frames"
    );
    let published = published.expect("the mode change must be published while frames are held");
    assert!(
        published - changed <= Duration::from_millis(100),
        "published {:?} after the change",
        published - changed
    );
    // Held frames read at most once per 100 ms.
    assert!(records.snapshot().line_reads - 1 <= 10);
    worker.finish();
}

fn clipboard_worker() -> (
    TerminalWorker,
    ScriptedPtyRecords,
    async_channel::Receiver<ClipboardRequest>,
    mpsc::SyncSender<NativePtyOutput>,
    CommandSender<Command>,
) {
    let (command_tx, commands) = mpsc::channel();
    let (reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, _) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, _) = async_channel::bounded(1);
    let (clipboard, requests) = WorkerClipboard::connect(command_tx.clone());
    clipboard.authority.client_focus(Some(true));
    (
        TerminalWorker {
            metadata_state: TerminalSessionMetadataState::default(),
            native_pty: direct_native_pty(records.clone()),
            input: PtyInput::default(),
            emulator: TerminalEmulator::new(test_geometry()).unwrap(),
            commands,
            reader_events,
            events: events.into(),
            accessibility,
            pending_command: None,
            terminal_input_focused: true,
            focus_reporting_enabled: false,
            held_keys: HeldKeys::default(),
            schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
            osc52_filter: Osc52Filter::default(),
            permission_requests: PermissionRequestFilter::default(),
            clipboard,
            progress: Default::default(),
        },
        records,
        requests,
        reader_tx,
        command_tx,
    )
}

#[test]
fn clipboard_request_survives_reader_exit_until_output_is_resumed() {
    let (mut worker, records, requests, reader_tx, commands) = clipboard_worker();
    // Keep presentation channels open while checking terminal completion.
    let (events, _events_receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    worker.events = events.into();
    let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
    worker.accessibility = accessibility;
    reader_tx
        .send(NativePtyOutput::Bytes(
            b"before\x1b]52;c;?\x07after\x1b[5n".to_vec(),
        ))
        .unwrap();
    reader_tx.send(NativePtyOutput::Stopped(None)).unwrap();
    commands.send(Command::ReaderReady(Some(3))).unwrap();
    assert!(worker.process_reader_events(Some(3)));
    assert!(worker.clipboard.pending());
    assert!(records.snapshot().written.is_empty());
    drop(requests.try_recv().unwrap());
    let command = worker.commands.recv().unwrap();
    assert!(!worker.process_command(command));
    assert_eq!(records.snapshot().written, b"\x1b]52;c;\x07\x1b[0n");
    worker.finish();
}

#[test]
fn clipboard_wait_keeps_selection_queries_live_and_orders_read_before_later_replies() {
    let (mut worker, records, requests, _reader_tx, _commands) = clipboard_worker();
    assert!(worker.feed_test_output(vec![b"before\x1b]52;p;?\x1b\\after\x1b[5n".to_vec()]));
    assert!(worker.clipboard.pending());
    let (reply, selection) = mpsc::sync_channel(1);
    assert!(worker.process_command(Command::ServerAccessibilitySelection(None, reply)));
    assert!(selection.try_recv().is_ok());
    assert!(records.snapshot().written.is_empty());
    let request = requests.try_recv().unwrap();
    // Exercise the portable completion path without accessing the native clipboard.
    drop(request);
    assert!(worker.process_command(worker.commands.recv().unwrap()));
    assert_eq!(records.snapshot().written, b"\x1b]52;p;\x1b\\\x1b[0n");
    worker.finish();
}

#[test]
fn clipboard_reply_backpressure_keeps_selection_live_and_preserves_input_order() {
    let (mut worker, records, requests, _reader_tx, _commands) = clipboard_worker();
    let (events, _events_receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    worker.events = events.into();
    let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
    worker.accessibility = accessibility;
    assert!(worker.feed_test_output(vec![b"\x1b]52;c;?\x07\x1b[5n".to_vec()]));
    let request = requests.try_recv().unwrap();
    drop(request);
    let Command::CompleteClipboard(id, _) = worker.commands.recv().unwrap() else {
        panic!("completion expected")
    };
    records.update(|state| state.write_blocked = true);
    assert!(
        worker.complete_clipboard(Some(id), ClipboardCompletion::Text("x".repeat(1024 * 1024)))
    );
    let (reply, selection) = mpsc::sync_channel(1);
    assert!(worker.process_command(Command::ServerAccessibilitySelection(None, reply)));
    assert!(selection.try_recv().is_ok());
    assert!(worker.write_pty(b"later input"));
    assert!(records.snapshot().written.is_empty());
    records.update(|state| state.write_blocked = false);
    // Continue normal command scheduling until the queued input has drained.
    let deadline = Instant::now() + Duration::from_secs(2);
    while !records.snapshot().written.ends_with(b"\x1b[0nlater input") {
        assert!(
            Instant::now() < deadline,
            "queued input must drain within two seconds"
        );
        let command = worker.receive_next_command().unwrap();
        assert!(worker.process_command(command));
    }
    let mut expected =
        format!("\x1b]52;c;{}eA==\x07", "eHh4".repeat((1024 * 1024) / 3)).into_bytes();
    let written = records.snapshot().written;
    expected.extend_from_slice(b"\x1b[0nlater input");
    assert_eq!(written, expected);
    worker.finish();
}

#[test]
fn unsupported_clipboard_cut_buffers_never_request_a_native_selection() {
    for selector in ["0", "1", "2", "3", "4", "5", "6", "7", "q", "cp"] {
        let (mut worker, records, requests, _reader, _commands) = clipboard_worker();
        let output = format!("\x1b]52;{selector};?\x07\x1b]52;{selector};Y29waWVk\x1b\\\x1b[5n");
        assert!(worker.feed_test_output(vec![output.into_bytes()]));
        assert!(requests.try_recv().is_err());
        assert!(!worker.clipboard.pending());
        assert_eq!(records.snapshot().written, b"\x1b[0n");
        worker.finish();
    }
}

#[test]
fn queued_clipboard_operations_never_acquire_a_later_focus_grant() {
    for queued_while_focused in [true, false] {
        let (mut worker, records, requests, _reader, _commands) = clipboard_worker();
        assert!(worker.feed_test_output(vec![b"\x1b]52;c;?\x07".to_vec()]));
        let first = requests.try_recv().unwrap();
        if !queued_while_focused {
            worker.clipboard.authority.focus(false);
        }
        assert!(worker.feed_test_output(vec![
            b"\x1b]52;p;?\x1b\\\x1b]52;c;Y29waWVk\x07\x1b[5n".to_vec(),
        ]));
        worker.clipboard.authority.focus(false);
        worker.clipboard.authority.focus(true);
        drop(first);
        assert!(worker.process_command(worker.commands.recv().unwrap()));
        assert!(requests.try_recv().is_err());
        assert!(!worker.clipboard.pending());
        assert_eq!(
            records.snapshot().written,
            b"\x1b]52;c;\x07\x1b]52;p;\x1b\\\x1b[0n"
        );
        worker.finish();
    }
}

#[test]
fn deferred_reader_output_never_acquires_a_later_clipboard_focus_grant() {
    let (mut worker, records, requests, reader_tx, _commands) = clipboard_worker();
    assert!(worker.feed_test_output(vec![b"\x1b]52;c;?\x07".to_vec()]));
    let first = requests.try_recv().unwrap();
    reader_tx
        .send(NativePtyOutput::Bytes(b"\x1b]52;p;?\x07".to_vec()))
        .unwrap();
    assert!(worker.process_command(Command::ReaderReady(Some(3))));
    worker.clipboard.authority.focus(false);
    worker.clipboard.authority.focus(true);
    drop(first);
    assert!(worker.process_command(worker.commands.recv().unwrap()));
    let resume = worker.receive_next_command().unwrap();
    assert!(worker.process_command(resume));
    assert!(requests.try_recv().is_err());
    assert_eq!(records.snapshot().written, b"\x1b]52;c;\x07\x1b]52;p;\x07");
    worker.finish();
}

#[test]
fn fragmented_clipboard_operations_keep_their_initial_focus_grant() {
    for prefix_len in 1..b"\x1b]52;c;?\x07".len() {
        let (mut worker, records, requests, _reader, _commands) = clipboard_worker();
        let bytes = b"\x1b]52;c;?\x07";
        assert!(worker.feed_test_output(vec![bytes[..prefix_len].to_vec()]));
        worker.clipboard.authority.focus(false);
        worker.clipboard.authority.focus(true);
        assert!(worker.feed_test_output(vec![bytes[prefix_len..].to_vec()]));
        assert!(requests.try_recv().is_err());
        assert_eq!(records.snapshot().written, b"\x1b]52;c;\x07");
        worker.finish();
    }
}

#[test]
fn copy_or_forward_uses_enhanced_keyboard_and_suppresses_legacy_text() {
    for (modifiers, expected) in [
        (
            InputModifiers {
                platform: true,
                ..Default::default()
            },
            &b"\x1b[99;9u\x1b[99;9:3u"[..],
        ),
        (
            InputModifiers {
                control: true,
                shift: true,
                ..Default::default()
            },
            &b"\x1b[99;6u\x1b[99;6:3u"[..],
        ),
    ] {
        let (mut worker, records, _requests, _reader, _commands) = clipboard_worker();
        let (events, _events_receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
        worker.events = events.into();
        let (reply, result) = mpsc::sync_channel(1);
        assert!(worker.process_command(Command::CopyOrForward(modifiers, reply)));
        assert_eq!(result.recv().unwrap().unwrap(), None);
        assert!(records.snapshot().written.is_empty());
        worker.emulator.feed(b"\x1b[?1049h\x1b[>11u");
        let (reply, result) = mpsc::sync_channel(1);
        assert!(worker.process_command(Command::CopyOrForward(modifiers, reply)));
        assert_eq!(result.recv().unwrap().unwrap(), None);
        assert_eq!(records.snapshot().written, expected);
        let mut release = KeyInput::text_input("c");
        release.action = KeyAction::Release;
        release.physical_key = PhysicalKey::C;
        release.unshifted_codepoint = Some('c');
        release.modifiers = modifiers;
        release.text = None;
        assert!(worker.process_command(Command::Key(release)));
        assert_eq!(records.snapshot().written, expected);
        worker.finish();
    }
}

#[cfg(feature = "desktop")]
#[test]
fn unshifted_xterm_control_forms_write_exact_pty_bytes() {
    let (mut worker, records, _requests, _reader, _commands) = clipboard_worker();
    let (events, _events_receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    worker.events = events.into();
    let mut adapter = crate::terminal::testing::test_terminal_key_input_adapter();
    for (chord, expected) in [
        ("ctrl-2", b"\x00".as_slice()),
        ("ctrl-6", b"\x1e".as_slice()),
        ("ctrl-/", b"\x1f".as_slice()),
        ("ctrl-[", b"\x1b".as_slice()),
        ("ctrl-]", b"\x1d".as_slice()),
    ] {
        let before = records.snapshot().written.len();
        let keystroke = gpui::Keystroke::parse(chord).unwrap();
        let press = gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            prefer_character_input: false,
            is_held: false,
        };
        let crate::terminal::KeyTranslation::Encoded(input) = adapter.key_down(&press) else {
            panic!("{chord} must be encoded");
        };
        assert!(worker.process_command(Command::Key(input)));
        let release = gpui::KeyUpEvent { keystroke };
        let crate::terminal::KeyTranslation::Encoded(input) = adapter.key_up(&release) else {
            panic!("{chord} must release its key");
        };
        assert!(worker.process_command(Command::Key(input)));
        assert_eq!(&records.snapshot().written[before..], expected, "{chord}");
    }
    let before = records.snapshot().written.len();
    worker.emulator.feed(b"\x1b[>11u");
    let keystroke = gpui::Keystroke::parse("ctrl-[").unwrap();
    let crate::terminal::KeyTranslation::Encoded(input) = adapter.key_down(&gpui::KeyDownEvent {
        keystroke: keystroke.clone(),
        prefer_character_input: false,
        is_held: false,
    }) else {
        panic!("enhanced Ctrl+[ must be encoded");
    };
    assert!(worker.process_command(Command::Key(input)));
    let crate::terminal::KeyTranslation::Encoded(input) =
        adapter.key_up(&gpui::KeyUpEvent { keystroke })
    else {
        panic!("enhanced Ctrl+[ must release its key");
    };
    assert!(worker.process_command(Command::Key(input)));
    assert_eq!(
        &records.snapshot().written[before..],
        b"\x1b[91;5u\x1b[91;5:3u"
    );
    worker.finish();
}

impl TerminalWorker {
    fn feed_test_output(&mut self, chunks: Vec<Vec<u8>>) -> bool {
        let epoch = self.clipboard.authority.grant();
        self.process_output_chunks(chunks.into_iter().map(|bytes| (bytes, epoch)).collect())
    }
}

#[test]
fn worker_spawn_failure_diagnostics_exclude_native_content() {
    let error = TerminalSessionError::from(io::Error::new(
        ErrorKind::PermissionDenied,
        "native /private/work token=secret",
    ));
    assert_eq!(
        error.to_string(),
        "failed to start the terminal worker thread: PermissionDenied"
    );
    assert_eq!(format!("{error:?}"), "SpawnWorker(PermissionDenied)");
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn paste_confirmation_focus_owns_one_payload_without_terminal_input_authority() {
    let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, _events, _accessibility) = result.unwrap();
    let PasteRequestOutcome::ConfirmationRequired(confirmation) = session
        .request_paste("first\nsecond".to_owned().into())
        .recv_blocking()
        .unwrap()
        .unwrap()
    else {
        panic!("multiline paste requires confirmation")
    };

    session.focus_paste_confirmation(confirmation.id);
    assert_eq!(
        session
            .request_paste("other".to_owned().into())
            .recv_blocking()
            .unwrap(),
        Ok(PasteRequestOutcome::Rejected(
            PasteRejection::TerminalUnfocused
        ))
    );
    assert_eq!(
        session
            .resolve_paste(confirmation.id, PasteDecision::Confirm)
            .recv_blocking()
            .unwrap(),
        Ok(PasteResolution::Written)
    );
    assert_eq!(records.snapshot().written, b"first\rsecond");
    assert_eq!(
        session
            .resolve_paste(confirmation.id, PasteDecision::Confirm)
            .recv_blocking()
            .unwrap(),
        Ok(PasteResolution::Stale)
    );
    assert_eq!(records.snapshot().written, b"first\rsecond");
    session.shutdown();
}

#[test]
fn leaving_paste_confirmation_focus_cancels_even_after_terminal_focus_was_revoked() {
    for refocus in [false, true] {
        let (result, _reader_steps, records) =
            start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, _events, _accessibility) = result.unwrap();
        let PasteRequestOutcome::ConfirmationRequired(confirmation) = session
            .request_paste("first\nsecond".to_owned().into())
            .recv_blocking()
            .unwrap()
            .unwrap()
        else {
            panic!("multiline paste requires confirmation")
        };
        session.focus_paste_confirmation(confirmation.id);
        session.focus(false);
        if refocus {
            session.focus(true);
        }
        assert_eq!(
            session
                .resolve_paste(confirmation.id, PasteDecision::Confirm)
                .recv_blocking()
                .unwrap(),
            Ok(PasteResolution::Stale)
        );
        assert!(records.snapshot().written.is_empty());
        session.shutdown();
    }
}

#[test]
fn paste_confirmation_focus_cannot_claim_a_different_or_cancelled_payload() {
    for cancelled in [false, true] {
        let (result, _reader_steps, records) =
            start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, _events, _accessibility) = result.unwrap();
        let PasteRequestOutcome::ConfirmationRequired(confirmation) = session
            .request_paste("first\nsecond".to_owned().into())
            .recv_blocking()
            .unwrap()
            .unwrap()
        else {
            panic!("multiline paste requires confirmation")
        };
        if cancelled {
            session.focus(false);
        }
        session.focus_paste_confirmation(if cancelled {
            confirmation.id
        } else {
            PasteConfirmationId::new(999)
        });
        assert_eq!(
            session
                .resolve_paste(confirmation.id, PasteDecision::Confirm)
                .recv_blocking()
                .unwrap(),
            Ok(PasteResolution::Stale)
        );
        assert!(records.snapshot().written.is_empty());
        session.shutdown();
    }
}

#[test]
fn metadata_only_screen_changes_do_not_schedule_another_semantic_document() {
    let (mut worker, _records, _requests, _reader, _commands) = clipboard_worker();
    let (accessibility, _receiver) = async_channel::bounded(1);
    worker.accessibility = accessibility;
    let now = Instant::now();
    let input = ScheduleInput::default();
    worker.schedules = WorkerSchedules::new(now, input.clone());
    input.enqueue_accessibility_demand(now);
    worker.schedules.accessibility_demand_received(now);
    worker.emulator.feed(b"unchanged text");
    assert!(worker.publish_screen_snapshot());
    assert!(worker.publish_accessibility(true));
    worker.emulator.mark_metadata_stale();
    assert!(worker.publish_screen_snapshot());
    assert!(
        !worker
            .schedules
            .accessibility_presentation_due(now + Duration::from_secs(1))
    );
    worker.finish();
}

#[test]
fn hidden_server_semantic_demand_completes_without_materializing_a_visual_screen() {
    let (mut worker, _records, _requests, _reader, _commands) = clipboard_worker();
    let (events, event_receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    worker.events = events.into();
    let (accessibility, semantic_receiver) = async_channel::bounded(1);
    worker.accessibility = accessibility;
    let now = Instant::now();
    let input = ScheduleInput::default();
    worker.schedules = WorkerSchedules::new(now, input.clone());
    worker.emulator.feed(b"visible baseline");
    assert!(worker.publish_screen_snapshot());
    while event_receiver.try_recv().is_ok() {}
    let visible_generation = worker.emulator.presentation_generation();
    assert!(worker.process_command(Command::SetPresentable(false)));
    worker
        .emulator
        .feed("hidden semantic row\r\n".repeat(800).as_bytes());
    worker.schedules.request_presentation();
    assert!(input.enqueue_accessibility_demand(now));
    assert!(worker.process_command(Command::AccessibilityDemand));
    let command = worker.order_presentation_barrier(Command::PublishAccessibility);
    assert!(matches!(command, Command::PublishAccessibility));
    assert!(worker.process_command(command));
    for _ in 0..32 {
        if !worker.schedules.take_accessibility_continuation() {
            break;
        }
        let command = worker.order_presentation_barrier(Command::AccessibilityContinue);
        assert!(matches!(command, Command::AccessibilityContinue));
        assert!(worker.process_command(command));
    }
    let model = semantic_receiver.try_recv().unwrap();
    assert!(
        model
            .rows()
            .any(|row| row.text.contains("hidden semantic row"))
    );
    assert!(worker.process_output_chunks(vec![(b"\r\nnew hidden semantic row".to_vec(), None)]));
    assert!(
        worker
            .schedules
            .accessibility_presentation_due(Instant::now() + Duration::from_secs(1))
    );
    let command = worker.order_presentation_barrier(Command::PublishAccessibility);
    assert!(matches!(command, Command::PublishAccessibility));
    assert!(worker.process_command(command));
    for _ in 0..32 {
        if !worker.schedules.take_accessibility_continuation() {
            break;
        }
        let command = worker.order_presentation_barrier(Command::AccessibilityContinue);
        assert!(matches!(command, Command::AccessibilityContinue));
        assert!(worker.process_command(command));
    }
    let changed = semantic_receiver.try_recv().unwrap();
    assert!(
        changed
            .rows()
            .any(|row| row.text.contains("new hidden semantic row"))
    );
    assert!(worker.process_command(Command::EndAccessibilityDemand(Instant::now())));
    assert!(worker.process_output_chunks(vec![(b"\r\nundemanded output".to_vec(), None)]));
    assert!(
        !worker
            .schedules
            .accessibility_presentation_due(Instant::now() + Duration::from_secs(2))
    );
    assert!(!worker.schedules.accessibility_pending());
    assert!(!worker.schedules.is_presentable());
    assert!(
        !worker
            .schedules
            .presentation_due(now + Duration::from_secs(1))
    );
    assert_eq!(
        worker.emulator.presentation_generation(),
        visible_generation
    );
    while let Ok(event) = event_receiver.try_recv() {
        assert!(!matches!(event, TerminalSessionEvent::Screen(_)));
    }
    worker.finish();
}

#[test]
fn hidden_server_semantic_demand_is_enqueued_before_the_visibility_command_runs() {
    let (commands, receiver) = mpsc::channel();
    let session = TerminalRuntime {
        metadata_state: TerminalSessionMetadataState::default(),
        commands: Some(commands),
        worker: None,
        native_pty_close: None,
        cleanup_confirmation: None,
        schedule_input: ScheduleInput::default(),
        event_publisher: async_channel::bounded(1).0.into(),
        clipboard_authority: Arc::default(),
    };
    session.set_presentable(false);
    session.accessibility_demand_sender().unwrap().request();
    assert!(matches!(
        receiver.try_recv(),
        Ok(Command::SetPresentable(false))
    ));
    assert!(matches!(
        receiver.try_recv(),
        Ok(Command::AccessibilityDemand)
    ));
}

#[test]
fn a_hidden_session_reports_retitles_at_most_once_per_interval() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_events, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
    let start = Instant::now();
    let mut worker = TerminalWorker {
        metadata_state: TerminalSessionMetadataState::default(),
        native_pty: direct_native_pty(ScriptedPtyRecords::default()),
        input: PtyInput::default(),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events: events.into(),
        accessibility,
        pending_command: None,
        terminal_input_focused: false,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(start, ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
        permission_requests: PermissionRequestFilter::default(),
        clipboard: WorkerClipboard::default(),
        progress: Default::default(),
    };
    run_due_commands(&mut worker, start);
    assert!(worker.process_command(Command::SetPresentable(false)));
    assert!(worker.feed_test_output(vec![b"\x1b]2;first\x07".to_vec()]));
    // The consumer reads the change and drops its wakeup.
    assert!(matches!(
        receiver.try_recv().unwrap(),
        TerminalSessionEvent::MetadataChanged(_)
    ));
    for title in 0..32 {
        let output = format!("\x1b]2;title {title}\x07");
        assert!(worker.feed_test_output(vec![output.into_bytes()]));
    }
    assert!(
        receiver.try_recv().is_err(),
        "retitles within the interval wait"
    );
    let due = worker.schedules.deadline(None).unwrap();
    assert!(due > start, "the pending retitle is reported later");
    assert!(matches!(
        worker.schedules.take_due(due),
        Some(Command::PublishMetadata)
    ));
    worker.schedules.fix_clock(due);
    assert!(worker.process_command(Command::PublishMetadata));
    assert!(matches!(
        receiver.try_recv().unwrap(),
        TerminalSessionEvent::MetadataChanged(_)
    ));
    assert!(receiver.try_recv().is_err());
    assert_eq!(
        worker
            .metadata_state
            .snapshot()
            .unwrap()
            .title
            .value
            .as_ref(),
        "title 31"
    );
    worker.finish();
}
