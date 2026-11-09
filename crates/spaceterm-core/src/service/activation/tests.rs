use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct SupervisorFixture {
    approved: Mutex<PathBuf>,
    running: AtomicBool,
    stops: AtomicUsize,
    starts: AtomicUsize,
    automatic_start: AtomicBool,
    fail_stop: AtomicBool,
    fail_replace: AtomicBool,
    /// How long a dispatched Stop takes before the server exits. `None` exits at once.
    stop_takes: Mutex<Option<Duration>>,
    /// When the server dispatched to stop exits.
    exits_at: Mutex<Option<Instant>>,
    /// When the last Start was dispatched. Activation holds the service lock meanwhile.
    started_at: Mutex<Option<Instant>>,
    /// The account whose server this fixture's Start runs.
    paths: Mutex<Option<Paths>>,
    /// Runtimes whose Start runs a server that becomes ready, as its own run records it.
    ready: Mutex<Vec<PathBuf>>,
    /// Runtimes whose Start runs a server that never becomes ready.
    hangs: Mutex<Vec<PathBuf>>,
    /// Every runtime a Start was dispatched for, in order.
    started: Mutex<Vec<PathBuf>>,
    /// What a server that never becomes ready changes in the retained state meanwhile.
    meddles: Mutex<Option<Meddle>>,
    /// Start runs the server, then reports a failure, as a service manager can after launch.
    start_reports_failure: AtomicBool,
    /// Changing automatic startup fails once a Start was dispatched.
    policy_fails_after_start: AtomicBool,
    /// The runtime whose recovery helper the boot recovery entry runs, while one is armed.
    recovery: Mutex<Option<PathBuf>>,
    /// The recovery entry each registration replacement found, as `(registered, armed)`.
    replacements: Mutex<Vec<(PathBuf, Option<PathBuf>)>>,
}
impl SupervisorFixture {
    /// The server's own run: it admits itself into the service state, then marks readiness.
    fn serve(&self, ready: bool) {
        let paths = self.paths.lock().unwrap().clone().unwrap();
        let data = Directory::open(&paths.data, false).unwrap();
        {
            let _intent = data.lock("service-intent.lock", false).unwrap();
            let mut state = Service::state(&data).unwrap().unwrap();
            let run = Run {
                boot: boot().unwrap(),
                token: crate::protocol::random().unwrap(),
            };
            admit_run(&mut state, &run).unwrap();
            state.run = Some(run);
            data.write_json(c"service.json", &state).unwrap();
        }
        if ready {
            super::ready(&paths).unwrap();
        } else if let Some(meddle) = self.meddles.lock().unwrap().as_mut() {
            meddle(&data);
        }
    }
    /// The server exited, and its run recorded that it ended, as `service::run` does.
    fn exited(&self) {
        let paths = self.paths.lock().unwrap().clone().unwrap();
        let data = Directory::open(&paths.data, false).unwrap();
        let _intent = data.lock("service-intent.lock", false).unwrap();
        let mut state = Service::state(&data).unwrap().unwrap();
        state.run = None;
        data.write_json(c"service.json", &state).unwrap();
    }
}
/// What a server that never becomes ready does to the retained state while it runs.
type Meddle = Box<dyn FnMut(&Directory) + Send>;
/// How long this fixture's service manager lets a stopping server finish.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);
impl Supervisor for SupervisorFixture {
    fn startup(&self) -> ServiceStartup {
        ServiceStartup::AtLogin
    }
    fn check(&self, runtime: &Path, _: bool) -> Result<(), CoreError> {
        if *self.approved.lock().unwrap() == runtime {
            Ok(())
        } else {
            Err(CoreError::CleanupUnresolved)
        }
    }
    fn install(&self, _: &Path) -> Result<(), CoreError> {
        panic!("activation does not install a fresh service")
    }
    fn prepare_start(&self) -> Result<(), CoreError> {
        Ok(())
    }
    fn start(&self) -> Result<(), CoreError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        *self.started_at.lock().unwrap() = Some(Instant::now());
        let runtime = self.approved.lock().unwrap().clone();
        self.started.lock().unwrap().push(runtime.clone());
        let ready = self.ready.lock().unwrap().contains(&runtime);
        if !ready && !self.hangs.lock().unwrap().contains(&runtime) {
            return Err(CoreError::StartupFailed);
        }
        self.running.store(true, Ordering::SeqCst);
        self.serve(ready);
        if self.start_reports_failure.load(Ordering::SeqCst) {
            return Err(CoreError::ServiceUnavailable);
        }
        Ok(())
    }
    fn ready_deadline(&self) -> Duration {
        Duration::from_secs(1)
    }
    fn set_automatic_start(&self, enabled: bool) -> Result<(), CoreError> {
        if self.policy_fails_after_start.load(Ordering::SeqCst)
            && self.starts.load(Ordering::SeqCst) > 0
        {
            return Err(CoreError::ServiceUnavailable);
        }
        self.automatic_start.store(enabled, Ordering::SeqCst);
        Ok(())
    }
    fn stop(&self) -> Result<(), CoreError> {
        self.set_automatic_start(false)?;
        self.stops.fetch_add(1, Ordering::SeqCst);
        if self.fail_stop.load(Ordering::SeqCst) {
            return Err(CoreError::ServiceUnavailable);
        }
        match *self.stop_takes.lock().unwrap() {
            Some(takes) => *self.exits_at.lock().unwrap() = Some(Instant::now() + takes),
            None => {
                self.running.store(false, Ordering::SeqCst);
                if self.paths.lock().unwrap().is_some() {
                    self.exited();
                }
            }
        }
        Ok(())
    }
    fn stop_timeout(&self) -> Duration {
        STOP_TIMEOUT
    }
    fn uninstall(&self, _: &Path) -> Result<(), CoreError> {
        panic!("activation does not uninstall")
    }
    fn running(&self) -> Result<bool, CoreError> {
        let mut exits_at = self.exits_at.lock().unwrap();
        if exits_at.is_some_and(|at| Instant::now() >= at) {
            *exits_at = None;
            self.running.store(false, Ordering::SeqCst);
        }
        Ok(self.running.load(Ordering::SeqCst))
    }
    fn arm_recovery(&self, runtime: &Path) -> Result<(), CoreError> {
        *self.recovery.lock().unwrap() = Some(runtime.to_owned());
        Ok(())
    }
    fn disarm_recovery(&self) -> Result<(), CoreError> {
        *self.recovery.lock().unwrap() = None;
        Ok(())
    }
    fn replace(&self, previous: &Path, candidate: &Path) -> Result<(), CoreError> {
        self.replacements
            .lock()
            .unwrap()
            .push((candidate.to_owned(), self.recovery.lock().unwrap().clone()));
        let mut current = self.approved.lock().unwrap();
        assert!(current.as_path() == previous || current.as_path() == candidate);
        *current = candidate.to_owned();
        if self.fail_replace.load(Ordering::SeqCst) {
            Err(CoreError::ServiceUnavailable)
        } else {
            Ok(())
        }
    }
}

fn with_service(
    run: impl FnOnce(Service, RetainedInstallation, RetainedInstallation, Arc<SupervisorFixture>),
) {
    with_service_identity("spaceterm-development", run);
}

fn with_service_identity(
    identity: &str,
    run: impl FnOnce(Service, RetainedInstallation, RetainedInstallation, Arc<SupervisorFixture>),
) {
    use ring::signature::KeyPair;
    use runtime::test_support::{COMMIT, VERSION, target, with_artifact};
    let mut fixture = super::super::tests::Fixture::new();
    fixture.paths.identity = identity.to_owned();
    with_artifact(
        &fixture.root.join("source"),
        &fixture.paths.identity,
        |archive, signature| {
            let parent = fixture.paths.data.join("installations");
            let retain = |service: &Service| {
                service
                    .provision(archive, signature, target(), COMMIT, VERSION)
                    .unwrap()
            };
            let supervisor = Arc::new(SupervisorFixture {
                approved: Mutex::new(PathBuf::new()),
                running: AtomicBool::new(false),
                stops: AtomicUsize::new(0),
                starts: AtomicUsize::new(0),
                automatic_start: AtomicBool::new(false),
                fail_stop: AtomicBool::new(false),
                fail_replace: AtomicBool::new(false),
                stop_takes: Mutex::new(None),
                exits_at: Mutex::new(None),
                started_at: Mutex::new(None),
                paths: Mutex::new(None),
                ready: Mutex::new(Vec::new()),
                hangs: Mutex::new(Vec::new()),
                started: Mutex::new(Vec::new()),
                meddles: Mutex::new(None),
                start_reports_failure: AtomicBool::new(false),
                policy_fails_after_start: AtomicBool::new(false),
                recovery: Mutex::new(None),
                replacements: Mutex::new(Vec::new()),
            });
            let service = Service::new(
                fixture.paths.clone(),
                fixture.paths.identity.clone(),
                supervisor.clone(),
            );
            let previous = retain(&service);
            let mut bytes = std::fs::read(archive).unwrap();
            bytes[4] = 1; // A signed gzip timestamp changes the complete archive, not its files.
            std::fs::write(archive, &bytes).unwrap();
            let key = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap();
            assert_eq!(key.public_key().as_ref().len(), 32);
            std::fs::write(signature, key.sign(&bytes).as_ref()).unwrap();
            let candidate = retain(&service);
            assert_ne!(previous.directory, candidate.directory);
            assert_eq!(candidate.directory.parent(), Some(parent.as_path()));
            let data = Directory::open(&fixture.paths.data, false).unwrap();
            let mut state = Service::state(&data).unwrap().unwrap();
            state.installation = previous
                .directory
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .into();
            state.stopped = true;
            data.write_json(c"service.json", &state).unwrap();
            *supervisor.approved.lock().unwrap() = previous.directory.clone();
            run(service, previous, candidate, supervisor);
        },
    );
}

#[test]
fn stopped_activation_changes_only_the_exact_runtime_and_preserves_stop() {
    with_service(|service, _, candidate, supervisor| {
        let before = std::fs::read(service.paths.data.join("identity.json")).unwrap();
        let approval = service.prepare_activation(&candidate).unwrap().unwrap();
        let status = service.activate(approval).unwrap();
        assert_eq!(status.phase, ActivationPhase::Completed { running: false });
        assert!(service.status().unwrap().stopped);
        assert_eq!(*supervisor.approved.lock().unwrap(), candidate.directory);
        assert_eq!(supervisor.stops.load(Ordering::SeqCst), 0);
        assert_eq!(
            before,
            std::fs::read(service.paths.data.join("identity.json")).unwrap()
        );
    });
}

#[test]
fn an_install_before_a_stopped_activation_first_starts_keeps_its_rollback_installation() {
    with_service(|service, previous, candidate, _| {
        let approval = service.prepare_activation(&candidate).unwrap().unwrap();
        let status = service.activate(approval).unwrap();
        assert_eq!(status.phase, ActivationPhase::Completed { running: false });
        // A newer version stages before the approved one ever became ready.
        let source = service.paths.data.join("../newer-source");
        let (archive, signature) = runtime::test_support::with_build(
            "1.0.0",
            crate::update::facts::Protocols::CURRENT,
            || {
                runtime::test_support::with_artifact(
                    &source,
                    &service.identity,
                    |archive, signature| (archive.to_owned(), signature.to_owned()),
                )
            },
        );
        let sha256: String =
            ring::digest::digest(&ring::digest::SHA256, &std::fs::read(&archive).unwrap())
                .as_ref()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
        runtime::test_support::with_fixture_trust(|| {
            runtime::updates::Updates::open(service.clone())
                .unwrap()
                .install(runtime::updates::Archive {
                    archive: &archive,
                    signature: &signature,
                    sha256: &sha256,
                    source: runtime::updates::Source::Local,
                })
                .unwrap()
        });
        assert!(
            previous.directory.exists(),
            "the approved version's first Start may still return to it"
        );
        assert!(candidate.directory.exists());
    });
}

#[test]
fn a_launcher_that_cannot_follow_an_update_never_blocks_it() {
    use crate::service::{CliLauncher, CliLink};
    use std::os::unix::fs::PermissionsExt;
    with_service(|service, previous, candidate, supervisor| {
        let bin = service.account_home().unwrap().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let launcher = bin.join("spaceterm-development");
        let earlier = previous.directory.join(
            crate::service::runtime::Layout::host("spaceterm-development")
                .unwrap()
                .executable(),
        );
        std::os::unix::fs::symlink(&earlier, &launcher).unwrap();
        assert_eq!(service.cli_launcher(), Ok(CliLauncher::Linked));
        // Another account could write here, so SpaceTerm refuses to replace the launcher.
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o775)).unwrap();

        let approval = service.prepare_activation(&candidate).unwrap().unwrap();
        let status = service.activate(approval).unwrap();
        assert_eq!(status.phase, ActivationPhase::Completed { running: false });
        assert_eq!(*supervisor.approved.lock().unwrap(), candidate.directory);
        assert_eq!(std::fs::read_link(&launcher).unwrap(), earlier);
        assert_eq!(service.cli_launcher(), Ok(CliLauncher::Outdated));

        // Once the directory is safe again, linking follows the approved runtime.
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let unused = || -> Result<PathBuf, CoreError> {
            panic!("an approved runtime needs no provisioning")
        };
        assert_eq!(service.link_cli(unused), Ok(CliLink::Linked));
        assert_eq!(
            std::fs::read_link(&launcher).unwrap(),
            candidate.directory.join(
                crate::service::runtime::Layout::host("spaceterm-development")
                    .unwrap()
                    .executable()
            )
        );
        assert_eq!(service.cli_launcher(), Ok(CliLauncher::Linked));
    });
}

#[test]
fn a_candidate_of_another_protocol_major_activates() {
    runtime::test_support::with_major(crate::protocol::MAJOR + 1, || {
        with_service(|service, _, candidate, supervisor| {
            let approval = service.prepare_activation(&candidate).unwrap().unwrap();
            let status = service.activate(approval).unwrap();
            assert_eq!(status.phase, ActivationPhase::Completed { running: false });
            assert_eq!(*supervisor.approved.lock().unwrap(), candidate.directory);
        });
    });
}

#[test]
fn completed_nonproduction_updates_with_dirty_versions_remain_readable_after_relaunch() {
    for identity in ["spaceterm-development", "spaceterm-preflight"] {
        for dirty_previous in [false, true] {
            with_service_identity(
                identity,
                |service, mut previous, mut candidate, supervisor| {
                    // This saved-state fixture exercises the retained reader. Signed runtime
                    // admission owns identity checks and verifies the executable separately.
                    if dirty_previous {
                        previous.version.push_str(".dirty");
                    } else {
                        candidate.version.push_str(".dirty");
                    }
                    let operation = interrupted(
                        &service,
                        &previous,
                        &candidate,
                        &supervisor,
                        Step::Completed,
                        true,
                    );
                    let policy = std::fs::read(service.paths.data.join("service.json")).unwrap();
                    let reopened =
                        Service::new(service.paths.clone(), identity.into(), supervisor.clone());

                    let status = reopened.activation_status().unwrap().unwrap();
                    assert_eq!(status.operation, operation);
                    assert_eq!(status.previous, previous);
                    assert_eq!(status.candidate, candidate);
                    assert_eq!(status.phase, ActivationPhase::Completed { running: false });
                    assert_eq!(reopened.recover_activation().unwrap(), Some(status));
                    assert!(reopened.status().unwrap().stopped);
                    assert_eq!(
                        std::fs::read(service.paths.data.join("service.json")).unwrap(),
                        policy
                    );
                    assert_eq!(supervisor.starts.load(Ordering::SeqCst), 0);
                    assert_eq!(supervisor.stops.load(Ordering::SeqCst), 0);
                },
            );
        }
    }
}

#[test]
fn malformed_dirty_activation_versions_are_refused_without_dispatch_or_mutation() {
    for version in [
        "dev.bbbbbbbbbbbb.dirty",
        "dev.aaaaaaaaaaaa.dirty.dirty",
        "1.2.3.dirty",
    ] {
        with_service(|service, previous, mut candidate, supervisor| {
            candidate.version = version.into();
            interrupted(
                &service,
                &previous,
                &candidate,
                &supervisor,
                Step::Completed,
                true,
            );
            let policy = std::fs::read(service.paths.data.join("service.json")).unwrap();

            assert_eq!(service.recover_activation(), Err(CoreError::UnsafeStorage));
            assert_eq!(
                std::fs::read(service.paths.data.join("service.json")).unwrap(),
                policy
            );
            assert_eq!(supervisor.starts.load(Ordering::SeqCst), 0);
            assert_eq!(supervisor.stops.load(Ordering::SeqCst), 0);
        });
    }
}

#[test]
fn changed_stop_observation_requires_fresh_approval_without_dispatch() {
    with_service(|service, _, candidate, supervisor| {
        let approval = service.prepare_activation(&candidate).unwrap().unwrap();
        let data = Directory::open(&service.paths.data, false).unwrap();
        let mut state = Service::state(&data).unwrap().unwrap();
        state.stopped = false;
        data.write_json(c"service.json", &state).unwrap();
        assert!(matches!(
            service.activate(approval),
            Err(CoreError::Changed)
        ));
        assert_eq!(supervisor.stops.load(Ordering::SeqCst), 0);
    });
}

/// The service lane's Stop preview recovers a recorded activation first, which takes the service
/// lock. While a later operation holds it, that preview refuses instead of waiting, because the
/// running Core's shutdown would join the worker that waits.
#[test]
fn a_lane_stop_preview_refuses_while_another_operation_holds_the_service_lock() {
    with_service(|service, previous, candidate, supervisor| {
        interrupted(
            &service,
            &previous,
            &candidate,
            &supervisor,
            Step::Completed,
            false,
        );
        let data = Directory::open(&service.paths.data, false).unwrap();
        let (held, locked) = std::sync::mpsc::channel();
        let operation = std::thread::spawn(move || {
            let _operation = data.lock("service.lock", false).unwrap();
            held.send(()).unwrap();
            std::thread::sleep(Duration::from_secs(3));
        });
        locked.recv().unwrap();
        let started = Instant::now();
        let prepared = super::super::control::Controller::new(service.clone())
            .prepare_with(super::super::control::Action::Stop, Contention::Refuse);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the preview waited for the service lock"
        );
        assert!(matches!(prepared, Err(CoreError::ServiceBusy)));
        operation.join().unwrap();
    });
}

// These fixtures are saved post-fsync states, not claims of a native supervisor interruption.
fn interrupted(
    service: &Service,
    previous: &RetainedInstallation,
    candidate: &RetainedInstallation,
    supervisor: &SupervisorFixture,
    step: Step,
    stopped: bool,
) -> [u8; 32] {
    let data = Directory::open(&service.paths.data, false).unwrap();
    let mut state = Service::state(&data).unwrap().unwrap();
    let id = crate::protocol::random().unwrap();
    let old = Build::from_installation(previous).unwrap();
    let new = Build::from_installation(candidate).unwrap();
    state.installation = if matches!(
        step,
        Step::Approved | Step::StartDispatched | Step::Completed
    ) {
        new.digest.clone()
    } else {
        old.digest.clone()
    };
    state.stopped = stopped;
    state.activation = Some(Operation {
        schema: 1,
        id,
        previous: old,
        candidate: new,
        stopped,
        original_run: state.run.clone(),
        candidate_run: None,
        step,
        saved: if matches!(step, Step::Admitted | Step::StopDispatched) {
            None
        } else {
            Some(take_snapshot(&data).unwrap())
        },
        started_at: None,
        ready: false,
        failed: false,
        failure: matches!(
            step,
            Step::ReturnApproved | Step::ReturnStartDispatched | Step::Returned
        )
        .then_some(StartFailure::NotReady),
    });
    data.write_json(c"service.json", &state).unwrap();
    *supervisor.approved.lock().unwrap() = if matches!(
        step,
        Step::RegistrationReplaced | Step::Approved | Step::StartDispatched | Step::Completed
    ) {
        candidate.directory.clone()
    } else {
        previous.directory.clone()
    };
    id
}

#[test]
fn passive_inventory_preserves_an_admitted_operation_without_dispatch_or_setup() {
    with_service(|service, previous, candidate, supervisor| {
        interrupted(
            &service,
            &previous,
            &candidate,
            &supervisor,
            Step::Admitted,
            false,
        );
        supervisor.running.store(true, Ordering::SeqCst);
        let policy = std::fs::read(service.paths.data.join("service.json")).unwrap();
        let identity = std::fs::read(service.paths.data.join("identity.json")).unwrap();
        assert_eq!(
            service.passive_status(),
            Err(CoreError::ActivationUnresolved)
        );
        assert!(service.passive_machine_id().unwrap().is_some());
        assert_eq!(supervisor.stops.load(Ordering::SeqCst), 0);
        assert_eq!(supervisor.starts.load(Ordering::SeqCst), 0);
        assert_eq!(*supervisor.approved.lock().unwrap(), previous.directory);
        assert_eq!(
            std::fs::read(service.paths.data.join("service.json")).unwrap(),
            policy
        );
        assert_eq!(
            std::fs::read(service.paths.data.join("identity.json")).unwrap(),
            identity
        );

        std::fs::remove_file(service.paths.data.join("identity.json")).unwrap();
        assert_eq!(service.passive_machine_id().unwrap(), None);
        assert!(!service.paths.data.join("identity.json").exists());
    });
}

#[test]
fn stopped_recovery_resolves_each_prelaunch_boundary_without_starting_or_stopping() {
    for step in [
        Step::Admitted,
        Step::StopDispatched,
        Step::Quiescent,
        Step::RegistrationReplaced,
        Step::Approved,
    ] {
        with_service(|service, previous, candidate, supervisor| {
            let id = interrupted(&service, &previous, &candidate, &supervisor, step, true);
            let status = service.recover_activation().unwrap().unwrap();
            assert_eq!(
                (status.operation, status.phase),
                (id, ActivationPhase::Completed { running: false })
            );
            assert_eq!(*supervisor.approved.lock().unwrap(), candidate.directory);
            assert_eq!(
                (
                    supervisor.stops.load(Ordering::SeqCst),
                    supervisor.starts.load(Ordering::SeqCst)
                ),
                (0, 0)
            );
            assert!(service.status().unwrap().stopped);
        });
    }
}

#[test]
fn stop_dispatch_gap_refuses_repeated_dispatch_and_ordinary_lifecycle_consent() {
    with_service(|service, previous, candidate, supervisor| {
        let id = interrupted(
            &service,
            &previous,
            &candidate,
            &supervisor,
            Step::StopDispatched,
            false,
        );
        supervisor.running.store(true, Ordering::SeqCst);
        for result in [
            service.recover_activation().map(|_| ()),
            service.start().map(|_| ()),
            service.restart().map(|_| ()),
        ] {
            assert_eq!(result, Err(CoreError::ActivationUnresolved));
        }
        let status = service.activation_status().unwrap().unwrap();
        assert_eq!(
            (status.operation, status.phase),
            (id, ActivationPhase::Uncertain)
        );
        assert_eq!(supervisor.stops.load(Ordering::SeqCst), 0);
        assert_eq!(*supervisor.approved.lock().unwrap(), previous.directory);
        assert_eq!(
            check_start(&service.paths),
            Err(CoreError::ActivationUnresolved)
        );
    });
}

#[test]
fn admitted_running_operation_stops_once_and_a_refused_start_returns_without_redispatch() {
    with_service(|service, previous, candidate, supervisor| {
        *supervisor.paths.lock().unwrap() = Some(service.paths.clone());
        supervisor
            .ready
            .lock()
            .unwrap()
            .push(previous.directory.clone());
        let id = interrupted(
            &service,
            &previous,
            &candidate,
            &supervisor,
            Step::Admitted,
            false,
        );
        supervisor.running.store(true, Ordering::SeqCst);
        let status = service.recover_activation().unwrap().unwrap();
        assert_eq!(
            (status.operation, status.phase),
            (
                id,
                ActivationPhase::Returned {
                    failure: StartFailure::NotReady,
                    running: true
                }
            )
        );
        service.recover_activation().unwrap();
        assert_eq!(supervisor.stops.load(Ordering::SeqCst), 1);
        assert_eq!(
            *supervisor.started.lock().unwrap(),
            [candidate.directory.clone(), previous.directory.clone()]
        );
        assert_eq!(*supervisor.approved.lock().unwrap(), previous.directory);
    });
}

/// The service manager takes most of its stop timeout to stop the old server, as systemd took
/// 18 s on a loaded machine. The update waits for it and goes on to start the candidate.
#[test]
fn update_waits_for_a_slow_stop_within_the_service_managers_timeout() {
    with_service(|service, _, candidate, supervisor| {
        set_running(&service, &supervisor);
        *supervisor.stop_takes.lock().unwrap() = Some(STOP_TIMEOUT * 3 / 4);
        let approval = service.prepare_activation(&candidate).unwrap().unwrap();
        // The fixture's Start fails, so reaching it proves the stop was confirmed. The return
        // then starts the previous version, whose Start fails as well.
        assert_eq!(service.activate(approval), Err(CoreError::StartupFailed));
        let status = service.activation_status().unwrap().unwrap();
        assert_eq!(status.phase, ActivationPhase::Uncertain);
        assert_eq!(
            (
                supervisor.stops.load(Ordering::SeqCst),
                *supervisor.started.lock().unwrap().first().unwrap() == candidate.directory
            ),
            (1, true)
        );
    });
}

/// A stop that outlasts the service manager's timeout leaves the update unresolved at that
/// timeout. Recovery refuses until the old server exits, then finishes without another Stop.
#[test]
fn stop_past_the_service_managers_timeout_stays_recoverable_without_another_stop() {
    with_service(|service, previous, candidate, supervisor| {
        set_running(&service, &supervisor);
        *supervisor.stop_takes.lock().unwrap() = Some(Duration::from_secs(3600));
        let approval = service.prepare_activation(&candidate).unwrap().unwrap();
        let started = Instant::now();
        assert_eq!(
            service.activate(approval),
            Err(CoreError::CleanupUnresolved)
        );
        let waited = started.elapsed();
        assert!(
            waited >= STOP_TIMEOUT && waited < STOP_TIMEOUT + Duration::from_secs(5),
            "the service manager's stop timeout is the deadline, waited {waited:?}"
        );
        let status = service.activation_status().unwrap().unwrap();
        assert_eq!(status.phase, ActivationPhase::Uncertain);
        assert_eq!(*supervisor.approved.lock().unwrap(), previous.directory);
        assert_eq!(
            service.recover_activation(),
            Err(CoreError::ActivationUnresolved)
        );

        *supervisor.exits_at.lock().unwrap() = Some(Instant::now());
        // The candidate's Start fails, and so does the previous version's after the return.
        assert_eq!(service.recover_activation(), Err(CoreError::StartupFailed));
        assert_eq!(supervisor.stops.load(Ordering::SeqCst), 1);
        assert_eq!(
            *supervisor.started.lock().unwrap(),
            [candidate.directory.clone(), previous.directory.clone()]
        );
    });
}

/// A client that connects while an update stops the old server waits for the update's outcome.
/// It never reports the update unfinished while the update still runs.
#[test]
fn admission_during_an_update_waits_for_its_outcome() {
    with_service(|service, _, candidate, supervisor| {
        set_running(&service, &supervisor);
        *supervisor.stop_takes.lock().unwrap() = Some(STOP_TIMEOUT / 2);
        let approval = service.prepare_activation(&candidate).unwrap().unwrap();
        let paths = service.paths.clone();
        let client = std::thread::spawn(move || {
            let data = Directory::open(&paths.data, false).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while Service::state(&data).unwrap().unwrap().activation.is_none() {
                assert!(Instant::now() < deadline, "the update never began");
                std::thread::sleep(Duration::from_millis(5));
            }
            let admission = crate::service::check_admission(&paths, Duration::from_secs(10), None);
            (admission, Instant::now())
        });
        // The fixture's Start always fails after the update dispatched it.
        assert_eq!(service.activate(approval), Err(CoreError::StartupFailed));
        let (admission, answered) = client.join().unwrap();
        // The update dispatched Start while it held the service lock, near its end.
        let started = supervisor.started_at.lock().unwrap().unwrap();
        assert!(
            answered > started,
            "admission answered before the update ended"
        );
        // A dispatched candidate start admits clients, which then find whether it runs.
        assert_eq!(admission, Ok(()));
    });
}

/// The approved server runs, as it does before an ordinary update.
fn set_running(service: &Service, supervisor: &SupervisorFixture) {
    let data = Directory::open(&service.paths.data, false).unwrap();
    let mut state = Service::state(&data).unwrap().unwrap();
    state.stopped = false;
    data.write_json(c"service.json", &state).unwrap();
    supervisor.running.store(true, Ordering::SeqCst);
}

#[test]
fn failed_native_stop_is_not_repeated_after_lost_reply() {
    with_service(|service, previous, candidate, supervisor| {
        interrupted(
            &service,
            &previous,
            &candidate,
            &supervisor,
            Step::Admitted,
            false,
        );
        supervisor.running.store(true, Ordering::SeqCst);
        supervisor.fail_stop.store(true, Ordering::SeqCst);
        assert_eq!(
            service.recover_activation(),
            Err(CoreError::ServiceUnavailable)
        );
        supervisor.fail_stop.store(false, Ordering::SeqCst);
        assert_eq!(
            service.recover_activation(),
            Err(CoreError::ActivationUnresolved)
        );
        assert_eq!(supervisor.stops.load(Ordering::SeqCst), 1);
        assert_eq!(*supervisor.approved.lock().unwrap(), previous.directory);
    });
}

#[test]
fn published_registration_with_failed_reload_resumes_only_the_same_candidate() {
    with_service(|service, previous, candidate, supervisor| {
        let id = interrupted(
            &service,
            &previous,
            &candidate,
            &supervisor,
            Step::Quiescent,
            true,
        );
        supervisor.fail_replace.store(true, Ordering::SeqCst);
        assert_eq!(
            service.recover_activation(),
            Err(CoreError::ServiceUnavailable)
        );
        assert_eq!(*supervisor.approved.lock().unwrap(), candidate.directory);
        let data = Directory::open(&service.paths.data, false).unwrap();
        assert_eq!(
            service.runtime(&Service::state(&data).unwrap().unwrap()),
            previous.directory
        );
        supervisor.fail_replace.store(false, Ordering::SeqCst);
        let status = service.recover_activation().unwrap().unwrap();
        assert_eq!(
            (status.operation, status.phase),
            (id, ActivationPhase::Completed { running: false })
        );
        assert_eq!(supervisor.stops.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn readiness_and_lost_reply_never_repeat_dispatch_or_offer_return() {
    with_service(|service, previous, candidate, supervisor| {
        let id = interrupted(
            &service,
            &previous,
            &candidate,
            &supervisor,
            Step::StartDispatched,
            false,
        );
        let data = Directory::open(&service.paths.data, false).unwrap();
        let mut state = Service::state(&data).unwrap().unwrap();
        let run = Run {
            boot: boot().unwrap(),
            token: crate::protocol::random().unwrap(),
        };
        admit_run(&mut state, &run).unwrap();
        state.run = Some(run);
        data.write_json(c"service.json", &state).unwrap();
        ready(&service.paths).unwrap();
        supervisor.running.store(true, Ordering::SeqCst);
        let status = service.recover_activation().unwrap().unwrap();
        assert_eq!(
            (status.operation, status.phase),
            (id, ActivationPhase::Completed { running: true })
        );
        assert_eq!(
            (
                supervisor.stops.load(Ordering::SeqCst),
                supervisor.starts.load(Ordering::SeqCst)
            ),
            (0, 0)
        );
    });
}

#[test]
fn failed_candidate_never_reexecutes_even_after_a_simulated_boot_change() {
    with_service(|service, previous, candidate, supervisor| {
        interrupted(
            &service,
            &previous,
            &candidate,
            &supervisor,
            Step::StartDispatched,
            false,
        );
        let data = Directory::open(&service.paths.data, false).unwrap();
        let mut state = Service::state(&data).unwrap().unwrap();
        let run = Run {
            boot: "simulated-previous-boot".into(),
            token: crate::protocol::random().unwrap(),
        };
        admit_run(&mut state, &run).unwrap();
        state.run = Some(run);
        data.write_json(c"service.json", &state).unwrap();
        let mut recovered = Service::state(&data).unwrap().unwrap();
        let retry = Run {
            boot: boot().unwrap(),
            token: crate::protocol::random().unwrap(),
        };
        assert_eq!(
            admit_run(&mut recovered, &retry),
            Err(CoreError::ActivationUnresolved)
        );
        *supervisor.paths.lock().unwrap() = Some(service.paths.clone());
        supervisor
            .ready
            .lock()
            .unwrap()
            .push(previous.directory.clone());
        let status = service.recover_activation().unwrap().unwrap();
        assert!(matches!(status.phase, ActivationPhase::Returned { .. }));
        assert_eq!(
            *supervisor.started.lock().unwrap(),
            std::slice::from_ref(&previous.directory),
            "the candidate never starts again"
        );
    });
}

#[test]
fn same_boot_cleanup_uncertainty_is_preserved_but_prior_boot_cleanup_can_finish() {
    with_service(|service, previous, candidate, supervisor| {
        let id = interrupted(
            &service,
            &previous,
            &candidate,
            &supervisor,
            Step::StopDispatched,
            false,
        );
        let data = Directory::open(&service.paths.data, false).unwrap();
        let mut state = Service::state(&data).unwrap().unwrap();
        let run = Run {
            boot: boot().unwrap(),
            token: crate::protocol::random().unwrap(),
        };
        state.run = Some(run.clone());
        let operation = state.activation.as_mut().unwrap();
        operation.original_run = Some(run);
        operation.failed = true;
        data.write_json(c"service.json", &state).unwrap();
        assert_eq!(
            service.recover_activation(),
            Err(CoreError::CleanupUnresolved)
        );
        let mut state = Service::state(&data).unwrap().unwrap();
        state.run.as_mut().unwrap().boot = "simulated-previous-boot".into();
        state.activation.as_mut().unwrap().original_run = state.run.clone();
        data.write_json(c"service.json", &state).unwrap();
        assert_eq!(service.recover_activation(), Err(CoreError::StartupFailed));
        assert_eq!(service.activation_status().unwrap().unwrap().operation, id);
        assert_eq!(supervisor.stops.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn stale_approval_and_completed_recovery_preserve_later_policy() {
    with_service(|service, _, candidate, supervisor| {
        let stale = service.prepare_activation(&candidate).unwrap().unwrap();
        let current = service.prepare_activation(&candidate).unwrap().unwrap();
        let status = service.activate(current).unwrap();
        assert!(matches!(service.activate(stale), Err(CoreError::Changed)));
        let data = Directory::open(&service.paths.data, false).unwrap();
        let mut state = Service::state(&data).unwrap().unwrap();
        state.stopped = false; // A later explicit lifecycle action owns this policy.
        data.write_json(c"service.json", &state).unwrap();
        let before = std::fs::read(service.paths.data.join("service.json")).unwrap();
        assert_eq!(
            service.recover_activation().unwrap().unwrap().operation,
            status.operation
        );
        assert_eq!(
            before,
            std::fs::read(service.paths.data.join("service.json")).unwrap()
        );
        assert_eq!(supervisor.stops.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn unknown_safety_intent_and_contradictory_target_are_refused_without_mutation() {
    with_service(|service, previous, candidate, supervisor| {
        interrupted(
            &service,
            &previous,
            &candidate,
            &supervisor,
            Step::Quiescent,
            true,
        );
        let path = service.paths.data.join("service.json");
        let bytes = std::fs::read(&path).unwrap();
        let data = Directory::open(&service.paths.data, false).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["activation"]["future_authority"] = true.into();
        data.write_json(c"service.json", &value).unwrap();
        let invalid = std::fs::read(&path).unwrap();
        assert_eq!(service.recover_activation(), Err(CoreError::UnsafeStorage));
        assert_eq!(std::fs::read(&path).unwrap(), invalid);
        value["activation"]
            .as_object_mut()
            .unwrap()
            .remove("future_authority");
        value["installation"] = candidate
            .directory
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .into();
        data.write_json(c"service.json", &value).unwrap();
        assert_eq!(service.recover_activation(), Err(CoreError::UnsafeStorage));
        assert_eq!(supervisor.stops.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn confirmation_token_changes_with_run_stop_and_candidate_observations() {
    with_service(|service, _, candidate, supervisor| {
        let token = |candidate: &RetainedInstallation| {
            service
                .prepare_activation(candidate)
                .unwrap()
                .unwrap()
                .confirmation_token()
                .unwrap()
        };
        let stopped = token(&candidate);
        assert_eq!(stopped.len(), 64);
        let data = Directory::open(&service.paths.data, false).unwrap();
        let mut state = Service::state(&data).unwrap().unwrap();
        state.stopped = false;
        data.write_json(c"service.json", &state).unwrap();
        let idle = token(&candidate);
        assert_ne!(stopped, idle, "stop policy must be part of the observation");
        supervisor.running.store(true, Ordering::SeqCst);
        let running = token(&candidate);
        assert_ne!(idle, running, "running must be part of the observation");
        use runtime::test_support::{COMMIT, VERSION, target, with_artifact};
        with_artifact(
            &service.paths.data.join("third-source"),
            &service.identity,
            |archive, signature| {
                let mut bytes = std::fs::read(archive).unwrap();
                bytes[4] = 2;
                let key = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap();
                std::fs::write(archive, &bytes).unwrap();
                std::fs::write(signature, key.sign(&bytes).as_ref()).unwrap();
                let other = service
                    .provision(archive, signature, target(), COMMIT, VERSION)
                    .unwrap();
                assert_ne!(other.directory, candidate.directory);
                assert_ne!(
                    running,
                    token(&other),
                    "candidate must be part of the observation"
                );
            },
        );
    });
}

#[test]
fn released_version_order_refuses_downgrades_without_inventing_development_order() {
    assert!(is_released_downgrade("1.9.0", "1.10.0"));
    assert!(is_released_downgrade("1.0.0", "18446744073709551616.0.0"));
    assert!(!is_released_downgrade("2.0.0", "1.10.0"));
    assert!(!is_released_downgrade(
        "dev.aaaaaaaaaaaa",
        "dev.bbbbbbbbbbbb"
    ));
}

#[test]
fn a_stopped_updates_failed_first_start_returns_to_the_previous_version() {
    with_service(|service, previous, candidate, supervisor| {
        *supervisor.paths.lock().unwrap() = Some(service.paths.clone());
        supervisor
            .ready
            .lock()
            .unwrap()
            .push(previous.directory.clone());
        let approval = service.prepare_activation(&candidate).unwrap().unwrap();
        service.activate(approval).unwrap();
        assert_eq!(service.start(), Err(CoreError::StartupFailed));
        assert_eq!(
            service.activation_status().unwrap().unwrap().phase,
            ActivationPhase::Returned {
                failure: StartFailure::NotReady,
                running: true
            }
        );
        let data = Directory::open(&service.paths.data, false).unwrap();
        assert_eq!(
            service.runtime(&Service::state(&data).unwrap().unwrap()),
            previous.directory
        );
    });
}

#[test]
fn a_stopped_updates_first_start_that_reaches_ready_completes_it() {
    with_service(|service, _, candidate, supervisor| {
        let approval = service.prepare_activation(&candidate).unwrap().unwrap();
        service.activate(approval).unwrap();
        let data = Directory::open(&service.paths.data, false).unwrap();
        let mut state = Service::state(&data).unwrap().unwrap();
        state.stopped = false;
        let run = Run {
            boot: boot().unwrap(),
            token: crate::protocol::random().unwrap(),
        };
        admit_run(&mut state, &run).unwrap();
        state.run = Some(run);
        data.write_json(c"service.json", &state).unwrap();
        ready(&service.paths).unwrap();
        supervisor.running.store(true, Ordering::SeqCst);
        assert_eq!(
            service.activation_status().unwrap().unwrap().phase,
            ActivationPhase::Completed { running: true }
        );
    });
}

/// Runs the update from a running previous version whose own Start becomes ready again.
fn update_running(
    service: &Service,
    previous: &RetainedInstallation,
    candidate: &RetainedInstallation,
    supervisor: &SupervisorFixture,
) -> Result<ActivationStatus, CoreError> {
    *supervisor.paths.lock().unwrap() = Some(service.paths.clone());
    supervisor
        .ready
        .lock()
        .unwrap()
        .push(previous.directory.clone());
    set_running(service, supervisor);
    let approval = service.prepare_activation(candidate).unwrap().unwrap();
    service.activate(approval)
}

fn update_facts(service: &Service) -> crate::update::facts::UpdateFacts {
    runtime::updates::Updates::stored(service).unwrap()
}

#[test]
fn a_candidate_that_never_becomes_ready_returns_with_the_retained_state_it_changed_restored() {
    with_service(|service, previous, candidate, supervisor| {
        let identity = std::fs::read(service.paths.data.join("identity.json")).unwrap();
        supervisor
            .hangs
            .lock()
            .unwrap()
            .push(candidate.directory.clone());
        // The candidate rewrites the identity, creates the organization and direct state, then
        // hangs.
        *supervisor.meddles.lock().unwrap() = Some(Box::new(|data: &Directory| {
            let mut record = data.read().unwrap();
            record.core = [42; 32];
            data.write(&record).unwrap();
            data.write_json(
                c"organization.json",
                &crate::hierarchy::State::first("/".as_ref())
                    .unwrap()
                    .envelope(),
            )
            .unwrap();
            data.write_json(c"direct.json", &serde_json::Value::Null)
                .unwrap();
        }));
        let status = update_running(&service, &previous, &candidate, &supervisor).unwrap();
        assert_eq!(
            status.phase,
            ActivationPhase::Returned {
                failure: StartFailure::NotReady,
                running: true
            }
        );
        assert_eq!(
            std::fs::read(service.paths.data.join("identity.json")).unwrap(),
            identity
        );
        assert!(!service.paths.data.join("direct.json").exists());
        assert!(!service.paths.data.join("organization.json").exists());
        for (_, copy) in SNAPSHOT {
            assert!(!service.paths.data.join(copy.to_str().unwrap()).exists());
        }
        assert_eq!(*supervisor.approved.lock().unwrap(), previous.directory);
        assert_eq!(supervisor.stops.load(Ordering::SeqCst), 2);
        let facts = update_facts(&service);
        let version = crate::update::facts::Version::parse(&candidate.version).unwrap();
        assert_eq!(facts.skipped, Some(version.clone()));
        let failure = facts.last_failure.unwrap();
        assert_eq!(
            (failure.version, failure.kind),
            (
                version,
                crate::update::facts::FailureKind::Start(StartFailure::NotReady)
            )
        );
    });
}

#[test]
fn a_port_another_program_holds_is_a_transient_failure_that_skips_nothing() {
    with_service(|service, previous, candidate, supervisor| {
        let held = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        drop(crate::direct::host::State::open(&service.paths).unwrap());
        let data = Directory::open(&service.paths.data, false).unwrap();
        let mut direct: serde_json::Value = data.read_json("direct.json").unwrap().unwrap();
        direct["policy"] = serde_json::json!({"bind": held.local_addr().unwrap()});
        direct["enabled"] = true.into();
        data.write_json(c"direct.json", &direct).unwrap();
        supervisor
            .hangs
            .lock()
            .unwrap()
            .push(candidate.directory.clone());
        let status = update_running(&service, &previous, &candidate, &supervisor).unwrap();
        assert_eq!(
            status.phase,
            ActivationPhase::Returned {
                failure: StartFailure::PortInUse,
                running: true
            }
        );
        let facts = update_facts(&service);
        assert_eq!(facts.skipped, None);
        assert_eq!(
            facts.last_failure.map(|failure| failure.kind),
            Some(crate::update::facts::FailureKind::Start(
                StartFailure::PortInUse
            ))
        );
    });
}

#[test]
fn a_candidate_that_becomes_ready_completes_and_drops_its_snapshot() {
    with_service(|service, previous, candidate, supervisor| {
        supervisor
            .ready
            .lock()
            .unwrap()
            .push(candidate.directory.clone());
        let status = update_running(&service, &previous, &candidate, &supervisor).unwrap();
        assert_eq!(status.phase, ActivationPhase::Completed { running: true });
        assert_eq!(*supervisor.approved.lock().unwrap(), candidate.directory);
        assert_eq!(supervisor.starts.load(Ordering::SeqCst), 1);
        for (_, copy) in SNAPSHOT {
            assert!(!service.paths.data.join(copy.to_str().unwrap()).exists());
        }
        let facts = update_facts(&service);
        assert_eq!((facts.skipped, facts.last_failure), (None, None));
    });
}

/// The update record names `candidate` as the Pending Version, as an install leaves it.
fn pending_record(service: &Service, candidate: &RetainedInstallation) {
    let data = Directory::open(&service.paths.data, false).unwrap();
    data.write_json(
        c"updates.json",
        &serde_json::json!({
            "pending": {
                "digest": candidate.directory.file_name().unwrap().to_str().unwrap(),
                "version": candidate.version,
                "protocols": crate::update::facts::Protocols::CURRENT,
                "installed_at": 10
            },
            "skipped": null, "last_failure": null, "failures": 0,
            "check": "never", "silent": null, "activation": null
        }),
    )
    .unwrap();
}

#[test]
fn a_ready_candidate_is_no_longer_pending_even_when_its_helper_never_recorded_it() {
    with_service(|service, previous, candidate, supervisor| {
        interrupted(
            &service,
            &previous,
            &candidate,
            &supervisor,
            Step::StartDispatched,
            false,
        );
        let data = Directory::open(&service.paths.data, false).unwrap();
        let mut state = Service::state(&data).unwrap().unwrap();
        let run = Run {
            boot: boot().unwrap(),
            token: crate::protocol::random().unwrap(),
        };
        admit_run(&mut state, &run).unwrap();
        state.run = Some(run);
        data.write_json(c"service.json", &state).unwrap();
        pending_record(&service, &candidate);
        // The candidate commits readiness, and a power loss ends the helper before it records it.
        ready(&service.paths).unwrap();
        assert_eq!(update_facts(&service).pending, None);
    });
}

#[test]
fn a_return_whose_failure_was_never_recorded_is_recorded_once_from_the_journal() {
    with_service(|service, previous, candidate, supervisor| {
        interrupted(
            &service,
            &previous,
            &candidate,
            &supervisor,
            Step::ReturnApproved,
            true,
        );
        pending_record(&service, &candidate);
        let version = crate::update::facts::Version::parse(&candidate.version).unwrap();
        for _ in 0..2 {
            let facts = update_facts(&service);
            assert_eq!(facts.pending, None);
            assert_eq!(facts.skipped, Some(version.clone()));
            assert_eq!(facts.last_failure.map(|failure| failure.id), Some(1));
        }
    });
}

#[test]
fn a_start_that_reports_failure_after_its_candidate_became_ready_completes() {
    with_service(|service, previous, candidate, supervisor| {
        supervisor
            .ready
            .lock()
            .unwrap()
            .push(candidate.directory.clone());
        supervisor
            .start_reports_failure
            .store(true, Ordering::SeqCst);
        let _ = update_running(&service, &previous, &candidate, &supervisor);
        assert_eq!(
            service.activation_status().unwrap().unwrap().phase,
            ActivationPhase::Completed { running: true }
        );
        assert_eq!(*supervisor.approved.lock().unwrap(), candidate.directory);
    });
}

#[test]
fn a_startup_policy_failure_after_readiness_leaves_the_update_completed() {
    with_service(|service, previous, candidate, supervisor| {
        supervisor
            .ready
            .lock()
            .unwrap()
            .push(candidate.directory.clone());
        supervisor
            .policy_fails_after_start
            .store(true, Ordering::SeqCst);
        assert_eq!(
            update_running(&service, &previous, &candidate, &supervisor).map(|v| v.phase),
            Err(CoreError::ServiceUnavailable),
            "the caller learns that automatic startup did not change"
        );
        assert_eq!(
            service.activation_status().unwrap().unwrap().phase,
            ActivationPhase::Completed { running: true }
        );
    });
}

#[test]
fn a_candidate_that_exited_during_startup_returns_in_the_same_boot() {
    with_service(|service, previous, candidate, supervisor| {
        interrupted(
            &service,
            &previous,
            &candidate,
            &supervisor,
            Step::StartDispatched,
            false,
        );
        let data = Directory::open(&service.paths.data, false).unwrap();
        let mut state = Service::state(&data).unwrap().unwrap();
        let run = Run {
            boot: boot().unwrap(),
            token: crate::protocol::random().unwrap(),
        };
        admit_run(&mut state, &run).unwrap();
        state.run = Some(run);
        state.activation.as_mut().unwrap().started_at = Some(0);
        // This process dispatched the Start, long enough ago that its deadline passed.
        let id = state.activation.as_ref().unwrap().id;
        watches().insert(
            (id, Step::StartDispatched),
            Watch {
                since: Instant::now() - 2 * supervisor.ready_deadline(),
                resumed: false,
            },
        );
        data.write_json(c"service.json", &state).unwrap();
        // The candidate's run stays recorded, as a run that fails or crashes during startup
        // leaves it, and its process is gone.
        supervisor.running.store(false, Ordering::SeqCst);
        *supervisor.paths.lock().unwrap() = Some(service.paths.clone());
        supervisor
            .ready
            .lock()
            .unwrap()
            .push(previous.directory.clone());
        let status = service.recover_activation().unwrap().unwrap();
        assert_eq!(
            status.phase,
            ActivationPhase::Returned {
                failure: StartFailure::NotReady,
                running: true
            }
        );
        assert_eq!(*supervisor.approved.lock().unwrap(), previous.directory);
    });
}

#[test]
fn a_return_resumed_with_no_watch_keeps_the_version_pending() {
    for step in [Step::StartFailed, Step::ReturnStopDispatched] {
        with_service(|service, previous, candidate, supervisor| {
            interrupted(
                &service,
                &previous,
                &candidate,
                &supervisor,
                Step::StartDispatched,
                false,
            );
            pending_record(&service, &candidate);
            // The helper that took over the Start ended during the return, so no process
            // watches it any more.
            let data = Directory::open(&service.paths.data, false).unwrap();
            let mut state = Service::state(&data).unwrap().unwrap();
            state.activation.as_mut().unwrap().step = step;
            data.write_json(c"service.json", &state).unwrap();
            supervisor.running.store(false, Ordering::SeqCst);
            *supervisor.paths.lock().unwrap() = Some(service.paths.clone());
            supervisor
                .ready
                .lock()
                .unwrap()
                .push(previous.directory.clone());
            let status = service.recover_activation().unwrap().unwrap();
            assert_eq!(
                status.phase,
                ActivationPhase::Returned {
                    failure: StartFailure::Interrupted,
                    running: true
                },
                "{step:?}"
            );
            let facts = runtime::updates::Updates::stored(&service).unwrap();
            assert_eq!(facts.skipped, None, "{step:?}");
            assert!(
                facts.pending.is_some(),
                "{step:?}: the version stays pending"
            );
            readable_by_0_11_2(&service);
        });
    }
}

/// The fields of the activation journal SpaceTerm 0.11.2 reads and writes. The previous and
/// the next runtime of an update share the journal, so neither may change its shape.
const JOURNAL_0_11_2: [&str; 13] = [
    "schema",
    "id",
    "previous",
    "candidate",
    "stopped",
    "original_run",
    "candidate_run",
    "step",
    "saved",
    "started_at",
    "ready",
    "failed",
    "failure",
];

#[test]
fn the_journal_keeps_the_shape_both_runtimes_of_an_update_read() {
    with_service(|service, previous, candidate, supervisor| {
        interrupted(
            &service,
            &previous,
            &candidate,
            &supervisor,
            Step::StartDispatched,
            false,
        );
        let path = service.paths.data.join("service.json");
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut fields: Vec<&str> = record["activation"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        fields.sort_unstable();
        let mut expected = JOURNAL_0_11_2;
        expected.sort_unstable();
        assert_eq!(fields, expected, "this runtime writes what 0.11.2 reads");
        // The 0.11.2 helper that starts this runtime as its candidate wrote the dispatch time.
        record["activation"]["started_at"] = 1_793_750_400.into();
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        let data = Directory::open(&service.paths.data, false).unwrap();
        let state = Service::state(&data).unwrap().unwrap();
        assert_eq!(
            state.activation.unwrap().started_at,
            Some(1_793_750_400),
            "this runtime reads what 0.11.2 writes"
        );
    });
}

#[test]
fn a_start_another_process_watched_returns_as_interrupted_and_stays_pending() {
    with_service(|service, previous, candidate, supervisor| {
        interrupted(
            &service,
            &previous,
            &candidate,
            &supervisor,
            Step::StartDispatched,
            false,
        );
        pending_record(&service, &candidate);
        let data = Directory::open(&service.paths.data, false).unwrap();
        let mut state = Service::state(&data).unwrap().unwrap();
        // A helper before a reboot, logout or crash watched this Start, by a clock long past.
        state.activation.as_mut().unwrap().started_at = Some(0);
        data.write_json(c"service.json", &state).unwrap();
        supervisor.running.store(false, Ordering::SeqCst);
        *supervisor.paths.lock().unwrap() = Some(service.paths.clone());
        supervisor
            .ready
            .lock()
            .unwrap()
            .push(previous.directory.clone());
        let status = service.recover_activation().unwrap().unwrap();
        assert_eq!(
            status.phase,
            ActivationPhase::Returned {
                failure: StartFailure::Interrupted,
                running: true
            }
        );
        let facts = runtime::updates::Updates::stored(&service).unwrap();
        assert_eq!(facts.skipped, None, "its readiness was never measured");
        assert!(facts.pending.is_some(), "the version stays pending");
        assert_eq!(
            facts.last_failure.map(|failure| failure.kind),
            Some(crate::update::facts::FailureKind::Start(
                StartFailure::Interrupted
            ))
        );
        readable_by_0_11_2(&service);
    });
}

/// SpaceTerm 0.11.2's start failures, the only ones the runtime a return reaches may know.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum StartFailure0_11_2 {
    NotReady,
    PortInUse,
    DiskFull,
}

/// Asserts that a runtime of 0.11.2 reads the start failures in the journal and the update
/// record, so a return to it can start.
fn readable_by_0_11_2(service: &Service) {
    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    #[allow(dead_code)]
    enum Kind {
        Install(crate::update::facts::InstallFailure),
        Start(StartFailure0_11_2),
    }
    #[derive(Debug, serde::Deserialize)]
    #[allow(dead_code)]
    struct Failure {
        id: u64,
        at: u64,
        version: String,
        kind: Kind,
    }
    #[derive(Debug, serde::Deserialize)]
    #[allow(dead_code)]
    struct Record {
        last_failure: Option<Failure>,
    }
    let read = |name: &str| -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(service.paths.data.join(name)).unwrap()).unwrap()
    };
    let journal = read("service.json");
    serde_json::from_value::<Option<StartFailure0_11_2>>(journal["activation"]["failure"].clone())
        .expect("0.11.2 reads the journal's failure");
    serde_json::from_value::<Record>(read("updates.json")).expect("0.11.2 reads the update record");
}

#[test]
fn a_return_restores_the_data_generation_a_candidate_migrated_from() {
    use crate::migration::{Area, Store};
    with_service(|service, previous, candidate, supervisor| {
        supervisor
            .hangs
            .lock()
            .unwrap()
            .push(candidate.directory.clone());
        let root = service.paths.data.clone();
        let identity = std::fs::read(root.join("identity.json")).unwrap();
        // The candidate migrates the data to a newer generation, as its runner does, then hangs.
        *supervisor.meddles.lock().unwrap() = Some(Box::new(move |_: &Directory| {
            let store = crate::migration::core_data::store(&root);
            store
                .write(Area::Backup, "identity.json", &identity)
                .unwrap();
            store
                .write(Area::Backup, "generation.json", br#"{"generation":1}"#)
                .unwrap();
            store
                .write(Area::Root, "generation.json", br#"{"generation":2}"#)
                .unwrap();
        }));
        let status = update_running(&service, &previous, &candidate, &supervisor).unwrap();
        assert!(matches!(status.phase, ActivationPhase::Returned { .. }));
        let marker: serde_json::Value = serde_json::from_slice(
            &std::fs::read(service.paths.data.join("generation.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(marker, serde_json::json!({ "generation": 1 }));
        assert!(crate::migration::core_data::prepare(&service.paths).is_ok());
        // The CLI of the version it returned to may restore its own state the same way.
        assert!(
            service
                .returned_to(&previous.directory.join("bin/spaceterm"))
                .unwrap()
        );
        assert!(
            !service
                .returned_to(&candidate.directory.join("bin/spaceterm"))
                .unwrap()
        );
    });
}

#[test]
fn existing_pairings_survive_a_candidate_that_rewrote_them() {
    with_service(|service, previous, candidate, supervisor| {
        drop(crate::direct::host::State::open(&service.paths).unwrap());
        let pairings = std::fs::read(service.paths.data.join("direct.json")).unwrap();
        supervisor
            .hangs
            .lock()
            .unwrap()
            .push(candidate.directory.clone());
        *supervisor.meddles.lock().unwrap() = Some(Box::new(|data: &Directory| {
            let mut direct: serde_json::Value = data.read_json("direct.json").unwrap().unwrap();
            direct["enabled"] = (!direct["enabled"].as_bool().unwrap()).into();
            direct["devices"] = serde_json::json!([]);
            data.write_json(c"direct.json", &direct).unwrap();
        }));
        let status = update_running(&service, &previous, &candidate, &supervisor).unwrap();
        assert!(matches!(status.phase, ActivationPhase::Returned { .. }));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &std::fs::read(service.paths.data.join("direct.json")).unwrap()
            )
            .unwrap(),
            serde_json::from_slice::<serde_json::Value>(&pairings).unwrap()
        );
        crate::direct::host::State::open(&service.paths).unwrap();
    });
}

#[test]
fn the_previous_runtime_recovers_an_unready_candidate_from_boot_until_the_activation_settles() {
    for ready in [true, false] {
        with_service(|service, previous, candidate, supervisor| {
            if ready {
                supervisor
                    .ready
                    .lock()
                    .unwrap()
                    .push(candidate.directory.clone());
            }
            update_running(&service, &previous, &candidate, &supervisor).unwrap();
            assert_eq!(
                supervisor.replacements.lock().unwrap().first(),
                Some(&(
                    candidate.directory.clone(),
                    Some(previous.directory.clone())
                )),
                "a boot recovery entry runs the previous runtime before the registration names \
                 the candidate"
            );
            assert_eq!(*supervisor.recovery.lock().unwrap(), None, "ready: {ready}");
        });
    }
}

/// The restarts a running server admitted, as `(candidate, silent)`.
#[derive(Default)]
struct Admitted(Mutex<Vec<(Option<[u8; 32]>, bool)>>);
impl runtime::updates::Restarter for Admitted {
    fn restart(
        self: Arc<Self>,
        candidate: Option<[u8; 32]>,
        silent: bool,
    ) -> Result<runtime::updates::Launch, runtime::updates::RestartRefusal> {
        self.0.lock().unwrap().push((candidate, silent));
        Ok(Box::new(|| {}))
    }
}

#[test]
fn boot_runs_the_approved_version_first_and_then_restarts_into_the_pending_one() {
    with_service(|service, previous, candidate, _| {
        let data = Directory::open(&service.paths.data, false).unwrap();
        let mut state = Service::state(&data).unwrap().unwrap();
        state.stopped = false;
        data.write_json(c"service.json", &state).unwrap();
        pending_record(&service, &candidate);
        let executable = previous.directory.join(
            runtime::Layout::host(&service.identity)
                .unwrap()
                .executable(),
        );
        let admitted = Arc::new(Admitted::default());
        let restarter: Arc<dyn runtime::updates::Restarter> = admitted.clone();
        let mut served = None;
        run_approved(&service, &executable, |paths| {
            let data = Directory::open(&paths.data, false)?;
            served = Service::state(&data)?.map(|state| state.installation);
            let updates = runtime::updates::Updates::open(service.clone())?;
            updates.run(Arc::downgrade(&restarter), Arc::new(|_| Vec::new()), || {
                None
            })?;
            quiet(&updates);
            Ok(())
        })
        .unwrap();
        let digest = |installation: &RetainedInstallation| {
            installation
                .directory
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned()
        };
        assert_eq!(
            served,
            Some(digest(&previous)),
            "the approved version serves first"
        );
        let mut pending = [0; 32];
        for (index, byte) in pending.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&digest(&candidate)[index * 2..index * 2 + 2], 16).unwrap();
        }
        assert_eq!(*admitted.0.lock().unwrap(), [(Some(pending), true)]);
    });
}

/// The server had no Terminal Session and no session request for the whole grace period.
pub(crate) fn quiet(updates: &runtime::updates::Updates) {
    let idle = runtime::updates::Activity {
        sessions: 0,
        requested: false,
    };
    let now = Instant::now();
    updates.observe(idle, now);
    updates.observe(idle, now + runtime::updates::SILENT_GRACE);
}
