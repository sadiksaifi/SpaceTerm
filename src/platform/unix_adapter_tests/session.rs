//! Native Adapter integration evidence.
use super::*;
use crate::terminal::key::KeyAction;
fn native_pty_adapter_factory() -> Arc<dyn NativePtyAdapterFactory> {
    Arc::new(crate::platform::unix_pty::test_factory())
}
fn test_geometry() -> TerminalGeometry {
    super::tests::test_geometry()
}
fn test_launch_planner(startup: &std::path::Path) -> ShellLaunchPlanner {
    ShellLaunchPlanner::for_test(
        "/bin/zsh".into(),
        crate::platform::launch_host::resource_root(),
    )
    .with_environment(
        crate::platform::shell_integration::ShellIntegrationMode::Automatic,
        crate::platform::shell_integration::ShellEnvironment {
            // Retain real integration while excluding the user's prompt plugins and login hooks.
            zdotdir: Some(startup.as_os_str().to_owned()),
            ..Default::default()
        },
    )
}
// Login startup can replace the line editor and discard input sent before the first prompt.
// Wait for the shell integration's observed command-input boundary, not an arbitrary delay.
fn wait_for_shell_prompt(
    session: &TerminalSession,
    events: &async_channel::Receiver<SessionEvent>,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if session.metadata_snapshot().is_some_and(|metadata| {
            metadata.prompt_zone == crate::terminal::metadata::PromptZone::CommandInput
        }) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "shell did not publish its first input prompt"
        );
        match events.try_recv() {
            Ok(SessionEvent::Failed(failure)) => {
                panic!("terminal session failed before prompt: {failure}")
            }
            Ok(SessionEvent::Exited(_)) | Err(async_channel::TryRecvError::Closed) => {
                panic!("shell ended before its first prompt")
            }
            Ok(_) => {}
            Err(async_channel::TryRecvError::Empty) => thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn enter_command(session: &TerminalSession, command: &str) {
    assert_eq!(
        session
            .request_paste(command.to_owned().into())
            .recv_blocking()
            .unwrap(),
        Ok(PasteRequestOutcome::Written)
    );
    session.key(KeyInput {
        action: KeyAction::Press,
        physical_key: PhysicalKey::Enter,
        native_key_code: None,
        logical_key: "enter".into(),
        text: None,
        unshifted_codepoint: None,
        modifiers: InputModifiers::default(),
        consumed_modifiers: InputModifiers::default(),
        option_as_alt: OptionAsAltPolicy::default(),
    });
}

#[test]
fn real_shell_output_round_trips_through_the_pty_and_emulator() {
    let _isolation = crate::platform::unix_pty::lock_real_pty_test();
    if crate::platform::unix_pty::isolate_real_pty_test(
        "terminal::session::unix_adapter_tests::real_shell_output_round_trips_through_the_pty_and_emulator",
    ) {
        return;
    }
    let startup = ShellStartupDirectory::new();
    let size = test_geometry();
    let (session, events, _accessibility) = TerminalSession::start(
        native_pty_adapter_factory(),
        test_launch_planner(startup.path()),
        size,
        &std::env::current_dir().unwrap(),
        Some("fixture.test"),
        LocalFilesystemAuthority::testing(),
    )
    .unwrap();
    let session = JoinedRealPtySession(session);
    wait_for_shell_prompt(&session, &events);

    // The command renders a red X. The echoed command contains an X too, but
    // only the shell's output passes through the SGR sequence and becomes red.
    enter_command(&session, "printf '\\033[31mX\\033[0m\\n'");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_red_x = false;
    while Instant::now() < deadline && !saw_red_x {
        match events.try_recv() {
            Ok(SessionEvent::Screen(screen)) => {
                saw_red_x = screen.rows.iter().flat_map(|row| row.iter()).any(|cell| {
                    cell.text == "X"
                        && cell.foreground_source == crate::terminal::TerminalColor::Palette(1)
                });
            }
            Ok(SessionEvent::Failed(failure)) => panic!("terminal session failed: {failure}"),
            Ok(SessionEvent::Exited(status)) => panic!("shell exited early: {status}"),
            Ok(
                SessionEvent::HiddenInputChanged(_)
                | SessionEvent::PermissionRequested(_)
                | SessionEvent::MetadataChanged(_)
                | SessionEvent::Attention(_),
            ) => {}
            Err(async_channel::TryRecvError::Empty) => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(async_channel::TryRecvError::Closed) => break,
        }
    }

    assert!(
        saw_red_x,
        "did not receive colored output from the real shell"
    );
}

#[test]
fn real_shell_exit_command_emits_an_exited_event() {
    let _isolation = crate::platform::unix_pty::lock_real_pty_test();
    if crate::platform::unix_pty::isolate_real_pty_test(
        "terminal::session::unix_adapter_tests::real_shell_exit_command_emits_an_exited_event",
    ) {
        return;
    }
    let startup = ShellStartupDirectory::new();
    let size = test_geometry();
    let (session, events, _accessibility) = TerminalSession::start(
        native_pty_adapter_factory(),
        test_launch_planner(startup.path()),
        size,
        &std::env::current_dir().unwrap(),
        Some("fixture.test"),
        LocalFilesystemAuthority::testing(),
    )
    .unwrap();
    let session = JoinedRealPtySession(session);
    wait_for_shell_prompt(&session, &events);

    enter_command(&session, "exit");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut exit_status = None;
    while Instant::now() < deadline && exit_status.is_none() {
        match events.try_recv() {
            Ok(SessionEvent::Screen(_)) => {}
            Ok(SessionEvent::Exited(status)) => exit_status = Some(status),
            Ok(SessionEvent::Failed(failure)) => panic!("terminal session failed: {failure}"),
            Ok(
                SessionEvent::HiddenInputChanged(_)
                | SessionEvent::PermissionRequested(_)
                | SessionEvent::MetadataChanged(_)
                | SessionEvent::Attention(_),
            ) => {}
            Err(async_channel::TryRecvError::Empty) => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(async_channel::TryRecvError::Closed) => break,
        }
    }

    drop(session);
    assert_eq!(exit_status, Some(SessionExit::Success));
}

struct JoinedRealPtySession(TerminalSession);

impl std::ops::Deref for JoinedRealPtySession {
    type Target = TerminalSession;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for JoinedRealPtySession {
    fn drop(&mut self) {
        self.0.shutdown_and_join();
    }
}

struct ShellStartupDirectory(std::path::PathBuf);

impl ShellStartupDirectory {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "spaceterm-session-startup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join(".zshrc"), "").unwrap();
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for ShellStartupDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
