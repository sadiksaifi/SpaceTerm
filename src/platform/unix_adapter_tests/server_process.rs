use super::*;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
#[test]
fn installer_executes_the_verified_archive_layout_and_cleans_its_temporary_files() {
    // This fixture exercises the generated shell and archive layout without service effects.
    let root = std::env::temp_dir().join(format!("st-install-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut bytes = Vec::new();
    {
        let compressed = flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::default());
        let mut archive = tar::Builder::new(compressed);
        let helper = b"#!/bin/sh\ncase $1 in ssh-enable-linger) exit 0;; ssh-enable-linger-password) IFS= read -r password; test \"$password\" = fixture;; ssh-install) test -f \"$3\" && test -f \"$4\";; *) exit 91;; esac\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(helper.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, "server/bin/spaceterm-server", &helper[..])
            .unwrap();
        archive.into_inner().unwrap().finish().unwrap();
    }
    let artifact = VerifiedArtifact {
        digest: hex_digest(&bytes),
        bytes,
        signature: vec![1; 64],
        version: "test".into(),
        platform: ("Linux".into(), "x86_64".into()),
    };
    for (password, input, expected_code) in [
        (false, None, 0),
        (true, Some("fixture"), 0),
        (true, Some("wrong"), 77),
    ] {
        let script = crate::platform::unix_ssh_process::UnixSshProcessAdapter
            .install(
                identity(),
                artifact.bytes.len(),
                &artifact.signature,
                &artifact.digest,
                password,
            )
            .replace("${TMPDIR:-/tmp}", root.to_str().unwrap())
            .replace("if [ \"$(uname -s)\" = Linux ]; then", "if true; then");
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut writer = child.stdin.take().unwrap();
        if let Some(input) = input {
            writeln!(writer, "{input}").unwrap();
        }
        writer.write_all(&artifact.bytes).unwrap();
        drop(writer);
        assert_eq!(child.wait().unwrap().code(), Some(expected_code));
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }
    std::fs::remove_dir(root).unwrap();
}

use crate::ssh::askpass::{
    AskPassAttempt, AskPassAttemptObservation, AskPassBrokerLease, AskPassLease, AskPassUnavailable,
};
struct NoPrompt;
impl AskPassLease for NoPrompt {
    fn entries(&self) -> Vec<(&'static str, &std::ffi::OsStr)> {
        vec![
            ("SSH_ASKPASS", std::ffi::OsStr::new("/usr/bin/false")),
            ("SSH_ASKPASS_REQUIRE", std::ffi::OsStr::new("force")),
            ("DISPLAY", std::ffi::OsStr::new(":0")),
        ]
    }
    fn cancel(&self) {}
    fn capability(&self) -> &[u8] {
        b"native-no-prompt"
    }
}
impl AskPassAttemptFactory for NoPrompt {
    fn start_attempt(&self, _: &AppPaths) -> Result<AskPassAttempt, AskPassUnavailable> {
        Ok(AskPassAttempt {
            lease: AskPassBrokerLease::new(Arc::new(NoPrompt)),
            observation: AskPassAttemptObservation::default(),
        })
    }
}
#[derive(Clone, Default)]
struct ObservedAdapter {
    spawns: Arc<std::sync::atomic::AtomicUsize>,
    last_exit: Arc<std::sync::atomic::AtomicI32>,
}
impl SshProcessAdapter for ObservedAdapter {
    type Process = crate::platform::unix_ssh_process::UnixSshProcess;
    fn spawn(
        &self,
        request: super::super::process::SshProcessSpawnRequest,
    ) -> Result<
        super::super::process::SpawnedSshProcess<Self::Process>,
        super::super::process::SshProcessMechanismError,
    > {
        self.spawns
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let result = crate::platform::unix_ssh_process::UnixSshProcessAdapter.spawn(request);
        if let Err(error) = &result {
            eprintln!("native spawn classification: {error:?}");
        }
        result
    }
    fn try_status(
        &self,
        process: &mut Self::Process,
    ) -> Result<
        Option<super::super::process::ProcessExit>,
        super::super::process::SshProcessMechanismError,
    > {
        let status =
            crate::platform::unix_ssh_process::UnixSshProcessAdapter.try_status(process)?;
        if let Some(exit) = status {
            self.last_exit.store(
                exit.code().unwrap_or(-1),
                std::sync::atomic::Ordering::SeqCst,
            );
        }
        Ok(status)
    }
    fn signal(
        &self,
        process: &mut Self::Process,
        signal: super::super::process::ProcessSignal,
    ) -> Result<(), super::super::process::SshProcessMechanismError> {
        crate::platform::unix_ssh_process::UnixSshProcessAdapter.signal(process, signal)
    }
    fn reap(
        &self,
        process: Self::Process,
    ) -> Result<(), super::super::process::SshProcessMechanismError> {
        crate::platform::unix_ssh_process::UnixSshProcessAdapter.reap(process)
    }
}
struct NativeBroker;
impl crate::ssh::askpass::AskPassPresenter for NativeBroker {
    fn cancel_active(&self) {}
    fn present(
        &self,
        _: crate::ssh::askpass::prompt::AskPassRequest,
        _: &std::sync::atomic::AtomicBool,
    ) -> Result<
        crate::ssh::askpass::AskPassProtocolReply,
        crate::ssh::askpass::AskPassPresentationFailure,
    > {
        Err(crate::ssh::askpass::AskPassPresentationFailure::Unavailable)
    }
}
impl AskPassAttemptFactory for NativeBroker {
    fn start_attempt(&self, paths: &AppPaths) -> Result<AskPassAttempt, AskPassUnavailable> {
        let attempt = crate::ssh::askpass::start_attempt_with_presenter(
            paths,
            std::env::current_exe().unwrap(),
            &crate::platform::unix_askpass_transport::UnixAskPassLocalIpc,
            Arc::new(NativeBroker),
        );
        eprintln!("native broker bound: {}", attempt.is_ok());
        attempt
    }
}
/// Explicit env selects a disposable account. This test never invokes Stop or Start.
#[gpui::test]
#[ignore = "requires an explicitly selected disposable OpenSSH destination and signed artifacts"]
fn native_account_server_connect(cx: &mut gpui::TestAppContext) {
    use crate::platform::{
        app_directories::{AppDirectories, AppDirectoryEnvironment},
        unix_local_socket::UnixControlSocketProbe,
        unix_secure_filesystem::UnixSecureFilesystem,
    };
    let destination = SshDestination::new(
        std::env::var("SPACETERM_SSH_NATIVE_DESTINATION").expect("explicit destination required"),
    )
    .unwrap();
    let expected =
        std::env::var("SPACETERM_SSH_NATIVE_EXPECT").unwrap_or_else(|_| "stopped".into());
    let root = std::fs::canonicalize("/tmp")
        .unwrap()
        .join(format!("sts-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let env = AppDirectoryEnvironment {
        home: Some(home.clone().into()),
        xdg_config_home: Some(root.join("config").into()),
        xdg_data_home: Some(root.join("data").into()),
        xdg_state_home: Some(root.join("state").into()),
        xdg_cache_home: Some(root.join("cache").into()),
        xdg_runtime_dir: Some(root.join("runtime").into()),
        ..Default::default()
    };
    let actual_paths = std::env::var_os("SPACETERM_SSH_NATIVE_REAL_PATHS").as_deref()
        == Some(std::ffi::OsStr::new("1"));
    let directories = if actual_paths {
        assert!(
            std::env::var_os("SPACETERM_SSH_NATIVE_CONFIG").is_none(),
            "actual configuration is read only"
        );
        AppDirectories::resolve(identity()).unwrap()
    } else {
        AppDirectories::resolve_xdg("spaceterm-development", &env, Some(root.clone())).unwrap()
    };
    let paths = Arc::new(
        AppPaths::from_directories(directories, 103, Arc::new(UnixSecureFilesystem)).unwrap(),
    );
    paths
        .ensure_secure_root(crate::platform::app_directories::AppDirectoryRoot::Config)
        .unwrap();
    if let Some(configuration) = std::env::var_os("SPACETERM_SSH_NATIVE_CONFIG") {
        std::fs::write(
            paths.managed_ssh_config(),
            std::fs::read(configuration).unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(
            paths.managed_ssh_config(),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let adapter = ObservedAdapter::default();
    let backend = NativeSshConnectBackend::new(NativeSshServerConfig {
        paths,
        home,
        startup: StartupSshEnvironment::from_environment(
            |key| std::env::var_os(key),
            "/usr/bin:/bin".into(),
        )
        .unwrap(),
        executable: OpenSshExecutable::new("/usr/bin/ssh".into()).unwrap(),
        adapter: adapter.clone(),
        socket_probe: Arc::new(UnixControlSocketProbe),
        askpass: if actual_paths {
            Arc::new(NativeBroker)
        } else {
            Arc::new(NoPrompt)
        },
        executor: cx.background_executor.clone(),
        artifacts: SshServerArtifacts::new(
            std::env::var_os("SPACETERM_SSH_SERVER_ARTIFACTS").map(PathBuf::from),
        ),
        filesystem: Arc::new(
            crate::platform::unix_host_config_filesystem::UnixHostConfigFilesystem,
        ),
        bootstrap: Arc::new(crate::platform::unix_ssh_process::UnixSshProcessAdapter),
    });
    let cancellation = SshCancellationToken::default();
    // Check Status reads the account's state without preparing, starting or installing anything.
    let observed = backend.observe(&destination, None, &cancellation, &|phase| {
        eprintln!("native check phase: {phase:?}")
    });
    backend.disconnect(&destination);
    match (expected.as_str(), &observed) {
        ("stopped", Err(SshConnectFailure::Stopped)) => {}
        ("missing", Ok(observation))
            if observation.availability == ServerAvailability::NotInstalled => {}
        ("ready", Ok(observation)) if observation.availability == ServerAvailability::Running => {
            assert!(observation.facts.version.is_some());
        }
        (_, Err(error)) => panic!("unexpected check classification: {}", failure_name(error)),
        _ => panic!("unexpected check availability"),
    }
    let mut result = backend.connect(
        &destination,
        ConnectMode::Requested,
        None,
        &cancellation,
        &|phase| eprintln!("native connect phase: {phase:?}"),
    );
    if let Err(SshConnectFailure::InstallationOffered(offer)) = result {
        if std::env::var_os("SPACETERM_SSH_NATIVE_INSTALL").as_deref()
            == Some(std::ffi::OsStr::new("1"))
        {
            result = offer.installation.install(&cancellation, &|_| {});
        } else {
            result = Err(SshConnectFailure::InstallationOffered(offer));
        }
    }
    eprintln!(
        "native observations: spawns={}, last_exit={}",
        adapter.spawns.load(std::sync::atomic::Ordering::SeqCst),
        adapter.last_exit.load(std::sync::atomic::Ordering::SeqCst)
    );
    match (expected.as_str(), result) {
        ("stopped", Err(SshConnectFailure::Stopped)) => {}
        ("missing", Err(SshConnectFailure::InstallationOffered(_))) => {}
        ("ready", Ok(connected)) => {
            assert_eq!(connected.server.refresh().unwrap().protocol_major, 1);
            let reused = backend
                .connect(
                    &destination,
                    ConnectMode::QuietContinuation,
                    Some(&connected.continuity),
                    &cancellation,
                    &|_| {},
                )
                .unwrap_or_else(|_| panic!("existing route did not quietly reconnect"));
            assert_eq!(reused.server.refresh().unwrap().protocol_major, 1);
            let observed = adapter.spawns.load(std::sync::atomic::Ordering::SeqCst);
            for _ in 0..4 {
                assert!(connected.lease.is_alive());
            }
            assert_eq!(
                adapter.spawns.load(std::sync::atomic::Ordering::SeqCst),
                observed,
                "idle health observations must reuse their authenticated transport"
            );
            // Kill only this test's master. Eligible direct software keys recover automatically.
            backend.disconnect(&destination);
            assert!(
                !connected.lease.is_alive(),
                "closed original route must become unavailable"
            );
            let recovered = backend.connect(
                &destination,
                ConnectMode::QuietContinuation,
                Some(&connected.continuity),
                &cancellation,
                &|_| {},
            );
            if std::env::var_os("SPACETERM_SSH_NATIVE_QUIET").as_deref()
                == Some(std::ffi::OsStr::new("1"))
            {
                if let Err(error) = recovered {
                    panic!(
                        "eligible direct route did not reconnect after master loss: {}",
                        failure_name(&error)
                    );
                }
            } else {
                assert!(
                    matches!(recovered, Err(SshConnectFailure::NeedsPerson(_))),
                    "unproven route attempted background authentication"
                );
            }
        }
        (_, Err(error)) => panic!("unexpected classification: {}", failure_name(&error)),
        _ => panic!("unexpected connected result"),
    }
    backend.disconnect(&destination);
    let cancelled = SshCancellationToken::cancelled();
    assert!(matches!(
        backend.connect(
            &destination,
            ConnectMode::Requested,
            None,
            &cancelled,
            &|_| {}
        ),
        Err(SshConnectFailure::Cancelled)
    ));
    drop(backend);
    let _ = std::fs::remove_dir_all(root);
}
fn failure_name(error: &SshConnectFailure) -> &'static str {
    match error {
        SshConnectFailure::Cancelled => "cancelled",
        SshConnectFailure::Stopped => "stopped",
        SshConnectFailure::Unavailable(UnavailableKind::LocalIpcUnavailable) => {
            "local IPC capacity"
        }
        SshConnectFailure::Unavailable(UnavailableKind::Network) => "network",
        SshConnectFailure::Unavailable(UnavailableKind::SshFailed) => "ssh failure",
        SshConnectFailure::Unavailable(UnavailableKind::ServiceUnavailable) => {
            "service unavailable"
        }
        SshConnectFailure::Unavailable(_) => "unavailable",
        SshConnectFailure::NeedsPerson(_) => "needs person",
        SshConnectFailure::Changed(_) => "continuity changed",
        SshConnectFailure::Refused(_) => "refused",
        SshConnectFailure::Incompatible(_) => "incompatible",
        SshConnectFailure::Unconfirmed(_) => "unconfirmed",
        SshConnectFailure::InstallationOffered(_) => "offered",
    }
}
