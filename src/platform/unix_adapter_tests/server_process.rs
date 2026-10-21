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
/// One explicitly selected disposable account, its own client state and the real adapters.
struct NativeFixture {
    destination: SshDestination,
    /// What the person running this expects of the account: stopped, missing or ready.
    expected: String,
    /// The disposable client state this fixture owns and removes.
    root: PathBuf,
    /// The disposable client's own private storage, which retains its records.
    paths: Arc<AppPaths>,
    adapter: ObservedAdapter,
    backend: NativeSshConnectBackend<ObservedAdapter>,
}
fn native_fixture(cx: &mut gpui::TestAppContext, name: &str) -> NativeFixture {
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
        .join(format!("sts-{name}-{}", std::process::id()));
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
    let fixture_paths = Arc::clone(&paths);
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
    NativeFixture {
        destination,
        expected,
        root,
        paths: fixture_paths,
        adapter,
        backend,
    }
}

/// Explicit env selects a disposable account. This test never invokes Stop or Start.
#[gpui::test]
#[ignore = "requires an explicitly selected disposable OpenSSH destination and signed artifacts"]
fn native_account_server_connect(cx: &mut gpui::TestAppContext) {
    let NativeFixture {
        destination,
        expected,
        root,
        adapter,
        backend,
        ..
    } = native_fixture(cx, "connect");
    let cancellation = SshCancellationToken::default();
    // Check Status reads the account's state without preparing, starting or installing anything.
    let observed = backend.observe(&destination, None, &cancellation, &|phase| {
        eprintln!("native check phase: {phase:?}")
    });
    backend.disconnect(&destination);
    match (expected.as_str(), &observed) {
        ("stopped", Err(SshConnectFailure::Stopped(_))) => {}
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
        ("stopped", Err(SshConnectFailure::Stopped(_))) => {}
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
/// Explicit env selects a disposable account whose server is ready. This test reads one remote
/// maintenance observation and dispatches nothing: it never stops, restarts, uninstalls, stages,
/// updates or starts a server.
#[gpui::test]
#[ignore = "requires an explicitly selected disposable OpenSSH destination with a ready server"]
fn native_account_server_maintenance_observe(cx: &mut gpui::TestAppContext) {
    use crate::ssh::server_maintenance::{
        MaintenanceAuthority, MaintenanceTarget, SshMaintenanceBackend, SshMaintenanceFailure,
    };
    let NativeFixture {
        destination,
        expected,
        root,
        adapter,
        backend,
        ..
    } = native_fixture(cx, "maintenance");
    let cancellation = SshCancellationToken::default();
    // Only a connection admitted here can bind an original. Recovering an operation of a stopped
    // or uninstalled server belongs to the retained records in issue #560.
    assert_eq!(
        expected, "ready",
        "remote maintenance observation needs a ready server"
    );
    let connected = backend
        .connect(
            &destination,
            ConnectMode::Requested,
            None,
            &cancellation,
            &|phase| eprintln!("native connect phase: {phase:?}"),
        )
        .unwrap_or_else(|error| panic!("unexpected classification: {}", failure_name(&error)));
    let target = MaintenanceTarget {
        destination: destination.clone(),
        continuity: connected.continuity.clone(),
    };

    let facts = SshMaintenanceBackend::observe(
        &backend,
        &target,
        MaintenanceAuthority::Requested,
        &cancellation,
        &|phase| eprintln!("native maintenance phase: {phase:?}"),
    )
    .unwrap_or_else(|error| panic!("unexpected maintenance classification: {error:?}"));
    eprintln!(
        "native maintenance facts: status={:?}, startup={:?}, approved={:?}, activation={:?}",
        facts.status,
        facts.startup,
        facts.approved.as_ref().map(|build| &build.version),
        facts.activation.as_ref().map(|activation| activation.phase),
    );
    assert_eq!(
        (facts.server, facts.account, facts.build.as_str()),
        (
            connected.continuity.server,
            connected.continuity.account,
            connected.continuity.build.as_str()
        ),
        "the observation must come from the server the original bound"
    );
    assert!(facts.status.installed && !facts.status.stopped);

    // Background continuation reuses the admitted route without asking for authority.
    SshMaintenanceBackend::observe(
        &backend,
        &target,
        MaintenanceAuthority::Quiet,
        &cancellation,
        &|_| {},
    )
    .unwrap_or_else(|error| panic!("quiet continuation did not reuse the route: {error:?}"));

    // A server that is not the one bound pauses instead of being read.
    let changed = MaintenanceTarget {
        destination: destination.clone(),
        continuity: crate::ssh::server_connection::SshServerContinuity {
            server: [0xff; 32],
            ..connected.continuity.clone()
        },
    };
    assert!(
        matches!(
            SshMaintenanceBackend::observe(
                &backend,
                &changed,
                MaintenanceAuthority::Requested,
                &cancellation,
                &|_| {},
            ),
            Err(SshMaintenanceFailure::Changed(_))
        ),
        "a changed server must refuse before it is read"
    );

    eprintln!(
        "native maintenance observations: spawns={}, last_exit={}",
        adapter.spawns.load(std::sync::atomic::Ordering::SeqCst),
        adapter.last_exit.load(std::sync::atomic::Ordering::SeqCst)
    );
    SshMaintenanceBackend::release(&backend, &destination);
    drop(connected);
    backend.disconnect(&destination);
    drop(backend);
    let _ = std::fs::remove_dir_all(root);
}

/// Explicit env selects a disposable account whose server is ready, and a second explicit flag
/// authorizes the destructive steps. This test stops, starts, updates and resolves originals on
/// that one account. It installs no server, touches no other account and never reverses Stop
/// except through the explicit Start it performs itself.
///
/// `SPACETERM_SSH_SERVER_ARTIFACTS` must hold a verified candidate that differs from the
/// account's approved build, so the update step has an exact artifact to stage.
#[gpui::test]
#[ignore = "requires an explicitly authorized disposable OpenSSH destination with a ready server"]
fn native_account_server_maintenance_originals(cx: &mut gpui::TestAppContext) {
    use crate::ssh::server_maintenance::{
        MaintenanceAuthority, MaintenanceTarget, OperationSubmission, SshMaintenanceBackend,
        SubmittedKind,
    };
    use spaceterm_server::service::maintenance::{Action, MaintenanceRefusal, OperationResolution};
    assert_eq!(
        std::env::var_os("SPACETERM_SSH_NATIVE_MAINTAIN").as_deref(),
        Some(std::ffi::OsStr::new("1")),
        "destructive remote maintenance needs its own explicit authorization"
    );
    let NativeFixture {
        destination,
        expected,
        root,
        adapter,
        backend,
        ..
    } = native_fixture(cx, "originals");
    let cancellation = SshCancellationToken::default();
    assert_eq!(
        expected, "ready",
        "binding an original needs a ready server"
    );
    let connected = backend
        .connect(
            &destination,
            ConnectMode::Requested,
            None,
            &cancellation,
            &|_| {},
        )
        .unwrap_or_else(|error| panic!("unexpected classification: {}", failure_name(&error)));
    let target = MaintenanceTarget {
        destination: destination.clone(),
        continuity: connected.continuity.clone(),
    };
    let observe = |authority| {
        SshMaintenanceBackend::observe(&backend, &target, authority, &cancellation, &|_| {})
    };
    // Each run submits fresh originals. Reusing one identity would make the service replay its
    // retained outcome instead of dispatching, which is the guarantee this test relies on
    // elsewhere rather than something it should trip over here.
    let next_operation = || {
        let mut operation = [0; 32];
        getrandom::fill(&mut operation).expect("an operation identity");
        operation
    };

    // Describing Stop changes nothing: the server is still running afterwards.
    let stop = SshMaintenanceBackend::prepare(
        &backend,
        &target,
        Action::Stop,
        MaintenanceAuthority::Requested,
        &cancellation,
        &|phase| eprintln!("native prepare phase: {phase:?}"),
    )
    .unwrap_or_else(|error| panic!("describing Stop failed: {error:?}"));
    assert!(stop.running && !stop.stopped);
    assert!(
        observe(MaintenanceAuthority::Requested)
            .unwrap()
            .status
            .running
    );

    // The described Stop runs once and its result is validated, not assumed.
    let stopped_original = next_operation();
    let outcome = SshMaintenanceBackend::apply(
        &backend,
        &target,
        &OperationSubmission {
            operation: stopped_original,
            kind: SubmittedKind::Service(Action::Stop),
            confirmation: Some(stop.confirmation.clone()),
        },
        &cancellation,
        &|phase| eprintln!("native stop phase: {phase:?}"),
    )
    .unwrap_or_else(|error| panic!("Stop failed: {error:?}"));
    eprintln!("native stop outcome: {:?}", outcome.resolution);
    assert_eq!(outcome.resolution, OperationResolution::Completed);
    assert!(
        outcome
            .status
            .is_some_and(|status| status.stopped && !status.running)
    );

    // Resolving the same original reports it again and dispatches nothing, so a server the
    // person stopped stays stopped rather than being stopped a second time.
    let resolved = SshMaintenanceBackend::resolve(
        &backend,
        &target,
        stopped_original,
        MaintenanceAuthority::Quiet,
        &cancellation,
        &|_| {},
    )
    .unwrap_or_else(|error| panic!("resolving the original failed: {error:?}"));
    assert_eq!(resolved.operation, stopped_original);
    assert_eq!(resolved.resolution, OperationResolution::Completed);
    // Reading a stopped server still works, so its result stays understandable afterwards.
    let after = observe(MaintenanceAuthority::Requested).unwrap();
    assert!(
        after.status.stopped && !after.status.running && !after.status.enabled,
        "Stop must be durable: {:?}",
        after.status
    );

    // Explicit Start is the only operation that reverses that Stop.
    let started = SshMaintenanceBackend::apply(
        &backend,
        &target,
        &OperationSubmission {
            operation: next_operation(),
            kind: SubmittedKind::Start,
            confirmation: None,
        },
        &cancellation,
        &|phase| eprintln!("native start phase: {phase:?}"),
    )
    .unwrap_or_else(|error| panic!("Start failed: {error:?}"));
    assert_eq!(started.resolution, OperationResolution::Completed);
    let running = observe(MaintenanceAuthority::Requested).unwrap();
    assert!(running.status.running && !running.status.stopped);

    // The confirmation taken while the server was running before cannot stop the work that
    // started afterwards, even under a new original.
    let stale = SshMaintenanceBackend::apply(
        &backend,
        &target,
        &OperationSubmission {
            operation: next_operation(),
            kind: SubmittedKind::Service(Action::Stop),
            confirmation: Some(stop.confirmation.clone()),
        },
        &cancellation,
        &|_| {},
    )
    .unwrap_or_else(|error| panic!("a stale confirmation must be refused, not lost: {error:?}"));
    assert_eq!(
        stale.resolution,
        OperationResolution::Refused(MaintenanceRefusal::Changed),
        "a confirmation bound to an earlier run must not stop later work"
    );
    assert!(
        observe(MaintenanceAuthority::Requested)
            .unwrap()
            .status
            .running
    );

    // Staging transfers the exact verified candidate and describes its activation. The approved
    // build and the running server are unchanged until the activation is performed.
    let approved = observe(MaintenanceAuthority::Requested)
        .unwrap()
        .approved
        .expect("an installed server names its approved build");
    let update = SshMaintenanceBackend::prepare_update(
        &backend,
        &target,
        MaintenanceAuthority::Requested,
        &cancellation,
        &|phase| eprintln!("native staging phase: {phase:?}"),
    )
    .unwrap_or_else(|error| panic!("staging failed: {error:?}"))
    .expect("the candidate must differ from the approved build");
    let candidate = update
        .candidate
        .clone()
        .expect("an update names its candidate");
    assert_ne!(candidate.digest, approved.digest);
    let staged = observe(MaintenanceAuthority::Requested).unwrap();
    assert_eq!(
        staged.approved.as_ref().map(|build| &build.digest),
        Some(&approved.digest)
    );
    assert!(
        staged.status.running,
        "staging leaves the approved runtime running"
    );

    let activated = SshMaintenanceBackend::apply(
        &backend,
        &target,
        &OperationSubmission {
            operation: next_operation(),
            kind: SubmittedKind::Update {
                candidate: candidate.digest.clone(),
            },
            confirmation: Some(update.confirmation.clone()),
        },
        &cancellation,
        &|phase| eprintln!("native activation phase: {phase:?}"),
    )
    .unwrap_or_else(|error| panic!("activation failed: {error:?}"));
    eprintln!("native activation outcome: {:?}", activated.resolution);
    assert_eq!(activated.resolution, OperationResolution::Completed);
    let updated = observe(MaintenanceAuthority::Requested).unwrap();
    assert_eq!(
        updated.approved.as_ref().map(|build| &build.digest),
        Some(&candidate.digest),
        "the account runs the exact candidate it approved"
    );
    assert_eq!(
        updated.server, connected.continuity.server,
        "activation preserves the server's identity"
    );

    eprintln!(
        "native originals observations: spawns={}, last_exit={}",
        adapter.spawns.load(std::sync::atomic::Ordering::SeqCst),
        adapter.last_exit.load(std::sync::atomic::Ordering::SeqCst)
    );
    SshMaintenanceBackend::release(&backend, &destination);
    drop(connected);
    backend.disconnect(&destination);
    drop(backend);
    let _ = std::fs::remove_dir_all(root);
}

/// Explicit env selects a disposable account whose server is ready, and the same destructive
/// authorization as the originals test. This test loses the client that submitted an original,
/// twice, and reads what a new client makes of the retained records each time.
///
/// It leaves the account's server stopped, because that is what the original it finishes asked
/// for. Start it again explicitly when the run is over.
#[gpui::test]
#[ignore = "requires an explicitly authorized disposable OpenSSH destination with a ready server"]
fn native_remote_original_finishes_after_the_client_is_lost(cx: &mut gpui::TestAppContext) {
    use crate::ssh::server_connection::{
        RemoteConnectionStatus, SshConnectBackend, SshConnectionIntentStore, SshServerConnections,
    };
    use crate::ssh::server_maintenance::{SshMaintenanceBackend, SubmittedKind};
    use crate::ssh::server_operation_records::PrivateRemoteOperationRecords;
    use crate::ssh::server_operations::{
        RemoteMaintenanceRequest, RemoteOperationHandle, RemoteOperationState,
        RemoteOperationStore, RemoteServerOperations, SshServerOperations,
    };
    use spaceterm_server::service::maintenance::Action;
    assert_eq!(
        std::env::var_os("SPACETERM_SSH_NATIVE_MAINTAIN").as_deref(),
        Some(std::ffi::OsStr::new("1")),
        "destructive remote maintenance needs its own explicit authorization"
    );
    let NativeFixture {
        destination,
        expected,
        root,
        paths,
        backend,
        ..
    } = native_fixture(cx, "relaunch");
    assert_eq!(
        expected, "ready",
        "binding an original needs a ready server"
    );
    let backend = Arc::new(backend);
    let maintenance: Arc<dyn SshMaintenanceBackend> = backend.clone();
    let records = Arc::new(PrivateRemoteOperationRecords::new(Arc::clone(&paths)));
    let intents =
        Arc::new(crate::ssh::server_intents::PrivateSshConnectionIntents::new(Arc::clone(&paths)));
    let connections = SshServerConnections::new(
        Arc::clone(&backend) as Arc<dyn SshConnectBackend>,
        intents.clone() as Arc<dyn SshConnectionIntentStore>,
    );
    let until = |mut ready: Box<dyn FnMut() -> bool>| {
        let deadline = Instant::now() + Duration::from_secs(90);
        while Instant::now() < deadline {
            if ready() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    };
    let client = |records: &Arc<PrivateRemoteOperationRecords>| {
        SshServerOperations::new(
            Arc::clone(&maintenance),
            Arc::clone(records) as Arc<dyn RemoteOperationStore>,
            connections.clone(),
        )
    };
    let connect = || {
        connections.connect(destination.clone());
        let watched = connections.clone();
        let watching = destination.clone();
        assert!(
            until(Box::new(move || matches!(
                watched.status(&watching),
                RemoteConnectionStatus::Connected { .. }
            ))),
            "the connection that admits the identity did not establish"
        );
    };
    let describe_stop = |owner: &Arc<SshServerOperations>| {
        owner.describe(
            &destination,
            RemoteMaintenanceRequest::Service(Action::Stop),
        );
        let watched = Arc::clone(owner);
        let watching = destination.clone();
        assert!(
            until(Box::new(move || matches!(
                watched.state(&watching),
                RemoteOperationState::Described { .. }
            ))),
            "the service did not describe the Stop"
        );
        match owner.state(&destination) {
            RemoteOperationState::Described { handle, .. } => handle,
            state => panic!("a described change names its handle: {state:?}"),
        }
    };
    let lose_the_route = || {
        maintenance.release(&destination);
        connections.disconnect(&destination);
        SshConnectBackend::disconnect(backend.as_ref(), &destination);
    };
    let settled = |owner: &Arc<SshServerOperations>| {
        let watched = Arc::clone(owner);
        let watching = destination.clone();
        assert!(
            until(Box::new(move || matches!(
                watched.state(&watching),
                RemoteOperationState::Resolved { .. }
                    | RemoteOperationState::Unconfirmed { .. }
                    | RemoteOperationState::Failed { .. }
            ))),
            "the original was never settled"
        );
        owner.state(&destination)
    };

    connect();

    // An original whose dispatch never left this client is never invented by the service.
    let submitting = client(&records);
    let handle = describe_stop(&submitting);
    submitting.confirm(handle);
    // Nothing is dispatched until the record that resolves it is retained, so waiting for that
    // record makes losing the client afterwards a real interruption.
    let retained = records.clone();
    assert!(
        until(Box::new(move || {
            retained.load().is_ok_and(|records| records.len() == 1)
        })),
        "the submitted original was never retained"
    );
    drop(submitting);
    lose_the_route();

    let resuming = client(&records);
    resuming.resume_retained();
    let quiet = Arc::clone(&resuming);
    let watching = destination.clone();
    assert!(
        until(Box::new(move || !matches!(
            quiet.state(&watching),
            RemoteOperationState::Unread
        ))),
        "the new client did not read its retained original"
    );
    assert!(
        matches!(
            resuming.state(&destination),
            RemoteOperationState::NeedsPerson { .. }
        ),
        "background continuation must wait for the person, not ask: {:?}",
        resuming.state(&destination)
    );
    resuming.resume(&destination);
    eprintln!("native undispatched original: {:?}", settled(&resuming));
    drop(resuming);

    // The server is untouched, because that original never reached it.
    connect();
    let running = client(&records);
    let handle = describe_stop(&running);

    // This original completes, and the client that submitted it is lost before the person can
    // acknowledge the result.
    running.confirm(handle);
    let watched = Arc::clone(&running);
    let watching = destination.clone();
    assert!(
        until(Box::new(move || matches!(
            watched.state(&watching),
            RemoteOperationState::Resolved { .. }
        ))),
        "the Stop was never validated: {:?}",
        running.state(&destination)
    );
    drop(running);
    lose_the_route();

    // A new client presents the retained result without asking any computer.
    let reopened = client(&records);
    reopened.resume_retained();
    let watched = Arc::clone(&reopened);
    let watching = destination.clone();
    assert!(
        until(Box::new(move || matches!(
            watched.state(&watching),
            RemoteOperationState::Resolved { .. }
        ))),
        "a reloaded result did not wait for acknowledgement: {:?}",
        reopened.state(&destination)
    );
    let RemoteOperationState::Resolved { handle, kind, .. } = reopened.state(&destination) else {
        unreachable!("matched above")
    };
    assert_eq!(kind, SubmittedKind::Service(Action::Stop));

    // Removing the saved connection keeps that result and never saves the connection again.
    connections.remove(&destination);
    let removed = Arc::clone(&reopened);
    let watching = destination.clone();
    assert!(
        until(Box::new(move || removed
            .entries()
            .iter()
            .any(|entry| entry.destination == watching && entry.removed))),
        "the removed connection's result stopped being presented"
    );
    let saved = intents.clone();
    assert!(
        until(Box::new(move || saved
            .load()
            .is_ok_and(|connections| connections.is_empty()))),
        "presenting a removed connection's original must not save it again"
    );

    // Acknowledging retires the record, so nothing is left to tell.
    reopened.acknowledge(RemoteOperationHandle(handle.0));
    let acknowledged = Arc::clone(&reopened);
    assert!(
        until(Box::new(move || acknowledged.entries().is_empty())),
        "an acknowledged result stayed"
    );
    assert!(records.load().unwrap().is_empty());

    drop(reopened);
    drop(connections);
    drop(backend);
    let _ = std::fs::remove_dir_all(root);
}

fn failure_name(error: &SshConnectFailure) -> &'static str {
    match error {
        SshConnectFailure::Cancelled => "cancelled",
        SshConnectFailure::Stopped(_) => "stopped",
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
