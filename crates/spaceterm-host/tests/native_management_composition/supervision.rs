use spaceterm_host::{
    installation::{QuiescenceProgress, ServerLifecyclePort},
    native_deployment::{
        contracts::{
            NativeFailure, NativeManagementFuture, NativeRequestId, PublicationDisposition,
        },
        profile::NativePlatform,
    },
    native_management::{
        facade::{RuntimeAssessment, supervision::*},
        helper_lifetime::HelperLifetimeGuard,
    },
    startup::{
        AccountBuildKey, BuildIdentity, DesiredServerState, InstallationId, ProtectedRegistration,
    },
};
use std::{
    future::Future,
    num::NonZeroU64,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

fn id(value: u64) -> InstallationId {
    InstallationId(NonZeroU64::new(value).unwrap())
}
fn request_id() -> NativeRequestId {
    NativeRequestId::new([17; 16]).unwrap()
}
fn key() -> AccountBuildKey {
    AccountBuildKey {
        account_uid: 501,
        build_identity: BuildIdentity::Development,
    }
}
#[derive(Clone)]
struct NativePeer(Arc<Mutex<State>>);
struct State {
    registration: ProtectedRegistration,
    configuration: Option<Vec<u8>>,
    events: Vec<SupervisorCommand>,
    published: usize,
    observation: SupervisorObservation,
    fault: Option<SupervisorCommand>,
    uncertain: bool,
    publication_uncertain: bool,
    restart_phase: spaceterm_host::native_management::controller::ManagementPhase,
    restart_publication: Option<NativeFailure>,
    utility_custody_available: bool,
    utility_pending: bool,
    retirement_delayed: bool,
    storage_locked: bool,
    background: Option<spaceterm_host::native_management::macos::BackgroundItemsStatus>,
}
struct Transaction {
    original: InstallationId,
}
struct Custody {
    peer: NativePeer,
    platform: NativePlatform,
    utility: Option<std::path::PathBuf>,
    scenario: &'static str,
}
impl SupervisorCustody for Custody {
    type Transaction = Transaction;
    type Lease = ServiceDefinition;
    fn registration(&self, _: &Transaction) -> Result<ProtectedRegistration, NativeFailure> {
        let state = self.peer.0.lock().unwrap();
        if state.storage_locked {
            return Err(NativeFailure::StorageLocked);
        }
        if state.uncertain {
            return Err(NativeFailure::Indeterminate);
        }
        Ok(state.registration)
    }
    fn original(&self, tx: &Transaction) -> (NativeRequestId, InstallationId) {
        (request_id(), tx.original)
    }
    fn restart_phase(
        &self,
        _: &Transaction,
    ) -> Result<spaceterm_host::native_management::controller::ManagementPhase, NativeFailure> {
        let state = self.peer.0.lock().unwrap();
        if state.uncertain {
            return Err(NativeFailure::Indeterminate);
        }
        Ok(state.restart_phase)
    }
    fn commit_restart_supervision_removed(
        &self,
        _: &Transaction,
        _: &PriorInstanceRetirement,
    ) -> PublicationDisposition<()> {
        let mut state = self.peer.0.lock().unwrap();
        if let Some(failure) = state.restart_publication.take() {
            if failure == NativeFailure::Indeterminate {
                state.uncertain = true;
                return PublicationDisposition::Indeterminate;
            }
            return PublicationDisposition::NotApplied(failure);
        }
        state.restart_phase =
            spaceterm_host::native_management::controller::ManagementPhase::SupervisionRemoved;
        PublicationDisposition::Durable(())
    }
    fn retain(&self, _: &Transaction) -> Result<ServiceDefinition, NativeFailure> {
        ServiceDefinition::new(
            self.platform,
            key(),
            Path::new("/opt/spaceterm/protected-launcher"),
        )
    }
    fn definition<'a>(&self, lease: &'a ServiceDefinition) -> &'a ServiceDefinition {
        lease
    }
    fn recheck(&self, _: &Transaction, _: &mut ServiceDefinition) -> Result<(), NativeFailure> {
        if self.peer.0.lock().unwrap().uncertain {
            Err(NativeFailure::Indeterminate)
        } else {
            Ok(())
        }
    }
    fn publish_configuration(
        &self,
        _: &Transaction,
        _: &mut ServiceDefinition,
        bytes: &[u8],
    ) -> PublicationDisposition<()> {
        let mut state = self.peer.0.lock().unwrap();
        if state.utility_pending {
            return PublicationDisposition::NotApplied(NativeFailure::CleanupUnresolved);
        }
        state.published += 1;
        if state
            .configuration
            .as_deref()
            .is_some_and(|old| old != bytes)
        {
            return PublicationDisposition::NotApplied(NativeFailure::IdentityMismatch);
        }
        state.configuration = Some(bytes.to_vec());
        if state.publication_uncertain {
            state.uncertain = true;
            PublicationDisposition::Indeterminate
        } else {
            PublicationDisposition::Durable(())
        }
    }
    fn bind_exec(
        &self,
        _: &mut ServiceDefinition,
        command: &mut Command,
    ) -> Result<(), NativeFailure> {
        let program = self
            .utility
            .as_ref()
            .ok_or(NativeFailure::ExecutionBindingUnproven)?;
        let arguments: Vec<_> = command
            .get_args()
            .map(|argument| argument.to_os_string())
            .collect();
        *command = Command::new(program);
        command.args(["--scenario", self.scenario]).args(arguments);
        Ok(())
    }
    fn begin_utility_custody(&self, _: &mut ServiceDefinition) -> PublicationDisposition<()> {
        let mut state = self.peer.0.lock().unwrap();
        if !state.utility_custody_available {
            return PublicationDisposition::NotApplied(NativeFailure::CustodyUnavailable);
        }
        if state.utility_pending {
            return PublicationDisposition::NotApplied(NativeFailure::CleanupUnresolved);
        }
        state.utility_pending = true;
        PublicationDisposition::Durable(())
    }
    fn finish_utility_custody(&self, _: &mut ServiceDefinition) -> Result<(), NativeFailure> {
        self.peer.0.lock().unwrap().utility_pending = false;
        Ok(())
    }
    fn background_items(
        &self,
        _: &ServiceDefinition,
    ) -> Result<
        Option<spaceterm_host::native_management::macos::BackgroundItemsStatus>,
        NativeFailure,
    > {
        Ok(self.peer.0.lock().unwrap().background)
    }
}
impl SupervisorCommands<Custody> for NativePeer {
    fn call(
        &self,
        _: &Custody,
        _: &mut ServiceDefinition,
        command: SupervisorCommand,
        _: spaceterm_host::native_management::helper_lifetime::HelperDeadline,
    ) -> Result<SupervisorReply, NativeFailure> {
        let mut state = self.0.lock().unwrap();
        state.events.push(command);
        match command {
            SupervisorCommand::Bootstrap | SupervisorCommand::Start => {
                state.observation = SupervisorObservation::Running
            }
            SupervisorCommand::Retire if !state.retirement_delayed => {
                state.observation = SupervisorObservation::NotLoaded
            }
            _ => {}
        }
        if state.fault == Some(command)
            && (command != SupervisorCommand::Observe
                || state.observation == SupervisorObservation::Running)
        {
            state.fault = None;
            return Err(NativeFailure::SupervisorUnavailable);
        }
        Ok(if command == SupervisorCommand::Observe {
            SupervisorReply::Observation(state.observation)
        } else {
            SupervisorReply::Applied
        })
    }
}
fn fixture(
    platform: NativePlatform,
) -> (
    NativePeer,
    Transaction,
    NativeSupervisor<Custody, NativePeer>,
    HelperLifetimeGuard,
) {
    let peer = NativePeer(Arc::new(Mutex::new(State {
        registration: ProtectedRegistration {
            key: key(),
            generation: NonZeroU64::new(7).unwrap(),
            desired_state: DesiredServerState::Running,
            approved_installation: id(42),
            staged_installation: Some(id(43)),
        },
        configuration: None,
        events: vec![],
        published: 0,
        observation: SupervisorObservation::NotLoaded,
        fault: None,
        uncertain: false,
        publication_uncertain: false,
        restart_phase: spaceterm_host::native_management::controller::ManagementPhase::Quiesced,
        restart_publication: None,
        utility_custody_available: false,
        utility_pending: false,
        retirement_delayed: false,
        storage_locked: false,
        background: if platform == NativePlatform::Macos {
            Some(spaceterm_host::native_management::macos::BackgroundItemsStatus::Enabled)
        } else {
            None
        },
    })));
    let supervisor = NativeSupervisor::new(
        Custody {
            peer: peer.clone(),
            platform,
            utility: None,
            scenario: "",
        },
        peer.clone(),
    );
    (
        peer,
        Transaction { original: id(42) },
        supervisor,
        HelperLifetimeGuard::install().unwrap(),
    )
}

#[test]
fn stage_then_crash_or_reboot_restores_approved_code_with_the_same_stable_service() {
    for platform in [NativePlatform::Macos, NativePlatform::Linux] {
        let (peer, tx, supervisor, guard) = fixture(platform);
        supervisor.restore_approved(&tx, guard.deadline()).unwrap();
        let configuration = peer.0.lock().unwrap().configuration.clone().unwrap();
        {
            let mut state = peer.0.lock().unwrap();
            state.registration.generation = NonZeroU64::new(8).unwrap();
            state.registration.staged_installation = Some(id(44));
            state.observation = SupervisorObservation::NotLoaded;
        }
        let restored = supervisor.restore_approved(&tx, guard.deadline()).unwrap();
        assert_eq!(restored, SupervisorObservation::Running);
        let state = peer.0.lock().unwrap();
        assert_eq!(state.configuration.as_ref(), Some(&configuration));
        let text = std::str::from_utf8(&configuration).unwrap();
        assert!(!text.contains("--registration-generation"));
        assert!(!text.contains("--build"));
        assert_eq!(state.registration.approved_installation, id(42));
    }
}

#[test]
fn interrupted_supervisor_restore_reconciles_each_effect_without_force_restart() {
    for platform in [NativePlatform::Macos, NativePlatform::Linux] {
        let effects = if platform == NativePlatform::Macos {
            vec![SupervisorCommand::Enable, SupervisorCommand::Bootstrap]
        } else {
            vec![
                SupervisorCommand::Reload,
                SupervisorCommand::Enable,
                SupervisorCommand::Start,
            ]
        };
        for fault in effects {
            let (peer, tx, supervisor, guard) = fixture(platform);
            peer.0.lock().unwrap().fault = Some(fault);
            assert_eq!(
                supervisor.restore_approved(&tx, guard.deadline()),
                Err(NativeFailure::SupervisorUnavailable)
            );
            assert_eq!(
                supervisor.restore_approved(&tx, guard.deadline()).unwrap(),
                SupervisorObservation::Running
            );
            let state = peer.0.lock().unwrap();
            assert!(!state.events.contains(&SupervisorCommand::Retire));
            assert!(
                state
                    .events
                    .iter()
                    .filter(|event| **event == SupervisorCommand::Start
                        || **event == SupervisorCommand::Bootstrap)
                    .count()
                    <= 1
            );
        }
    }
}

#[test]
fn stopped_through_reboot_and_storage_unlock_has_no_publication_or_start_effects() {
    for platform in [NativePlatform::Macos, NativePlatform::Linux] {
        let (peer, tx, supervisor, guard) = fixture(platform);
        peer.0.lock().unwrap().registration.desired_state = DesiredServerState::Stopped;
        assert_eq!(
            supervisor.restore_approved(&tx, guard.deadline()),
            Err(NativeFailure::UnsafeNativeState)
        );
        assert_eq!(peer.0.lock().unwrap().published, 0);
        assert!(peer.0.lock().unwrap().events.is_empty());
        peer.0.lock().unwrap().registration.desired_state = DesiredServerState::Running;
        peer.0.lock().unwrap().storage_locked = true;
        assert_eq!(
            supervisor.restore_approved(&tx, guard.deadline()),
            Err(NativeFailure::StorageLocked)
        );
        assert_eq!(peer.0.lock().unwrap().published, 0);
        peer.0.lock().unwrap().storage_locked = false;
        assert_eq!(
            supervisor.restore_approved(&tx, guard.deadline()).unwrap(),
            SupervisorObservation::Running
        );
        assert_eq!(
            peer.0.lock().unwrap().registration.approved_installation,
            id(42)
        );
    }
}

#[test]
fn background_items_suppression_never_looks_like_a_missing_installation_or_a_successful_start() {
    let (peer, tx, supervisor, guard) = fixture(NativePlatform::Macos);
    peer.0.lock().unwrap().background =
        Some(spaceterm_host::native_management::macos::BackgroundItemsStatus::RequiresApproval);
    assert_eq!(
        supervisor.observe(&tx, guard.deadline()).unwrap(),
        SupervisorObservation::Suppressed
    );
    assert_eq!(
        supervisor.restore_approved(&tx, guard.deadline()),
        Err(NativeFailure::BackgroundItemsSuppressed)
    );
    assert!(peer.0.lock().unwrap().events.is_empty());
    peer.0.lock().unwrap().background =
        Some(spaceterm_host::native_management::macos::BackgroundItemsStatus::Enabled);
    assert_eq!(
        supervisor.restore_approved(&tx, guard.deadline()).unwrap(),
        SupervisorObservation::Running
    );
}

#[test]
fn supervisor_refuses_changed_approved_selection_and_uncertain_publication() {
    let (peer, tx, supervisor, guard) = fixture(NativePlatform::Linux);
    peer.0.lock().unwrap().registration.approved_installation = id(44);
    assert_eq!(
        supervisor.restore_approved(&tx, guard.deadline()),
        Err(NativeFailure::GenerationMismatch)
    );
    assert!(peer.0.lock().unwrap().events.is_empty());
    peer.0.lock().unwrap().registration.approved_installation = id(42);
    peer.0.lock().unwrap().publication_uncertain = true;
    assert_eq!(
        supervisor.restore_approved(&tx, guard.deadline()),
        Err(NativeFailure::Indeterminate)
    );
    assert!(peer.0.lock().unwrap().events.is_empty());
    assert_eq!(peer.0.lock().unwrap().published, 1);
    assert_eq!(
        supervisor.restore_approved(&tx, guard.deadline()),
        Err(NativeFailure::Indeterminate)
    );
    assert_eq!(peer.0.lock().unwrap().published, 1);
}

struct Lifecycle(Arc<Mutex<QuiescenceProgress>>);
impl ServerLifecyclePort<RuntimeAssessment> for Lifecycle {
    fn revalidate(&self, _: &RuntimeAssessment) -> NativeManagementFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn begin_quiescence(
        &self,
        _: NativeRequestId,
        _: &RuntimeAssessment,
    ) -> NativeManagementFuture<'_, QuiescenceProgress> {
        Box::pin(async { Ok(*self.0.lock().unwrap()) })
    }
    fn resolve_quiescence(
        &self,
        _: NativeRequestId,
    ) -> NativeManagementFuture<'_, QuiescenceProgress> {
        Box::pin(async { Ok(*self.0.lock().unwrap()) })
    }
}
fn ready<F: Future>(future: F) -> F::Output {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = std::pin::pin!(future);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("injected lifecycle unexpectedly waited"),
    }
}

#[test]
fn stop_and_restart_require_exact_prior_instance_retirement_from_the_real_lifecycle_port() {
    for platform in [NativePlatform::Macos, NativePlatform::Linux] {
        let (peer, tx, supervisor, guard) = fixture(platform);
        let progress = Arc::new(Mutex::new(QuiescenceProgress::Pending));
        let lifecycle = SupervisedLifecycle::new(Lifecycle(progress.clone()));
        assert!(ready(lifecycle.retirement(request_id())).unwrap().is_none());
        assert!(peer.0.lock().unwrap().events.is_empty());
        *progress.lock().unwrap() = QuiescenceProgress::CleanupUnresolved;
        assert!(matches!(
            ready(lifecycle.retirement(request_id())),
            Err(NativeFailure::CleanupUnresolved)
        ));
        *progress.lock().unwrap() = QuiescenceProgress::Complete;
        let retirement = ready(lifecycle.retirement(request_id())).unwrap().unwrap();
        let other = ready(lifecycle.retirement(NativeRequestId::new([18; 16]).unwrap()))
            .unwrap()
            .unwrap();
        assert_eq!(
            supervisor.retire(&tx, &other, guard.deadline()),
            Err(NativeFailure::IdentityMismatch)
        );
        assert!(peer.0.lock().unwrap().events.is_empty());
        peer.0.lock().unwrap().observation = SupervisorObservation::Inactive;
        supervisor
            .restart_approved(&tx, &retirement, guard.deadline())
            .unwrap();
        assert_eq!(peer.0.lock().unwrap().events[0], SupervisorCommand::Observe);
        assert_eq!(peer.0.lock().unwrap().events[1], SupervisorCommand::Retire);
        assert_eq!(
            peer.0.lock().unwrap().registration.desired_state,
            DesiredServerState::Running
        );
        peer.0.lock().unwrap().registration.desired_state = DesiredServerState::Stopped;
        assert_eq!(
            supervisor.restart_approved(&tx, &retirement, guard.deadline()),
            Err(NativeFailure::UnsafeNativeState)
        );
    }
}

#[test]
fn fixed_native_commands_never_stop_during_suspension_or_force_a_restart() {
    use commands::fixed_command;
    for platform in [NativePlatform::Macos, NativePlatform::Linux] {
        let definition = ServiceDefinition::new(
            platform,
            key(),
            Path::new("/opt/spaceterm/protected-launcher"),
        )
        .unwrap();
        let suspend = fixed_command(&definition, SupervisorCommand::Disable).unwrap();
        let args: Vec<_> = suspend.get_args().map(|a| a.to_str().unwrap()).collect();
        assert!(args.contains(&"disable"));
        assert!(
            !args
                .iter()
                .any(|a| matches!(*a, "stop" | "bootout" | "--now"))
        );
        let start = fixed_command(&definition, SupervisorCommand::Start).unwrap();
        assert!(!start.get_args().any(|a| a == "-k" || a == "restart"));
    }
}

#[test]
fn native_observation_decoder_refuses_partial_duplicate_or_error_output() {
    use commands::decode_reply;
    assert!(matches!(
        decode_reply(
            NativePlatform::Linux,
            SupervisorCommand::Observe,
            Some(0),
            b"LoadState=loaded\nActiveState=active\n"
        ),
        Ok(SupervisorReply::Observation(SupervisorObservation::Running))
    ));
    for bytes in [
        b"LoadState=loaded\n".as_slice(),
        b"LoadState=loaded\nLoadState=loaded\nActiveState=active\n",
        b"LoadState=loaded\nActiveState=alien\n",
    ] {
        assert!(
            decode_reply(
                NativePlatform::Linux,
                SupervisorCommand::Observe,
                Some(0),
                bytes
            )
            .is_err()
        );
    }
    assert!(matches!(
        decode_reply(
            NativePlatform::Macos,
            SupervisorCommand::Observe,
            Some(113),
            b""
        ),
        Ok(SupervisorReply::Observation(
            SupervisorObservation::NotLoaded
        ))
    ));
    assert!(
        decode_reply(
            NativePlatform::Macos,
            SupervisorCommand::Observe,
            Some(1),
            b"\tstate = running\n"
        )
        .is_err()
    );
    assert!(
        decode_reply(
            NativePlatform::Macos,
            SupervisorCommand::Observe,
            Some(0),
            b"\t\tstate = running\n"
        )
        .is_err()
    );
}

struct RegistrationPeer {
    head: Mutex<ProtectedRegistration>,
    retained: Mutex<Vec<(u64, InstallationId)>>,
}
impl spaceterm_host::startup::NativeStartupRegistration for RegistrationPeer {
    type RetainedInstallation = InstallationId;
    fn read_fixed_registration(
        &self,
        _: AccountBuildKey,
    ) -> Result<ProtectedRegistration, spaceterm_host::startup::StartupError> {
        Ok(*self.head.lock().unwrap())
    }
    fn retain_approved_installation(
        &self,
        _: AccountBuildKey,
        generation: NonZeroU64,
        installation: InstallationId,
    ) -> Result<InstallationId, spaceterm_host::startup::StartupError> {
        let head = self.head.lock().unwrap();
        if head.generation != generation || head.approved_installation != installation {
            return Err(spaceterm_host::startup::StartupError::GenerationMismatch);
        }
        self.retained
            .lock()
            .unwrap()
            .push((generation.get(), installation));
        Ok(installation)
    }
}
#[test]
fn stable_launcher_rereads_staged_heads_and_revalidates_a_racing_stop_before_retention() {
    use spaceterm_host::startup::{StartupAdmission, StartupError, admit_startup};
    let (peer, _, _, _) = fixture(NativePlatform::Linux);
    let registration = RegistrationPeer {
        head: Mutex::new(peer.0.lock().unwrap().registration),
        retained: Mutex::new(vec![]),
    };
    // Stage changes the head, not the service invocation or the approved selection.
    registration.head.lock().unwrap().generation = NonZeroU64::new(8).unwrap();
    let generation = current_registration_generation(key(), &registration).unwrap();
    assert!(matches!(
        admit_startup(key(), generation, &registration),
        Ok(StartupAdmission::Ready(_))
    ));
    assert_eq!(*registration.retained.lock().unwrap(), [(8, id(42))]);
    registration.retained.lock().unwrap().clear();
    let generation = current_registration_generation(key(), &registration).unwrap();
    {
        let mut head = registration.head.lock().unwrap();
        head.generation = NonZeroU64::new(9).unwrap();
        head.desired_state = DesiredServerState::Stopped;
    }
    assert!(matches!(
        admit_startup(key(), generation, &registration),
        Err(StartupError::GenerationMismatch)
    ));
    let generation = current_registration_generation(key(), &registration).unwrap();
    assert!(matches!(
        admit_startup(key(), generation, &registration),
        Ok(StartupAdmission::Stopped)
    ));
    assert!(registration.retained.lock().unwrap().is_empty());
}

#[test]
fn system_supervisor_adapter_bounds_native_output_and_reaps_timed_out_utility_children() {
    use commands::SystemSupervisorCommands;
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let directory = tempfile::Builder::new()
        .prefix("spaceterm-supervisor-")
        .tempdir()
        .unwrap();
    let binary = directory.path().join("utility");
    let compilation = Command::new("/usr/bin/cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(root.join("tests/native_management_composition/supervisor_fixture.c"))
        .arg("-o")
        .arg(&binary)
        .output();
    assert!(compilation.is_ok_and(|result| result.status.success()));
    let (peer, _, _, _) = fixture(NativePlatform::Linux);
    peer.0.lock().unwrap().utility_custody_available = true;
    for (scenario, expected) in [
        ("observe", None),
        ("denied", Some(NativeFailure::SupervisorUnavailable)),
        ("overflow", Some(NativeFailure::LimitExceeded)),
        ("wait", Some(NativeFailure::TimedOut)),
    ] {
        let custody = Custody {
            peer: peer.clone(),
            platform: NativePlatform::Linux,
            utility: Some(binary.clone()),
            scenario,
        };
        let mut lease = custody.retain(&Transaction { original: id(42) }).unwrap();
        // Ordinary utility startup uses the production budget, including native
        // executable validation under concurrent builds. Only the timeout case
        // needs a short execution deadline.
        let guard = if scenario == "wait" {
            HelperLifetimeGuard::install_for(std::time::Duration::from_secs(5)).unwrap()
        } else {
            HelperLifetimeGuard::install().unwrap()
        };
        let deadline = if scenario == "wait" {
            guard
                .deadline()
                .reserve_cleanup(std::time::Duration::from_secs(4))
                .unwrap()
        } else {
            guard.deadline()
        };
        let reply = SystemSupervisorCommands.call(
            &custody,
            &mut lease,
            SupervisorCommand::Observe,
            deadline,
        );
        match expected {
            None => assert!(
                matches!(
                    reply,
                    Ok(SupervisorReply::Observation(SupervisorObservation::Running))
                ),
                "typed reply {reply:?}"
            ),
            Some(expected) => assert!(
                matches!(reply, Err(failure) if failure == expected),
                "scenario {scenario}, typed reply {reply:?}"
            ),
        }
        assert!(
            !peer.0.lock().unwrap().utility_pending,
            "scenario {scenario} must reap the utility and release its exclusion"
        );
    }
    directory.close().unwrap();
}

#[test]
fn suppressed_background_items_does_not_block_exact_stop_cleanup() {
    let (peer, tx, supervisor, guard) = fixture(NativePlatform::Macos);
    let lifecycle = SupervisedLifecycle::new(Lifecycle(Arc::new(Mutex::new(
        QuiescenceProgress::Complete,
    ))));
    let retirement = ready(lifecycle.retirement(request_id())).unwrap().unwrap();
    peer.0.lock().unwrap().background =
        Some(spaceterm_host::native_management::macos::BackgroundItemsStatus::RequiresApproval);
    peer.0.lock().unwrap().observation = SupervisorObservation::Inactive;
    supervisor
        .retire(&tx, &retirement, guard.deadline())
        .unwrap();
    assert!(
        peer.0
            .lock()
            .unwrap()
            .events
            .contains(&SupervisorCommand::Retire)
    );
}

#[test]
fn accepted_bootout_is_not_complete_until_launchd_confirms_service_absence() {
    let (peer, tx, supervisor, guard) = fixture(NativePlatform::Macos);
    let lifecycle = SupervisedLifecycle::new(Lifecycle(Arc::new(Mutex::new(
        QuiescenceProgress::Complete,
    ))));
    let retirement = ready(lifecycle.retirement(request_id())).unwrap().unwrap();
    peer.0.lock().unwrap().observation = SupervisorObservation::Inactive;
    peer.0.lock().unwrap().retirement_delayed = true;
    assert_eq!(
        supervisor.retire(&tx, &retirement, guard.deadline()),
        Err(NativeFailure::OutcomePending)
    );
    peer.0.lock().unwrap().observation = SupervisorObservation::NotLoaded;
    supervisor
        .retire(&tx, &retirement, guard.deadline())
        .unwrap();
}

#[test]
fn supervisor_refuses_configuration_substitution_before_reload_or_enable() {
    let (peer, tx, supervisor, guard) = fixture(NativePlatform::Linux);
    peer.0.lock().unwrap().configuration = Some(b"substituted-native-peer".to_vec());
    assert_eq!(
        supervisor.restore_approved(&tx, guard.deadline()),
        Err(NativeFailure::IdentityMismatch)
    );
    assert!(peer.0.lock().unwrap().events.is_empty());
}

#[test]
fn interrupted_restart_resolves_the_replacement_without_retiring_it_again() {
    for (platform, failed_start) in [
        (NativePlatform::Macos, true),
        (NativePlatform::Linux, true),
        (NativePlatform::Macos, false),
        (NativePlatform::Linux, false),
    ] {
        let (peer, tx, supervisor, guard) = fixture(platform);
        let lifecycle = SupervisedLifecycle::new(Lifecycle(Arc::new(Mutex::new(
            QuiescenceProgress::Complete,
        ))));
        let retirement = ready(lifecycle.retirement(request_id())).unwrap().unwrap();
        let start = if platform == NativePlatform::Macos {
            SupervisorCommand::Bootstrap
        } else {
            SupervisorCommand::Start
        };
        {
            let mut state = peer.0.lock().unwrap();
            state.observation = SupervisorObservation::Inactive;
            state.fault = Some(if failed_start {
                start
            } else {
                SupervisorCommand::Observe
            });
        }
        assert_eq!(
            supervisor.restart_approved(&tx, &retirement, guard.deadline()),
            Err(NativeFailure::SupervisorUnavailable)
        );
        // Resolution runs in a fresh helper over the same existing journal owner.
        drop(supervisor);
        let supervisor = NativeSupervisor::new(
            Custody {
                peer: peer.clone(),
                platform,
                utility: None,
                scenario: "",
            },
            peer.clone(),
        );
        assert_eq!(
            supervisor.restart_approved(&tx, &retirement, guard.deadline()),
            Ok(SupervisorObservation::Running)
        );
        let state = peer.0.lock().unwrap();
        assert_eq!(
            state.events.iter().filter(|event| **event == start).count(),
            1
        );
        assert_eq!(
            state
                .events
                .iter()
                .filter(|event| **event == SupervisorCommand::Retire)
                .count(),
            1
        );
        assert_eq!(
            state.registration.desired_state,
            DesiredServerState::Running
        );
        assert_eq!(
            state.restart_phase,
            spaceterm_host::native_management::controller::ManagementPhase::SupervisionRemoved
        );
        assert_eq!(state.registration.approved_installation, id(42));
        assert_eq!(state.registration.staged_installation, Some(id(43)));
    }
}

#[test]
fn restart_records_supervision_absence_before_any_replacement_effect() {
    for failure in [
        NativeFailure::SupervisorUnavailable,
        NativeFailure::Indeterminate,
    ] {
        let (peer, tx, supervisor, guard) = fixture(NativePlatform::Linux);
        let lifecycle = SupervisedLifecycle::new(Lifecycle(Arc::new(Mutex::new(
            QuiescenceProgress::Complete,
        ))));
        let retirement = ready(lifecycle.retirement(request_id())).unwrap().unwrap();
        {
            let mut state = peer.0.lock().unwrap();
            state.observation = SupervisorObservation::Inactive;
            state.restart_publication = Some(failure);
        }
        assert_eq!(
            supervisor.restart_approved(&tx, &retirement, guard.deadline()),
            Err(failure)
        );
        {
            let state = peer.0.lock().unwrap();
            assert!(!state.events.contains(&SupervisorCommand::Start));
            assert!(!state.events.contains(&SupervisorCommand::Enable));
            assert_eq!(state.registration.approved_installation, id(42));
            assert_eq!(
                state.registration.desired_state,
                DesiredServerState::Running
            );
        }
        if failure == NativeFailure::Indeterminate {
            assert_eq!(
                supervisor.restart_approved(&tx, &retirement, guard.deadline()),
                Err(NativeFailure::Indeterminate)
            );
        } else {
            assert_eq!(
                supervisor.restart_approved(&tx, &retirement, guard.deadline()),
                Ok(SupervisorObservation::Running)
            );
            assert_eq!(
                peer.0
                    .lock()
                    .unwrap()
                    .events
                    .iter()
                    .filter(|event| **event == SupervisorCommand::Retire)
                    .count(),
                1
            );
        }
    }
}

#[test]
fn utility_execution_refuses_custody_that_cannot_survive_the_helper_deadline() {
    let directory = std::env::temp_dir().join(format!(
        "spaceterm-supervisor-no-custody-{}",
        std::process::id()
    ));
    std::fs::create_dir(&directory).unwrap();
    let binary = directory.join("utility");
    let compilation = Command::new("/usr/bin/cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/native_management_composition/supervisor_fixture.c"),
        )
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(compilation.status.success());
    let (peer, tx, _, guard) = fixture(NativePlatform::Linux);
    let custody = Custody {
        peer,
        platform: NativePlatform::Linux,
        utility: Some(binary.clone()),
        scenario: "observe",
    };
    let mut lease = custody.retain(&tx).unwrap();
    let reply = commands::SystemSupervisorCommands.call(
        &custody,
        &mut lease,
        SupervisorCommand::Observe,
        guard.deadline(),
    );
    std::fs::remove_file(&binary).unwrap();
    std::fs::remove_dir(&directory).unwrap();
    assert!(
        matches!(reply, Err(NativeFailure::CustodyUnavailable)),
        "typed reply {reply:?}"
    );
}

struct RetirementPeer {
    reaped: bool,
    exit_ready: bool,
    signal_failure: bool,
    signal_races_exit: bool,
    signals: usize,
}
impl commands::UtilityChild for RetirementPeer {
    fn poll_reaped(&mut self) -> Result<bool, NativeFailure> {
        if self.exit_ready {
            self.reaped = true;
        }
        Ok(self.reaped)
    }
    fn kill(&mut self) -> Result<(), NativeFailure> {
        self.signals += 1;
        if self.signal_races_exit {
            self.exit_ready = true;
        }
        if self.signal_failure {
            Err(NativeFailure::CleanupUnresolved)
        } else {
            Ok(())
        }
    }
}

#[test]
fn delayed_reap_and_failed_signal_keep_publication_inhibited_after_the_original_budget() {
    use std::time::{Duration, Instant};
    for signal_failure in [false, true] {
        let (peer, tx, _, _) = fixture(NativePlatform::Linux);
        let guard = HelperLifetimeGuard::install_for(Duration::from_secs(5)).unwrap();
        peer.0.lock().unwrap().utility_custody_available = true;
        let custody = Custody {
            peer: peer.clone(),
            platform: NativePlatform::Linux,
            utility: None,
            scenario: "",
        };
        let mut lease = custody.retain(&tx).unwrap();
        assert!(matches!(
            custody.begin_utility_custody(&mut lease),
            PublicationDisposition::Durable(())
        ));
        let mut child = RetirementPeer {
            reaped: false,
            exit_ready: false,
            signal_failure,
            signal_races_exit: false,
            signals: 0,
        };
        let cleanup = guard
            .deadline()
            .reserve_cleanup(Duration::from_millis(4950))
            .unwrap();
        let started = Instant::now();
        assert_eq!(
            commands::retire_utility(&mut child, cleanup),
            Err(NativeFailure::CleanupUnresolved)
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!child.reaped);
        assert_eq!(child.signals, 1);
        drop(guard);
        // The same journal owner survives this helper, not its process-local lease.
        let other = Custody {
            peer: peer.clone(),
            platform: NativePlatform::Linux,
            utility: None,
            scenario: "",
        };
        let mut other_lease = other.retain(&tx).unwrap();
        assert!(matches!(
            other.begin_utility_custody(&mut other_lease),
            PublicationDisposition::NotApplied(NativeFailure::CleanupUnresolved)
        ));
        assert!(matches!(
            other.publish_configuration(&tx, &mut other_lease, b"bounded-configuration"),
            PublicationDisposition::NotApplied(NativeFailure::CleanupUnresolved)
        ));
        assert_eq!(peer.0.lock().unwrap().published, 0);
        child.exit_ready = true;
        let recovery = HelperLifetimeGuard::install().unwrap();
        commands::retire_utility(&mut child, recovery.deadline()).unwrap();
        assert!(child.reaped);
        assert_eq!(child.signals, 1);
        custody.finish_utility_custody(&mut lease).unwrap();
        assert!(!peer.0.lock().unwrap().utility_pending);
    }
}

#[test]
fn failed_utility_signal_is_not_absence_but_an_exact_reap_can_resolve_the_exit_race() {
    let guard = HelperLifetimeGuard::install().unwrap();
    let mut child = RetirementPeer {
        reaped: false,
        exit_ready: false,
        signal_failure: true,
        signal_races_exit: true,
        signals: 0,
    };
    commands::retire_utility(&mut child, guard.deadline()).unwrap();
    assert!(child.reaped);
    assert_eq!(child.signals, 1);
}
