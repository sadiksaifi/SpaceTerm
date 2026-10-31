//! Exercises the public runtime with a native PTY and no graphical application.
use std::error::Error;
use std::sync::Arc;
use std::time::{Duration, Instant};

use spaceterm::runtime::{
    BackingScale, CellGridSize, KeyAction, KeyInput, LocalMachine, LocalTerminalLaunchPlan,
    LogicalCellSize, LogicalSize, NativeTerminalSessionFactory, PhysicalKey, ScreenSnapshot,
    ShellEnvironment, ShellIntegrationMode, ShellIntegrationPolicy, ShellLaunchPlanner,
    TerminalAppearanceUpdate, TerminalGeometry, TerminalRuntime, TerminalSessionEvent,
    local_filesystem, native_pty_factory,
};

fn geometry(columns: u16, rows: u16) -> TerminalGeometry {
    TerminalGeometry::from_viewport(
        LogicalSize::new(f32::from(columns) * 8.0, f32::from(rows) * 18.0),
        LogicalCellSize::new(8.0, 18.0),
        BackingScale::ONE,
        CellGridSize::new(1, 1),
    )
}

fn lines(snapshot: &ScreenSnapshot) -> Vec<String> {
    snapshot
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
        })
        .map(|line| line.trim_end().to_owned())
        .collect()
}

fn send_command(runtime: &TerminalRuntime, command: &str) {
    runtime.key(KeyInput::text_input(command));
    let mut enter = KeyInput::text_input("");
    enter.physical_key = PhysicalKey::Enter;
    enter.logical_key = "enter".to_owned();
    enter.text = None;
    runtime.key(enter.clone());
    enter.action = KeyAction::Release;
    runtime.key(enter);
}

fn await_screen(
    events: &async_channel::Receiver<TerminalSessionEvent>,
    stage: &'static str,
    expected: impl Fn(&ScreenSnapshot) -> bool,
) -> Result<Arc<ScreenSnapshot>, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match events.try_recv() {
            Ok(TerminalSessionEvent::Screen(snapshot)) if expected(&snapshot) => {
                return Ok(snapshot);
            }
            Ok(TerminalSessionEvent::Failed(failure)) => return Err(failure.into()),
            Ok(TerminalSessionEvent::Exited(_)) => {
                return Err("shell exited before verification".into());
            }
            Err(async_channel::TryRecvError::Closed) => {
                return Err("runtime subscription closed before verification".into());
            }
            Ok(_) | Err(async_channel::TryRecvError::Empty) => {}
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err(format!("timed out waiting for the native PTY screen during {stage}").into())
}

fn main() -> Result<(), Box<dyn Error>> {
    if std::env::args().any(|argument| argument == "--require-headless-environment")
        && ["DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS"]
            .iter()
            .any(|name| std::env::var_os(name).is_some())
    {
        return Err("headless verification inherited a graphical session".into());
    }
    let filesystem = local_filesystem();
    let directory = filesystem.validate_directory(&std::env::current_dir()?)?;
    let factory = NativeTerminalSessionFactory::new(
        native_pty_factory(),
        ShellLaunchPlanner::new(
            "/bin/sh".into(),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets"),
            ShellIntegrationMode::Disabled,
            ShellEnvironment::default(),
            ShellIntegrationPolicy {
                supported: true,
                path_list_separator: ':',
                fallback_xdg_data_dirs: "/usr/local/share:/usr/share".into(),
            },
            &[],
        ),
        filesystem,
        LocalMachine::default(),
    );
    let mut runtime = factory.start_runtime(
        geometry(80, 24),
        LocalTerminalLaunchPlan::new(directory),
        TerminalAppearanceUpdate::builtin()?,
    )?;
    let first = runtime.subscribe();
    runtime.focus(true);
    send_command(
        &runtime,
        "stty -echo; printf '\\033[2J\\033[HREADY\\n'; printf 'PTY_PID=%s\\n' \"$$\"; stty size",
    );
    let ready = await_screen(&first.events, "initial handshake", |snapshot| {
        let lines = lines(snapshot);
        lines.iter().any(|line| line == "READY") && lines.iter().any(|line| line == "24 80")
    })?;
    let child: i32 = lines(&ready)
        .iter()
        .find_map(|line| line.strip_prefix("PTY_PID="))
        .ok_or("the shell did not identify its process")?
        .parse()?;
    if child <= 1 {
        return Err("the shell reported an invalid process identifier".into());
    }

    runtime.resize(geometry(100, 30));
    send_command(&runtime, "printf '\\033[2J\\033[HRESIZED\\n'; stty size");
    await_screen(&first.events, "resize", |snapshot| {
        let lines = lines(snapshot);
        snapshot.size.cols == 100
            && snapshot.size.rows == 30
            && lines.iter().any(|line| line == "RESIZED")
            && lines.iter().any(|line| line == "30 100")
    })?;

    drop(first);
    send_command(&runtime, "printf '\\033[2J\\033[HDETACHED\\n'");
    let second = runtime.subscribe();
    await_screen(&second.events, "resubscribe", |snapshot| {
        lines(snapshot).iter().any(|line| line == "DETACHED")
    })?;

    runtime.shutdown();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        // SAFETY: signal zero observes only the test shell's process existence.
        if unsafe { libc::kill(child, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            break;
        }
        if Instant::now() >= deadline {
            return Err("runtime shutdown did not terminate and reap the shell".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("Headless PTY launch, geometry, input, resubscription, and shutdown passed");
    Ok(())
}
