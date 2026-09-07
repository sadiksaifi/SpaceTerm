//! Native Adapter integration evidence.
use super::*;
use crate::platform::macos_pty::MacosNativePtyAdapterFactory;
fn macos_native_pty_adapter_factory() -> Arc<dyn NativePtyAdapterFactory> {
    Arc::new(MacosNativePtyAdapterFactory)
}
fn test_geometry() -> TerminalGeometry {
    super::tests::test_geometry()
}
fn test_launch_planner() -> ShellLaunchPlanner {
    ShellLaunchPlanner::for_test(
        "/bin/zsh".into(),
        crate::platform::launch_host::resource_root(),
    )
}
#[test]
fn real_shell_output_round_trips_through_the_pty_and_emulator() {
    let _isolation = crate::platform::macos_pty::lock_real_pty_test();
    let size = test_geometry();
    let (session, events, _accessibility) = TerminalSession::start(
        macos_native_pty_adapter_factory(),
        test_launch_planner(),
        size,
        &std::env::current_dir().unwrap(),
        Some("fixture.test"),
        LocalFilesystemAuthority::testing(),
    )
    .unwrap();
    let session = JoinedRealPtySession(session);

    // The command renders a red X. The echoed command contains an X too, but
    // only the shell's output passes through the SGR sequence and becomes red.
    let request = session
        .request_paste("printf '\\033[31mX\\033[0m\\n'\n".to_owned().into())
        .recv_blocking()
        .unwrap()
        .unwrap();
    let PasteRequestOutcome::ConfirmationRequired(confirmation) = request else {
        panic!("multiline paste must require confirmation")
    };
    assert_eq!(
        session
            .resolve_paste(confirmation.id, PasteDecision::Confirm)
            .recv_blocking()
            .unwrap(),
        Ok(PasteResolution::Written)
    );

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
            Ok(SessionEvent::HiddenInputChanged(_) | SessionEvent::Attention(_)) => {}
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
    let _isolation = crate::platform::macos_pty::lock_real_pty_test();
    let size = test_geometry();
    let (session, events, _accessibility) = TerminalSession::start(
        macos_native_pty_adapter_factory(),
        test_launch_planner(),
        size,
        &std::env::current_dir().unwrap(),
        Some("fixture.test"),
        LocalFilesystemAuthority::testing(),
    )
    .unwrap();
    let session = JoinedRealPtySession(session);

    let request = session
        .request_paste("exit\n".to_owned().into())
        .recv_blocking()
        .unwrap()
        .unwrap();
    let PasteRequestOutcome::ConfirmationRequired(confirmation) = request else {
        panic!("multiline paste must require confirmation")
    };
    let _ = session
        .resolve_paste(confirmation.id, PasteDecision::Confirm)
        .recv_blocking();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut exit_status = None;
    while Instant::now() < deadline && exit_status.is_none() {
        match events.try_recv() {
            Ok(SessionEvent::Screen(_)) => {}
            Ok(SessionEvent::Exited(status)) => exit_status = Some(status),
            Ok(SessionEvent::Failed(failure)) => panic!("terminal session failed: {failure}"),
            Ok(SessionEvent::HiddenInputChanged(_) | SessionEvent::Attention(_)) => {}
            Err(async_channel::TryRecvError::Empty) => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(async_channel::TryRecvError::Closed) => break,
        }
    }

    drop(session);
    assert!(
        exit_status.is_some(),
        "shell exit did not produce a terminal lifecycle event"
    );
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
