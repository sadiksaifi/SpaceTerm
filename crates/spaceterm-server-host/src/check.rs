//! Explicit, state-free verification of an installed server's PTY and shell resources.
use spaceterm::runtime::*;
use spaceterm_server::ServerError;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

pub fn resources() -> Result<PathBuf, ServerError> {
    let executable = std::env::current_exe().map_err(|_| ServerError::StartupFailed)?;
    let directory = executable.parent().ok_or(ServerError::StartupFailed)?;
    let root = if directory.file_name().is_some_and(|name| name == "MacOS") {
        directory
            .parent()
            .ok_or(ServerError::StartupFailed)?
            .join("Resources")
    } else {
        directory
            .parent()
            .ok_or(ServerError::StartupFailed)?
            .join("share/spaceterm")
    };
    if root.join("shell-integration/VERSION").is_file() && root.join("terminfo").is_dir() {
        return Ok(root);
    }
    if env!("SPACETERM_SERVER_IDENTITY") == "spaceterm-development"
        && !directory
            .file_name()
            .is_some_and(|name| name == "bin" || name == "MacOS")
    {
        return Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets"));
    }
    Err(ServerError::StartupFailed)
}

pub fn run() -> Result<(), ServerError> {
    let resources = resources()?;
    let filesystem = local_filesystem();
    let directory = filesystem
        .validate_directory(&std::env::current_dir().map_err(|_| ServerError::StartupFailed)?)
        .map_err(|_| ServerError::StartupFailed)?;
    let factory = NativeTerminalSessionFactory::new(
        native_pty_factory(),
        ShellLaunchPlanner::new(
            "/bin/sh".into(),
            resources,
            ShellIntegrationMode::Automatic,
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
    let geometry = TerminalGeometry::from_viewport(
        LogicalSize::new(640.0, 432.0),
        LogicalCellSize::new(8.0, 18.0),
        BackingScale::ONE,
        CellGridSize::new(1, 1),
    );
    let mut runtime = factory
        .start_runtime(
            geometry,
            TerminalLaunchPlan::Local(LocalTerminalLaunchPlan::new(directory)),
            TerminalAppearanceUpdate::builtin().map_err(|_| ServerError::StartupFailed)?,
        )
        .map_err(|_| ServerError::StartupFailed)?;
    let subscription = runtime.subscribe();
    runtime.focus(true);
    // The child checks the installed environment, never a source-tree fallback.
    runtime.key(KeyInput::text_input("stty -echo; test -f \"$TERMINFO/x/xterm-spaceterm\" -o -f \"$TERMINFO/78/xterm-spaceterm\" && printf '\\033[2J\\033[HSPACETERM_PACKAGE_OK\\n'"));
    let mut enter = KeyInput::text_input("");
    enter.physical_key = PhysicalKey::Enter;
    enter.logical_key = "enter".into();
    enter.text = None;
    runtime.key(enter);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut success = false;
    while Instant::now() < deadline {
        if let Ok(TerminalSessionEvent::Screen(snapshot)) = subscription.events.try_recv() {
            success = snapshot.rows.iter().any(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .trim()
                    == "SPACETERM_PACKAGE_OK"
            });
            if success {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    runtime.shutdown();
    if !success {
        return Err(ServerError::StartupFailed);
    }
    println!("Installed server PTY and shell resources verified.");
    Ok(())
}
