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
}
/// How long this fixture's service manager lets a stopping server finish.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);
impl Supervisor for SupervisorFixture {
    fn startup(&self) -> ServiceStartup {
        ServiceStartup::AtLogin
    }
    fn check(&self, runtime: &Path, _: bool) -> Result<(), ServerError> {
        if *self.approved.lock().unwrap() == runtime {
            Ok(())
        } else {
            Err(ServerError::CleanupUnresolved)
        }
    }
    fn install(&self, _: &Path) -> Result<(), ServerError> {
        panic!("activation does not install a fresh service")
    }
    fn prepare_start(&self) -> Result<(), ServerError> {
        Ok(())
    }
    fn start(&self) -> Result<(), ServerError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Err(ServerError::StartupFailed)
    }
    fn set_automatic_start(&self, enabled: bool) -> Result<(), ServerError> {
        self.automatic_start.store(enabled, Ordering::SeqCst);
        Ok(())
    }
    fn stop(&self) -> Result<(), ServerError> {
        self.set_automatic_start(false)?;
        self.stops.fetch_add(1, Ordering::SeqCst);
        if self.fail_stop.load(Ordering::SeqCst) {
            return Err(ServerError::ServiceUnavailable);
        }
        match *self.stop_takes.lock().unwrap() {
            Some(takes) => *self.exits_at.lock().unwrap() = Some(Instant::now() + takes),
            None => self.running.store(false, Ordering::SeqCst),
        }
        Ok(())
    }
    fn stop_timeout(&self) -> Duration {
        STOP_TIMEOUT
    }
    fn uninstall(&self, _: &Path) -> Result<(), ServerError> {
        panic!("activation does not uninstall")
    }
    fn running(&self) -> Result<bool, ServerError> {
        let mut exits_at = self.exits_at.lock().unwrap();
        if exits_at.is_some_and(|at| Instant::now() >= at) {
            *exits_at = None;
            self.running.store(false, Ordering::SeqCst);
        }
        Ok(self.running.load(Ordering::SeqCst))
    }
    fn replace(&self, previous: &Path, candidate: &Path) -> Result<(), ServerError> {
        let mut current = self.approved.lock().unwrap();
        assert!(current.as_path() == previous || current.as_path() == candidate);
        *current = candidate.to_owned();
        if self.fail_replace.load(Ordering::SeqCst) {
            Err(ServerError::ServiceUnavailable)
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

            assert_eq!(
                service.recover_activation(),
                Err(ServerError::UnsafeStorage)
            );
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
            Err(ServerError::Changed)
        ));
        assert_eq!(supervisor.stops.load(Ordering::SeqCst), 0);
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
        Step::Approved | Step::StartDispatched | Step::Completed | Step::ReturnAdmitted
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
            Some(saved_state(&data).unwrap())
        },
        ready: false,
        failed: false,
    });
    data.write_json(c"service.json", &state).unwrap();
    *supervisor.approved.lock().unwrap() = if matches!(
        step,
        Step::RegistrationReplaced
            | Step::Approved
            | Step::StartDispatched
            | Step::Completed
            | Step::ReturnAdmitted
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
            Err(ServerError::ActivationUnresolved)
        );
        assert!(service.passive_server_id().unwrap().is_some());
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
        assert_eq!(service.passive_server_id().unwrap(), None);
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
            assert_eq!(result, Err(ServerError::ActivationUnresolved));
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
            Err(ServerError::ActivationUnresolved)
        );
    });
}

#[test]
fn admitted_running_operation_stops_once_and_failed_start_is_never_redispatched() {
    with_service(|service, previous, candidate, supervisor| {
        let id = interrupted(
            &service,
            &previous,
            &candidate,
            &supervisor,
            Step::Admitted,
            false,
        );
        supervisor.running.store(true, Ordering::SeqCst);
        assert_eq!(
            service.recover_activation(),
            Err(ServerError::StartupFailed)
        );
        assert_eq!(
            (
                supervisor.stops.load(Ordering::SeqCst),
                supervisor.starts.load(Ordering::SeqCst)
            ),
            (1, 1)
        );
        let status = service.activation_status().unwrap().unwrap();
        assert_eq!(
            (status.operation, status.phase),
            (id, ActivationPhase::Failed { can_return: true })
        );
        assert_eq!(
            service.recover_activation(),
            Err(ServerError::ActivationUnresolved)
        );
        assert_eq!(
            (
                supervisor.stops.load(Ordering::SeqCst),
                supervisor.starts.load(Ordering::SeqCst)
            ),
            (1, 1)
        );
        assert_eq!(*supervisor.approved.lock().unwrap(), candidate.directory);
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
        // The fixture's Start always fails, so reaching it proves the stop was confirmed.
        assert_eq!(service.activate(approval), Err(ServerError::StartupFailed));
        let status = service.activation_status().unwrap().unwrap();
        assert_eq!(status.phase, ActivationPhase::Failed { can_return: true });
        assert_eq!(
            (
                supervisor.stops.load(Ordering::SeqCst),
                supervisor.starts.load(Ordering::SeqCst)
            ),
            (1, 1)
        );
        assert_eq!(*supervisor.approved.lock().unwrap(), candidate.directory);
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
            Err(ServerError::CleanupUnresolved)
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
            Err(ServerError::ActivationUnresolved)
        );

        *supervisor.exits_at.lock().unwrap() = Some(Instant::now());
        assert_eq!(
            service.recover_activation(),
            Err(ServerError::StartupFailed)
        );
        assert_eq!(
            (
                supervisor.stops.load(Ordering::SeqCst),
                supervisor.starts.load(Ordering::SeqCst)
            ),
            (1, 1)
        );
        assert_eq!(*supervisor.approved.lock().unwrap(), candidate.directory);
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
            Err(ServerError::ServiceUnavailable)
        );
        supervisor.fail_stop.store(false, Ordering::SeqCst);
        assert_eq!(
            service.recover_activation(),
            Err(ServerError::ActivationUnresolved)
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
            Err(ServerError::ServiceUnavailable)
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
fn explicit_return_requires_never_ready_and_unchanged_saved_state_and_stays_stopped() {
    with_service(|service, previous, candidate, supervisor| {
        let id = interrupted(
            &service,
            &previous,
            &candidate,
            &supervisor,
            Step::StartDispatched,
            false,
        );
        assert_eq!(
            service.recover_activation(),
            Err(ServerError::ActivationUnresolved)
        );
        assert_eq!(supervisor.starts.load(Ordering::SeqCst), 0);
        let before = std::fs::read(service.paths.data.join("identity.json")).unwrap();
        let approval = service.prepare_return().unwrap();
        let status = service.return_to_previous(approval).unwrap();
        assert!(status.returned && status.stopped);
        assert_eq!(status.operation, id);
        assert_eq!(*supervisor.approved.lock().unwrap(), previous.directory);
        assert_eq!(
            service.passive_status(),
            Ok(ServiceStatus {
                installed: true,
                stopped: true,
                enabled: true,
                running: false,
            }),
            "a completed return settles the failed candidate without another recovery"
        );
        assert_eq!(
            service.status().unwrap(),
            ServiceStatus {
                installed: true,
                stopped: true,
                enabled: true,
                running: false
            }
        );
        assert_eq!(
            before,
            std::fs::read(service.paths.data.join("identity.json")).unwrap()
        );
    });
}

#[test]
fn candidate_state_mutation_refuses_return_without_restoring_a_snapshot() {
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
        let mut record = data.read().unwrap();
        record.organization =
            Some(crate::organization::Organization::first(PathBuf::from("/")).unwrap());
        data.write(&record).unwrap();
        let newer = std::fs::read(service.paths.data.join("identity.json")).unwrap();
        assert!(matches!(
            service.prepare_return(),
            Err(ServerError::ReturnUnavailable)
        ));
        assert_eq!(
            service.activation_status().unwrap().unwrap().phase,
            ActivationPhase::Failed { can_return: false }
        );
        assert_eq!(*supervisor.approved.lock().unwrap(), candidate.directory);
        assert_eq!(
            newer,
            std::fs::read(service.paths.data.join("identity.json")).unwrap()
        );
    });
}

#[test]
fn changed_direct_policy_refuses_return_and_preserves_the_current_server_credentials() {
    with_service(|service, previous, candidate, supervisor| {
        let direct = crate::direct::host::State::open(&service.paths).unwrap();
        drop(direct);
        interrupted(
            &service,
            &previous,
            &candidate,
            &supervisor,
            Step::StartDispatched,
            false,
        );
        let data = Directory::open(&service.paths.data, false).unwrap();
        let mut direct: serde_json::Value = data.read_json("direct.json").unwrap().unwrap();
        direct["policy"] = serde_json::json!({"bind":"127.0.0.1:9231"});
        data.write_json(c"direct.json", &direct).unwrap();
        let retained = std::fs::read(service.paths.data.join("direct.json")).unwrap();
        assert!(matches!(
            service.prepare_return(),
            Err(ServerError::ReturnUnavailable)
        ));
        assert_eq!(
            service.activation_status().unwrap().unwrap().phase,
            ActivationPhase::Failed { can_return: false }
        );
        assert_eq!(*supervisor.approved.lock().unwrap(), candidate.directory);
        assert!(
            retained == std::fs::read(service.paths.data.join("direct.json")).unwrap(),
            "a refused return must preserve exact current direct state"
        );
    });
}

#[test]
fn creating_a_direct_document_distinguishes_absence_from_a_present_json_null_before_return() {
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
        data.write_json(c"direct.json", &serde_json::Value::Null)
            .unwrap();
        assert!(matches!(
            service.prepare_return(),
            Err(ServerError::ReturnUnavailable)
        ));
        assert_eq!(*supervisor.approved.lock().unwrap(), candidate.directory);
        assert_eq!(
            data.read_json::<serde_json::Value>("direct.json").unwrap(),
            Some(serde_json::Value::Null)
        );
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
        assert!(matches!(
            service.prepare_return(),
            Err(ServerError::ReturnUnavailable)
        ));
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
            Err(ServerError::ActivationUnresolved)
        );
        assert_eq!(
            service.recover_activation(),
            Err(ServerError::ActivationUnresolved)
        );
        assert_eq!(supervisor.starts.load(Ordering::SeqCst), 0);
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
            Err(ServerError::CleanupUnresolved)
        );
        let mut state = Service::state(&data).unwrap().unwrap();
        state.run.as_mut().unwrap().boot = "simulated-previous-boot".into();
        state.activation.as_mut().unwrap().original_run = state.run.clone();
        data.write_json(c"service.json", &state).unwrap();
        assert_eq!(
            service.recover_activation(),
            Err(ServerError::StartupFailed)
        );
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
        assert!(matches!(service.activate(stale), Err(ServerError::Changed)));
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
        assert_eq!(
            service.recover_activation(),
            Err(ServerError::UnsafeStorage)
        );
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
        assert_eq!(
            service.recover_activation(),
            Err(ServerError::UnsafeStorage)
        );
        assert_eq!(supervisor.stops.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn confirmation_token_changes_with_run_stop_and_candidate_observations() {
    with_service(|service, _, candidate, _| {
        let approval = service.prepare_activation(&candidate).unwrap().unwrap();
        let first = approval.confirmation_token().unwrap();
        let data = Directory::open(&service.paths.data, false).unwrap();
        let mut state = Service::state(&data).unwrap().unwrap();
        state.stopped = false;
        data.write_json(c"service.json", &state).unwrap();
        let second = service
            .prepare_activation(&candidate)
            .unwrap()
            .unwrap()
            .confirmation_token()
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(first.len(), 64);
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
fn a_stopped_update_tracks_its_later_first_start_failure_before_offering_safe_return() {
    with_service(|service, previous, candidate, _| {
        let approval = service.prepare_activation(&candidate).unwrap().unwrap();
        service.activate(approval).unwrap();
        assert_eq!(service.start(), Err(ServerError::StartupFailed));
        assert_eq!(
            service.activation_status().unwrap().unwrap().phase,
            ActivationPhase::Failed { can_return: true }
        );
        let approval = service.prepare_return().unwrap();
        service.return_to_previous(approval).unwrap();
        assert!(service.status().unwrap().stopped);
        let data = Directory::open(&service.paths.data, false).unwrap();
        assert_eq!(
            service.runtime(&Service::state(&data).unwrap().unwrap()),
            previous.directory
        );
    });
}

#[test]
fn returning_after_a_stopped_update_is_unavailable_once_its_first_start_reaches_ready() {
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
        assert!(matches!(
            service.prepare_return(),
            Err(ServerError::ReturnUnavailable)
        ));
        assert_eq!(
            service.activation_status().unwrap().unwrap().phase,
            ActivationPhase::Completed { running: true }
        );
    });
}
