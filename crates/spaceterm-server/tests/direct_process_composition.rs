#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::{
    fs,
    io::Write as _,
    os::unix::fs::PermissionsExt as _,
    path::Path,
    sync::{Arc, mpsc},
    time::Duration,
};

use spaceterm_core::{
    authority::*,
    control::*,
    geometry::*,
    identity::*,
    operations::*,
    organization::ServerDirectory,
    runtime_authority::{RuntimeAuthority, RuntimeAuthorityParts},
    worktrees::intent::{RepositoryLocator, RepositoryLocatorParts},
};
use spaceterm_host::{
    account_service::ServiceSupervisor,
    git::{self, RepositoryProgramRunner, UnixRepositoryProgramRunner},
    pty::{NativePtyCloseHandle, NativePtyExit, NativePtyOutput, NativePtyOutputSink},
    server_directories::native::NativeDirectoryProvider,
    shell_launch::{ShellLaunchPlanner, ShellTerminalIdentity},
};
use spaceterm_server::{
    adapters::terminal::{AccountShellLaunch, LaunchGate},
    contracts::ServerFailureClass,
    git::host::{GitReadCancellation, NativeServerGitHost, ServerGitHost},
};
use spaceterm_terminal::{
    appearance::{Appearance, AppearanceGeneration},
    metadata::PromptOwner,
    runtime_contracts::{RuntimeAppearance, RuntimeStartRequest},
};

fn server() -> ServerId {
    ServerId::new([19; 16])
}
fn directory(path: &Path) -> ServerDirectory {
    ServerDirectory::new(server(), path.as_os_str().as_encoded_bytes().to_vec()).unwrap()
}
fn request(path: &Path, grid: MeasuredGrid) -> RuntimeStartRequest {
    let pane = PaneId::new(server(), 1).unwrap();
    let client = ClientId::new(server(), 1).unwrap();
    let pairing = PairingId::new(server(), 1).unwrap();
    let old_session = SessionKey {
        pane,
        epoch: ServerEpoch::new(1),
        generation: SessionGeneration::new(1),
    };
    let new_session = SessionKey {
        generation: SessionGeneration::new(2),
        ..old_session
    };
    let grant = ControlGrant::new(pane, client, ControlGeneration::new(1)).unwrap();
    let connection = ConnectionBinding {
        client,
        generation: ConnectionGeneration::new(1),
    };
    let context = ContextGeneration::new(1);
    let fence = EffectFence::new(old_session, grant, connection, context, None).unwrap();
    let operation = OperationId::new(
        OperationStreamId::new(
            OperationStreamKey::new(server(), 1).unwrap(),
            pairing,
            client,
        )
        .unwrap(),
        OperationSequence::new(1).unwrap(),
    );
    let intent = StartupIntent::new(operation, fence, StartupSource::ExplicitSelection).unwrap();
    let reservation = MeasuredStartupReservation::new(
        intent,
        MeasuredStartupMeasurement {
            intent: operation,
            fence,
            grid,
        },
        new_session,
    )
    .unwrap();
    let authority = RuntimeAuthority::new(RuntimeAuthorityParts {
        session: new_session,
        grant: Some(grant),
        connection: Some(connection),
        context,
        focused_pane: true,
        terminal_input_focus: true,
        native_effects_eligible: true,
    })
    .unwrap();
    let color = spaceterm_core::content::Rgb { r: 0, g: 0, b: 0 };
    RuntimeStartRequest::new(
        reservation,
        directory(path),
        authority,
        RuntimeAppearance {
            generation: AppearanceGeneration::new(1),
            foreground: color,
            background: color,
            palette: [color; 256],
            bold_as_bright: false,
            appearance: Appearance::Dark,
        },
    )
    .unwrap()
}
struct Output(mpsc::SyncSender<NativePtyOutput>);
impl NativePtyOutputSink for Output {
    fn publish(&self, output: NativePtyOutput) -> bool {
        self.0.send(output).is_ok()
    }
}
fn executable(root: &Path, name: &str, script: &str) -> std::path::PathBuf {
    let path = root.join(name);
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
fn grid() -> MeasuredGrid {
    MeasuredGrid::new(
        31,
        103,
        CellPixels::new(7, 15).unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap()
}

#[test]
fn shell_spawns_directly_as_the_account_at_measured_geometry_and_directory() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap();
    let shell = executable(
        &path,
        "shell",
        "#!/bin/sh\nprintf 'uid='; /usr/bin/id -u\nprintf 'groups='; /usr/bin/id -G\nprintf 'grid='; /bin/stty size\nprintf 'directory='; /bin/pwd\nprintf 'login=%s\\n' \"$1\"\nprintf 'terminal=%s\\n' \"$TERM_PROGRAM\"\n",
    );
    let planner = ShellLaunchPlanner::for_test(shell, path.clone()).with_terminal_identity(
        ShellTerminalIdentity {
            fallback_term: "xterm-256color",
            colorterm: "truecolor",
            program_name: "SpaceTerm",
            program_version: "fixture",
            terminfo: None,
        },
    );
    let directories = NativeDirectoryProvider::new(server(), path.clone()).unwrap();
    let launch = AccountShellLaunch::prepare(
        &planner,
        &directories,
        &request(&path, grid()),
        &PromptOwner::generate().unwrap(),
    )
    .unwrap();
    assert_eq!(
        (
            launch.size().rows,
            launch.size().columns,
            launch.size().pixel_width,
            launch.size().pixel_height
        ),
        (31, 103, 721, 465)
    );
    let gate = LaunchGate::default();
    let (sender, receiver) = mpsc::sync_channel(32);
    let close = NativePtyCloseHandle::default();
    let mut owner = launch
        .start(&gate, Arc::new(Output(sender)), &close)
        .unwrap();
    let mut bytes = Vec::new();
    loop {
        match receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
            NativePtyOutput::Bytes(chunk) => {
                bytes.extend(chunk);
                assert!(bytes.len() < 64 * 1024);
            }
            NativePtyOutput::Stopped(None) => break,
            NativePtyOutput::Stopped(Some(_)) => panic!("native output failed"),
        }
    }
    assert_eq!(
        owner.wait_for_exit(Duration::from_secs(5)).unwrap(),
        NativePtyExit::Success
    );
    drop(owner);
    assert!(gate.process_admitted());
    let text = String::from_utf8(bytes).unwrap().replace('\r', "");
    assert!(text.contains(&format!(
        "uid={}\n",
        ServiceSupervisor::current().unwrap().key().account_uid
    )));
    let mut id = std::process::Command::new("/usr/bin/id");
    id.arg("-G");
    let expected_groups = spaceterm_host::process::spawn(id.stdout(std::process::Stdio::piped()))
        .unwrap()
        .wait_with_output()
        .unwrap()
        .stdout;
    assert!(text.contains(&format!(
        "groups={}",
        String::from_utf8(expected_groups).unwrap()
    )));
    assert!(text.contains("grid=31 103\n"));
    assert!(text.contains(&format!("directory={}\n", path.display())));
    assert!(text.contains("login=-l\n"));
    assert!(text.contains("terminal=SpaceTerm\n"));
}

#[test]
fn direct_bash_integration_confirms_only_the_sessions_prompt_owner_after_cd() {
    use spaceterm_terminal::{
        local_path::LocalPathSemantics,
        metadata::{LocalMachine, MetadataTracker, TerminalMetadataContext},
    };

    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap();
    let changed = path.join("changed");
    fs::create_dir(&changed).unwrap();
    fs::write(path.join(".bash_profile"), "PS1='fixture> '\n").unwrap();
    // The wrapper isolates login configuration without replacing Bash or the
    // packaged integration. No real account profile is sourced by this test.
    let shell = executable(
        &path,
        "bash",
        &format!(
            "#!/bin/sh\nexport HOME='{}'\nunset BASH_ENV PROMPT_COMMAND _SPACETERM_INTEGRATION_LOADED\nexec /bin/bash \"$@\" -i\n",
            path.display()
        ),
    );
    let resources = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
    let planner = ShellLaunchPlanner::for_test(shell, resources);
    assert_eq!(
        planner.local(&path).unwrap().integration_status(),
        Some(
            spaceterm_host::shell_integration::ShellIntegrationStatus::Applied(
                spaceterm_host::shell_integration::ShellKind::Bash,
            )
        )
    );
    let directories = NativeDirectoryProvider::new(server(), path.clone()).unwrap();
    let prompt_owner = PromptOwner::generate().unwrap();
    let launch = AccountShellLaunch::prepare(
        &planner,
        &directories,
        &request(&path, grid()),
        &prompt_owner,
    )
    .unwrap();
    let (sender, receiver) = mpsc::sync_channel(32);
    let mut owner = launch
        .start(
            &LaunchGate::default(),
            Arc::new(Output(sender)),
            &NativePtyCloseHandle::default(),
        )
        .unwrap();
    // The system Bash login fixture may not source ENV automatically. Exercise
    // the packaged integration itself, independently of profile discovery.
    owner
        .write_all(b"if [[ $_SPACETERM_INTEGRATION_LOADED != 1 ]]; then source \"$ENV\"; fi\ncd changed\nexit\n")
        .unwrap();
    let mut bytes = Vec::new();
    loop {
        match receiver.recv_timeout(Duration::from_secs(10)).unwrap() {
            NativePtyOutput::Bytes(chunk) => {
                bytes.extend(chunk);
                assert!(bytes.len() < 64 * 1024);
            }
            NativePtyOutput::Stopped(None) => break,
            NativePtyOutput::Stopped(Some(_)) => panic!("native output failed"),
        }
    }
    assert_eq!(
        owner.wait_for_exit(Duration::from_secs(5)).unwrap(),
        NativePtyExit::Success
    );
    drop(owner);
    let now = std::time::Instant::now();
    let mut metadata = MetadataTracker::new_with_context(
        TerminalMetadataContext::local(
            LocalPathSemantics::Posix,
            path.to_str().unwrap(),
            LocalMachine::default(),
        )
        .with_prompt_owner(prompt_owner.clone()),
        "bash",
        now,
    );
    let Ok(output) = String::from_utf8(bytes) else {
        panic!("Bash fixture emitted invalid UTF-8");
    };
    let expected = format!("A;redraw=last;spaceterm={}", prompt_owner.value());
    let mut owned_prompts = 0;
    for osc in output.split("\x1b]").skip(1) {
        let Some((value, _)) = osc.split_once('\x07') else {
            continue;
        };
        if let Some(directory) = value.strip_prefix("7;") {
            metadata.set_reported_directory(directory);
        } else if let Some(prompt) = value.strip_prefix("133;") {
            owned_prompts += usize::from(prompt == expected);
            metadata.apply_semantic_prompt(prompt, now);
        }
    }
    assert!(
        output.matches("\x1b]133;A;").count() >= 2,
        "Bash integration fixture emitted no prompts"
    );
    assert!(
        owned_prompts >= 2,
        "Bash emitted no matching owning-shell prompts"
    );
    assert!(metadata.snapshot().repository_directory.as_ref() == changed.to_str().unwrap());
    let confirmed = metadata.snapshot().repository_directory.clone();
    metadata.set_reported_directory("file://localhost/nested-only");
    metadata.apply_semantic_prompt("A", now);
    assert!(metadata.snapshot().repository_directory == confirmed);
    let nested_owner = PromptOwner::generate().unwrap();
    metadata.apply_semantic_prompt(&format!("A;spaceterm={}", nested_owner.value()), now);
    assert!(metadata.snapshot().repository_directory == confirmed);
}

#[test]
fn cancellation_before_the_process_boundary_starts_no_shell() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap();
    let marker = path.join("executed");
    let shell = executable(
        &path,
        "shell",
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );
    let planner = ShellLaunchPlanner::for_test(shell, path.clone()).with_terminal_identity(
        ShellTerminalIdentity {
            fallback_term: "xterm-256color",
            colorterm: "truecolor",
            program_name: "SpaceTerm",
            program_version: "fixture",
            terminfo: None,
        },
    );
    let directories = NativeDirectoryProvider::new(server(), path.clone()).unwrap();
    let launch = AccountShellLaunch::prepare(
        &planner,
        &directories,
        &request(&path, grid()),
        &PromptOwner::generate().unwrap(),
    )
    .unwrap();
    let gate = LaunchGate::default();
    gate.cancel();
    let (sender, _receiver) = mpsc::sync_channel(1);
    let result = launch.start(
        &gate,
        Arc::new(Output(sender)),
        &NativePtyCloseHandle::default(),
    );
    assert!(matches!(result, Err(error) if error.class == ServerFailureClass::StaleAuthority));
    assert!(!marker.exists());
}

#[test]
fn replaced_launch_directory_is_rejected_before_process_admission() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap();
    let working = path.join("working");
    fs::create_dir(&working).unwrap();
    let marker = path.join("executed");
    let shell = executable(
        &path,
        "shell",
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );
    let planner = ShellLaunchPlanner::for_test(shell, path.clone()).with_terminal_identity(
        ShellTerminalIdentity {
            fallback_term: "xterm-256color",
            colorterm: "truecolor",
            program_name: "SpaceTerm",
            program_version: "fixture",
            terminfo: None,
        },
    );
    let directories = NativeDirectoryProvider::new(server(), path.clone()).unwrap();
    let launch = AccountShellLaunch::prepare(
        &planner,
        &directories,
        &request(&working, grid()),
        &PromptOwner::generate().unwrap(),
    )
    .unwrap();
    fs::rename(&working, path.join("original")).unwrap();
    fs::create_dir(&working).unwrap();
    let gate = LaunchGate::default();
    let (sender, _receiver) = mpsc::sync_channel(1);
    assert!(
        launch
            .start(
                &gate,
                Arc::new(Output(sender)),
                &NativePtyCloseHandle::default()
            )
            .is_err()
    );
    assert!(!gate.process_admitted());
    assert!(!marker.exists());
}

#[test]
fn git_uses_the_direct_hardened_account_runner_without_native_spawn_authority() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap();
    let runner = UnixRepositoryProgramRunner::new();
    let cancellation = git::CancellationToken::default();
    let run = |arguments: &[&str]| {
        let request = git::ProgramRequest {
            executable: "/usr/bin/git".into(),
            arguments: arguments.iter().map(Into::into).collect(),
            directory: path.clone(),
            environment: git::local_read::git_environment(Path::new("/usr/bin/git"), &path),
            stdout_limit: Some(64 * 1024),
            deadline: Some(std::time::Instant::now() + Duration::from_secs(10)),
            keep_process_group_on_exit: false,
        };
        let mut bytes = Vec::new();
        assert!(
            runner
                .run(
                    &request,
                    &mut |chunk| bytes.extend_from_slice(chunk),
                    &cancellation
                )
                .unwrap()
                .success()
        );
        bytes
    };
    let version = git::tools::parse_git_version(&run(&["--version"])).unwrap();
    run(&["init", "-q", "--initial-branch=main"]);
    run(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@invalid",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "fixture",
    ]);
    let marker = path.join("git-account");
    let git = executable(
        &path,
        "git",
        &format!(
            "#!/bin/sh\n/usr/bin/id -u > '{}'\nexec /usr/bin/git \"$@\"\n",
            marker.display()
        ),
    );
    let directories = Arc::new(NativeDirectoryProvider::new(server(), path.clone()).unwrap());
    let host = NativeServerGitHost::for_account(git, path.clone(), version, directories);
    let repository = RepositoryLocator::new(RepositoryLocatorParts {
        scope: RepositoryScope {
            server: server(),
            repository: RepositoryId::new(server(), 1).unwrap(),
        },
        main_root: directory(&path),
        common_directory: directory(&path.join(".git")),
    })
    .unwrap();
    let branches = host
        .branches(&repository, &GitReadCancellation::default())
        .unwrap();
    host.shutdown().unwrap();
    assert_eq!(branches.len(), 1);
    assert_eq!(
        fs::read_to_string(marker)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap(),
        ServiceSupervisor::current().unwrap().key().account_uid
    );
}
