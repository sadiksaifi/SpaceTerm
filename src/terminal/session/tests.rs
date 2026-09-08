use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, ErrorKind, Read};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use super::*;
use crate::domain::{RemoteWorkspaceDirectory, SshDestination, WorkspaceDirectoryIdentity};
use crate::platform::native_pty::{NativePtyAdapter, NativePtyAdapterParts, NativePtyTermination};
use crate::ssh::command::{
    RemotePaneShellCommandBuilder, SshCommandContext, ValidatedRemoteLoginShell,
    ValidatedRemoteShellCommand,
};

fn native_terminal_session_factory() -> NativeTerminalSessionFactory {
    NativeTerminalSessionFactory::new(
        Arc::new(RecordingSessionAdapterFactory {
            constructions: mpsc::channel().0,
        }),
        test_launch_planner(),
        LocalFilesystemAuthority::testing(),
        Some("fixture.test".into()),
    )
}

fn remote_pane_command(directory: &RemoteWorkspaceDirectory) -> ValidatedRemoteShellCommand {
    let shell = ValidatedRemoteLoginShell::new("/bin/zsh".to_owned()).unwrap();
    RemotePaneShellCommandBuilder::new(directory, &shell)
        .build()
        .unwrap()
}

#[test]
fn remote_launch_plan_should_preserve_typed_context_and_reject_reused_channels() {
    let local_home = crate::domain::ValidatedWorkspaceDirectory::new(
        PathBuf::from("/Users/local"),
        WorkspaceDirectoryIdentity::for_test(7011),
    );
    let destination = SshDestination::new("user@remote".to_owned()).unwrap();
    let remote_directory = RemoteWorkspaceDirectory::new("~/project".to_owned()).unwrap();
    let prepared = SshCommandContext::new(
        crate::ssh::command::OpenSshExecutable::for_test(),
        PathBuf::from("/private/config/spaceterm/ssh_config"),
        destination.clone(),
        PathBuf::from("/private/runtime/spaceterm/control.sock"),
    )
    .unwrap()
    .prepare_pane_channel(remote_pane_command(&remote_directory));
    let plan = RemoteTerminalLaunchPlan::new(
        local_home.clone(),
        destination.clone(),
        remote_directory.clone(),
        "project on remote".to_owned(),
        prepared.clone(),
    );

    assert_eq!(plan.local_home(), &local_home);
    assert_eq!(plan.destination(), &destination);
    assert_eq!(plan.remote_directory(), &remote_directory);
    assert_eq!(plan.fallback_title(), "project on remote");
    assert_eq!(plan.metadata_context().destination(), &destination);
    let debug = format!("{:?}", TerminalLaunchPlan::Remote(Box::new(plan.clone())));
    assert_eq!(debug, "Remote(RemoteTerminalLaunchPlan { .. })");
    assert!(!debug.contains("user@remote"));
    assert!(!debug.contains("~/project"));
    assert!(!debug.contains("/Users/local"));
    let _consumed = prepared.take().unwrap();

    let error = native_terminal_session_factory()
        .start(test_geometry(), TerminalLaunchPlan::Remote(Box::new(plan)))
        .err();

    assert!(matches!(
        error,
        Some(SessionError::PreparedSshPaneChannel(
            crate::ssh::command::PreparedSshPaneChannelError::AlreadyConsumed
        ))
    ));
}

#[test]
fn native_factory_routes_local_launches_through_injected_factory() {
    let (factory, constructions) = recording_native_terminal_session_factory();
    let working_directory = std::env::temp_dir().join(".");
    let plan = TerminalLaunchPlan::Local(LocalTerminalLaunchPlan::new(
        crate::domain::ValidatedWorkspaceDirectory::new(
            working_directory.clone(),
            WorkspaceDirectoryIdentity::for_test(7011),
        ),
    ));

    let started = factory.start(test_geometry(), plan).unwrap();
    let construction = constructions
        .recv_timeout(Duration::from_secs(1))
        .expect("Local construction should reach the injected factory");
    drop(started.handle);

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
}

#[test]
fn native_factory_routes_remote_launches_through_injected_factory() {
    let (factory, constructions) = recording_native_terminal_session_factory();
    let local_home = std::env::temp_dir();
    let destination = SshDestination::new("user@remote".to_owned()).unwrap();
    let remote_directory = RemoteWorkspaceDirectory::new("~/project".to_owned()).unwrap();
    let context = SshCommandContext::new(
        crate::ssh::command::OpenSshExecutable::for_test(),
        PathBuf::from("/private/config/spaceterm/ssh_config"),
        destination.clone(),
        PathBuf::from("/private/runtime/spaceterm/control.sock"),
    )
    .unwrap();
    let expected_command = context.pane_channel(remote_pane_command(&remote_directory));
    let expected_executable = expected_command.executable().to_owned();
    let expected_arguments = expected_command.arguments().to_vec();
    let plan = TerminalLaunchPlan::Remote(Box::new(RemoteTerminalLaunchPlan::new(
        crate::domain::ValidatedWorkspaceDirectory::new(
            local_home.clone(),
            WorkspaceDirectoryIdentity::for_test(7011),
        ),
        destination,
        remote_directory.clone(),
        "project on remote".to_owned(),
        context.prepare_pane_channel(remote_pane_command(&remote_directory)),
    )));

    let started = factory.start(test_geometry(), plan).unwrap();
    let construction = constructions
        .recv_timeout(Duration::from_secs(1))
        .expect("Remote construction should reach the injected factory");
    drop(started.handle);

    assert_eq!(construction.working_directory, local_home);
    assert_eq!(construction.executable, expected_executable);
    assert_eq!(construction.arguments, expected_arguments);
    assert_eq!(construction.size, pty_size(test_geometry()));
    assert!(construction.inherit_environment);
    assert!(
        construction
            .environment_removals
            .contains(&OsString::from("SPACETERM_SHELL_INTEGRATION_VERSION"))
    );
    assert_eq!(
        construction.environment,
        vec![("TERM".into(), "xterm-256color".into())]
    );
}

#[test]
fn bounded_accessibility_lane_retains_only_the_latest_snapshot() {
    let (sender, receiver) = async_channel::bounded(1);
    let first = Arc::new(TerminalAccessibilityModel::new(
        vec![crate::terminal::AccessibilityLine::new(
            vec![crate::terminal::AccessibilityCell::new("first", 1, false)],
            false,
        )],
        0..1,
        Some((0, 0)),
    ));
    let latest = Arc::new(TerminalAccessibilityModel::new(
        vec![crate::terminal::AccessibilityLine::new(
            vec![crate::terminal::AccessibilityCell::new("latest", 1, false)],
            false,
        )],
        0..1,
        Some((0, 0)),
    ));

    assert!(sender.force_send(first).is_ok());
    assert!(sender.force_send(latest.clone()).is_ok());

    let received = receiver.try_recv().unwrap();
    assert!(Arc::ptr_eq(&received, &latest));
    assert!(receiver.try_recv().is_err());
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
    hidden_input: bool,
    hidden_input_polls: usize,
    take_reader_calls: usize,
    write_attempts: usize,
    written: Vec<u8>,
    flushes: usize,
    resizes: Vec<NativePtySize>,
    waits: usize,
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
    fn hidden_input(&self) -> Result<bool, NativePtyOperationFailure> {
        self.records.update(|state| state.hidden_input_polls += 1);
        Ok(self.records.snapshot().hidden_input)
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
            Some(message) => Err(NativePtyOperationFailure::new(message.clone())),
            None => Ok(()),
        }
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> Result<NativePtyExit, NativePtyWaitFailure> {
        self.records.update(|state| state.waits += 1);
        if self.wait_times_out {
            return Err(NativePtyWaitFailure::new(format!(
                "timed out after {} ms waiting for the scripted shell process to exit",
                timeout.as_millis()
            )));
        }
        match &self.wait_error {
            Some(message) => Err(NativePtyWaitFailure::new(message.clone())),
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
            Some("fixture.test".into()),
        ),
        observed,
    )
}

fn direct_native_pty(records: ScriptedPtyRecords) -> NativePtyOwner {
    NativePtyOwner::from_adapter_parts(
        NativePtyAdapterParts {
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
        },
        Arc::new(DiscardNativePtyOutput),
        &NativePtyCloseHandle::default(),
    )
    .unwrap()
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
        assert_eq!(size, pty_size(test_geometry()));
        if let Some(gate) = &self.startup_gate {
            gate.entered.send(()).unwrap();
            gate.release.lock().unwrap().recv().unwrap();
        }
        self.parts.lock().unwrap().take().ok_or(
            crate::platform::native_pty::NativePtyAdapterConstructionFailure::ResourceCreationFailed,
        )
    }
}

type ScriptedStart = Result<StartedSession, SessionError>;

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
    let result = TerminalSession::start_with(
        test_geometry(),
        Path::new("/scripted"),
        move |size, output, close_handle| {
            NativePtyOwner::start(
                &adapter_factory,
                PreparedShellLaunch::for_test(std::env::temp_dir()),
                size,
                output,
                close_handle,
            )
        },
    );

    (result, reader_steps, records)
}

fn receive_event(
    events: &async_channel::Receiver<SessionEvent>,
    description: &str,
    predicate: impl Fn(&SessionEvent) -> bool + Send + 'static,
) -> SessionEvent {
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
            panic!("session events closed while waiting for {description}")
        }
        Err(error) => {
            drop(waiter);
            panic!("timed out waiting for {description}: {error}")
        }
    }
}

fn screen_text(screen: &ScreenSnapshot) -> String {
    screen
        .rows
        .iter()
        .flat_map(|row| row.iter())
        .map(|cell| cell.text.as_str())
        .collect()
}

#[test]
fn shell_exit_should_preserve_normal_signal_and_shutdown_classifications() {
    assert_eq!(
        classify_native_pty_exit(NativePtyExit::Success),
        SessionExit::Success
    );
    assert_eq!(
        classify_native_pty_exit(NativePtyExit::ExitCode(17)),
        SessionExit::ExitCode(17)
    );
    assert_eq!(
        classify_native_pty_exit(NativePtyExit::Signal("Hangup".to_owned())),
        SessionExit::Signal("Hangup".to_owned())
    );
    assert_eq!(
        classify_native_pty_exit(NativePtyExit::GracefulShutdown),
        SessionExit::GracefulShutdown
    );
    assert_eq!(
        classify_native_pty_exit(NativePtyExit::ForcedShutdown),
        SessionExit::ForcedShutdown
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
        (SessionEvent::Screen(screen), SessionEvent::Exited(status)) => {
            let model = accessibility.try_recv().unwrap();
            assert!(model.text().contains("bounded line 31"));
            assert_eq!(model.generation(), screen.generation);
            (
                screen_text(&screen).contains("bounded line 31"),
                status == SessionExit::Success,
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
        SessionEvent::Screen(screen) if screen_text(&screen).contains("final output")
    ));
    assert!(matches!(exited, SessionEvent::Exited(SessionExit::Success)));
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
        |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains("first row")),
    );
    reader_steps
        .send(ReaderStep::Bytes(b"\r\nsecond row".to_vec()))
        .unwrap();
    let second = receive_event(
        &events,
        "the second row snapshot",
        |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains("second row")),
    );

    let (SessionEvent::Screen(first), SessionEvent::Screen(second)) = (first, second) else {
        unreachable!("the event predicates accept only terminal screens")
    };
    assert!(Arc::ptr_eq(&first.rows[0], &second.rows[0]));
    assert!(!Arc::ptr_eq(&first.rows[1], &second.rows[1]));

    session.shutdown();
}

#[test]
fn session_failure_display_should_explain_each_classification() {
    let failures = [
        SessionFailure::Startup {
            stage: SessionStartupStage::Pty,
            message: "open unavailable".to_owned(),
        },
        SessionFailure::Runtime("write unavailable".to_owned()),
        SessionFailure::PtyRead {
            read_error: "read unavailable".to_owned(),
            exit_status: "exit code 7".to_owned(),
        },
        SessionFailure::ShellWait {
            read_error: Some("read unavailable".to_owned()),
            wait_error: "wait unavailable".to_owned(),
        },
        SessionFailure::ShellWait {
            read_error: None,
            wait_error: "wait unavailable".to_owned(),
        },
    ];

    let statuses = failures.each_ref().map(ToString::to_string);

    assert_eq!(
        statuses,
        [
            "Terminal Session startup failed during PTY creation: open unavailable",
            "Terminal runtime failed: write unavailable",
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

    let (mut session, events, _accessibility) = TerminalSession::start_deferred_with(
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
            SessionEvent::Failed(SessionFailure::Startup {
                stage: SessionStartupStage::Pty,
                ..
            })
        )
    });

    let SessionEvent::Failed(SessionFailure::Startup { message, .. }) = event else {
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
    let (mut session, _events, _accessibility) = TerminalSession::start(
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
fn native_factory_should_report_pty_spawn_failures_through_session_events() {
    let StartedTerminalSession {
        handle: session,
        events,
        accessibility: _,
    } = native_terminal_session_factory()
        .start(
            test_geometry(),
            TerminalLaunchPlan::Local(LocalTerminalLaunchPlan::new(
                crate::domain::ValidatedWorkspaceDirectory::new(
                    PathBuf::from("/private/tmp/spaceterm-missing-session-workspace"),
                    crate::domain::WorkspaceDirectoryIdentity::for_test(0),
                ),
            )),
        )
        .unwrap();

    let event = receive_event(&events, "the native PTY startup failure", |event| {
        matches!(
            event,
            SessionEvent::Failed(SessionFailure::Startup {
                stage: SessionStartupStage::Pty,
                ..
            })
        )
    });

    let SessionEvent::Failed(SessionFailure::Startup { message, .. }) = event else {
        unreachable!("the event predicate accepts only typed PTY startup failures")
    };
    assert_eq!(
        message,
        "Shell launch directory is unavailable; select an existing directory and retry"
    );
    drop(session);
}

#[test]
fn remote_factory_should_report_missing_local_home_without_starting_ssh() {
    let destination = SshDestination::new("user@remote".to_owned()).unwrap();
    let remote_directory = RemoteWorkspaceDirectory::new("~/project".to_owned()).unwrap();
    let prepared = SshCommandContext::new(
        crate::ssh::command::OpenSshExecutable::for_test(),
        PathBuf::from("/private/config/spaceterm/ssh_config"),
        destination.clone(),
        PathBuf::from("/private/runtime/spaceterm/control.sock"),
    )
    .unwrap()
    .prepare_pane_channel(remote_pane_command(&remote_directory));
    let plan = RemoteTerminalLaunchPlan::new(
        crate::domain::ValidatedWorkspaceDirectory::new(
            PathBuf::from("/private/tmp/spaceterm-missing-local-home"),
            WorkspaceDirectoryIdentity::for_test(0),
        ),
        destination,
        remote_directory,
        "project on remote".to_owned(),
        prepared,
    );
    let StartedTerminalSession {
        handle: session,
        events,
        accessibility: _,
    } = native_terminal_session_factory()
        .start(test_geometry(), TerminalLaunchPlan::Remote(Box::new(plan)))
        .unwrap();

    let event = receive_event(&events, "the remote local HOME failure", |event| {
        matches!(
            event,
            SessionEvent::Failed(SessionFailure::Startup {
                stage: SessionStartupStage::Pty,
                ..
            })
        )
    });

    let SessionEvent::Failed(SessionFailure::Startup { message, .. }) = event else {
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
        reader_error: Some("reader unavailable".to_owned()),
        ..ScriptedPtyOptions::default()
    });

    let error = match result {
        Ok(_) => panic!("a missing PTY reader must fail startup"),
        Err(error) => error,
    };
    let state = records.snapshot();

    assert!(matches!(
        error,
        SessionError::EmulatorStartup(message) if message == "reader unavailable"
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
        |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains("scripted output")),
    );

    let SessionEvent::Screen(screen) = event else {
        unreachable!("the event predicate accepts only terminal screens")
    };
    assert!(screen_text(&screen).contains("scripted output"));

    session.shutdown();
    let state = records.wait_for("the scripted session owners to be released", |state| {
        state.pty_drops == 1 && state.reader_drops == 1
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
fn bounded_output_should_preserve_the_control_lane_and_unblock_the_producer() {
    const CHUNK_SIZE: usize = 16 * 1024;
    let (command_tx, commands) = mpsc::channel();
    let (reader_events, reader_event_rx) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let (attempted, attempts) = mpsc::channel();
    let (completed, completions) = mpsc::channel();
    let output = SessionNativePtyOutputSink {
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
        assert!(matches!(commands.recv().unwrap(), Command::ReaderReady));
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
fn key_actions_should_retain_fifo_order_in_the_reliable_command_lane() {
    let (commands, receiver) = mpsc::channel();
    for action in [KeyAction::Press, KeyAction::Repeat, KeyAction::Release] {
        commands
            .send(Command::Key(KeyInput {
                action,
                physical_key: PhysicalKey::A,
                native_key_code: Some(0),
                logical_key: "a".to_owned(),
                text: Some("a".to_owned()),
                unshifted_codepoint: Some('a'),
                modifiers: InputModifiers::default(),
                consumed_modifiers: InputModifiers::default(),
                option_as_alt: OptionAsAltPolicy::default(),
            }))
            .unwrap();
    }

    let actions = [receiver.recv(), receiver.recv(), receiver.recv()].map(|command| {
        let Command::Key(input) = command.unwrap() else {
            panic!("the command lane should contain only typed key input")
        };
        input.action
    });

    assert_eq!(
        actions,
        [KeyAction::Press, KeyAction::Repeat, KeyAction::Release]
    );
}

#[test]
fn held_keys_track_only_terminal_routed_keys_and_modifiers() {
    let mut held = HeldKeys::default();
    held.route(&text_key(KeyAction::Press));
    held.route(&modifier_key(KeyAction::Press));
    held.route(&text_key(KeyAction::Release));

    let releases = held.take_releases();

    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].physical_key, PhysicalKey::ShiftLeft);
    assert_eq!(releases[0].action, KeyAction::Release);
    assert!(held.take_releases().is_empty());

    let application_shortcut_never_routed = HeldKeys::default();
    assert!(application_shortcut_never_routed.held.is_empty());
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

    reader_steps
        .send(ReaderStep::Bytes(b"\x1b[?1004l".to_vec()))
        .unwrap();
    session.resize(geometry(82, 24, 8.0, 20.0));
    records.wait_for("the focus-reporting disable barrier", |state| {
        state.resizes.len() == 3
    });
    session.focus(false);
    session.resize(geometry(83, 24, 8.0, 20.0));
    records.wait_for("the disabled focus edge barrier", |state| {
        state.resizes.len() == 4
    });
    assert_eq!(records.snapshot().written, b"\x1b[O\x1b[I");

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
        .send(ReaderStep::Bytes(b"selected".to_vec()))
        .unwrap();
    let SessionEvent::Screen(screen) = receive_event(
        &events,
        "the selectable terminal output after the held key press",
        |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains("selected")),
    ) else {
        unreachable!()
    };
    let pointer = |phase, position| PointerInput {
        generation: screen.generation,
        phase,
        button: (phase != PointerPhase::Motion).then_some(PointerButton::Left),
        position,
        modifiers: InputModifiers::default(),
        shift_selection: ShiftSelectionPolicy::default(),
    };
    session.pointer(pointer(
        PointerPhase::Press,
        SurfacePosition { x: 1.0, y: 1.0 },
    ));
    session.pointer(pointer(
        PointerPhase::Motion,
        SurfacePosition { x: 63.0, y: 1.0 },
    ));
    session.pointer(pointer(
        PointerPhase::Release,
        SurfacePosition { x: 63.0, y: 1.0 },
    ));
    assert_eq!(
        session.copy_selection().unwrap().unwrap().plain_text,
        "selected"
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
    assert_eq!(
        session.copy_selection().unwrap().unwrap().plain_text,
        "selected"
    );

    session.key(text_key(KeyAction::Press));
    assert_eq!(session.copy_selection().unwrap(), None);
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
            native_pty: direct_native_pty(records.clone()),
            emulator: TerminalEmulator::new(test_geometry()).unwrap(),
            commands,
            reader_events,
            events,
            accessibility,
            pending_command: None,
            terminal_input_focused: true,
            focus_reporting_enabled: false,
            held_keys: HeldKeys::default(),
            schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
            osc52_filter: Osc52Filter::default(),
        };
        let output = [b"\x1b[?1004h".as_slice(), operation, b"\x1b[5n"].concat();

        assert!(worker.process_output_chunks(vec![output]));
        assert_eq!(records.snapshot().written, b"\x1b[I\x1b[0n");
        worker.finish();
    }
}

#[test]
fn osc52_is_discarded_without_replies_and_later_terminal_output_remains_ordered() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        native_pty: direct_native_pty(records.clone()),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events,
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
    };

    assert!(worker.process_output_chunks(vec![
        b"before\x1b]52;c;?\x07".to_vec(),
        b"\x1b]52;c;c2VjcmV0\x1b\\".to_vec(),
    ]));
    assert!(records.snapshot().written.is_empty());
    let SessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
        panic!("denied clipboard operations must only publish the ordinary screen");
    };
    assert!(screen_text(&screen).contains("before"));
    assert!(receiver.try_recv().is_err());

    assert!(worker.process_output_chunks(vec![b"after\x1b[5n\r\nlater\x1b[6n".to_vec(),]));
    assert_eq!(records.snapshot().written, b"\x1b[0n\x1b[2;6R");
    assert!(worker.publish_screen());
    let SessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
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

#[test]
fn consecutive_output_chunks_should_publish_one_ordered_coalesced_screen() {
    let (command_tx, commands) = mpsc::channel();
    let (reader_events, reader_event_rx) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let output = SessionNativePtyOutputSink {
        commands: command_tx,
        events: reader_events,
    };
    assert!(output.publish(NativePtyOutput::Bytes(b"first".to_vec())));
    assert!(output.publish(NativePtyOutput::Bytes(b" second".to_vec())));
    assert!(matches!(commands.recv().unwrap(), Command::ReaderReady));

    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        native_pty: direct_native_pty(records),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events: reader_event_rx,
        events,
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
    };

    assert!(worker.process_reader_events());
    assert!(worker.pending_command.is_none());
    let SessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
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
fn rapid_output_coalesces_before_screen_and_accessibility_construction() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        native_pty: direct_native_pty(records.clone()),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events,
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
    };
    worker
        .schedules
        .mark_presented(Instant::now() + Duration::from_secs(1));

    for index in 0..64 {
        assert!(worker.process_output_chunks(vec![format!("line {index}\r\n").into_bytes()]));
    }
    assert!(worker.process_output_chunks(vec![b"\x1b[5n".to_vec()]));

    assert_eq!(records.snapshot().written, b"\x1b[0n");
    assert!(receiver.try_recv().is_err());
    assert!(accessibility_receiver.try_recv().is_err());
    assert!(worker.schedules.take_presentation_barrier());
    assert!(worker.publish_screen());
    let SessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
        panic!("the coalesced presentation must publish one Screen")
    };
    assert!(screen_text(&screen).contains("line 63"));
    assert!(receiver.try_recv().is_err());
    assert!(accessibility_receiver.try_recv().is_err());
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
        native_pty: direct_native_pty(records),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events,
        accessibility,
        pending_command: Some(Command::Key(text_key(KeyAction::Press))),
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
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
fn accessibility_demand_flushes_a_pending_screen_before_binding_its_model() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, accessibility_receiver) = async_channel::bounded(1);
    let schedule_input = ScheduleInput::default();
    let mut worker = TerminalWorker {
        native_pty: direct_native_pty(records),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events,
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), schedule_input.clone()),
        osc52_filter: Osc52Filter::default(),
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
    let SessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
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
        native_pty: direct_native_pty(records),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events,
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
    };
    worker.schedules.set_presentable(false, Instant::now());

    assert!(worker.process_output_chunks(vec![b"hidden one\r\n".to_vec()]));
    assert!(worker.process_output_chunks(vec![b"hidden latest".to_vec()]));
    assert!(receiver.try_recv().is_err());
    assert!(accessibility_receiver.try_recv().is_err());

    assert!(worker.process_command(Command::SetPresentable(true)));
    let SessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
        panic!("restoration must publish the latest hidden state")
    };
    assert!(screen_text(&screen).contains("hidden latest"));
    assert!(matches!(
        worker.receive_next_command(),
        Some(Command::PublishAccessibility)
    ));
    assert!(worker.process_command(Command::PublishAccessibility));
    while accessibility_receiver.is_empty() {
        let Some(command) = worker.take_accessibility_continuation() else {
            panic!("restored accessibility construction must continue to completion")
        };
        assert!(worker.process_command(command));
    }
    assert!(accessibility_receiver.try_recv().is_ok());
    worker.finish();
}

#[test]
fn synchronized_output_between_accessibility_chunks_preserves_the_eager_seed() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        native_pty: direct_native_pty(records),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events,
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
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
    while accessibility_receiver.is_empty() {
        let Some(command) = worker.take_accessibility_continuation() else {
            panic!("the eager accessibility seed must survive synchronized output")
        };
        assert!(worker.process_command(command));
    }
    worker.finish();
}

#[test]
fn restoring_visibility_restarts_an_interrupted_accessibility_update_without_output() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        native_pty: direct_native_pty(records),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events,
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
    };
    worker.emulator.feed(b"visible state");
    assert!(worker.publish_screen());
    let _ = receiver.try_recv().unwrap();
    assert!(worker.process_command(Command::PublishAccessibility));
    assert!(worker.schedules.accessibility_pending());

    assert!(worker.process_command(Command::SetPresentable(false)));
    assert!(!worker.schedules.accessibility_pending());
    assert!(worker.process_command(Command::SetPresentable(true)));
    assert!(receiver.try_recv().is_err());
    assert!(matches!(
        worker.receive_next_command(),
        Some(Command::PublishAccessibility)
    ));
    assert!(worker.process_command(Command::PublishAccessibility));
    while accessibility_receiver.is_empty() {
        let Some(command) = worker.take_accessibility_continuation() else {
            panic!("restoration must restart the interrupted accessibility update")
        };
        assert!(worker.process_command(command));
    }
    worker.finish();
}

#[test]
fn closed_screen_lane_stops_before_snapshot_or_accessibility_construction() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_tx, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(1);
    let (accessibility, accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        native_pty: direct_native_pty(records),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events,
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
    };
    worker.emulator.feed(b"unobserved");
    let generation = worker.emulator.presentation_generation();
    drop(receiver);

    assert!(!worker.publish_screen());
    assert_eq!(worker.emulator.presentation_generation(), generation);
    assert!(accessibility_receiver.try_recv().is_err());
    worker.finish();
}

#[test]
fn synchronized_output_deadline_should_publish_only_after_output_stalls() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_events, reader_event_rx) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        native_pty: direct_native_pty(records),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events: reader_event_rx,
        events,
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
    };
    assert!(worker.publish_screen());
    let _ = receiver.try_recv().unwrap();

    let started = Instant::now();
    worker.emulator.feed_at(b"\x1b[?2026hlong", started);
    assert!(worker.publish_screen());
    assert!(receiver.try_recv().is_err());

    let progressed = started + Duration::from_millis(900);
    worker.emulator.feed_at(b" remote redraw", progressed);
    assert!(worker.release_synchronized_output_if_due(started + MAX_SYNCHRONIZED_OUTPUT_DURATION));
    assert!(
        receiver.try_recv().is_err(),
        "an active remote redraw must remain atomic after one second of total duration"
    );

    assert!(
        worker.release_synchronized_output_if_due(progressed + MAX_SYNCHRONIZED_OUTPUT_DURATION)
    );
    let SessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
        panic!("the synchronized-output deadline must publish a screen")
    };
    assert!(screen_text(&screen).contains("long remote redraw"));
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
        native_pty: direct_native_pty(records),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events: reader_event_rx,
        events,
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
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
    let SessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
        panic!("restoration must publish the completed synchronized redraw")
    };
    assert!(screen_text(&screen).contains("hidden redraw"));
    assert!(receiver.try_recv().is_err());
    worker.finish();
}

#[test]
fn output_control_output_should_preserve_screen_order_through_the_session_interface() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();

    reader_steps
        .send(ReaderStep::Bytes(b"first".to_vec()))
        .unwrap();
    let first = receive_event(
        &events,
        "the first output Screen",
        |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains("first")),
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
        |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains("first second")),
    );

    let (SessionEvent::Screen(first), SessionEvent::Screen(second)) = (first, second) else {
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
fn copy_selection_at_rejects_a_stale_menu_generation_when_the_worker_is_ahead() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();
    reader_steps
        .send(ReaderStep::Bytes(b"selected".to_vec()))
        .unwrap();
    let SessionEvent::Screen(screen) = receive_event(
        &events,
        "the selectable terminal output",
        |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains("selected")),
    ) else {
        unreachable!()
    };
    let pointer = |phase, x| PointerInput {
        generation: screen.generation,
        phase,
        button: (phase != PointerPhase::Motion).then_some(PointerButton::Left),
        position: SurfacePosition { x, y: 1.0 },
        modifiers: InputModifiers::default(),
        shift_selection: ShiftSelectionPolicy::default(),
    };
    session.pointer(pointer(PointerPhase::Press, 1.0));
    session.pointer(pointer(PointerPhase::Motion, 63.0));
    assert_eq!(
        session
            .pointer_and_copy_selection(pointer(PointerPhase::Release, 63.0))
            .unwrap()
            .unwrap()
            .plain_text,
        "selected",
    );
    let mut menu_generation = screen.generation;
    while let Ok(event) = events.try_recv() {
        if let SessionEvent::Screen(screen) = event {
            menu_generation = screen.generation;
        }
    }
    assert_eq!(
        session
            .copy_selection_at(menu_generation)
            .unwrap()
            .unwrap()
            .plain_text,
        "selected"
    );

    // Keep the UI's snapshot unread while terminal output advances the worker.
    reader_steps
        .send(ReaderStep::Bytes(b"\r\nnew\x1b[6n".to_vec()))
        .unwrap();
    records.wait_for("the worker to process later output", |state| {
        !state.written.is_empty()
    });
    assert_eq!(session.copy_selection_at(menu_generation).unwrap(), None);
    assert_eq!(
        session.copy_selection().unwrap().unwrap().plain_text,
        "selected"
    );

    let SessionEvent::Screen(current) = receive_event(
        &events,
        "the newer worker presentation",
        |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains("new")),
    ) else {
        unreachable!()
    };
    assert!(current.generation > menu_generation);
    assert_eq!(
        session
            .copy_selection_at(current.generation)
            .unwrap()
            .unwrap()
            .plain_text,
        "selected"
    );
    // A synchronized transaction retains its published generation while hiding mutations.
    let replies = records.snapshot().written.len();
    reader_steps
        .send(ReaderStep::Bytes(
            b"\x1b[?2026h\r\ntransaction\x1b[6n".to_vec(),
        ))
        .unwrap();
    records.wait_for(
        "the worker to process a synchronized transaction",
        |state| state.written.len() > replies,
    );
    assert_eq!(session.copy_selection_at(current.generation).unwrap(), None);
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
fn pointer_release_copy_should_observe_the_completed_selection_atomically() {
    let (result, reader_steps, _records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();
    reader_steps
        .send(ReaderStep::Bytes(b"selected".to_vec()))
        .unwrap();
    let SessionEvent::Screen(screen) = receive_event(
        &events,
        "the selectable terminal output",
        |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains("selected")),
    ) else {
        unreachable!()
    };
    let pointer = |phase, position| PointerInput {
        generation: screen.generation,
        phase,
        button: (phase != PointerPhase::Motion).then_some(PointerButton::Left),
        position,
        modifiers: InputModifiers::default(),
        shift_selection: ShiftSelectionPolicy::default(),
    };

    session.pointer(pointer(
        PointerPhase::Press,
        SurfacePosition { x: 1.0, y: 1.0 },
    ));
    session.pointer(pointer(
        PointerPhase::Motion,
        SurfacePosition { x: 63.0, y: 1.0 },
    ));
    let copy = session
        .pointer_and_copy_selection(pointer(
            PointerPhase::Release,
            SurfacePosition { x: 63.0, y: 1.0 },
        ))
        .unwrap()
        .unwrap();

    assert_eq!(copy.plain_text, "selected");
    session.shutdown();
}

#[test]
fn application_mouse_release_should_not_return_a_selection_copy() {
    let (result, reader_steps, _records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();
    reader_steps
        .send(ReaderStep::Bytes(
            b"selected\x1b[?1000h\x1b[?1006h".to_vec(),
        ))
        .unwrap();
    let SessionEvent::Screen(screen) = receive_event(
        &events,
        "the mouse-tracking terminal screen",
        |event| matches!(event, SessionEvent::Screen(screen) if screen.mouse_tracking),
    ) else {
        unreachable!()
    };
    let pointer = |phase| PointerInput {
        generation: screen.generation,
        phase,
        button: Some(PointerButton::Left),
        position: SurfacePosition { x: 1.0, y: 1.0 },
        modifiers: InputModifiers::default(),
        shift_selection: ShiftSelectionPolicy::default(),
    };

    session.pointer(pointer(PointerPhase::Press));
    let copy = session
        .pointer_and_copy_selection(pointer(PointerPhase::Release))
        .unwrap();

    assert_eq!(copy, None);
    session.shutdown();
}

#[test]
fn resize_should_reach_the_pty_with_pixel_dimensions() {
    let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, _events, _accessibility) = result.unwrap();
    let resized = geometry(100, 30, 9.0, 21.0);

    session.resize(resized);
    let state = records.wait_for("the scripted PTY resize", |state| state.resizes.len() == 1);

    assert_eq!(state.resizes, vec![pty_size(resized)]);
    session.shutdown();
}

#[test]
fn pixel_only_resize_should_reach_the_pty_without_publishing_a_grid_screen() {
    let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();
    let initial = receive_event(&events, "the initial terminal screen", |event| {
        matches!(event, SessionEvent::Screen(_))
    });
    let SessionEvent::Screen(initial) = initial else {
        unreachable!()
    };
    let grid = test_geometry().grid();
    let resized = geometry(grid.cols, grid.rows, 9.0, 21.0);

    session.resize(resized);
    assert!(session.copy_selection().is_ok());

    assert_eq!(records.snapshot().resizes, vec![pty_size(resized)]);
    assert!(events.try_recv().is_err());
    assert_eq!(initial.size.cols, grid.cols);
    assert_eq!(initial.size.rows, grid.rows);
    session.shutdown();
}

#[test]
fn fractional_backing_geometry_should_reach_the_pty_without_per_cell_rounding() {
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

    assert_eq!(
        state.resizes,
        vec![NativePtySize {
            rows: 2,
            columns: 10,
            pixel_width: 113,
            pixel_height: 60,
        }]
    );
    session.shutdown();
}

#[test]
fn rapid_resizes_should_queue_one_notification_and_retain_only_the_latest_geometry() {
    let (commands, receiver) = mpsc::channel();
    let schedule_input = ScheduleInput::default();
    let mut schedules = WorkerSchedules::new(Instant::now(), schedule_input.clone());
    let mut session = TerminalSession {
        commands: Some(commands),
        worker: None,
        native_pty_close: None,
        schedule_input,
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
fn rapid_find_queries_should_queue_one_notification_and_retain_only_the_latest_query() {
    let (commands, receiver) = mpsc::channel();
    let schedule_input = ScheduleInput::default();
    let mut schedules = WorkerSchedules::new(Instant::now(), schedule_input.clone());
    let mut session = TerminalSession {
        commands: Some(commands),
        worker: None,
        native_pty_close: None,
        schedule_input,
    };

    session.set_find_query(FindQueryGeneration::test(1), "n".to_owned());
    session.set_find_query(FindQueryGeneration::test(2), "needle".to_owned());

    assert!(matches!(receiver.try_recv(), Ok(Command::FindQueryChanged)));
    assert!(matches!(
        schedules.take_find_query(),
        Some(FindQueryUpdate::Set(generation, query))
            if generation == FindQueryGeneration::test(2) && query == "needle"
    ));
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    session.shutdown();
}

#[test]
fn find_close_should_supersede_a_pending_query_update() {
    let (commands, receiver) = mpsc::channel();
    let schedule_input = ScheduleInput::default();
    let mut schedules = WorkerSchedules::new(Instant::now(), schedule_input.clone());
    let mut session = TerminalSession {
        commands: Some(commands),
        worker: None,
        native_pty_close: None,
        schedule_input,
    };

    session.set_find_query(FindQueryGeneration::test(1), "needle".to_owned());
    session.end_find(FindQueryGeneration::test(2));

    assert!(matches!(receiver.try_recv(), Ok(Command::FindQueryChanged)));
    assert!(matches!(
        schedules.take_find_query(),
        Some(FindQueryUpdate::End(generation))
            if generation == FindQueryGeneration::test(2)
    ));
    session.shutdown();
}

#[test]
fn pending_pty_responses_should_precede_later_input_through_the_session_interface() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();
    let resized = geometry(20, 4, 8.0, 18.0);

    reader_steps
        .send(ReaderStep::Bytes(b"\x1b[?2048hX".to_vec()))
        .unwrap();
    receive_event(
        &events,
        "the mode-setting terminal output",
        |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains('X')),
    );
    session.resize(resized);
    records.wait_for("the in-band terminal resize response", |state| {
        state.written == b"\x1b[48;4;20;72;160t"
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

    assert_eq!(state.written, b"\x1b[48;4;20;72;160tlater");
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
        |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains('X')),
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
        |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains('X')),
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
    let _ = session.copy_selection();
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
        write_error: Some("write unavailable".to_owned()),
        ..ScriptedPtyOptions::default()
    });
    let (mut session, events, _accessibility) = result.unwrap();

    let _ = session
        .request_paste("input".to_owned().into())
        .recv_blocking();
    let event = receive_event(&events, "the PTY write failure", |event| {
        matches!(event, SessionEvent::Failed(_))
    });

    let SessionEvent::Failed(failure) = event else {
        unreachable!("the event predicate accepts only terminal failures")
    };
    assert_eq!(
        failure,
        SessionFailure::Runtime("failed to write to the shell PTY: write unavailable".to_owned())
    );
    let state = records.wait_for("the failed PTY worker to release ownership", |state| {
        state.pty_drops == 1
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
fn reader_error_with_successful_wait_should_emit_a_pty_read_failure() {
    let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
        exit_code: 7,
        ..ScriptedPtyOptions::default()
    });
    let (mut session, events, _accessibility) = result.unwrap();

    reader_steps
        .send(ReaderStep::Error("read unavailable".to_owned()))
        .unwrap();
    let event = receive_event(&events, "the PTY read failure", |event| {
        matches!(event, SessionEvent::Failed(_))
    });

    let SessionEvent::Failed(SessionFailure::PtyRead {
        read_error,
        exit_status,
    }) = event
    else {
        panic!("a read error followed by a successful wait must be classified as PtyRead")
    };
    assert_eq!(read_error, "read unavailable");
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
        .send(ReaderStep::Error("read unavailable".to_owned()))
        .unwrap();
    let event = receive_event(&events, "the PTY read and wait failure", |event| {
        matches!(event, SessionEvent::Failed(_))
    });

    let SessionEvent::Failed(failure) = event else {
        unreachable!("the event predicate accepts only terminal failures")
    };
    assert_eq!(
        failure,
        SessionFailure::ShellWait {
            read_error: Some("read unavailable".to_owned()),
            wait_error: "wait unavailable".to_owned(),
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
        matches!(event, SessionEvent::Failed(_))
    });

    let SessionEvent::Failed(failure) = event else {
        unreachable!("the event predicate accepts only terminal failures")
    };
    assert_eq!(
        failure,
        SessionFailure::ShellWait {
            read_error: None,
            wait_error: "wait unavailable".to_owned(),
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
        matches!(event, SessionEvent::Failed(_))
    });

    let SessionEvent::Failed(SessionFailure::ShellWait {
        read_error,
        wait_error,
    }) = event
    else {
        panic!("a child wait timeout must be classified as ShellWait")
    };
    assert_eq!(read_error, None);
    assert_eq!(
        wait_error,
        "timed out after 2000 ms waiting for the scripted shell process to exit"
    );
    let state = records.wait_for("the timed-out PTY worker to release ownership", |state| {
        state.pty_drops == 1 && state.reader_drops == 1
    });
    assert_eq!(
        (state.waits, state.pty_drops, state.reader_drops),
        (1, 1, 1)
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
        matches!(event, SessionEvent::Exited(_))
    });

    assert!(matches!(
        event,
        SessionEvent::Exited(SessionExit::ExitCode(7))
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
        state.pty_drops == 1
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
        state.pty_drops == 1
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
    let session = TerminalSession {
        commands: Some(commands),
        worker: None,
        native_pty_close: None,
        schedule_input: schedule_input.clone(),
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

    session.set_presentable(false);
    assert!(matches!(
        receiver.try_recv(),
        Ok(Command::SetPresentable(false))
    ));
    sender.request();
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    session.set_presentable(true);
    assert!(matches!(
        receiver.try_recv(),
        Ok(Command::SetPresentable(true))
    ));
    sender.request();
    assert!(matches!(
        receiver.try_recv(),
        Ok(Command::AccessibilityDemand)
    ));
}

#[test]
fn accessibility_selection_authority_uses_the_reliable_worker_command_lane() {
    let (_command_tx, commands) = mpsc::channel();
    let (_reader_events, reader_events) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
    let records = ScriptedPtyRecords::default();
    let (events, _event_receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
    let (accessibility, _accessibility_receiver) = async_channel::bounded(1);
    let mut worker = TerminalWorker {
        native_pty: direct_native_pty(records.clone()),
        emulator: TerminalEmulator::new(test_geometry()).unwrap(),
        commands,
        reader_events,
        events,
        accessibility,
        pending_command: None,
        terminal_input_focused: true,
        focus_reporting_enabled: false,
        held_keys: HeldKeys::default(),
        schedules: WorkerSchedules::new(Instant::now(), ScheduleInput::default()),
        osc52_filter: Osc52Filter::default(),
    };
    worker.emulator.feed("a😀b".as_bytes());
    let _ = worker.emulator.snapshot().unwrap();
    let (mut model, mut more) = worker
        .emulator
        .accessibility_snapshot_for_current_presentation()
        .unwrap();
    while more {
        (model, more) = worker.emulator.accessibility_snapshot(false).unwrap();
    }
    let request = model.unwrap().selection_request(2..3).unwrap();
    assert_eq!(request.range, 1..3);
    let (commands, receiver) = mpsc::channel();
    worker.commands = receiver;
    let session = TerminalSession {
        commands: Some(commands),
        worker: None,
        native_pty_close: None,
        schedule_input: ScheduleInput::default(),
    };
    let handle: &dyn TerminalSessionHandle = &session;
    let sender = handle.accessibility_selection_sender().unwrap();
    session.focus(false);
    sender.request(request.clone());
    assert!(
        worker
            .emulator
            .selection_copy(SelectionCopyOptions::default())
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        worker.commands.try_recv().unwrap(),
        Command::Focus(false)
    ));
    let command = worker.commands.try_recv().unwrap();
    assert!(matches!(&command, Command::AccessibilitySelection(actual) if actual == &request));
    assert!(worker.process_command(command));
    assert_eq!(
        worker
            .emulator
            .selection_copy(SelectionCopyOptions::default())
            .unwrap()
            .unwrap()
            .plain_text,
        "😀"
    );
    assert!(records.snapshot().written.is_empty());
}

#[test]
fn accessibility_selection_rejects_a_model_from_before_the_latest_screen() {
    let mut emulator = TerminalEmulator::new(test_geometry()).unwrap();
    emulator.feed(b"old text");
    let _ = emulator.snapshot().unwrap();
    let (mut model, mut more) = emulator
        .accessibility_snapshot_for_current_presentation()
        .unwrap();
    while more {
        (model, more) = emulator.accessibility_snapshot(false).unwrap();
    }
    let request = model.unwrap().selection_request(0..3).unwrap();

    emulator.feed(b" changed");
    let _ = emulator.snapshot().unwrap();
    let action = emulator.set_accessibility_selection(request).unwrap();

    assert!(!action.screen_changed);
    assert!(
        emulator
            .selection_copy(SelectionCopyOptions::default())
            .unwrap()
            .is_none()
    );
}

#[test]
fn accessibility_selection_authority_is_inert_after_worker_shutdown() {
    let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, _events, _accessibility) = result.unwrap();
    let sender = session.accessibility_selection_sender().unwrap();
    let model = TerminalAccessibilityModel::from_screen(&ScreenSnapshot::empty(
        crate::local_path::LocalPathSemantics::Posix,
    ));
    let request = model.selection_request(0..0).unwrap();
    session.shutdown_and_join();
    let writes = records.snapshot().written;
    sender.request(request);
    assert!(session.accessibility_selection_sender().is_none());
    assert_eq!(records.snapshot().written, writes);
}

#[test]
fn stopped_session_returns_an_error_for_selection_requests() {
    let session = TerminalSession {
        commands: None,
        worker: None,
        native_pty_close: None,
        schedule_input: ScheduleInput::default(),
    };

    assert_eq!(
        session.copy_selection(),
        Err(SelectionCopyError::WorkerStopped)
    );
}

#[test]
fn worker_autoscroll_ticks_publish_scrollback_without_more_pointer_motion() {
    let (result, reader_steps, _records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = result.unwrap();
    let mut output = Vec::new();
    for row in 0..30 {
        output.extend_from_slice(format!("row {row:02}\r\n").as_bytes());
    }
    reader_steps.send(ReaderStep::Bytes(output)).unwrap();
    let SessionEvent::Screen(bottom) = receive_event(
        &events,
        "scrollback at the bottom",
        |event| matches!(event, SessionEvent::Screen(screen) if screen.scrollbar.total_rows > screen.scrollbar.visible_rows),
    ) else {
        unreachable!()
    };
    let bottom_offset = bottom.scrollbar.offset_rows;
    let pointer = |phase, position, generation| PointerInput {
        generation,
        phase,
        button: (phase != PointerPhase::Motion).then_some(PointerButton::Left),
        position,
        modifiers: InputModifiers::default(),
        shift_selection: ShiftSelectionPolicy::default(),
    };
    session.pointer(pointer(
        PointerPhase::Press,
        SurfacePosition { x: 1.0, y: 470.0 },
        bottom.generation,
    ));
    session.pointer(pointer(
        PointerPhase::Motion,
        SurfacePosition { x: 1.0, y: -1.0 },
        bottom.generation,
    ));

    let SessionEvent::Screen(autoscrolled) = receive_event(
        &events,
        "worker-driven selection autoscroll",
        move |event| matches!(event, SessionEvent::Screen(screen) if screen.scrollbar.offset_rows < bottom_offset),
    ) else {
        unreachable!()
    };
    session.pointer(pointer(
        PointerPhase::Release,
        SurfacePosition { x: 1.0, y: -1.0 },
        autoscrolled.generation,
    ));
    session.shutdown();
}

#[test]
fn hidden_input_transitions_are_reported_on_output_and_focus_during_idle_backoff() {
    let (started, reader, records) = start_scripted_session(ScriptedPtyOptions::default());
    let (mut session, events, _accessibility) = started.unwrap();
    records.wait_for("initial hidden-input poll", |state| {
        state.hidden_input_polls > 0
    });
    records.update(|state| state.hidden_input = true);
    reader
        .send(ReaderStep::Bytes(b"Password: ".to_vec()))
        .unwrap();
    receive_event(&events, "password prompt", |event| {
        matches!(event, SessionEvent::HiddenInputChanged(true))
    });
    records.update(|state| state.hidden_input = false);
    session.focus(false);
    receive_event(&events, "focus transition", |event| {
        matches!(event, SessionEvent::HiddenInputChanged(false))
    });
    records.update(|state| state.hidden_input = true);
    session.focus(true);
    receive_event(&events, "focus restoration", |event| {
        matches!(event, SessionEvent::HiddenInputChanged(true))
    });
    session.shutdown_and_join();
}
