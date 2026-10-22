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
    fail_stop: AtomicBool,
    fail_replace: AtomicBool,
}
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
    fn disable(&self) -> Result<(), ServerError> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        if self.fail_stop.load(Ordering::SeqCst) {
            return Err(ServerError::ServiceUnavailable);
        }
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn uninstall(&self, _: &Path) -> Result<(), ServerError> {
        panic!("activation does not uninstall")
    }
    fn running(&self) -> Result<bool, ServerError> {
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
                fail_stop: AtomicBool::new(false),
                fail_replace: AtomicBool::new(false),
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
fn approved_legacy_preflight_can_explicitly_update_without_losing_stop_or_saved_state() {
    for recover in [false, true] {
        with_service_identity(
            "spaceterm-preflight",
            |service, previous, candidate, supervisor| {
                runtime::test_support::sign_with_previous_production_trust(
                    &previous.directory.join("artifact.tar.gz"),
                    &previous.directory.join("artifact.sig"),
                );
                {
                    let data = Directory::open(&service.paths.data, false).unwrap();
                    let _setup = data.lock("setup.lock", false).unwrap();
                    let mut record = data.read().unwrap();
                    let mut organization =
                        crate::organization::Organization::first(service.paths.data.clone())
                            .unwrap();
                    organization.name = "Retained Preflight work".into();
                    record.organization = Some(organization);
                    data.write(&record).unwrap();
                }
                let identity = std::fs::read(service.paths.data.join("identity.json")).unwrap();
                let policy = std::fs::read(service.paths.data.join("service.json")).unwrap();
                assert_eq!(
                    service.retained_installations().unwrap(),
                    vec![previous.clone()]
                );
                assert_eq!(service.connect(), Err(ServerError::Stopped));
                let approval = service.prepare_activation(&candidate).unwrap().unwrap();
                assert_eq!(*supervisor.approved.lock().unwrap(), previous.directory);
                assert_eq!(
                    std::fs::read(service.paths.data.join("service.json")).unwrap(),
                    policy
                );
                assert_eq!(supervisor.starts.load(Ordering::SeqCst), 0);
                let status = if recover {
                    let id = interrupted(
                        &service,
                        &previous,
                        &candidate,
                        &supervisor,
                        Step::Admitted,
                        true,
                    );
                    let status = service.recover_activation().unwrap().unwrap();
                    assert_eq!(status.operation, id);
                    status
                } else {
                    service.activate(approval).unwrap()
                };
                assert_eq!(status.phase, ActivationPhase::Completed { running: false });
                assert!(service.status().unwrap().stopped);
                assert_eq!(*supervisor.approved.lock().unwrap(), candidate.directory);
                assert_eq!(supervisor.starts.load(Ordering::SeqCst), 0);
                assert_eq!(supervisor.stops.load(Ordering::SeqCst), 0);
                assert_eq!(
                    std::fs::read(service.paths.data.join("identity.json")).unwrap(),
                    identity
                );
            },
        );
    }
}

#[test]
fn explicit_start_of_approved_legacy_preflight_reaches_the_native_supervisor() {
    with_service_identity("spaceterm-preflight", |service, previous, _, supervisor| {
        runtime::test_support::sign_with_previous_production_trust(
            &previous.directory.join("artifact.tar.gz"),
            &previous.directory.join("artifact.sig"),
        );
        let identity = std::fs::read(service.paths.data.join("identity.json")).unwrap();
        assert!(service.status().unwrap().stopped);
        assert_eq!(supervisor.starts.load(Ordering::SeqCst), 0);
        // This fixture deliberately fails native startup. Authentication must pass before
        // dispatch, and explicit Start must keep selecting the previously approved runtime.
        assert_eq!(service.start(), Err(ServerError::StartupFailed));
        assert_eq!(supervisor.starts.load(Ordering::SeqCst), 1);
        assert_eq!(*supervisor.approved.lock().unwrap(), previous.directory);
        assert_eq!(
            std::fs::read(service.paths.data.join("identity.json")).unwrap(),
            identity
        );
    });
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
            service.status().unwrap(),
            ServiceStatus {
                installed: true,
                stopped: true,
                enabled: false,
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
