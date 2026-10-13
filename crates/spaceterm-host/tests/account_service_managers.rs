#![cfg(all(
    feature = "test-support",
    any(target_os = "macos", target_os = "linux")
))]

use spaceterm_host::{
    account_service::{
        LingerAuthorization, LingerState, ServiceFailure, ServiceManager, SudoMode,
        SupervisorState,
        test_support::native_managers::{self as native, CommandReply, RecordingRunner},
    },
    startup::{BuildIdentity, DesiredServerState},
};
use std::sync::Arc;

fn private_fixture_directory() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

#[test]
fn launch_agent_uses_each_identity_and_escapes_the_approved_executable() {
    for (identity, label) in [
        (
            BuildIdentity::Release,
            "io.github.sadiksaifi.spaceterm.server",
        ),
        (
            BuildIdentity::Preflight,
            "io.github.sadiksaifi.spaceterm-preflight.server",
        ),
        (
            BuildIdentity::Development,
            "io.github.sadiksaifi.spaceterm-development.server",
        ),
    ] {
        let plist = native::launch_agent(
            identity,
            DesiredServerState::Running,
            "/approved/a & <b> \"c\"/server",
        )
        .unwrap();
        assert_eq!(
            plist,
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key><string>{label}</string>\n  <key>ProgramArguments</key>\n  <array><string>/approved/a &amp; &lt;b&gt; &quot;c&quot;/server</string><string>serve</string></array>\n  <key>RunAtLoad</key><true/>\n  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n  <key>ProcessType</key><string>Background</string>\n</dict>\n</plist>\n"
            )
        );
        let stopped =
            native::launch_agent(identity, DesiredServerState::Stopped, "/approved/server")
                .unwrap();
        assert!(stopped.contains("<key>RunAtLoad</key><false/>\n  <key>KeepAlive</key><false/>"));
    }
}

#[test]
fn user_unit_escapes_systemd_expansions_and_preserves_started_writes() {
    assert_eq!(
        native::user_unit(
            BuildIdentity::Release,
            DesiredServerState::Running,
            "/approved/a % $ space/server"
        )
        .unwrap(),
        "[Unit]\nDescription=SpaceTerm Server\nStartLimitIntervalSec=0\n\n[Service]\nType=simple\nExecStart=:\"/approved/a %% $ space/server\" serve\nRestart=on-failure\nRestartSec=2s\nKillSignal=SIGTERM\nKillMode=process\nSendSIGKILL=no\nTimeoutStopSec=infinity\n\n[Install]\nWantedBy=default.target\n"
    );
    assert!(
        native::user_unit(
            BuildIdentity::Release,
            DesiredServerState::Stopped,
            "/approved/server"
        )
        .unwrap()
        .contains("Restart=no\n")
    );
    for invalid in [
        "relative/server",
        "/approved/../server",
        "/approved/\nserver",
        "/approved/\0server",
    ] {
        assert_eq!(
            native::user_unit(BuildIdentity::Release, DesiredServerState::Running, invalid),
            Err(ServiceFailure::InvalidRecord)
        );
    }
}

#[test]
fn user_unit_refuses_unsupported_native_executable_characters() {
    for executable in [
        "/approved/a\"b/server",
        "/approved/a'b/server",
        "/approved/a\\b/server",
    ] {
        assert_eq!(
            native::user_unit(
                BuildIdentity::Development,
                DesiredServerState::Running,
                executable
            ),
            Err(ServiceFailure::InvalidRecord),
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn user_unit_offline_parser_resolves_the_exact_dollar_executable() {
    use std::{
        os::unix::fs::PermissionsExt,
        process::{Command, Stdio},
    };
    let directory = private_fixture_directory();
    let root = directory.path().canonicalize().unwrap();
    let installation = root.join("$data % approved");
    std::fs::create_dir(&installation).unwrap();
    let executable = installation.join("spaceterm-server");
    // verify checks executable existence/permissions but never executes this file
    // or talks to the real manager. A doubled dollar selects a nonexistent file.
    std::fs::write(&executable, b"offline parser fixture\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let unit = root.join("spaceterm-development-server.service");
    std::fs::write(
        &unit,
        native::user_unit(
            BuildIdentity::Development,
            DesiredServerState::Running,
            executable.to_str().unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let verify = || {
        spaceterm_host::process::spawn(
            Command::new("/usr/bin/systemd-analyze")
                .args(["verify", "--man=no", "--generators=no"])
                .arg(&unit)
                .env_clear()
                .env("LC_ALL", "C")
                .env("PATH", "/usr/bin:/bin")
                .current_dir("/")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap()
        .wait()
        .unwrap()
    };
    assert!(
        verify().success(),
        "offline parser rejected the selected executable"
    );
    let wrong = std::fs::read_to_string(&unit)
        .unwrap()
        .replace("$data", "$$data");
    std::fs::write(&unit, wrong).unwrap();
    assert!(
        !verify().success(),
        "offline parser accepted a different executable path"
    );
}

#[test]
fn user_unit_allows_crash_recovery_with_a_surviving_session_child() {
    let unit = native::user_unit(
        BuildIdentity::Development,
        DesiredServerState::Running,
        "/approved/server",
    )
    .unwrap();
    // systemd 249's service_adverse_to_leftover_processes refuses a new main
    // process beside a surviving child with KillMode=mixed/control-group and
    // SendSIGKILL=no. Main-process-only signalling leaves exact Session cleanup
    // to the replacement server before readiness, without killing started Git.
    assert!(unit.contains("KillMode=process\n"));
    assert!(unit.contains("SendSIGKILL=no\nTimeoutStopSec=infinity\n"));
}

#[tokio::test]
async fn launchctl_uses_only_the_login_domain_and_never_replaces_running_code() {
    let runner = Arc::new(RecordingRunner::new([
        CommandReply::success(b"domain"),
        CommandReply::exit(113, b""),
        CommandReply::success(b""),
        CommandReply::success(b""),
    ]));
    native::launch_start(
        runner.clone(),
        501,
        BuildIdentity::Release,
        "/home/Library/LaunchAgents/io.github.sadiksaifi.spaceterm.server.plist",
    )
    .await
    .unwrap();
    assert_eq!(
        runner.arguments(),
        vec![
            vec!["print", "gui/501"],
            vec!["print", "gui/501/io.github.sadiksaifi.spaceterm.server"],
            vec![
                "bootstrap",
                "gui/501",
                "/home/Library/LaunchAgents/io.github.sadiksaifi.spaceterm.server.plist"
            ],
            vec!["kickstart", "gui/501/io.github.sadiksaifi.spaceterm.server"],
        ]
    );
    assert!(
        runner
            .programs()
            .iter()
            .all(|program| program == "/bin/launchctl")
    );
    let missing = Arc::new(RecordingRunner::new([CommandReply::exit(
        113,
        b"private native error",
    )]));
    assert_eq!(
        native::launch_start(
            missing.clone(),
            501,
            BuildIdentity::Release,
            "/unused.plist"
        )
        .await,
        Err(ServiceFailure::LoginSessionUnavailable)
    );
    assert_eq!(missing.arguments(), vec![vec!["print", "gui/501"]]);
}

#[tokio::test]
async fn user_manager_uses_fixed_commands_and_ignores_caller_environment() {
    let runner = Arc::new(RecordingRunner::new(
        (0..7).map(|_| CommandReply::success(b"")),
    ));
    native::user_commands(runner.clone(), 1000, BuildIdentity::Development)
        .await
        .unwrap();
    let unit = "spaceterm-development-server.service";
    assert_eq!(
        runner.arguments(),
        vec![
            vec!["--user", "--no-pager", "--no-ask-password", "daemon-reload"],
            vec![
                "--user",
                "--no-pager",
                "--no-ask-password",
                "enable",
                "--",
                unit
            ],
            vec![
                "--user",
                "--no-pager",
                "--no-ask-password",
                "disable",
                "--",
                unit
            ],
            vec![
                "--user",
                "--no-pager",
                "--no-ask-password",
                "--no-block",
                "start",
                "--",
                unit
            ],
            vec![
                "--user",
                "--no-pager",
                "--no-ask-password",
                "--no-block",
                "stop",
                "--",
                unit
            ],
            vec![
                "--user",
                "--no-pager",
                "--no-ask-password",
                "show",
                "--property=LoadState,ActiveState,SubState,UnitFileState",
                "--",
                unit
            ],
            vec![
                "--user",
                "--no-pager",
                "--no-ask-password",
                "show",
                "--property=UnitPath",
                "--value"
            ],
        ]
    );
    assert!(
        runner
            .programs()
            .iter()
            .all(|program| program == "/usr/bin/systemctl")
    );
    assert!(
        runner
            .environments()
            .iter()
            .all(|environment| environment.contains(&(
                "DBUS_SESSION_BUS_ADDRESS".into(),
                "unix:path=/run/user/1000/bus".into()
            )))
    );
}

#[test]
fn manager_state_decoding_is_bounded_and_content_free() {
    assert_eq!(
        native::systemd_state(
            0,
            b"LoadState=loaded\nActiveState=active\nSubState=running\nUnitFileState=enabled\n"
        ),
        Ok(SupervisorState::Running)
    );
    assert_eq!(
        native::systemd_state(
            4,
            b"LoadState=not-found\nActiveState=inactive\nSubState=dead\nUnitFileState=\n"
        ),
        Ok(SupervisorState::NotInstalled)
    );
    assert_eq!(
        native::systemd_state(1, b"native path and error"),
        Err(ServiceFailure::SupervisorUnavailable)
    );
    assert_eq!(
        native::systemd_state(0, b"LoadState=loaded\nLoadState=not-found\n"),
        Err(ServiceFailure::SupervisorUnavailable)
    );
}

#[tokio::test]
async fn linger_checks_before_authorizing_and_elevates_only_loginctl() {
    let enabled = Arc::new(RecordingRunner::new([CommandReply::success(b"yes\n")]));
    assert_eq!(
        native::linger(enabled.clone(), 1000).await.unwrap(),
        LingerState::Enabled
    );
    assert_eq!(
        enabled.arguments(),
        vec![vec!["show-user", "1000", "--property=Linger", "--value"]]
    );
    for (mode, expected) in [
        (
            SudoMode::Interactive,
            vec!["-k", "--", "/usr/bin/loginctl", "enable-linger", "1000"],
        ),
        (
            SudoMode::NonInteractive,
            vec![
                "-k",
                "-n",
                "--",
                "/usr/bin/loginctl",
                "enable-linger",
                "1000",
            ],
        ),
    ] {
        let runner = Arc::new(RecordingRunner::new([
            CommandReply::success(b"no\n"),
            CommandReply::success(b""),
            CommandReply::success(b"yes\n"),
        ]));
        native::sudo_authorization(runner.clone(), mode)
            .enable_linger(&native::linger_request(1000))
            .await
            .unwrap();
        assert_eq!(
            runner.programs(),
            ["/usr/bin/loginctl", "/usr/bin/sudo", "/usr/bin/loginctl"]
        );
        assert_eq!(runner.arguments()[1], expected);
    }
    let runner = Arc::new(RecordingRunner::new([
        CommandReply::success(b"no\n"),
        CommandReply::exit(126, b"secret denied"),
    ]));
    assert_eq!(
        native::pkexec_authorization(runner.clone())
            .enable_linger(&native::linger_request(1000))
            .await,
        Err(ServiceFailure::AuthorizationCancelled)
    );
    assert_eq!(
        runner.arguments()[1],
        ["/usr/bin/loginctl", "enable-linger", "1000"]
    );
}

#[tokio::test]
async fn stopped_configuration_suppresses_without_resolving_executable() {
    let directory = private_fixture_directory();
    let root = directory.path().canonicalize().unwrap();
    let search = format!("{}\n", root.join("preferences/systemd/user").display());
    let runner = Arc::new(RecordingRunner::new([
        CommandReply::success(b""),
        CommandReply::success(b""),
        CommandReply::success(search.as_bytes()),
        CommandReply::success(b""),
    ]));
    let fixture = native::stopped_managers(&root, runner.clone()).unwrap();
    fixture
        .launchd
        .configure(fixture.registration())
        .await
        .unwrap();
    fixture
        .systemd
        .configure(fixture.registration())
        .await
        .unwrap();
    assert_eq!(
        runner.arguments()[0],
        ["print", &format!("gui/{}", native::current_uid())]
    );
    assert_eq!(runner.arguments()[1][0], "disable");
    assert_eq!(runner.arguments()[2][3], "show");
    assert_eq!(runner.arguments()[3][3], "disable");
    assert!(runner.environments()[3].contains(&(
        "XDG_CONFIG_HOME".into(),
        root.join("preferences").to_str().unwrap().into(),
    )));
    assert!(!root.join("preferences").exists());
}

#[tokio::test]
async fn enabled_linger_never_invokes_an_authorization_utility() {
    let runner = Arc::new(RecordingRunner::new([CommandReply::success(b"yes\n")]));
    native::sudo_authorization(runner.clone(), SudoMode::Interactive)
        .enable_linger(&native::linger_request(1000))
        .await
        .unwrap();
    assert_eq!(runner.programs(), ["/usr/bin/loginctl"]);
    let runner = Arc::new(RecordingRunner::new([
        CommandReply::success(b"no\n"),
        CommandReply::exit(1, b"private sudo error"),
    ]));
    assert_eq!(
        native::sudo_authorization(runner.clone(), SudoMode::NonInteractive)
            .enable_linger(&native::linger_request(1000))
            .await,
        Err(ServiceFailure::AuthorizationDenied)
    );
    assert_eq!(runner.programs(), ["/usr/bin/loginctl", "/usr/bin/sudo"]);
}

#[test]
fn launchctl_state_ignores_nested_values_and_rejects_duplicate_states() {
    assert_eq!(
        native::launchd_state(
            b"gui/501/label = {\n state = running\n environment = {\n state = exited\n }\n}\n"
        ),
        Ok(SupervisorState::Running)
    );
    assert_eq!(
        native::launchd_state(b"gui/501/label = {\n state = running\n state = exited\n}\n"),
        Err(ServiceFailure::SupervisorUnavailable)
    );
}

#[tokio::test]
async fn loaded_launch_agent_is_kickstarted_without_bootstrap_or_force_replace() {
    let runner = Arc::new(RecordingRunner::new(
        (0..3).map(|_| CommandReply::success(b"")),
    ));
    native::launch_start(
        runner.clone(),
        501,
        BuildIdentity::Preflight,
        "/home/Library/LaunchAgents/io.github.sadiksaifi.spaceterm-preflight.server.plist",
    )
    .await
    .unwrap();
    assert_eq!(
        runner.arguments(),
        [
            vec!["print", "gui/501"],
            vec![
                "print",
                "gui/501/io.github.sadiksaifi.spaceterm-preflight.server"
            ],
            vec![
                "kickstart",
                "gui/501/io.github.sadiksaifi.spaceterm-preflight.server"
            ]
        ]
    );
}

#[test]
fn configuration_publication_is_private_and_rejects_symlinks_and_hardlinks() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    let directory = private_fixture_directory();
    let root = directory.path().canonicalize().unwrap();
    native::publish_configuration(&root, b"first").unwrap();
    let file = root.join("services/fixture.service");
    assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o777, 0o600);
    native::publish_configuration(&root, b"second").unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"second");
    std::fs::hard_link(&file, root.join("retained")).unwrap();
    assert_eq!(
        native::publish_configuration(&root, b"replacement"),
        Err(ServiceFailure::UnsafeStorage)
    );
    assert_eq!(
        native::remove_configuration(&root),
        Err(ServiceFailure::UnsafeStorage)
    );
    std::fs::remove_file(root.join("retained")).unwrap();
    native::remove_configuration(&root).unwrap();
    symlink(root.join("unrelated"), &file).unwrap();
    assert_eq!(
        native::publish_configuration(&root, b"replacement"),
        Err(ServiceFailure::UnsafeStorage)
    );
    std::fs::remove_file(&file).unwrap();
    std::fs::set_permissions(
        root.join("services"),
        std::fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    assert_eq!(
        native::publish_configuration(&root, b"replacement"),
        Err(ServiceFailure::UnsafeStorage)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn configuration_refuses_allow_acls_but_accepts_home_deny_delete() {
    let directory = private_fixture_directory();
    let root = directory.path().canonicalize().unwrap();
    let status = spaceterm_host::process::spawn(
        std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone deny delete"])
            .arg(&root),
    )
    .unwrap()
    .wait()
    .unwrap();
    assert!(status.success());
    native::publish_configuration(&root, b"first").unwrap();
    let file = root.join("services/fixture.service");
    let status = spaceterm_host::process::spawn(
        std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone allow read"])
            .arg(&file),
    )
    .unwrap()
    .wait()
    .unwrap();
    assert!(status.success());
    assert_eq!(
        native::publish_configuration(&root, b"replacement"),
        Err(ServiceFailure::UnsafeStorage)
    );
}

#[tokio::test]
async fn native_utility_output_and_deadlines_are_bounded() {
    use std::time::{Duration, Instant};
    assert_eq!(
        native::fixture_command(
            "printf ok; printf 'private error' >&2",
            Duration::from_secs(2)
        )
        .await
        .unwrap(),
        b"ok"
    );
    assert_eq!(
        native::fixture_command("exec /usr/bin/yes", Duration::from_secs(2)).await,
        Err(ServiceFailure::LimitExceeded)
    );
    let started = Instant::now();
    assert_eq!(
        native::fixture_command("exec /bin/sleep 10", Duration::from_millis(20)).await,
        Err(ServiceFailure::SupervisorUnavailable)
    );
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn dropping_a_utility_observer_does_not_cancel_accepted_execution() {
    use std::time::Duration;
    let directory = private_fixture_directory();
    let script = format!(
        "sleep 0.1; printf done > '{}/completed'",
        directory.path().display()
    );
    let mut operation = Box::pin(native::fixture_command(&script, Duration::from_secs(2)));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut operation)
            .await
            .is_err()
    );
    drop(operation);
    for _ in 0..100 {
        if std::fs::read(directory.path().join("completed"))
            .ok()
            .as_deref()
            == Some(b"done")
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the accepted utility was abandoned");
}

struct SshPrompt {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    cancelled: bool,
}
impl spaceterm_host::account_service::LingerAuthenticationPrompt for SshPrompt {
    fn respond<'a>(
        &'a self,
        _: &'a spaceterm_host::account_service::LingerRequest,
    ) -> spaceterm_host::account_service::ServiceFuture<'a, zeroize::Zeroizing<Vec<u8>>> {
        Box::pin(async move {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.cancelled {
                Err(ServiceFailure::AuthorizationCancelled)
            } else {
                Ok(zeroize::Zeroizing::new(b"test-only-response".to_vec()))
            }
        })
    }
}
struct SshExchange {
    enabled: std::sync::atomic::AtomicBool,
    calls: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
}
impl spaceterm_host::account_service::SshLingerExchange for SshExchange {
    fn linger(
        &self,
        _: &spaceterm_host::account_service::LingerRequest,
    ) -> spaceterm_host::account_service::ServiceFuture<'_, LingerState> {
        Box::pin(async {
            Ok(if self.enabled.load(std::sync::atomic::Ordering::SeqCst) {
                LingerState::Enabled
            } else {
                LingerState::Disabled
            })
        })
    }
    fn invoke<'a>(
        &'a self,
        _: &'a spaceterm_host::account_service::LingerRequest,
        command: spaceterm_host::account_service::ServiceCommand,
        authentication: &'a spaceterm_host::account_service::ScopedLingerAuthentication<'a>,
    ) -> spaceterm_host::account_service::ServiceFuture<'a, ()> {
        Box::pin(async move {
            assert_eq!(command.program(), "/usr/bin/sudo");
            assert!(command.environment().is_empty());
            self.calls.lock().unwrap().push(
                command
                    .arguments()
                    .iter()
                    .map(|a| a.to_str().unwrap().to_owned())
                    .collect(),
            );
            for _ in 0..3 {
                drop(authentication.respond().await?);
            }
            assert!(matches!(
                authentication.respond().await,
                Err(ServiceFailure::LimitExceeded)
            ));
            self.enabled
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
    }
}

#[tokio::test]
async fn ssh_linger_uses_one_scoped_zeroizing_prompt_and_fixed_sudo_operation() {
    use spaceterm_host::account_service::SshLingerAuthorization;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let responses = Arc::new(AtomicUsize::new(0));
    let authorization = SshLingerAuthorization::new(
        SshExchange {
            enabled: AtomicBool::new(false),
            calls: calls.clone(),
        },
        SshPrompt {
            calls: responses.clone(),
            cancelled: false,
        },
    );
    authorization
        .enable_linger(&native::linger_request(1000))
        .await
        .unwrap();
    authorization
        .enable_linger(&native::linger_request(1000))
        .await
        .unwrap();
    assert_eq!(responses.load(Ordering::SeqCst), 3);
    assert_eq!(
        *calls.lock().unwrap(),
        [vec![
            "-k",
            "--",
            "/usr/bin/loginctl",
            "enable-linger",
            "1000"
        ]]
    );
    let authorization = SshLingerAuthorization::new(
        SshExchange {
            enabled: AtomicBool::new(false),
            calls,
        },
        SshPrompt {
            calls: responses.clone(),
            cancelled: true,
        },
    );
    assert_eq!(
        authorization
            .enable_linger(&native::linger_request(1000))
            .await,
        Err(ServiceFailure::AuthorizationCancelled)
    );
    assert_eq!(responses.load(Ordering::SeqCst), 4);
}

struct TestInstallations;
impl spaceterm_host::account_service::ServiceInstallations for TestInstallations {
    fn approved_executable(
        &self,
        _: &spaceterm_host::account_service::RegisteredServer,
    ) -> Result<std::path::PathBuf, ServiceFailure> {
        Ok("/approved/server".into())
    }
    fn uninstall(
        &self,
        registration: &spaceterm_host::account_service::RegisteredServer,
    ) -> Result<(), ServiceFailure> {
        assert!(registration.data_directory().exists());
        Ok(())
    }
}

#[tokio::test]
async fn launch_agent_public_operations_publish_and_remove_only_service_configuration() {
    let directory = private_fixture_directory();
    let root = directory.path().canonicalize().unwrap();
    let runner = Arc::new(RecordingRunner::new(
        (0..7).map(|_| CommandReply::success(b"")),
    ));
    let fixture = native::managers(
        &root,
        runner.clone(),
        Arc::new(TestInstallations),
        DesiredServerState::Running,
    )
    .unwrap();
    fixture
        .launchd
        .configure(fixture.registration())
        .await
        .unwrap();
    let path =
        root.join("Library/LaunchAgents/io.github.sadiksaifi.spaceterm-development.server.plist");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        native::launch_agent(
            BuildIdentity::Development,
            DesiredServerState::Running,
            "/approved/server"
        )
        .unwrap()
    );
    fixture.launchd.start(fixture.registration()).await.unwrap();
    fixture.launchd.stop(fixture.registration()).await.unwrap();
    fixture
        .launchd
        .uninstall(fixture.registration())
        .await
        .unwrap();
    assert!(!path.exists());
    assert!(fixture.registration().data_directory().exists());
    let target = format!(
        "gui/{}/io.github.sadiksaifi.spaceterm-development.server",
        native::current_uid()
    );
    assert_eq!(
        runner
            .arguments()
            .iter()
            .map(|args| args[0].as_str())
            .collect::<Vec<_>>(),
        [
            "print",
            "enable",
            "print",
            "print",
            "kickstart",
            "print",
            "bootout"
        ]
    );
    assert_eq!(runner.arguments()[6], ["bootout", target.as_str()]);
}

#[tokio::test]
async fn user_service_public_operations_publish_in_the_observed_search_path() {
    let directory = private_fixture_directory();
    let root = directory.path().canonicalize().unwrap();
    let search = format!(
        "{} /etc/systemd/user\n",
        root.join(".config/systemd/user").display()
    );
    let runner = Arc::new(RecordingRunner::new(
        std::iter::once(CommandReply::success(search.as_bytes()))
            .chain((0..6).map(|_| CommandReply::success(b""))),
    ));
    let fixture = native::managers(
        &root,
        runner.clone(),
        Arc::new(TestInstallations),
        DesiredServerState::Running,
    )
    .unwrap();
    fixture
        .systemd
        .configure(fixture.registration())
        .await
        .unwrap();
    let path = root.join(".config/systemd/user/spaceterm-development-server.service");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        native::user_unit(
            BuildIdentity::Development,
            DesiredServerState::Running,
            "/approved/server"
        )
        .unwrap()
    );
    fixture.systemd.start(fixture.registration()).await.unwrap();
    fixture.systemd.stop(fixture.registration()).await.unwrap();
    fixture
        .systemd
        .uninstall(fixture.registration())
        .await
        .unwrap();
    assert!(!path.exists());
    assert!(fixture.registration().data_directory().exists());
    assert_eq!(
        runner
            .arguments()
            .iter()
            .map(|args| args[3].as_str())
            .collect::<Vec<_>>(),
        [
            "show",
            "daemon-reload",
            "enable",
            "--no-block",
            "--no-block",
            "disable",
            "daemon-reload"
        ]
    );
    assert!(
        runner
            .programs()
            .iter()
            .all(|program| program == "/usr/bin/systemctl")
    );
}

#[tokio::test]
async fn user_service_retains_the_managers_nondefault_configuration_directory() {
    let directory = private_fixture_directory();
    let root = directory.path().canonicalize().unwrap();
    let configuration = root.join("preferences");
    let search = format!(
        "{} {} {} /etc/systemd/user {}\n",
        root.join("manager-runtime/systemd/user.control").display(),
        configuration.join("systemd/user.control").display(),
        configuration.join("systemd/user").display(),
        root.join(".local/share/systemd/user").display(),
    );
    let runner = Arc::new(RecordingRunner::new(
        std::iter::once(CommandReply::success(search.as_bytes()))
            .chain((0..4).map(|_| CommandReply::success(b""))),
    ));
    let fixture = native::managers(
        &root,
        runner.clone(),
        Arc::new(TestInstallations),
        DesiredServerState::Running,
    )
    .unwrap();
    fixture
        .systemd
        .configure(fixture.registration())
        .await
        .unwrap();
    let path = configuration.join("systemd/user/spaceterm-development-server.service");
    assert!(path.is_file());
    fixture
        .systemd
        .uninstall(fixture.registration())
        .await
        .unwrap();
    assert!(!path.exists());
    assert!(!root.join(".config").exists());
    // No second search or caller-environment lookup changes the admitted location.
    assert_eq!(runner.arguments().len(), 5);
    for environment in runner.environments().iter().skip(1) {
        assert!(environment.contains(&(
            "XDG_CONFIG_HOME".into(),
            configuration.to_str().unwrap().into(),
        )));
    }
}

#[tokio::test]
async fn user_service_decodes_systemctl_quoted_unit_paths_without_shell_evaluation() {
    for (component, printed) in [
        ("home a b", "home a b"),
        ("home\\name", "home\\\\name"),
        ("home\"$`name", "home\\\"\\$\\`name"),
        ("home'name", "home'name"),
    ] {
        let directory = private_fixture_directory();
        let root = directory.path().canonicalize().unwrap();
        let home = root.join(component);
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        // systemctl 249 uses shell_maybe_quote(str, 0), not hex escaping or
        // shell evaluation. Single quotes remain literal inside double quotes.
        let search = format!(
            "\"{}/{printed}/.config/systemd/user\" /etc/systemd/user\n",
            root.display()
        );
        let runner = Arc::new(RecordingRunner::new(
            std::iter::once(CommandReply::success(search.as_bytes()))
                .chain((0..4).map(|_| CommandReply::success(b""))),
        ));
        let fixture = native::managers(
            &home,
            runner.clone(),
            Arc::new(TestInstallations),
            DesiredServerState::Running,
        )
        .unwrap();
        fixture
            .systemd
            .configure(fixture.registration())
            .await
            .unwrap();
        let path = home.join(".config/systemd/user/spaceterm-development-server.service");
        assert!(path.is_file());
        assert!(runner.environments()[2].contains(&(
            "XDG_CONFIG_HOME".into(),
            home.join(".config").to_str().unwrap().into(),
        )));
        fixture
            .systemd
            .uninstall(fixture.registration())
            .await
            .unwrap();
        assert!(!path.exists());
    }
}

#[tokio::test]
async fn user_service_rejects_malformed_unit_path_quoting_before_publication() {
    for search in [
        "\"/home/unterminated",
        "\"/home/unknown\\q\"\n",
        "\"/home/quoted\"suffix\n",
        "\"/home/control\\n\"\n",
        "\"/home/control\\001\"\n",
        "\"/home/truncated\\\"",
    ] {
        let directory = private_fixture_directory();
        let runner = Arc::new(RecordingRunner::new([CommandReply::success(
            search.as_bytes(),
        )]));
        let fixture = native::managers(
            directory.path(),
            runner.clone(),
            Arc::new(TestInstallations),
            DesiredServerState::Running,
        )
        .unwrap();
        assert_eq!(
            fixture.systemd.configure(fixture.registration()).await,
            Err(ServiceFailure::SupervisorUnavailable),
        );
        assert!(!directory.path().join(".config").exists());
        assert_eq!(runner.arguments().len(), 1);
    }
}

#[tokio::test]
async fn user_service_does_not_publish_outside_the_managers_search_path() {
    let directory = private_fixture_directory();
    let root = directory.path().canonicalize().unwrap();
    let runner = Arc::new(RecordingRunner::new([CommandReply::success(
        b"/etc/systemd/user\n",
    )]));
    let fixture = native::managers(
        &root,
        runner.clone(),
        Arc::new(TestInstallations),
        DesiredServerState::Running,
    )
    .unwrap();
    assert_eq!(
        fixture.systemd.configure(fixture.registration()).await,
        Err(ServiceFailure::SupervisorUnavailable)
    );
    assert!(!root.join(".config").exists());
    assert_eq!(runner.programs(), ["/usr/bin/systemctl"]);
}

#[tokio::test]
async fn missing_user_unit_is_idempotent_only_after_a_native_absence_observation() {
    let directory = private_fixture_directory();
    let root = directory.path().canonicalize().unwrap();
    let search = format!("{}\n", root.join(".config/systemd/user").display());
    let missing = b"LoadState=not-found\nActiveState=inactive\nSubState=dead\nUnitFileState=\n";
    let runner = Arc::new(RecordingRunner::new([
        CommandReply::success(search.as_bytes()),
        CommandReply::exit(1, b"raw missing unit error"),
        CommandReply::exit(4, missing),
        CommandReply::exit(5, b"raw missing unit error"),
        CommandReply::exit(4, missing),
    ]));
    let fixture = native::stopped_managers(directory.path(), runner.clone()).unwrap();
    fixture
        .systemd
        .configure(fixture.registration())
        .await
        .unwrap();
    fixture.systemd.stop(fixture.registration()).await.unwrap();
    assert_eq!(
        runner
            .arguments()
            .iter()
            .map(|args| args[3].as_str())
            .collect::<Vec<_>>(),
        ["show", "disable", "show", "--no-block", "show"]
    );
}

#[tokio::test]
async fn sudo_interrupt_is_reported_as_cancellation_not_denial() {
    let runner = Arc::new(RecordingRunner::new([
        CommandReply::success(b"no\n"),
        CommandReply::exit(130, b"private native output"),
    ]));
    assert_eq!(
        native::sudo_authorization(runner, SudoMode::Interactive)
            .enable_linger(&native::linger_request(1000))
            .await,
        Err(ServiceFailure::AuthorizationCancelled)
    );
}
