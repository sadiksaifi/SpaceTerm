#[cfg(feature = "test-support")]
#[path = "support/signed_installation.rs"]
mod signed_installation;
use spaceterm_host::{
    installation::{propose_lifecycle, propose_staging},
    native_deployment::contracts::{DataGenerationId, NativeFailure, PublicationDisposition},
    native_registration::{
        RegistrationHeadV2, StartupBarrier, encode_registration_head,
        writer::{
            PublicationState, RegistrationHeadWriter, RegistrationPublicationStorage,
            SystemRegistrationStorage,
        },
    },
    startup::{AccountBuildKey, BuildIdentity, DesiredServerState, InstallationId},
};
use std::{
    num::NonZeroU64,
    sync::atomic::{AtomicUsize, Ordering},
    sync::{Arc, Mutex},
};
fn nz(v: u64) -> NonZeroU64 {
    NonZeroU64::new(v).unwrap()
}
fn head() -> RegistrationHeadV2 {
    RegistrationHeadV2 {
        key: AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        },
        generation: nz(7),
        lifecycle_generation: nz(2),
        desired_state: DesiredServerState::Running,
        approved_installation: InstallationId(nz(11)),
        staged_installation: None,
        approved_data_generation: DataGenerationId::new(1).unwrap(),
        active_request: None,
        startup_barrier: StartupBarrier::Open,
        profile_sha256: [1; 32],
        approved_manifest_sha256: [2; 32],
        approved_descriptor_sha256: [3; 32],
        staged_descriptor_sha256: None,
        installation_high_water: 11,
        data_high_water: 1,
        server_id: [4; 16],
    }
}
#[derive(Clone)]
struct Storage {
    inner: Arc<Mutex<Disk>>,
    calls: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}
struct Disk {
    current: [u8; 256],
    fresh: Option<[u8; 256]>,
    failure: Option<usize>,
    trace: Vec<usize>,
}
impl Storage {
    fn new(failure: Option<usize>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Disk {
                current: encode_registration_head(&head()).unwrap(),
                fresh: None,
                failure,
                trace: vec![],
            })),
            calls: Arc::new(AtomicUsize::new(0)),
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
        }
    }
    fn event(&self, number: usize) -> Result<(), NativeFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut d = self.inner.lock().unwrap();
        d.trace.push(number);
        if d.failure == Some(number) {
            Err(NativeFailure::IoUnavailable)
        } else {
            Ok(())
        }
    }
}
impl RegistrationPublicationStorage for Storage {
    fn read_current(&self, _: AccountBuildKey) -> Result<[u8; 256], NativeFailure> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(2));
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.event(0)?;
        Ok(self.inner.lock().unwrap().current)
    }
    fn write_fresh(&self, _: AccountBuildKey, bytes: &[u8; 256]) -> Result<(), NativeFailure> {
        self.inner.lock().unwrap().fresh = Some(*bytes);
        self.event(1)
    }
    fn sync_fresh(&self) -> Result<(), NativeFailure> {
        self.event(2)
    }
    fn replace_head(&self, _: AccountBuildKey) -> Result<(), NativeFailure> {
        self.event(3)?;
        let mut d = self.inner.lock().unwrap();
        d.current = d.fresh.take().unwrap();
        Ok(())
    }
    fn sync_parent(&self) -> Result<(), NativeFailure> {
        self.event(4)
    }
}

#[test]
fn file_or_directory_sync_failure_is_indeterminate() {
    for fault in [2, 4] {
        let storage = Storage::new(Some(fault));
        let writer = RegistrationHeadWriter::new(storage.clone());
        let next = propose_lifecycle(&head(), DesiredServerState::Stopped).unwrap();
        assert_eq!(
            writer.publish_proposal(&head(), &next),
            PublicationDisposition::Indeterminate
        );
        assert_eq!(writer.state(), PublicationState::RecoveryRequired);
        assert_eq!(
            storage.inner.lock().unwrap().current,
            if fault == 2 {
                encode_registration_head(&head()).unwrap()
            } else {
                encode_registration_head(&next).unwrap()
            }
        );
    }
}
#[test]
fn repeated_sync_does_not_clear_uncertain_durability() {
    let storage = Storage::new(Some(4));
    let writer = RegistrationHeadWriter::new(storage.clone());
    let next = propose_lifecycle(&head(), DesiredServerState::Stopped).unwrap();
    assert_eq!(
        writer.publish_proposal(&head(), &next),
        PublicationDisposition::Indeterminate
    );
    storage.inner.lock().unwrap().failure = None;
    storage.sync_parent().unwrap();
    let calls = storage.calls.load(Ordering::SeqCst);
    assert_eq!(
        writer.publish_proposal(&head(), &next),
        PublicationDisposition::NotApplied(NativeFailure::Indeterminate)
    );
    assert_eq!(storage.calls.load(Ordering::SeqCst), calls);
    assert_eq!(writer.state(), PublicationState::RecoveryRequired);
}
#[test]
fn publication_serializes_competing_installers() {
    let storage = Storage::new(None);
    let writer = Arc::new(RegistrationHeadWriter::new(storage.clone()));
    let mut threads = vec![];
    for _ in 0..8 {
        let w = Arc::clone(&writer);
        threads.push(std::thread::spawn(move || {
            let original = head();
            let next = propose_staging(&original, InstallationId(nz(12)), [9; 32]).unwrap();
            w.publish_proposal(&original, &next)
        }));
    }
    let outcomes: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| **o == PublicationDisposition::Durable(()))
            .count(),
        1
    );
    assert_eq!(outcomes.iter().filter(|o| **o == PublicationDisposition::NotApplied(NativeFailure::GenerationMismatch)).count(), 7);
    assert_eq!(storage.max_active.load(Ordering::SeqCst), 1);
}
#[test]
fn publication_orders_fresh_file_sync_atomic_replace_and_parent_sync() {
    let storage = Storage::new(None);
    let writer = RegistrationHeadWriter::new(storage.clone());
    let next = propose_lifecycle(&head(), DesiredServerState::Stopped).unwrap();
    assert_eq!(
        writer.publish_proposal(&head(), &next),
        PublicationDisposition::Durable(())
    );
    assert_eq!(storage.inner.lock().unwrap().trace, vec![0, 1, 2, 3, 4]);
}
#[test]
fn error_after_entering_any_mutation_callback_is_uncertain() {
    for fault in 1..=4 {
        let storage = Storage::new(Some(fault));
        let writer = RegistrationHeadWriter::new(storage);
        let next = propose_lifecycle(&head(), DesiredServerState::Stopped).unwrap();
        assert_eq!(
            writer.publish_proposal(&head(), &next),
            PublicationDisposition::Indeterminate
        );
    }
}
#[test]
fn generation_exhaustion_and_decreasing_allocations_reject_before_storage() {
    let mut original = head();
    original.generation = nz(u64::MAX);
    assert_eq!(
        propose_lifecycle(&original, DesiredServerState::Stopped),
        Err(NativeFailure::LimitExceeded)
    );
    let storage = Storage::new(None);
    let writer = RegistrationHeadWriter::new(storage.clone());
    let mut next = propose_lifecycle(&head(), DesiredServerState::Stopped).unwrap();
    next.data_high_water = 0;
    assert_eq!(
        writer.publish_proposal(&head(), &next),
        PublicationDisposition::NotApplied(NativeFailure::InvalidRecord)
    );
    assert_eq!(storage.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn staging_never_changes_approved_selection_or_running_intent() {
    let original = head();
    let next = propose_staging(&original, InstallationId(nz(12)), [9; 32]).unwrap();
    assert_eq!(next.approved_installation, original.approved_installation);
    assert_eq!(
        next.approved_data_generation,
        original.approved_data_generation
    );
    assert_eq!(next.lifecycle_generation, original.lifecycle_generation);
    assert_eq!(next.desired_state, original.desired_state);
}
#[test]
fn production_publication_never_adopts_sampled_root_ownership_as_custody() {
    let writer = RegistrationHeadWriter::new(SystemRegistrationStorage);
    let next = propose_lifecycle(&head(), DesiredServerState::Stopped).unwrap();
    assert_eq!(
        writer.publish_proposal(&head(), &next),
        PublicationDisposition::NotApplied(NativeFailure::CustodyUnavailable)
    );
    assert_eq!(writer.state(), PublicationState::Open);
}

#[test]
fn lifecycle_generation_cannot_skip_a_native_intent() {
    let storage = Storage::new(None);
    let writer = RegistrationHeadWriter::new(storage.clone());
    let mut next = propose_lifecycle(&head(), DesiredServerState::Running).unwrap();
    next.lifecycle_generation = nz(4);
    assert_eq!(
        writer.publish_proposal(&head(), &next),
        PublicationDisposition::NotApplied(NativeFailure::GenerationMismatch)
    );
    assert_eq!(storage.calls.load(Ordering::SeqCst), 0);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "native_authority/publication.rs"]
mod fresh_publication;

#[cfg(all(
    feature = "test-support",
    any(target_os = "macos", target_os = "linux")
))]
mod account_installation {
    fn private_root() -> tempfile::TempDir {
        #[cfg(target_os = "macos")]
        let parent = std::path::PathBuf::from("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let parent = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
        tempfile::tempdir_in(parent).unwrap()
    }
    use sha2::{Digest as _, Sha256};
    use spaceterm_distribution::*;
    use spaceterm_host::{
        account_service::test_support::AccountServiceFixture,
        server_installation::test_support,
        startup::{BuildIdentity, DesiredServerState},
    };

    fn installation_root() -> tempfile::TempDir {
        let home = spaceterm_host::account_service::TrustedAccountFacts::current().unwrap();
        let root = if cfg!(target_os = "macos") {
            std::path::Path::new("/private/tmp")
        } else {
            home.home()
        };
        tempfile::Builder::new()
            .prefix("hn")
            .tempdir_in(root)
            .unwrap()
    }

    use super::signed_installation::{fixture, native_machine};

    #[derive(Default)]
    struct ConfigurationPause {
        reached: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    struct InstallManager {
        key: UpdateKey,
        configuration_pause: std::sync::Mutex<Option<std::sync::Arc<ConfigurationPause>>>,
        running: std::sync::atomic::AtomicBool,
        running_at: std::sync::Mutex<Option<tokio::time::Instant>>,
        starting_observations: std::sync::atomic::AtomicUsize,
        startup_observed: tokio::sync::Notify,
        effects: std::sync::Mutex<Vec<&'static str>>,
        require_organization_store: std::sync::atomic::AtomicBool,
        scope: std::sync::Mutex<Vec<u32>>,
    }
    impl InstallManager {
        fn new(key: UpdateKey) -> Self {
            Self {
                key,
                configuration_pause: std::sync::Mutex::new(None),
                running: std::sync::atomic::AtomicBool::new(false),
                running_at: std::sync::Mutex::new(None),
                starting_observations: std::sync::atomic::AtomicUsize::new(0),
                startup_observed: tokio::sync::Notify::new(),
                effects: std::sync::Mutex::new(Vec::new()),
                require_organization_store: std::sync::atomic::AtomicBool::new(false),
                scope: std::sync::Mutex::new(Vec::new()),
            }
        }
    }
    impl spaceterm_host::account_service::ServiceManager for InstallManager {
        fn scoped_processes<'a>(
            &'a self,
            _: &'a spaceterm_host::account_service::RegisteredServer,
        ) -> spaceterm_host::account_service::ServiceFuture<'a, Vec<u32>> {
            Box::pin(async { Ok(self.scope.lock().unwrap().clone()) })
        }
        fn status<'a>(
            &'a self,
            _: Option<&'a spaceterm_host::account_service::RegisteredServer>,
        ) -> spaceterm_host::account_service::ServiceFuture<
            'a,
            spaceterm_host::account_service::SupervisorState,
        > {
            Box::pin(async {
                Ok(if !self.running.load(std::sync::atomic::Ordering::SeqCst) {
                    spaceterm_host::account_service::SupervisorState::Inactive
                } else if self
                    .running_at
                    .lock()
                    .unwrap()
                    .is_some_and(|until| tokio::time::Instant::now() < until)
                    || self
                        .starting_observations
                        .fetch_update(
                            std::sync::atomic::Ordering::SeqCst,
                            std::sync::atomic::Ordering::SeqCst,
                            |remaining| remaining.checked_sub(1),
                        )
                        .is_ok()
                {
                    self.startup_observed.notify_one();
                    spaceterm_host::account_service::SupervisorState::Starting
                } else {
                    spaceterm_host::account_service::SupervisorState::Running
                })
            })
        }
        fn linger(
            &self,
        ) -> spaceterm_host::account_service::ServiceFuture<
            '_,
            Option<spaceterm_host::account_service::LingerState>,
        > {
            Box::pin(async {
                Ok(if cfg!(target_os = "linux") {
                    Some(spaceterm_host::account_service::LingerState::Enabled)
                } else {
                    None
                })
            })
        }
        fn configure<'a>(
            &'a self,
            registration: &'a spaceterm_host::account_service::RegisteredServer,
        ) -> spaceterm_host::account_service::ServiceFuture<'a, ()> {
            Box::pin(async move {
                self.verify_installation(registration)?;
                let pause = self.configuration_pause.lock().unwrap().take();
                if let Some(pause) = pause {
                    pause.reached.notify_one();
                    pause.release.notified().await;
                }
                self.effects.lock().unwrap().push("configure");
                Ok(())
            })
        }
        fn start<'a>(
            &'a self,
            registration: &'a spaceterm_host::account_service::RegisteredServer,
        ) -> spaceterm_host::account_service::ServiceFuture<'a, ()> {
            Box::pin(async move {
                if self
                    .require_organization_store
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    use spaceterm_host::{
                        organization_store::{migrations, schema},
                        storage::Database,
                    };
                    let database = Database::open_existing_for_application(
                        registration.organization_directory(),
                        migrations(),
                        schema::APPLICATION_ID,
                    )
                    .expect("the original install must seed storage before starting the service");
                    let identity: Vec<u8> = database
                        .query_row("SELECT server_id FROM store_metadata", [], |row| row.get(0))
                        .unwrap();
                    assert_eq!(identity, registration.server_pin().identity);
                }
                self.effects.lock().unwrap().push("start");
                self.running
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
        }
        fn stop<'a>(
            &'a self,
            _: &'a spaceterm_host::account_service::RegisteredServer,
        ) -> spaceterm_host::account_service::ServiceFuture<'a, ()> {
            Box::pin(async {
                self.running
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                self.effects.lock().unwrap().push("stop");
                Ok(())
            })
        }
        fn uninstall<'a>(
            &'a self,
            _: &'a spaceterm_host::account_service::RegisteredServer,
        ) -> spaceterm_host::account_service::ServiceFuture<'a, ()> {
            Box::pin(async {
                Err(spaceterm_host::account_service::ServiceFailure::SupervisorUnavailable)
            })
        }
        fn verify_installation(
            &self,
            registration: &spaceterm_host::account_service::RegisteredServer,
        ) -> Result<(), spaceterm_host::account_service::ServiceFailure> {
            spaceterm_host::account_service::NativeServiceInstallations::test_approved_executable(
                registration,
                self.key.clone(),
            )
            .map(drop)
        }
    }
    struct InstallClock;
    impl spaceterm_host::account_service::ServiceClock for InstallClock {
        fn now(
            &self,
        ) -> Result<
            spaceterm_host::account_service::ServiceTime,
            spaceterm_host::account_service::ServiceFailure,
        > {
            Ok(spaceterm_host::account_service::ServiceTime {
                boot: [1; 32],
                millis: 0,
            })
        }
    }
    fn installer(
        account: &AccountServiceFixture,
        key: UpdateKey,
        manager: std::sync::Arc<InstallManager>,
    ) -> spaceterm_host::account_service::ServiceSupervisor {
        use spaceterm_host::account_service::{NativeServiceInstallations, NativeServiceProcesses};
        let processes = std::sync::Arc::new(
            NativeServiceProcesses::current(std::sync::Arc::new(NativeServiceInstallations))
                .unwrap()
                .with_manager(manager.clone()),
        );
        let supervisor =
            account.with_providers(manager, processes, std::sync::Arc::new(InstallClock), None);
        account.with_installation_trust(supervisor, key)
    }
    fn read_template(registration: &spaceterm_host::account_service::RegisteredServer) -> String {
        use spaceterm_host::{
            organization_store::{
                SqliteOrganizationStore, StoreIdentity, StoreLimits, StoreServices, migrations,
                receipts::SignedReceiptReader, schema,
            },
            storage::Database,
        };
        let server = spaceterm_core::identity::ServerId::new(registration.server_pin().identity);
        let home = spaceterm_core::organization::ServerDirectory::new(
            server,
            spaceterm_host::account_service::TrustedAccountFacts::current()
                .unwrap()
                .home()
                .as_os_str()
                .as_encoded_bytes()
                .to_vec(),
        )
        .unwrap();
        let database = Database::open_existing_for_application(
            registration.organization_directory(),
            migrations(),
            schema::APPLICATION_ID,
        )
        .unwrap();
        let mut store = SqliteOrganizationStore::open_existing(
            database,
            StoreIdentity {
                server,
                account: registration.key(),
                server_public_key: registration.server_pin().public_key,
            },
            StoreLimits::production(),
            StoreServices::production(
                home,
                std::sync::Arc::new(SignedReceiptReader::new(registration.server_pin()).unwrap()),
            ),
        )
        .unwrap();
        store
            .recover_bundle()
            .unwrap()
            .core
            .organization
            .settings
            .worktree_location
            .value
            .as_str()
            .to_owned()
    }

    #[tokio::test]
    async fn signed_update_staging_preserves_running_and_stopped_intent_and_original_request() {
        use spaceterm_host::account_service::{
            InstallationStatus, ServiceOutcome, ServiceRequestState,
        };
        for desired in [DesiredServerState::Running, DesiredServerState::Stopped] {
            let root = installation_root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let (key, original_artifact) = fixture("1.2.3", 7, native_machine());
            let approved = InstallationStatus {
                installation: spaceterm_host::startup::InstallationId(
                    std::num::NonZeroU64::new(1).unwrap(),
                ),
                version: original_artifact.manifest().version.clone(),
                artifact_sha256: original_artifact.artifact_sha256(),
            };
            let registration = account
                .initialize_with_installation(desired, Some(approved))
                .unwrap();
            let store = test_support::store(&registration, key.clone()).unwrap();
            let original_id = store.stage(&original_artifact).unwrap();
            store
                .activate(original_id, store.snapshot().unwrap().generation, 1)
                .unwrap();
            drop(store);
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            let supervisor = installer(&account, key.clone(), manager.clone());
            let before = supervisor.discover().unwrap().unwrap();
            let effects = manager.effects.lock().unwrap().clone();
            let (_, candidate) = fixture("1.2.4", 7, native_machine());
            let request = supervisor.allocate_request().unwrap();
            let result = supervisor
                .stage_installation(request, account.confirm_install(candidate))
                .await
                .unwrap();
            let ServiceRequestState::Completed(ServiceOutcome::Staged(staged)) = &result.state
            else {
                panic!("candidate must be staged")
            };
            assert_eq!(staged.version.as_str(), "1.2.4");
            let after = supervisor.discover().unwrap().unwrap();
            assert_eq!(after.desired_state(), desired);
            assert_eq!(after.server_pin(), before.server_pin());
            assert_eq!(
                after.approved_installation(),
                before.approved_installation()
            );
            assert_eq!(after.staged_installation(), Some(staged));
            assert_eq!(*manager.effects.lock().unwrap(), effects);
            let reopened = installer(&account, key.clone(), manager.clone());
            assert_eq!(reopened.resolve(request).await.unwrap(), result);
            let (_, candidate) = fixture("1.2.4", 7, native_machine());
            assert_eq!(
                reopened
                    .stage_installation(request, account.confirm_install(candidate))
                    .await
                    .unwrap(),
                result
            );
            assert_eq!(*manager.effects.lock().unwrap(), effects);
            let (_, conflicting) = fixture("1.2.5", 7, native_machine());
            let conflicting_request = reopened.allocate_request().unwrap();
            assert_eq!(
                reopened
                    .stage_installation(conflicting_request, account.confirm_install(conflicting))
                    .await,
                Err(spaceterm_host::account_service::ServiceFailure::RequestConflict)
            );
            assert_eq!(
                reopened.request_progress(conflicting_request),
                Err(spaceterm_host::account_service::ServiceFailure::Pending)
            );
        }
    }

    #[tokio::test]
    async fn activation_reconciles_signed_selection_for_running_and_stopped_servers() {
        use spaceterm_host::account_service::{ServiceAction, ServiceOutcome, ServiceRequestState};
        for (stopped, interrupted) in [(false, false), (true, false), (false, true), (true, true)] {
            let root = private_root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let (key, artifact) = fixture("1.2.3", 7, native_machine());
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            let supervisor = account.with_installation_trust(account.with_providers(
                manager.clone(),
                std::sync::Arc::new(spaceterm_host::account_service::runtime_inventory::RetainedRuntimeProcesses),
                std::sync::Arc::new(InstallClock), None,
            ), key.clone());
            let install = supervisor.allocate_request().unwrap();
            supervisor
                .install(install, account.confirm_install(artifact))
                .await
                .unwrap();
            if stopped {
                let request = supervisor.allocate_request().unwrap();
                let confirmation = supervisor
                    .assess(request, ServiceAction::Stop)
                    .await
                    .unwrap()
                    .confirm()
                    .unwrap();
                supervisor.stop(confirmation).await.unwrap();
            }
            let registration = supervisor.discover().unwrap().unwrap();
            let store = test_support::store(&registration, key.clone()).unwrap();
            let (_, next) = fixture("1.2.4", 7, native_machine());
            let id = store.stage(&next).unwrap();
            let staged = store.staged_status().unwrap().unwrap();
            account
                .stage_installation(spaceterm_host::account_service::InstallationStatus {
                    installation: staged.installation,
                    version: staged.version,
                    artifact_sha256: staged.artifact_sha256,
                })
                .unwrap();
            let request = supervisor.allocate_request().unwrap();
            let confirmation = supervisor
                .assess(request, ServiceAction::Activate(id))
                .await
                .unwrap()
                .confirm()
                .unwrap();
            if interrupted {
                test_support::after_next_publication(|| {
                    spaceterm_host::account_service::test_support::fail_next_publication_before_effect();
                });
            }
            let result = supervisor.activate(confirmation).await;
            let progress = if interrupted {
                assert!(
                    result.is_err(),
                    "registration interruption must retain the original activation"
                );
                supervisor.resolve(request).await.unwrap()
            } else {
                result.unwrap()
            };
            assert!(
                matches!(progress.state, ServiceRequestState::Completed(ServiceOutcome::Activated(selected)) if selected.installation == id)
            );
            let registration = supervisor.discover().unwrap().unwrap();
            assert_eq!(
                registration.desired_state(),
                if stopped {
                    DesiredServerState::Stopped
                } else {
                    DesiredServerState::Running
                }
            );
            assert_eq!(
                registration.approved_installation().unwrap().installation,
                id
            );
            assert!(registration.staged_installation().is_none());
            let store = test_support::existing_store(&registration, key).unwrap();
            assert_eq!(store.snapshot().unwrap().approved, Some(id));
            assert!(store.snapshot().unwrap().staged.is_none());
            spaceterm_host::account_service::ServiceManager::verify_installation(
                manager.as_ref(),
                &registration,
            )
            .unwrap();
        }
    }

    #[tokio::test]
    async fn confirmed_stopped_start_rejects_activation_after_endpoint_inspection_before_admission()
    {
        use spaceterm_host::{
            account_service::{
                InstallationStatus, ServiceAction, ServiceFailure, ServiceOutcome,
                ServiceRequestState, StoppedServerExpectation,
            },
            bootstrap::setup::{AccountBootstrapSetup, BootstrapAccountRegistration},
        };
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let approved = InstallationStatus {
            installation: spaceterm_host::startup::InstallationId(
                std::num::NonZeroU64::new(1).unwrap(),
            ),
            version: artifact.manifest().version.clone(),
            artifact_sha256: artifact.artifact_sha256(),
        };
        let registration = account
            .initialize_with_installation(DesiredServerState::Stopped, Some(approved.clone()))
            .unwrap();
        let store = test_support::store(&registration, key.clone()).unwrap();
        let id = store.stage(&artifact).unwrap();
        store
            .activate(id, store.snapshot().unwrap().generation, 1)
            .unwrap();
        drop(store);
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let supervisor = account.with_installation_trust(
            account.with_providers(
                manager.clone(),
                std::sync::Arc::new(
                    spaceterm_host::account_service::runtime_inventory::RetainedRuntimeProcesses,
                ),
                std::sync::Arc::new(InstallClock),
                None,
            ),
            key,
        );
        let (_, candidate) = fixture("1.2.4", 7, native_machine());
        let staged = supervisor
            .stage_installation(
                supervisor.allocate_request().unwrap(),
                account.confirm_install(candidate),
            )
            .await
            .unwrap();
        let ServiceRequestState::Completed(ServiceOutcome::Staged(staged)) = staged.state else {
            panic!("the candidate must be staged without changing Stopped");
        };
        let setup = AccountBootstrapSetup::new(supervisor.clone());
        let confirmed = setup.inspect().await.unwrap().registration;
        let BootstrapAccountRegistration::Stopped { pin, generation } = confirmed.clone() else {
            panic!("Start confirmation must cover a stopped registration");
        };
        let expected = StoppedServerExpectation {
            pin,
            generation,
            approved,
        };
        let activation = supervisor
            .assess(
                supervisor.allocate_request().unwrap(),
                ServiceAction::Activate(staged.installation),
            )
            .await
            .unwrap()
            .confirm()
            .unwrap();
        let pause = std::sync::Arc::new(ConfigurationPause::default());
        *manager.configuration_pause.lock().unwrap() = Some(pause.clone());
        let worker = supervisor.clone();
        let activation = tokio::spawn(async move { worker.activate(activation).await });
        tokio::time::timeout(
            std::time::Duration::from_secs(300),
            pause.reached.notified(),
        )
        .await
        .expect("activation must hold the lifecycle lease before publishing its selection");
        assert_eq!(setup.inspect().await.unwrap().registration, confirmed);
        let request = supervisor.allocate_request().unwrap();
        let start = supervisor.start_stopped(request, expected.clone());

        // Delay Start admission until the competing lease owner publishes its new selection.
        pause.release.notify_one();
        activation.await.unwrap().unwrap();
        let current = supervisor.discover().unwrap().unwrap();
        assert_eq!(current.server_pin(), expected.pin);
        assert!(current.generation() > expected.generation);
        assert_eq!(current.desired_state(), DesiredServerState::Stopped);
        assert_eq!(current.approved_installation(), Some(&staged));
        let effects = manager.effects.lock().unwrap().clone();
        let journal = std::fs::read(account.anchor().join("requests")).unwrap();

        assert_eq!(start.await, Err(ServiceFailure::StaleAssessment));
        assert_eq!(*manager.effects.lock().unwrap(), effects);
        assert_eq!(
            std::fs::read(account.anchor().join("requests")).unwrap(),
            journal
        );
        assert_eq!(
            supervisor.request_progress(request),
            Err(ServiceFailure::Pending)
        );
        assert_eq!(
            supervisor.resolve(request).await,
            Err(ServiceFailure::Pending)
        );
        assert_eq!(
            supervisor.discover().unwrap().unwrap().desired_state(),
            DesiredServerState::Stopped
        );
    }

    #[tokio::test]
    async fn confirmed_stopped_start_replays_original_binding_and_rejects_changed_expectations() {
        use spaceterm_host::account_service::{
            InstallationStatus, ServiceFailure, ServiceOutcome, ServicePhase, ServiceRequestState,
            StoppedServerExpectation,
        };
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let approved = InstallationStatus {
            installation: spaceterm_host::startup::InstallationId(
                std::num::NonZeroU64::new(1).unwrap(),
            ),
            version: artifact.manifest().version.clone(),
            artifact_sha256: artifact.artifact_sha256(),
        };
        let registration = account
            .initialize_with_installation(DesiredServerState::Stopped, Some(approved.clone()))
            .unwrap();
        let store = test_support::store(&registration, key.clone()).unwrap();
        let id = store.stage(&artifact).unwrap();
        store
            .activate(id, store.snapshot().unwrap().generation, 1)
            .unwrap();
        drop(store);
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let supervisor = installer(&account, key.clone(), manager.clone());
        let expected = StoppedServerExpectation {
            pin: registration.server_pin(),
            generation: registration.generation(),
            approved,
        };
        let rejected_request = supervisor.allocate_request().unwrap();
        let mut changed_artifact = expected.clone();
        changed_artifact.approved.artifact_sha256[0] ^= 1;
        let journal = std::fs::read(account.anchor().join("requests")).unwrap();
        assert_eq!(
            supervisor
                .start_stopped(rejected_request, changed_artifact)
                .await,
            Err(ServiceFailure::StaleAssessment)
        );
        assert!(manager.effects.lock().unwrap().is_empty());
        assert_eq!(
            std::fs::read(account.anchor().join("requests")).unwrap(),
            journal
        );
        let request = supervisor.allocate_request().unwrap();
        let completed = supervisor
            .start_stopped(request, expected.clone())
            .await
            .unwrap();
        assert_eq!(completed.request, request);
        assert_eq!(completed.phase, ServicePhase::Completed);
        assert_eq!(
            completed.state,
            ServiceRequestState::Completed(ServiceOutcome::Started)
        );
        let current = supervisor.discover().unwrap().unwrap();
        assert_eq!(current.server_pin(), expected.pin);
        assert_eq!(current.desired_state(), DesiredServerState::Running);
        assert!(current.generation() > expected.generation);
        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            &["configure", "start"]
        );
        let reopened = installer(&account, key, manager.clone());
        assert_eq!(
            reopened
                .start_stopped(request, expected.clone())
                .await
                .unwrap(),
            completed
        );
        assert_eq!(reopened.start(request).await.unwrap(), completed);
        assert_eq!(reopened.resolve(request).await.unwrap(), completed);
        let journal = std::fs::read(account.anchor().join("requests")).unwrap();
        for field in 0..4 {
            let mut changed = expected.clone();
            match field {
                0 => changed.pin.identity[0] ^= 1,
                1 => changed.pin.public_key[0] ^= 1,
                2 => changed.generation = current.generation(),
                _ => changed.approved.artifact_sha256[0] ^= 1,
            }
            assert_eq!(
                reopened.start_stopped(request, changed).await,
                Err(ServiceFailure::RequestConflict)
            );
            assert_eq!(reopened.request_progress(request).unwrap(), completed);
        }
        assert_eq!(
            std::fs::read(account.anchor().join("requests")).unwrap(),
            journal
        );

        // Matching pin and generation cannot satisfy the stopped-state expectation on Running.
        let running_request = reopened.allocate_request().unwrap();
        let journal = std::fs::read(account.anchor().join("requests")).unwrap();
        assert_eq!(
            reopened
                .start_stopped(
                    running_request,
                    StoppedServerExpectation {
                        pin: current.server_pin(),
                        generation: current.generation(),
                        approved: current.approved_installation().unwrap().clone(),
                    },
                )
                .await,
            Err(ServiceFailure::StaleAssessment)
        );
        assert_eq!(
            std::fs::read(account.anchor().join("requests")).unwrap(),
            journal
        );
        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            &["configure", "start"]
        );
    }

    #[tokio::test]
    async fn signed_stage_response_loss_resolves_same_candidate_without_activation() {
        use spaceterm_host::account_service::{
            InstallationStatus, ServiceFailure, ServiceOutcome, ServicePhase, ServiceRequestState,
        };
        for phase in [
            ServicePhase::Accepted,
            ServicePhase::Publishing,
            ServicePhase::Completed,
        ] {
            let root = installation_root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let (key, original_artifact) = fixture("1.2.3", 7, native_machine());
            let approved = InstallationStatus {
                installation: spaceterm_host::startup::InstallationId(
                    std::num::NonZeroU64::new(1).unwrap(),
                ),
                version: original_artifact.manifest().version.clone(),
                artifact_sha256: original_artifact.artifact_sha256(),
            };
            let registration = account
                .initialize_with_installation(DesiredServerState::Stopped, Some(approved.clone()))
                .unwrap();
            let store = test_support::store(&registration, key.clone()).unwrap();
            let id = store.stage(&original_artifact).unwrap();
            store
                .activate(id, store.snapshot().unwrap().generation, 1)
                .unwrap();
            drop(store);
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            let supervisor = installer(&account, key.clone(), manager.clone());
            let (_, candidate) = fixture("1.2.4", 7, native_machine());
            let digest = candidate.artifact_sha256();
            let request = supervisor.allocate_request().unwrap();
            spaceterm_host::account_service::test_support::interrupt_after_phase(phase);
            assert_eq!(
                supervisor
                    .stage_installation(request, account.confirm_install(candidate))
                    .await,
                Err(ServiceFailure::SupervisorUnavailable)
            );
            let reopened = installer(&account, key.clone(), manager.clone());
            let result = reopened.resolve(request).await.unwrap();
            let ServiceRequestState::Completed(ServiceOutcome::Staged(staged)) = &result.state
            else {
                panic!("original staging must resolve")
            };
            assert_eq!(result.request, request);
            assert_eq!(staged.artifact_sha256, digest);
            let registration = reopened.discover().unwrap().unwrap();
            assert_eq!(registration.approved_installation(), Some(&approved));
            assert_eq!(registration.desired_state(), DesiredServerState::Stopped);
            assert_eq!(registration.staged_installation(), Some(staged));
            assert!(manager.effects.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn signed_stage_cache_initialization_retry_recovers_original_request_without_activation()
    {
        use spaceterm_host::account_service::{
            InstallationStatus, ServiceFailure, ServiceOutcome, ServiceRequestState,
        };
        use std::os::unix::fs::MetadataExt as _;

        for boundary in [0, 1] {
            for desired in [DesiredServerState::Running, DesiredServerState::Stopped] {
                let root = installation_root();
                let account =
                    AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
                let (key, original) = fixture("1.2.3", 7, native_machine());
                let approved = InstallationStatus {
                    installation: spaceterm_host::startup::InstallationId(super::nz(1)),
                    version: original.manifest().version.clone(),
                    artifact_sha256: original.artifact_sha256(),
                };
                let registration = account
                    .initialize_with_installation(desired, Some(approved.clone()))
                    .unwrap();
                let store = test_support::store(&registration, key.clone()).unwrap();
                let id = store.stage(&original).unwrap();
                store
                    .activate(id, store.snapshot().unwrap().generation, 1)
                    .unwrap();
                drop(store);
                drop(original);
                let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
                let supervisor = installer(&account, key.clone(), manager.clone());
                let (_, candidate) = fixture("1.2.4", 7, native_machine());
                let digest = candidate.artifact_sha256();
                let request = supervisor.allocate_request().unwrap();
                spaceterm_host::storage::testing::interrupt_initialization_at(boundary);
                assert_eq!(
                    supervisor
                        .stage_installation(request, account.confirm_install(candidate))
                        .await,
                    Err(ServiceFailure::StorageUnavailable),
                    "cache initialization boundary {boundary}"
                );
                assert_eq!(
                    supervisor.request_progress(request),
                    Err(ServiceFailure::Pending),
                    "cache initialization must precede request acceptance"
                );
                let before = supervisor.discover().unwrap().unwrap();
                assert_eq!(before.approved_installation(), Some(&approved));
                assert!(before.staged_installation().is_none());
                assert_eq!(before.desired_state(), desired);
                assert!(manager.effects.lock().unwrap().is_empty());
                let cache = supervisor
                    .client_connections_directory()
                    .parent()
                    .unwrap()
                    .join("stage-artifacts");
                let database = cache.join(spaceterm_host::storage::DATABASE_FILE_NAME);
                let directory_inode = std::fs::metadata(&cache).unwrap().ino();
                let database_inode = std::fs::metadata(&database).unwrap().ino();
                drop(supervisor);

                let reopened = installer(&account, key.clone(), manager.clone());
                let (_, candidate) = fixture("1.2.4", 7, native_machine());
                let result = reopened
                    .stage_installation(request, account.confirm_install(candidate))
                    .await
                    .unwrap_or_else(|error| {
                        panic!(
                            "original cache initialization must recover boundary {boundary}: {error:?}"
                        )
                    });
                let ServiceRequestState::Completed(ServiceOutcome::Staged(staged)) = &result.state
                else {
                    panic!("original request must stage its signed candidate")
                };
                assert_eq!(result.request, request);
                assert_eq!(staged.artifact_sha256, digest);
                let after = reopened.discover().unwrap().unwrap();
                assert_eq!(after.server_pin(), before.server_pin());
                assert_eq!(after.approved_installation(), Some(&approved));
                assert_eq!(after.staged_installation(), Some(staged));
                assert_eq!(after.desired_state(), desired);
                assert_eq!(std::fs::metadata(&cache).unwrap().ino(), directory_inode);
                assert_eq!(std::fs::metadata(&database).unwrap().ino(), database_inode);
                let selection = test_support::existing_store(&after, key.clone())
                    .unwrap()
                    .snapshot()
                    .unwrap();
                assert_eq!(selection.approved, Some(id));
                assert_eq!(selection.staged, Some(staged.installation));
                drop(reopened);
                assert_eq!(
                    installer(&account, key, manager.clone())
                        .resolve(request)
                        .await
                        .unwrap(),
                    result
                );
                assert!(manager.effects.lock().unwrap().is_empty());
            }
        }
    }

    #[tokio::test]
    async fn signed_stage_cache_initialization_refuses_unowned_or_replaced_storage() {
        use spaceterm_host::account_service::{InstallationStatus, ServiceFailure};
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        for replacement in 0..3 {
            let root = installation_root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let (key, original) = fixture("1.2.3", 7, native_machine());
            let approved = InstallationStatus {
                installation: spaceterm_host::startup::InstallationId(super::nz(1)),
                version: original.manifest().version.clone(),
                artifact_sha256: original.artifact_sha256(),
            };
            let registration = account
                .initialize_with_installation(DesiredServerState::Stopped, Some(approved.clone()))
                .unwrap();
            let store = test_support::store(&registration, key.clone()).unwrap();
            let id = store.stage(&original).unwrap();
            store
                .activate(id, store.snapshot().unwrap().generation, 1)
                .unwrap();
            drop(store);
            drop(original);
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            let supervisor = installer(&account, key.clone(), manager.clone());
            let request = supervisor.allocate_request().unwrap();
            let cache = supervisor
                .client_connections_directory()
                .parent()
                .unwrap()
                .join("stage-artifacts");
            let database = cache.join(spaceterm_host::storage::DATABASE_FILE_NAME);
            if replacement == 0 {
                // An arbitrary private empty database has no creation authority.
                std::fs::create_dir(&cache).unwrap();
                std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700)).unwrap();
                std::fs::write(&database, []).unwrap();
                std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
            } else {
                let (_, candidate) = fixture("1.2.4", 7, native_machine());
                spaceterm_host::storage::testing::interrupt_initialization_at(0);
                assert_eq!(
                    supervisor
                        .stage_installation(request, account.confirm_install(candidate))
                        .await,
                    Err(ServiceFailure::StorageUnavailable)
                );
            }
            drop(supervisor);
            if replacement == 1 {
                // Retain the original inode so the empty replacement cannot reuse it.
                let original_inode = std::fs::metadata(&database).unwrap().ino();
                std::fs::rename(&database, database.with_extension("retained-original")).unwrap();
                std::fs::write(&database, []).unwrap();
                std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
                assert_ne!(std::fs::metadata(&database).unwrap().ino(), original_inode);
            } else if replacement == 2 {
                // Preserve every cache file, including the database inode and any
                // creation record, while replacing only the private directory.
                let original_inode = std::fs::metadata(&cache).unwrap().ino();
                let database_inode = std::fs::metadata(&database).unwrap().ino();
                let original = cache.with_extension("retained-original");
                std::fs::rename(&cache, &original).unwrap();
                std::fs::create_dir(&cache).unwrap();
                std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700)).unwrap();
                for entry in std::fs::read_dir(&original).unwrap() {
                    let entry = entry.unwrap();
                    std::fs::rename(entry.path(), cache.join(entry.file_name())).unwrap();
                }
                assert_ne!(std::fs::metadata(&cache).unwrap().ino(), original_inode);
                assert_eq!(std::fs::metadata(&database).unwrap().ino(), database_inode);
            }
            let reopened = installer(&account, key, manager.clone());
            let (_, candidate) = fixture("1.2.4", 7, native_machine());
            assert!(
                reopened
                    .stage_installation(request, account.confirm_install(candidate))
                    .await
                    .is_err(),
                "unowned or substituted cache must not be adopted: {replacement}"
            );
            assert_eq!(
                reopened.request_progress(request),
                Err(ServiceFailure::Pending)
            );
            assert_eq!(std::fs::metadata(&database).unwrap().len(), 0);
            let registration = reopened.discover().unwrap().unwrap();
            assert_eq!(registration.approved_installation(), Some(&approved));
            assert!(registration.staged_installation().is_none());
            assert_eq!(registration.desired_state(), DesiredServerState::Stopped);
            assert!(manager.effects.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn signed_stage_cache_process_crash_before_sqlite_binding_refuses_empty_replacement() {
        cache_creation_process_regression(
            "account_installation::signed_stage_cache_process_crash_before_sqlite_binding_refuses_empty_replacement",
            &["sqlite-created"],
            CacheSubstitution::Database,
        )
        .await;
    }

    #[tokio::test]
    async fn signed_stage_cache_process_publication_recovery_preserves_original_scope() {
        cache_creation_process_regression(
            "account_installation::signed_stage_cache_process_publication_recovery_preserves_original_scope",
            &[
                "identity-before-effect",
                "identity-durability",
                "identity-completion",
                "identity-published",
                "seal-before-effect",
                "seal-durability",
                "seal-completion",
                "seal-published",
                "bound-before-database-publication",
            ],
            CacheSubstitution::None,
        )
        .await;
    }

    #[tokio::test]
    async fn signed_stage_cache_pending_seal_recovery_refuses_empty_database_replacement() {
        cache_creation_process_regression(
            "account_installation::signed_stage_cache_pending_seal_recovery_refuses_empty_database_replacement",
            &["seal-durability", "seal-completion", "seal-published"],
            CacheSubstitution::Database,
        )
        .await;
    }

    #[tokio::test]
    async fn signed_stage_cache_pending_publication_recovery_refuses_replaced_directory() {
        cache_creation_process_regression(
            "account_installation::signed_stage_cache_pending_publication_recovery_refuses_replaced_directory",
            &["identity-published", "seal-published"],
            CacheSubstitution::Directory,
        )
        .await;
    }

    #[tokio::test]
    async fn signed_stage_cache_bound_creation_refuses_replaced_unpublished_inode() {
        cache_creation_process_regression(
            "account_installation::signed_stage_cache_bound_creation_refuses_replaced_unpublished_inode",
            &["bound-before-database-publication"],
            CacheSubstitution::StagedDatabase,
        )
        .await;
    }

    #[derive(Clone, Copy)]
    enum CacheSubstitution {
        None,
        Database,
        Directory,
        StagedDatabase,
    }

    async fn cache_creation_process_regression(
        filter: &str,
        faults: &[&str],
        substitution: CacheSubstitution,
    ) {
        use spaceterm_host::account_service::{
            InstallationStatus, ServiceFailure, ServiceOutcome, ServiceRequestState,
        };
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        use spaceterm_host::account_service::test_support as account_faults;

        const ROOT: &str = "SPACETERM_CACHE_CREATION_ROOT";
        const FAULT: &str = "SPACETERM_CACHE_CREATION_FAULT";
        const REQUEST: &str = "original-cache-request.json";
        let replace = !matches!(substitution, CacheSubstitution::None);
        if let Some(root) = std::env::var_os(ROOT) {
            let root = std::path::Path::new(&root);
            let account = AccountServiceFixture::new(root, BuildIdentity::Development).unwrap();
            let (key, candidate) = fixture("1.2.4", 7, native_machine());
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            let supervisor = installer(&account, key, manager.clone());
            let request =
                serde_json::from_slice(&std::fs::read(root.join(REQUEST)).unwrap()).unwrap();
            let fault = std::env::var(FAULT).unwrap();
            let crash_manager = manager.clone();
            match fault.as_str() {
                "sqlite-created" => {
                    spaceterm_host::storage::testing::after_initialization_boundary(6, move || {
                        assert!(crash_manager.effects.lock().unwrap().is_empty());
                        std::process::exit(91)
                    });
                }
                "bound-before-database-publication" => {
                    spaceterm_host::storage::testing::after_initialization_boundary(8, move || {
                        assert!(crash_manager.effects.lock().unwrap().is_empty());
                        std::process::exit(91)
                    });
                }
                _ => {
                    let boundary = if fault.starts_with("identity-") { 5 } else { 2 };
                    spaceterm_host::storage::testing::after_initialization_boundary(
                        boundary,
                        move || match fault.as_str() {
                            "identity-before-effect" | "seal-before-effect" => {
                                account_faults::fail_next_publication_before_effect()
                            }
                            "identity-durability" | "seal-durability" => {
                                account_faults::fail_next_publication_durability()
                            }
                            "identity-completion" | "seal-completion" => {
                                account_faults::fail_next_mutation_completion_sync()
                            }
                            "identity-published" | "seal-published" => {
                                spaceterm_host::storage::testing::after_initialization_boundary(
                                    10,
                                    move || {
                                        assert!(crash_manager.effects.lock().unwrap().is_empty());
                                        std::process::exit(91)
                                    },
                                );
                            }
                            _ => panic!("unknown bounded cache fault"),
                        },
                    );
                }
            }
            let result = supervisor
                .stage_installation(request, account.confirm_install(candidate))
                .await;
            assert!(result.is_err(), "the selected publication fault must fire");
            assert!(manager.effects.lock().unwrap().is_empty());
            std::process::exit(91);
        }

        for fault in faults {
            for desired in [DesiredServerState::Running, DesiredServerState::Stopped] {
                let root = installation_root();
                let account =
                    AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
                let (key, original) = fixture("1.2.3", 7, native_machine());
                let approved = InstallationStatus {
                    installation: spaceterm_host::startup::InstallationId(super::nz(1)),
                    version: original.manifest().version.clone(),
                    artifact_sha256: original.artifact_sha256(),
                };
                let before = account
                    .initialize_with_installation(desired, Some(approved.clone()))
                    .unwrap();
                let store = test_support::store(&before, key.clone()).unwrap();
                let id = store.stage(&original).unwrap();
                store
                    .activate(id, store.snapshot().unwrap().generation, 1)
                    .unwrap();
                drop(store);
                drop(original);
                let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
                let supervisor = installer(&account, key.clone(), manager.clone());
                let request = supervisor.allocate_request().unwrap();
                std::fs::write(
                    root.path().join(REQUEST),
                    serde_json::to_vec(&request).unwrap(),
                )
                .unwrap();
                let runtime_anchor = supervisor.client_connections_directory().to_owned();
                let cache = runtime_anchor.parent().unwrap().join("stage-artifacts");
                drop(supervisor);
                let child_root = root.path().to_owned();
                let child_filter = filter.to_owned();
                let child_fault = fault.to_string();
                let output = tokio::task::spawn_blocking(move || {
                    std::process::Command::new(std::env::current_exe().unwrap())
                        .arg("--exact")
                        .arg(child_filter)
                        .env(ROOT, child_root)
                        .env(FAULT, child_fault)
                        .output()
                        .unwrap()
                })
                .await
                .unwrap();
                assert_eq!(
                    output.status.code(),
                    Some(91),
                    "cache crash boundary {fault}"
                );
                let directory_inode = std::fs::metadata(&cache).unwrap().ino();
                let database = cache.join(spaceterm_host::storage::DATABASE_FILE_NAME);
                let original_inode = std::fs::metadata(&database).ok().map(|meta| meta.ino());
                let creation: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(cache.join("organization-creation")).unwrap(),
                )
                .unwrap();
                let retained_database_inode = creation["database"]["inode"]
                    .as_u64()
                    .or(original_inode)
                    .or_else(|| {
                        std::fs::read_dir(&cache)
                            .unwrap()
                            .filter_map(Result::ok)
                            .find(|entry| {
                                entry
                                    .file_name()
                                    .as_encoded_bytes()
                                    .starts_with(b".organization-create-")
                            })
                            .map(|entry| entry.metadata().unwrap().ino())
                    });
                let observe_staged = matches!(substitution, CacheSubstitution::StagedDatabase)
                    || (matches!(substitution, CacheSubstitution::Directory)
                        && original_inode.is_none());
                let observed_file = if observe_staged {
                    cache.join(format!(
                        ".organization-create-{:016x}-{:016x}",
                        creation["database"]["device"].as_u64().unwrap(),
                        creation["database"]["inode"].as_u64().unwrap(),
                    ))
                } else {
                    database.clone()
                };
                match substitution {
                    CacheSubstitution::Database | CacheSubstitution::StagedDatabase => {
                        // Preserve the original object so its inode cannot be reused.
                        let old_inode = std::fs::metadata(&observed_file).unwrap().ino();
                        std::fs::rename(
                            &observed_file,
                            observed_file.with_extension("retained-original"),
                        )
                        .unwrap();
                        std::fs::write(&observed_file, []).unwrap();
                        std::fs::set_permissions(
                            &observed_file,
                            std::fs::Permissions::from_mode(0o600),
                        )
                        .unwrap();
                        assert_ne!(std::fs::metadata(&observed_file).unwrap().ino(), old_inode);
                    }
                    CacheSubstitution::Directory => {
                        // Retain every file object, but move them into a different
                        // private directory. Matching DB bytes/inode are insufficient.
                        let original = cache.with_extension("retained-original");
                        std::fs::rename(&cache, &original).unwrap();
                        std::fs::create_dir(&cache).unwrap();
                        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700))
                            .unwrap();
                        for entry in std::fs::read_dir(&original).unwrap() {
                            let entry = entry.unwrap();
                            std::fs::rename(entry.path(), cache.join(entry.file_name())).unwrap();
                        }
                        assert_ne!(std::fs::metadata(&cache).unwrap().ino(), directory_inode);
                        assert_eq!(
                            Some(std::fs::metadata(&observed_file).unwrap().ino()),
                            retained_database_inode,
                        );
                    }
                    CacheSubstitution::None => {}
                }
                let replacement_directory_inode = std::fs::metadata(&cache).unwrap().ino();
                let replacement_inode = std::fs::metadata(&observed_file)
                    .ok()
                    .map(|meta| meta.ino());
                let replacement_bytes = if replace {
                    std::fs::read(&observed_file).ok()
                } else {
                    None
                };
                let reopened = installer(&account, key.clone(), manager.clone());
                assert_eq!(
                    reopened.request_progress(request),
                    Err(ServiceFailure::Pending)
                );
                let (_, candidate) = fixture("1.2.4", 7, native_machine());
                let digest = candidate.artifact_sha256();
                if !replace {
                    let cache_database = database.clone();
                    let expected = (
                        candidate.feed().bytes().to_vec(),
                        candidate.feed().signature().to_vec(),
                        candidate.bytes().to_vec(),
                    );
                    account_faults::after_next_phase(
                        spaceterm_host::account_service::ServicePhase::Accepted,
                        move || {
                            let database = rusqlite::Connection::open(cache_database).unwrap();
                            let retained: (Vec<u8>, Vec<u8>, Vec<u8>) = database
                                .query_row(
                                    "SELECT feed,signature,archive FROM artifacts WHERE request=?1",
                                    [request.as_bytes().as_slice()],
                                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                                )
                                .unwrap();
                            assert_eq!(retained, expected);
                            Ok(())
                        },
                    );
                }
                let result = reopened
                    .stage_installation(request, account.confirm_install(candidate))
                    .await;
                if replace {
                    assert!(
                        result.is_err(),
                        "a substituted storage object must not be adopted"
                    );
                    assert_eq!(
                        reopened.request_progress(request),
                        Err(ServiceFailure::Pending)
                    );
                    assert_eq!(std::fs::read(&observed_file).ok(), replacement_bytes);
                    assert_eq!(
                        std::fs::metadata(&observed_file)
                            .ok()
                            .map(|meta| meta.ino()),
                        replacement_inode,
                    );
                    if !matches!(substitution, CacheSubstitution::Database) {
                        assert_eq!(
                            std::fs::metadata(&database).ok().map(|meta| meta.ino()),
                            original_inode,
                        );
                    }
                } else {
                    let result = result.expect("original cache publication must recover");
                    let ServiceRequestState::Completed(ServiceOutcome::Staged(staged)) =
                        &result.state
                    else {
                        panic!("original stage request must complete without activation")
                    };
                    assert_eq!(result.request, request);
                    assert_eq!(staged.artifact_sha256, digest);
                    assert_eq!(
                        Some(std::fs::metadata(&database).unwrap().ino()),
                        retained_database_inode.or(original_inode),
                    );
                    let after = reopened.discover().unwrap().unwrap();
                    assert_eq!(after.staged_installation(), Some(staged));
                    let selection = test_support::existing_store(&after, key.clone())
                        .unwrap()
                        .snapshot()
                        .unwrap();
                    assert_eq!(selection.approved, Some(id));
                    assert_eq!(selection.staged, Some(staged.installation));
                    assert_eq!(reopened.resolve(request).await.unwrap(), result);
                }
                let after = reopened.discover().unwrap().unwrap();
                assert_eq!(after.server_pin(), before.server_pin());
                assert_eq!(after.data_directory(), before.data_directory());
                assert_eq!(
                    after.organization_directory(),
                    before.organization_directory()
                );
                assert_eq!(after.approved_installation(), Some(&approved));
                assert_eq!(after.desired_state(), desired);
                if replace {
                    assert!(after.staged_installation().is_none());
                }
                assert_eq!(
                    reopened.client_connections_directory(),
                    runtime_anchor.as_path()
                );
                assert_eq!(
                    std::fs::metadata(&cache).unwrap().ino(),
                    replacement_directory_inode
                );
                assert!(manager.effects.lock().unwrap().is_empty());
            }
        }
    }

    #[tokio::test]
    async fn signed_stage_archive_copy_process_crash_resolves_original_verified_artifact() {
        use spaceterm_host::account_service::{
            InstallationStatus, ServiceOutcome, ServicePhase, ServiceRequestState,
        };
        if let Some(root) = std::env::var_os("SPACETERM_STAGE_CRASH_ROOT") {
            let account =
                AccountServiceFixture::new(std::path::Path::new(&root), BuildIdentity::Development)
                    .unwrap();
            let (key, candidate) = fixture("1.2.4", 7, native_machine());
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            let supervisor = installer(&account, key, manager);
            let request = supervisor.allocate_request().unwrap();
            std::fs::write(
                std::path::Path::new(&root).join("original-stage-request.json"),
                serde_json::to_vec(&request).unwrap(),
            )
            .unwrap();
            test_support::after_next_partial_copy(1, || std::process::exit(91));
            let _ = supervisor
                .stage_installation(request, account.confirm_install(candidate))
                .await;
            panic!("the process must exit while its original archive is being copied");
        }
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, original) = fixture("1.2.3", 7, native_machine());
        let approved = InstallationStatus {
            installation: spaceterm_host::startup::InstallationId(
                std::num::NonZeroU64::new(1).unwrap(),
            ),
            version: original.manifest().version.clone(),
            artifact_sha256: original.artifact_sha256(),
        };
        let registration = account
            .initialize_with_installation(DesiredServerState::Stopped, Some(approved.clone()))
            .unwrap();
        let store = test_support::store(&registration, key.clone()).unwrap();
        let id = store.stage(&original).unwrap();
        store
            .activate(id, store.snapshot().unwrap().generation, 1)
            .unwrap();
        drop(store);
        drop(original);
        let child_root = root.path().to_owned();
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact").arg("account_installation::signed_stage_archive_copy_process_crash_resolves_original_verified_artifact")
                .arg("--nocapture").env("SPACETERM_STAGE_CRASH_ROOT", child_root)
                .output().unwrap()
        }).await.unwrap();
        assert_eq!(
            output.status.code(),
            Some(91),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let request = serde_json::from_slice(
            &std::fs::read(root.path().join("original-stage-request.json")).unwrap(),
        )
        .unwrap();
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let reopened = installer(&account, key, manager.clone());
        let (original_progress, digest) = reopened.inspect_install_request(request).unwrap();
        assert_eq!(original_progress.phase, ServicePhase::Accepted);
        let result = reopened.resolve(request).await.unwrap();
        assert_eq!(result.request, request);
        assert_eq!(result.request_digest, original_progress.request_digest);
        let ServiceRequestState::Completed(ServiceOutcome::Staged(staged)) = result.state else {
            panic!("the original verified archive must be redelivered after process death")
        };
        assert_eq!(staged.artifact_sha256, digest);
        let registration = reopened.discover().unwrap().unwrap();
        assert_eq!(registration.approved_installation(), Some(&approved));
        assert_eq!(registration.staged_installation(), Some(&staged));
        assert_eq!(registration.desired_state(), DesiredServerState::Stopped);
        assert!(manager.effects.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn signed_stage_resolution_rejects_corrupted_or_substituted_cached_artifacts() {
        use spaceterm_host::account_service::{InstallationStatus, ServiceFailure, ServicePhase};
        for substitution in 0..3 {
            let root = installation_root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let (key, original) = fixture("1.2.3", 7, native_machine());
            let approved = InstallationStatus {
                installation: spaceterm_host::startup::InstallationId(
                    std::num::NonZeroU64::new(1).unwrap(),
                ),
                version: original.manifest().version.clone(),
                artifact_sha256: original.artifact_sha256(),
            };
            let registration = account
                .initialize_with_installation(DesiredServerState::Stopped, Some(approved.clone()))
                .unwrap();
            let store = test_support::store(&registration, key.clone()).unwrap();
            let id = store.stage(&original).unwrap();
            store
                .activate(id, store.snapshot().unwrap().generation, 1)
                .unwrap();
            drop(store);
            drop(original);
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            let supervisor = installer(&account, key.clone(), manager.clone());
            let (_, candidate) = fixture("1.2.4", 7, native_machine());
            let request = supervisor.allocate_request().unwrap();
            spaceterm_host::account_service::test_support::interrupt_after_phase(
                ServicePhase::Accepted,
            );
            assert_eq!(
                supervisor
                    .stage_installation(request, account.confirm_install(candidate))
                    .await,
                Err(ServiceFailure::SupervisorUnavailable)
            );
            let (before, _) = supervisor.inspect_install_request(request).unwrap();
            drop(supervisor);
            let cached = account
                .reopen()
                .client_connections_directory()
                .parent()
                .unwrap()
                .join("stage-artifacts")
                .join(spaceterm_host::storage::DATABASE_FILE_NAME);
            // Simulate disk corruption and same-account substitution only after
            // all native database handles close. Recovery must reauthenticate bytes.
            let database = rusqlite::Connection::open(cached).unwrap();
            let expected = if substitution == 0 {
                let mut archive: Vec<u8> = database
                    .query_row(
                        "SELECT archive FROM artifacts WHERE request=?1",
                        [request.as_bytes().as_slice()],
                        |row| row.get(0),
                    )
                    .unwrap();
                archive[0] ^= 1;
                database
                    .execute(
                        "UPDATE artifacts SET archive=?2 WHERE request=?1",
                        (request.as_bytes().as_slice(), archive),
                    )
                    .unwrap();
                ServiceFailure::ArtifactRejected
            } else {
                let (_, replacement) = fixture(
                    "1.2.5",
                    if substitution == 1 { 7 } else { 8 },
                    native_machine(),
                );
                database
                    .execute(
                        "UPDATE artifacts SET feed=?2,signature=?3,archive=?4 WHERE request=?1",
                        (
                            request.as_bytes().as_slice(),
                            replacement.feed().bytes(),
                            replacement.feed().signature(),
                            replacement.bytes(),
                        ),
                    )
                    .unwrap();
                if substitution == 1 {
                    ServiceFailure::RequestConflict
                } else {
                    ServiceFailure::ArtifactRejected
                }
            };
            drop(database);
            let reopened = installer(&account, key, manager.clone());
            assert_eq!(reopened.resolve(request).await, Err(expected));
            assert_eq!(reopened.inspect_install_request(request).unwrap().0, before);
            let registration = reopened.discover().unwrap().unwrap();
            assert_eq!(registration.approved_installation(), Some(&approved));
            assert!(registration.staged_installation().is_none());
            assert_eq!(registration.desired_state(), DesiredServerState::Stopped);
            assert!(manager.effects.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn original_first_install_recovers_every_database_initialization_boundary() {
        for boundary in 0..=9 {
            let root = private_root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let (key, artifact) = fixture("1.2.3", 7, native_machine());
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            let supervisor = installer(&account, key.clone(), manager.clone());
            let request = supervisor.allocate_request().unwrap();
            spaceterm_host::storage::testing::interrupt_initialization_at(boundary);
            assert!(
                supervisor
                    .install(request, account.confirm_install(artifact))
                    .await
                    .is_err(),
                "boundary {boundary}"
            );
            let pin = supervisor.discover().unwrap().unwrap().server_pin();
            drop(supervisor);
            let supervisor = installer(&account, key, manager.clone());
            let (_, artifact) = fixture("1.2.3", 7, native_machine());
            let result = supervisor
                .install(request, account.confirm_install(artifact))
                .await
                .unwrap_or_else(|error| {
                    panic!("original initialization must recover boundary {boundary}: {error:?}")
                });
            assert!(matches!(
                result.state,
                spaceterm_host::account_service::ServiceRequestState::Completed(_)
            ));
            let registration = supervisor.discover().unwrap().unwrap();
            assert_eq!(registration.server_pin(), pin);
            assert_eq!(
                read_template(&registration),
                spaceterm_host::git::path_template::DEFAULT_WORKTREE_PATH_TEMPLATE
            );
            assert_eq!(
                manager
                    .effects
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|effect| **effect == "start")
                    .count(),
                1
            );
        }
    }

    #[tokio::test]
    async fn original_first_install_recovers_uncertain_creation_before_binding_and_sealing() {
        // Boundary 5 precedes inode binding; boundary 4 precedes creation sealing.
        for boundary in [5, 4] {
            let root = private_root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let (key, artifact) = fixture("1.2.3", 7, native_machine());
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            let supervisor = installer(&account, key.clone(), manager.clone());
            let request = supervisor.allocate_request().unwrap();
            let replacement = supervisor.allocate_request().unwrap();
            spaceterm_host::storage::testing::after_initialization_boundary(boundary, || {
                spaceterm_host::account_service::test_support::fail_next_publication_durability();
            });
            assert!(
                supervisor
                    .install(request, account.confirm_install(artifact))
                    .await
                    .is_err(),
                "boundary {boundary}"
            );
            assert!(manager.effects.lock().unwrap().is_empty());
            drop(supervisor);
            let supervisor = installer(&account, key, manager.clone());
            assert_eq!(
                supervisor.resolve_install(replacement).await,
                Err(spaceterm_host::account_service::ServiceFailure::Pending)
            );
            assert!(matches!(
                supervisor.discover(),
                Err(spaceterm_host::account_service::ServiceFailure::Indeterminate)
            ));
            let (_, artifact) = fixture("1.2.3", 7, native_machine());
            let progress = supervisor
                .install(request, account.confirm_install(artifact))
                .await
                .unwrap_or_else(|error| {
                    panic!(
                        "original creation publication must recover boundary {boundary}: {error:?}"
                    )
                });
            assert!(matches!(
                progress.state,
                spaceterm_host::account_service::ServiceRequestState::Completed(_)
            ));
            assert_eq!(
                read_template(&supervisor.discover().unwrap().unwrap()),
                spaceterm_host::git::path_template::DEFAULT_WORKTREE_PATH_TEMPLATE
            );
            assert_eq!(supervisor.resolve_install(request).await.unwrap(), progress);
            assert_eq!(
                manager.effects.lock().unwrap().as_slice(),
                &["configure", "start"]
            );
        }
    }

    #[tokio::test]
    async fn original_first_install_recovers_uncertain_creation_identity_publication() {
        use std::os::unix::fs::MetadataExt as _;

        let root = private_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let supervisor = installer(&account, key.clone(), manager.clone());
        let request = supervisor.allocate_request().unwrap();
        spaceterm_host::storage::testing::after_initialization_boundary(5, || {
            spaceterm_host::account_service::test_support::fail_next_publication_durability();
        });
        assert!(
            supervisor
                .install(request, account.confirm_install(artifact))
                .await
                .is_err()
        );
        assert!(manager.effects.lock().unwrap().is_empty());
        // The original request's anchor remains readable. Its creation fence is
        // in registered data storage and must keep ordinary discovery closed.
        let (original, artifact_sha256) = supervisor.inspect_install_request(request).unwrap();
        assert!(matches!(
            supervisor.discover(),
            Err(spaceterm_host::account_service::ServiceFailure::Indeterminate)
        ));
        let registration_path = account.anchor().join("registration");
        let before: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registration_path).unwrap()).unwrap();
        let creation: serde_json::Value = serde_json::from_slice(
            &std::fs::read(account.data_directory().join("organization-creation")).unwrap(),
        )
        .unwrap();
        let database = account
            .data_directory()
            .join("organization")
            .join(spaceterm_host::storage::DATABASE_FILE_NAME);
        assert!(!database.exists());
        assert!(creation["database"]["inode"].as_u64().is_some());
        drop(supervisor);
        let supervisor = installer(&account, key, manager.clone());
        let (_, artifact) = fixture("1.2.3", 7, native_machine());
        let progress = supervisor
            .install(request, account.confirm_install(artifact))
            .await
            .unwrap();
        assert!(matches!(
            progress.state,
            spaceterm_host::account_service::ServiceRequestState::Completed(_)
        ));
        assert_eq!(progress.request, request);
        assert_eq!(progress.request_digest, original.request_digest);
        assert_eq!(
            supervisor.inspect_install_request(request).unwrap(),
            (progress, artifact_sha256)
        );
        let metadata = std::fs::metadata(database).unwrap();
        assert_eq!(
            Some(metadata.dev()),
            creation["database"]["device"].as_u64()
        );
        assert_eq!(Some(metadata.ino()), creation["database"]["inode"].as_u64());
        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(registration_path).unwrap()).unwrap();
        for field in [
            "key",
            "server",
            "public_key",
            "data",
            "organization",
            "runtime",
            "desired",
        ] {
            assert_eq!(after[field], before[field], "retained registration {field}");
        }
        let registration = supervisor.discover().unwrap().unwrap();
        assert_eq!(
            registration
                .approved_installation()
                .unwrap()
                .artifact_sha256,
            artifact_sha256
        );
        assert_eq!(
            read_template(&registration),
            spaceterm_host::git::path_template::DEFAULT_WORKTREE_PATH_TEMPLATE
        );
        assert_eq!(
            manager
                .effects
                .lock()
                .unwrap()
                .iter()
                .filter(|effect| **effect == "start")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn original_first_install_uncertain_publication_refuses_replaced_staged_inode() {
        use spaceterm_host::account_service::ServiceFailure;
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let root = private_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let supervisor = installer(&account, key.clone(), manager.clone());
        let request = supervisor.allocate_request().unwrap();
        spaceterm_host::storage::testing::after_initialization_boundary(5, || {
            spaceterm_host::account_service::test_support::fail_next_publication_durability();
        });
        assert!(
            supervisor
                .install(request, account.confirm_install(artifact))
                .await
                .is_err()
        );
        let original = supervisor.inspect_install_request(request).unwrap();
        let creation: serde_json::Value = serde_json::from_slice(
            &std::fs::read(account.data_directory().join("organization-creation")).unwrap(),
        )
        .unwrap();
        let organization = account.data_directory().join("organization");
        let staged = organization.join(format!(
            ".organization-create-{:016x}-{:016x}",
            creation["database"]["device"].as_u64().unwrap(),
            creation["database"]["inode"].as_u64().unwrap(),
        ));
        let retained = staged.with_extension("retained-original");
        std::fs::rename(&staged, &retained).unwrap();
        std::fs::write(&staged, []).unwrap();
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600)).unwrap();
        let replacement = std::fs::metadata(&staged).unwrap();
        assert_ne!(
            replacement.ino(),
            std::fs::metadata(&retained).unwrap().ino()
        );
        let fence_path = account.data_directory().join(".account-mutation");
        let fence = std::fs::read(&fence_path).unwrap();
        drop(supervisor);
        let supervisor = installer(&account, key, manager.clone());
        let (_, artifact) = fixture("1.2.3", 7, native_machine());
        assert_eq!(
            supervisor
                .install(request, account.confirm_install(artifact))
                .await,
            Err(ServiceFailure::UnsafeStorage)
        );
        assert_eq!(
            supervisor.inspect_install_request(request).unwrap(),
            original
        );
        assert!(matches!(
            supervisor.discover(),
            Err(ServiceFailure::Indeterminate)
        ));
        assert_eq!(std::fs::read(fence_path).unwrap(), fence);
        assert_eq!(std::fs::metadata(&staged).unwrap().ino(), replacement.ino());
        assert_eq!(std::fs::read(staged).unwrap(), Vec::<u8>::new());
        assert_eq!(
            Some(std::fs::metadata(retained).unwrap().ino()),
            creation["database"]["inode"].as_u64()
        );
        assert!(
            !organization
                .join(spaceterm_host::storage::DATABASE_FILE_NAME)
                .exists()
        );
        assert!(manager.effects.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn original_first_install_refuses_a_replacement_database_inode() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = private_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let supervisor = installer(&account, key, manager.clone());
        let request = supervisor.allocate_request().unwrap();
        spaceterm_host::storage::testing::interrupt_initialization_at(2);
        assert!(
            supervisor
                .install(request, account.confirm_install(artifact))
                .await
                .is_err()
        );
        let registration = supervisor.discover().unwrap().unwrap();
        let database = registration
            .organization_directory()
            .join(spaceterm_host::storage::DATABASE_FILE_NAME);
        let original = database.with_extension("retained-original");
        std::fs::rename(&database, &original).unwrap();
        std::fs::write(&database, []).unwrap();
        std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (_, artifact) = fixture("1.2.3", 7, native_machine());
        assert!(
            supervisor
                .install(request, account.confirm_install(artifact))
                .await
                .is_err()
        );
        assert!(manager.effects.lock().unwrap().is_empty());
        assert_eq!(std::fs::metadata(&database).unwrap().len(), 0);
    }

    #[tokio::test]
    async fn first_install_initializes_the_real_organization_store_before_starting() {
        let root = private_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let supervisor = installer(&account, key, manager);
        let request = supervisor.allocate_request().unwrap();
        supervisor
            .install(request, account.confirm_install(artifact))
            .await
            .unwrap();
        assert_eq!(
            read_template(&supervisor.discover().unwrap().unwrap()),
            spaceterm_host::git::path_template::DEFAULT_WORKTREE_PATH_TEMPLATE
        );
    }

    #[tokio::test]
    async fn new_local_server_seeds_legacy_worktree_template_once() {
        use spaceterm_host::organization_store::bootstrap::LegacyWorktreeLocation;
        let root = private_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let supervisor = installer(&account, key, manager.clone());
        let request = supervisor.allocate_request().unwrap();
        let confirmation = account
            .assess_test_install(artifact)
            .with_local_desktop_seed(Some(
                LegacyWorktreeLocation::parse("~/old/{repository}/{branch}".into()).unwrap(),
            ))
            .confirm();
        let original = supervisor.install(request, confirmation).await.unwrap();
        let registration = supervisor.discover().unwrap().unwrap();
        let pin = registration.server_pin();
        assert_eq!(read_template(&registration), "~/old/{repository}/{branch}");
        assert_eq!(
            installer(&account, manager.key.clone(), manager.clone())
                .resolve(request)
                .await
                .unwrap(),
            original
        );
        assert_eq!(supervisor.discover().unwrap().unwrap().server_pin(), pin);
        assert_eq!(read_template(&registration), "~/old/{repository}/{branch}");
        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            &["configure", "start"]
        );
    }

    #[tokio::test]
    async fn existing_headless_server_is_not_reseeded_from_client_settings() {
        use spaceterm_host::organization_store::bootstrap::LegacyWorktreeLocation;
        let root = private_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let supervisor = installer(&account, key, manager.clone());
        supervisor
            .install(
                supervisor.allocate_request().unwrap(),
                account.confirm_install(artifact),
            )
            .await
            .unwrap();
        let registration = supervisor.discover().unwrap().unwrap();
        let original = read_template(&registration);
        let (_, artifact) = fixture("1.2.3", 7, native_machine());
        let confirmation = account
            .assess_test_install(artifact)
            .with_local_desktop_seed(Some(
                LegacyWorktreeLocation::parse("~/desktop/{branch}".into()).unwrap(),
            ))
            .confirm();
        supervisor
            .install(supervisor.allocate_request().unwrap(), confirmation)
            .await
            .unwrap();
        assert_eq!(read_template(&registration), original);
        assert_eq!(
            supervisor.discover().unwrap().unwrap().server_pin(),
            registration.server_pin()
        );
        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            &["configure", "start"]
        );
    }

    #[tokio::test]
    async fn locked_server_storage_is_never_treated_as_a_new_server() {
        use spaceterm_host::{
            organization_store::{bootstrap::LegacyWorktreeLocation, migrations, schema},
            storage::Database,
        };
        let root = private_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let supervisor = installer(&account, key, manager.clone());
        supervisor
            .install(
                supervisor.allocate_request().unwrap(),
                account.confirm_install(artifact),
            )
            .await
            .unwrap();
        let registration = supervisor.discover().unwrap().unwrap();
        let original = read_template(&registration);
        let mut database = Database::open_existing_for_application(
            registration.organization_directory(),
            migrations(),
            schema::APPLICATION_ID,
        )
        .unwrap();
        let (ready, entered) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let lock = std::thread::spawn(move || {
            database.transaction(|_| {
                ready.send(()).unwrap();
                released.recv().unwrap();
                Ok(())
            })
        });
        entered.recv().unwrap();
        let (_, artifact) = fixture("1.2.3", 7, native_machine());
        let confirmation = account
            .assess_test_install(artifact)
            .with_local_desktop_seed(Some(
                LegacyWorktreeLocation::parse("~/replacement/{branch}".into()).unwrap(),
            ))
            .confirm();
        let outcome = supervisor
            .install(supervisor.allocate_request().unwrap(), confirmation)
            .await;
        release.send(()).unwrap();
        assert!(matches!(
            lock.join().unwrap(),
            spaceterm_host::storage::CommitDisposition::Durable { .. }
        ));
        assert!(outcome.is_ok());
        assert_eq!(
            supervisor.discover().unwrap().unwrap().server_pin(),
            registration.server_pin()
        );
        assert_eq!(read_template(&registration), original);
        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            &["configure", "start"]
        );
    }

    #[tokio::test]
    async fn confirmed_install_initializes_one_identity_and_replays_original_result_without_starting_again()
     {
        use spaceterm_host::account_service::{
            ServiceFailure, ServiceOutcome, ServiceRequestState,
        };
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let supervisor = installer(&account, key, manager.clone());
        let request = supervisor.allocate_request().unwrap();
        let result = supervisor
            .install(request, account.confirm_install(artifact))
            .await
            .unwrap();
        assert!(matches!(
            result.state,
            ServiceRequestState::Completed(ServiceOutcome::Installed { reused: false, .. })
        ));
        let registered = supervisor.discover().unwrap().unwrap();
        let pin = registered.server_pin();
        assert_eq!(registered.desired_state(), DesiredServerState::Running);
        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            &["configure", "start"]
        );
        assert_eq!(supervisor.resolve(request).await.unwrap(), result);
        assert_eq!(manager.effects.lock().unwrap().len(), 2);
        let (_, changed) = fixture("1.2.4", 7, native_machine());
        assert_eq!(
            supervisor
                .install(request, account.confirm_install(changed))
                .await,
            Err(ServiceFailure::RequestConflict)
        );
        assert_eq!(supervisor.discover().unwrap().unwrap().server_pin(), pin);
        supervisor.acknowledge_request(request).unwrap();
        assert_eq!(
            supervisor.resolve_install(request).await,
            Err(ServiceFailure::RequestRetired)
        );
    }
    #[tokio::test(start_paused = true)]
    async fn initial_install_observes_starting_until_running_without_resubmitting_start() {
        use spaceterm_host::account_service::{ServiceOutcome, ServicePhase, ServiceRequestState};
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let artifact_sha256 = artifact.artifact_sha256();
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        manager
            .starting_observations
            .store(2, std::sync::atomic::Ordering::SeqCst);
        let supervisor = installer(&account, key.clone(), manager.clone());
        let request = supervisor.allocate_request().unwrap();
        let completed = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            supervisor.install(request, account.confirm_install(artifact)),
        )
        .await
        .expect("the accepted install must observe startup without another mutation")
        .unwrap();
        let registration = supervisor.discover().unwrap().unwrap();
        assert_eq!(completed.request, request);
        assert_eq!(completed.phase, ServicePhase::Completed);
        assert_eq!(
            completed.state,
            ServiceRequestState::Completed(ServiceOutcome::Installed {
                server: registration.server_pin(),
                installation: registration.approved_installation().unwrap().clone(),
                reused: false,
            })
        );
        let reopened = installer(&account, key, manager.clone());
        assert_eq!(
            reopened.inspect_install_request(request).unwrap(),
            (completed, artifact_sha256)
        );
        assert_eq!(reopened.status().await.unwrap().pending, None);
        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            &["configure", "start"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn initial_install_observation_survives_observer_cancellation_and_completes_original_request()
     {
        use spaceterm_host::account_service::{ServiceOutcome, ServicePhase, ServiceRequestState};
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let artifact_sha256 = artifact.artifact_sha256();
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        manager
            .starting_observations
            .store(1, std::sync::atomic::Ordering::SeqCst);
        let supervisor = installer(&account, key.clone(), manager.clone());
        let request = supervisor.allocate_request().unwrap();
        let confirmation = account.confirm_install(artifact);
        let worker = supervisor.clone();
        let observer = tokio::spawn(async move { worker.install(request, confirmation).await });
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            manager.startup_observed.notified(),
        )
        .await
        .expect("the original worker must reach startup observation");
        let reopened = installer(&account, key, manager.clone());
        let (pending, digest) = reopened.inspect_install_request(request).unwrap();
        assert_eq!(digest, artifact_sha256);
        assert_eq!(pending.request, request);
        assert_eq!(pending.phase, ServicePhase::Starting);
        assert_eq!(pending.state, ServiceRequestState::Pending);
        observer.abort();
        assert!(observer.await.unwrap_err().is_cancelled());

        let completed = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let progress = reopened.inspect_install_request(request).unwrap().0;
                if progress.phase == ServicePhase::Completed {
                    break progress;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dropping the observer must not cancel accepted startup or its durable result");
        assert_eq!(completed.request, request);
        assert_eq!(completed.request_digest, pending.request_digest);
        assert!(matches!(
            completed.state,
            ServiceRequestState::Completed(ServiceOutcome::Installed { reused: false, .. })
        ));
        assert_eq!(
            reopened.inspect_install_request(request).unwrap(),
            (completed, artifact_sha256)
        );
        assert_eq!(reopened.status().await.unwrap().pending, None);
        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            &["configure", "start"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn initial_install_observation_is_bounded_and_keeps_original_starting_request_pending() {
        use spaceterm_host::account_service::{ServicePhase, ServiceRequestState, SupervisorState};
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let artifact_sha256 = artifact.artifact_sha256();
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        manager
            .starting_observations
            .store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
        let supervisor = installer(&account, key.clone(), manager.clone());
        let request = supervisor.allocate_request().unwrap();
        let started = tokio::time::Instant::now();
        let pending = tokio::time::timeout(
            std::time::Duration::from_secs(6),
            supervisor.install(request, account.confirm_install(artifact)),
        )
        .await
        .expect("an accepted worker must bound its startup observation")
        .unwrap();
        assert_eq!(started.elapsed(), std::time::Duration::from_secs(5));
        assert_eq!(pending.request, request);
        assert_eq!(pending.phase, ServicePhase::Starting);
        assert_eq!(pending.state, ServiceRequestState::Pending);
        let reopened = installer(&account, key, manager.clone());
        let journal = std::fs::read(account.anchor().join("requests")).unwrap();
        assert_eq!(
            reopened.inspect_install_request(request).unwrap(),
            (pending.clone(), artifact_sha256)
        );
        assert_eq!(
            reopened.retained_initial_install_requests().unwrap(),
            vec![pending]
        );
        manager
            .starting_observations
            .store(0, std::sync::atomic::Ordering::SeqCst);
        let status = reopened.status().await.unwrap();
        assert_eq!(status.supervisor, SupervisorState::Running);
        assert_eq!(status.pending, Some(request));
        assert_eq!(
            std::fs::read(account.anchor().join("requests")).unwrap(),
            journal
        );
        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            &["configure", "start"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn initial_install_pending_at_five_seconds_resolves_same_original_at_six_without_start_replay()
     {
        use spaceterm_host::account_service::{ServiceOutcome, ServicePhase, ServiceRequestState};
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let root = installation_root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let (key, artifact) = fixture("1.2.3", 7, native_machine());
            let digest = artifact.artifact_sha256();
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            let started = tokio::time::Instant::now();
            *manager.running_at.lock().unwrap() = Some(started + std::time::Duration::from_secs(6));
            let supervisor = installer(&account, key.clone(), manager.clone());
            let request = supervisor.allocate_request().unwrap();
            let pending = supervisor
                .install(request, account.confirm_install(artifact))
                .await
                .unwrap();
            assert_eq!(started.elapsed(), std::time::Duration::from_secs(5));
            assert_eq!(pending.phase, ServicePhase::Starting);
            assert_eq!(pending.state, ServiceRequestState::Pending);
            let registration = supervisor.discover().unwrap().unwrap();
            tokio::time::sleep_until(started + std::time::Duration::from_secs(6)).await;
            let reopened = installer(&account, key, manager.clone());
            let completed = reopened.resolve_install(request).await.unwrap();
            assert_eq!(completed.request, pending.request);
            assert_eq!(completed.request_digest, pending.request_digest);
            assert_eq!(
                completed.state,
                ServiceRequestState::Completed(ServiceOutcome::Installed {
                    server: registration.server_pin(),
                    installation: registration.approved_installation().unwrap().clone(),
                    reused: false,
                })
            );
            assert_eq!(
                reopened.inspect_install_request(request).unwrap(),
                (completed.clone(), digest)
            );
            assert_eq!(reopened.resolve_install(request).await.unwrap(), completed);
            assert_eq!(
                manager.effects.lock().unwrap().as_slice(),
                ["configure", "start"]
            );
        })
        .await
        .expect("the complete original-install recovery must remain bounded");
    }

    #[tokio::test(start_paused = true)]
    async fn bootstrap_install_observes_same_original_running_at_six_seconds() {
        use spaceterm_host::{
            account_service::{ServicePhase, ServiceRequestState},
            bootstrap::setup::{AccountBootstrapSetup, BootstrapAccountRegistration},
        };
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let root = installation_root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let (key, artifact) = fixture("1.2.3", 7, native_machine());
            let digest = artifact.artifact_sha256();
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            let started = tokio::time::Instant::now();
            *manager.running_at.lock().unwrap() = Some(started + std::time::Duration::from_secs(6));
            let supervisor = installer(&account, key, manager.clone());
            let setup = AccountBootstrapSetup::new(supervisor.clone());
            let request = setup.reserve_request().unwrap();
            let confirmation = account.confirm_install(artifact);
            let observer = tokio::spawn(async move {
                setup
                    .install_and_observe(request, confirmation, digest)
                    .await
            });
            manager.startup_observed.notified().await;
            let pending = supervisor.inspect_install_request(request).unwrap().0;
            assert_eq!(pending.phase, ServicePhase::Starting);
            assert_eq!(pending.state, ServiceRequestState::Pending);
            let facts = observer.await.unwrap().unwrap();
            assert_eq!(started.elapsed(), std::time::Duration::from_secs(6));
            let registration = supervisor.discover().unwrap().unwrap();
            assert!(
                matches!(facts.registration, BootstrapAccountRegistration::Running {
                pin, artifact_identity, ..
            } if pin == registration.server_pin() && artifact_identity == digest)
            );
            let completed = supervisor.inspect_install_request(request).unwrap().0;
            assert_eq!(completed.request, pending.request);
            assert_eq!(completed.request_digest, pending.request_digest);
            assert_eq!(completed.phase, ServicePhase::Completed);
            assert_eq!(supervisor.status().await.unwrap().pending, None);
            assert_eq!(
                manager.effects.lock().unwrap().as_slice(),
                ["configure", "start"]
            );
        })
        .await
        .expect("Bootstrap must complete its original request within the overall deadline");
    }

    #[tokio::test(start_paused = true)]
    async fn bootstrap_install_deadline_includes_native_wait_and_retains_original_pending_after_fifteen_seconds()
     {
        use spaceterm_host::{
            account_service::{ServiceFailure, ServicePhase, ServiceRequestState},
            bootstrap::setup::AccountBootstrapSetup,
        };
        tokio::time::timeout(std::time::Duration::from_secs(25), async {
            let root = installation_root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let (key, artifact) = fixture("1.2.3", 7, native_machine());
            let digest = artifact.artifact_sha256();
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            let started = tokio::time::Instant::now();
            *manager.running_at.lock().unwrap() =
                Some(started + std::time::Duration::from_secs(16));
            let supervisor = installer(&account, key.clone(), manager.clone());
            let setup = AccountBootstrapSetup::new(supervisor.clone());
            let request = setup.reserve_request().unwrap();
            let confirmation = account.confirm_install(artifact);
            let observer = tokio::spawn(async move {
                setup
                    .install_and_observe(request, confirmation, digest)
                    .await
            });
            manager.startup_observed.notified().await;
            let original = supervisor.inspect_install_request(request).unwrap();
            assert_eq!(observer.await.unwrap(), Err(ServiceFailure::Pending));
            assert_eq!(started.elapsed(), std::time::Duration::from_secs(15));
            let reopened = installer(&account, key, manager.clone());
            assert_eq!(reopened.inspect_install_request(request).unwrap(), original);
            assert_eq!(original.0.request, request);
            assert_eq!(original.0.phase, ServicePhase::Starting);
            assert_eq!(original.0.state, ServiceRequestState::Pending);
            assert_eq!(original.1, digest);
            let journal = std::fs::read(account.anchor().join("requests")).unwrap();
            assert_eq!(reopened.status().await.unwrap().pending, Some(request));
            assert_eq!(
                std::fs::read(account.anchor().join("requests")).unwrap(),
                journal
            );
            tokio::time::sleep_until(started + std::time::Duration::from_secs(16)).await;
            let recovered = AccountBootstrapSetup::new(reopened.clone())
                .resolve(request, digest)
                .await
                .unwrap();
            assert_eq!(recovered.key, reopened.key());
            let completed = reopened.inspect_install_request(request).unwrap().0;
            assert_eq!(completed.request_digest, original.0.request_digest);
            assert_eq!(completed.phase, ServicePhase::Completed);
            assert_eq!(
                manager.effects.lock().unwrap().as_slice(),
                ["configure", "start"]
            );
        })
        .await
        .expect("the whole Bootstrap timeout and original recovery must remain bounded");
    }

    #[tokio::test(start_paused = true)]
    async fn canceled_bootstrap_observer_recovers_original_after_native_five_second_wait_without_resubmission()
     {
        use spaceterm_host::{
            account_service::{ServicePhase, ServiceRequestState},
            bootstrap::setup::AccountBootstrapSetup,
        };
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let root = installation_root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let (key, artifact) = fixture("1.2.3", 7, native_machine());
            let digest = artifact.artifact_sha256();
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            let started = tokio::time::Instant::now();
            *manager.running_at.lock().unwrap() = Some(started + std::time::Duration::from_secs(6));
            let supervisor = installer(&account, key.clone(), manager.clone());
            let setup = AccountBootstrapSetup::new(supervisor.clone());
            let request = setup.reserve_request().unwrap();
            let confirmation = account.confirm_install(artifact);
            let observer = tokio::spawn(async move {
                setup
                    .install_and_observe(request, confirmation, digest)
                    .await
            });
            manager.startup_observed.notified().await;
            let original = supervisor.inspect_install_request(request).unwrap();
            observer.abort();
            assert!(observer.await.unwrap_err().is_cancelled());
            tokio::time::sleep_until(started + std::time::Duration::from_secs(5)).await;
            assert_eq!(
                supervisor.inspect_install_request(request).unwrap(),
                original
            );
            assert_eq!(original.0.phase, ServicePhase::Starting);
            assert_eq!(original.0.state, ServiceRequestState::Pending);
            tokio::time::sleep_until(started + std::time::Duration::from_secs(6)).await;
            let reopened = installer(&account, key, manager.clone());
            AccountBootstrapSetup::new(reopened.clone())
                .resolve(request, digest)
                .await
                .unwrap();
            let completed = reopened.inspect_install_request(request).unwrap().0;
            assert_eq!(completed.request, request);
            assert_eq!(completed.request_digest, original.0.request_digest);
            assert_eq!(completed.phase, ServicePhase::Completed);
            assert_eq!(
                manager.effects.lock().unwrap().as_slice(),
                ["configure", "start"]
            );
        })
        .await
        .expect("cancellation recovery must remain bounded without a replacement request");
    }

    #[tokio::test(start_paused = true)]
    async fn bootstrap_pending_original_refuses_changed_registration_pin_generation_or_approval() {
        use spaceterm_host::{
            account_service::ServiceFailure, bootstrap::setup::AccountBootstrapSetup,
        };
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            for field in ["server", "public_key", "generation", "approved"] {
                let root = installation_root();
                let account =
                    AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
                let (key, artifact) = fixture("1.2.3", 7, native_machine());
                let digest = artifact.artifact_sha256();
                let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
                *manager.running_at.lock().unwrap() =
                    Some(tokio::time::Instant::now() + std::time::Duration::from_secs(6));
                let supervisor = installer(&account, key, manager.clone());
                let request = supervisor.allocate_request().unwrap();
                supervisor
                    .install(request, account.confirm_install(artifact))
                    .await
                    .unwrap();
                let original = supervisor.inspect_install_request(request).unwrap();
                let path = account.anchor().join("registration");
                let mut registration: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                match field {
                    "server" | "public_key" => {
                        registration[field] =
                            serde_json::json!(vec![9; if field == "server" { 16 } else { 32 }])
                    }
                    "generation" => {
                        registration[field] =
                            serde_json::json!(registration[field].as_u64().unwrap() + 1)
                    }
                    "approved" => {
                        registration[field]["version"] =
                            serde_json::to_value(BuildVersion::parse("1.2.4").unwrap()).unwrap()
                    }
                    _ => unreachable!(),
                }
                registration.sort_all_objects();
                std::fs::write(path, serde_json::to_vec(&registration).unwrap()).unwrap();
                assert_eq!(
                    AccountBootstrapSetup::new(supervisor.clone())
                        .resolve(request, digest)
                        .await,
                    Err(ServiceFailure::RequestConflict),
                    "adopted changed {field}"
                );
                assert_eq!(
                    supervisor.inspect_install_request(request).unwrap(),
                    original
                );
                assert_eq!(
                    manager.effects.lock().unwrap().as_slice(),
                    ["configure", "start"]
                );
            }
        })
        .await
        .expect("changed-registration refusal must remain bounded");
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_assessment_keeps_original_install_pending_after_generation_drift() {
        use spaceterm_host::account_service::{
            ServiceAction, ServiceFailure, ServicePhase, ServiceRequestState, SupervisorState,
        };
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let root = installation_root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let (key, artifact) = fixture("1.2.3", 7, native_machine());
            let digest = artifact.artifact_sha256();
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            manager
                .starting_observations
                .store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
            let supervisor = installer(&account, key, manager.clone());
            let request = supervisor.allocate_request().unwrap();
            let pending = supervisor
                .install(request, account.confirm_install(artifact))
                .await
                .unwrap();
            assert_eq!(pending.phase, ServicePhase::Starting);
            assert_eq!(pending.state, ServiceRequestState::Pending);
            let original = (pending, digest);
            let registered = supervisor.discover().unwrap().unwrap();
            let registration_path = account.anchor().join("registration");
            let mut registration: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&registration_path).unwrap()).unwrap();
            registration["generation"] = serde_json::json!(registered.generation() + 1);
            registration.sort_all_objects();
            std::fs::write(
                &registration_path,
                serde_json::to_vec(&registration).unwrap(),
            )
            .unwrap();
            let changed = supervisor.discover().unwrap().unwrap();
            assert_eq!(changed.server_pin(), registered.server_pin());
            assert_eq!(
                changed.approved_installation(),
                registered.approved_installation()
            );
            assert_eq!(changed.generation(), registered.generation() + 1);
            manager
                .starting_observations
                .store(0, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(
                supervisor.status().await.unwrap().supervisor,
                SupervisorState::Running
            );
            let lifecycle_request = supervisor.allocate_request().unwrap();
            let journal_path = account.anchor().join("requests");
            let journal = std::fs::read(&journal_path).unwrap();
            let retained: serde_json::Value = serde_json::from_slice(&journal).unwrap();
            assert!(retained["reservations"][0]["install"]["start_basis"].is_object());
            assert_eq!(
                supervisor.inspect_install_request(request).unwrap(),
                original
            );

            let result = supervisor
                .assess(lifecycle_request, ServiceAction::Stop)
                .await
                .map(drop);

            assert_eq!(
                supervisor.inspect_install_request(request).unwrap(),
                original,
                "lifecycle assessment completed the original through changed generation"
            );
            assert_eq!(
                manager.effects.lock().unwrap().as_slice(),
                ["configure", "start"]
            );
            assert_eq!(
                std::fs::read(journal_path).unwrap(),
                journal,
                "lifecycle assessment replaced a request or its retained start basis"
            );
            assert_eq!(result, Err(ServiceFailure::RequestConflict));
            assert_eq!(supervisor.status().await.unwrap().pending, Some(request));
        })
        .await
        .expect(
            "lifecycle assessment must refuse original-install generation drift without effects",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_start_keeps_original_install_pending_after_generation_drift() {
        use spaceterm_host::account_service::{
            ServiceFailure, ServicePhase, ServiceRequestState, SupervisorState,
        };
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let root = installation_root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let (key, artifact) = fixture("1.2.3", 7, native_machine());
            let digest = artifact.artifact_sha256();
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            manager
                .starting_observations
                .store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
            let supervisor = installer(&account, key, manager.clone());
            let request = supervisor.allocate_request().unwrap();
            let pending = supervisor
                .install(request, account.confirm_install(artifact))
                .await
                .unwrap();
            assert_eq!(pending.phase, ServicePhase::Starting);
            assert_eq!(pending.state, ServiceRequestState::Pending);
            let original = (pending, digest);
            let registered = supervisor.discover().unwrap().unwrap();
            let registration_path = account.anchor().join("registration");
            let mut registration: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&registration_path).unwrap()).unwrap();
            registration["generation"] = serde_json::json!(registered.generation() + 1);
            registration.sort_all_objects();
            std::fs::write(
                &registration_path,
                serde_json::to_vec(&registration).unwrap(),
            )
            .unwrap();
            let changed = supervisor.discover().unwrap().unwrap();
            assert_eq!(changed.server_pin(), registered.server_pin());
            assert_eq!(
                changed.approved_installation(),
                registered.approved_installation()
            );
            assert_eq!(changed.generation(), registered.generation() + 1);
            manager
                .starting_observations
                .store(0, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(
                supervisor.status().await.unwrap().supervisor,
                SupervisorState::Running
            );
            let lifecycle_request = supervisor.allocate_request().unwrap();
            let journal_path = account.anchor().join("requests");
            let journal = std::fs::read(&journal_path).unwrap();
            let retained: serde_json::Value = serde_json::from_slice(&journal).unwrap();
            assert!(retained["reservations"][0]["install"]["start_basis"].is_object());
            assert_eq!(
                supervisor.inspect_install_request(request).unwrap(),
                original
            );

            let result = supervisor.start(lifecycle_request).await.map(drop);

            assert_eq!(
                supervisor.inspect_install_request(request).unwrap(),
                original,
                "lifecycle start completed the original through changed generation"
            );
            assert_eq!(
                manager.effects.lock().unwrap().as_slice(),
                ["configure", "start"]
            );
            assert_eq!(
                std::fs::read(journal_path).unwrap(),
                journal,
                "lifecycle start replaced a request or its retained start basis"
            );
            assert_eq!(result, Err(ServiceFailure::RequestConflict));
            assert_eq!(supervisor.status().await.unwrap().pending, Some(request));
        })
        .await
        .expect("lifecycle start must refuse original-install generation drift without effects");
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_assessment_keeps_original_install_pending_after_runtime_root_drift() {
        use spaceterm_host::account_service::{
            ServiceAction, ServiceFailure, ServicePhase, ServiceRequestState, SupervisorState,
        };
        use std::os::unix::fs::DirBuilderExt as _;
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let root = installation_root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let (key, artifact) = fixture("1.2.3", 7, native_machine());
            let digest = artifact.artifact_sha256();
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            manager
                .starting_observations
                .store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
            let supervisor = installer(&account, key, manager.clone());
            let request = supervisor.allocate_request().unwrap();
            let pending = supervisor
                .install(request, account.confirm_install(artifact))
                .await
                .unwrap();
            assert_eq!(pending.phase, ServicePhase::Starting);
            assert_eq!(pending.state, ServiceRequestState::Pending);
            let original = (pending, digest);
            let registered = supervisor.discover().unwrap().unwrap();
            let registration_path = account.anchor().join("registration");
            let mut registration: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&registration_path).unwrap()).unwrap();
            let runtime = account.anchor().join("changed-runtime");
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&runtime)
                .unwrap();
            registration["runtime"] = serde_json::to_value(&runtime).unwrap();
            registration.sort_all_objects();
            std::fs::write(
                &registration_path,
                serde_json::to_vec(&registration).unwrap(),
            )
            .unwrap();
            let changed = supervisor.discover().unwrap().unwrap();
            assert_eq!(changed.server_pin(), registered.server_pin());
            assert_eq!(
                changed.approved_installation(),
                registered.approved_installation()
            );
            assert_eq!(changed.generation(), registered.generation());
            changed.endpoint().unwrap();
            manager
                .starting_observations
                .store(0, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(
                supervisor.status().await.unwrap().supervisor,
                SupervisorState::Running
            );
            let lifecycle_request = supervisor.allocate_request().unwrap();
            let journal_path = account.anchor().join("requests");
            let journal = std::fs::read(&journal_path).unwrap();
            let retained: serde_json::Value = serde_json::from_slice(&journal).unwrap();
            assert!(retained["reservations"][0]["install"]["start_basis"].is_object());
            assert_eq!(
                supervisor.inspect_install_request(request).unwrap(),
                original
            );

            let result = supervisor
                .assess(lifecycle_request, ServiceAction::Stop)
                .await
                .map(drop);

            assert_eq!(
                supervisor.inspect_install_request(request).unwrap(),
                original,
                "lifecycle assessment completed the original through changed runtime_root"
            );
            assert_eq!(
                manager.effects.lock().unwrap().as_slice(),
                ["configure", "start"]
            );
            assert_eq!(
                std::fs::read(journal_path).unwrap(),
                journal,
                "lifecycle assessment replaced a request or its retained start basis"
            );
            assert_eq!(result, Err(ServiceFailure::RequestConflict));
            assert_eq!(supervisor.status().await.unwrap().pending, Some(request));
        })
        .await
        .expect(
            "lifecycle assessment must refuse original-install runtime_root drift without effects",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_start_keeps_original_install_pending_after_runtime_root_drift() {
        use spaceterm_host::account_service::{
            ServiceFailure, ServicePhase, ServiceRequestState, SupervisorState,
        };
        use std::os::unix::fs::DirBuilderExt as _;
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let root = installation_root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let (key, artifact) = fixture("1.2.3", 7, native_machine());
            let digest = artifact.artifact_sha256();
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            manager
                .starting_observations
                .store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
            let supervisor = installer(&account, key, manager.clone());
            let request = supervisor.allocate_request().unwrap();
            let pending = supervisor
                .install(request, account.confirm_install(artifact))
                .await
                .unwrap();
            assert_eq!(pending.phase, ServicePhase::Starting);
            assert_eq!(pending.state, ServiceRequestState::Pending);
            let original = (pending, digest);
            let registered = supervisor.discover().unwrap().unwrap();
            let registration_path = account.anchor().join("registration");
            let mut registration: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&registration_path).unwrap()).unwrap();
            let runtime = account.anchor().join("changed-runtime");
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&runtime)
                .unwrap();
            registration["runtime"] = serde_json::to_value(&runtime).unwrap();
            registration.sort_all_objects();
            std::fs::write(
                &registration_path,
                serde_json::to_vec(&registration).unwrap(),
            )
            .unwrap();
            let changed = supervisor.discover().unwrap().unwrap();
            assert_eq!(changed.server_pin(), registered.server_pin());
            assert_eq!(
                changed.approved_installation(),
                registered.approved_installation()
            );
            assert_eq!(changed.generation(), registered.generation());
            changed.endpoint().unwrap();
            manager
                .starting_observations
                .store(0, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(
                supervisor.status().await.unwrap().supervisor,
                SupervisorState::Running
            );
            let lifecycle_request = supervisor.allocate_request().unwrap();
            let journal_path = account.anchor().join("requests");
            let journal = std::fs::read(&journal_path).unwrap();
            let retained: serde_json::Value = serde_json::from_slice(&journal).unwrap();
            assert!(retained["reservations"][0]["install"]["start_basis"].is_object());
            assert_eq!(
                supervisor.inspect_install_request(request).unwrap(),
                original
            );

            let result = supervisor.start(lifecycle_request).await.map(drop);

            assert_eq!(
                supervisor.inspect_install_request(request).unwrap(),
                original,
                "lifecycle start completed the original through changed runtime_root"
            );
            assert_eq!(
                manager.effects.lock().unwrap().as_slice(),
                ["configure", "start"]
            );
            assert_eq!(
                std::fs::read(journal_path).unwrap(),
                journal,
                "lifecycle start replaced a request or its retained start basis"
            );
            assert_eq!(result, Err(ServiceFailure::RequestConflict));
            assert_eq!(supervisor.status().await.unwrap().pending, Some(request));
        })
        .await
        .expect("lifecycle start must refuse original-install runtime_root drift without effects");
    }

    #[tokio::test(start_paused = true)]
    async fn bootstrap_legacy_starting_original_without_checkpoint_stays_pending_without_adopting_registration()
     {
        use spaceterm_host::{
            account_service::ServiceFailure, bootstrap::setup::AccountBootstrapSetup,
        };
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let root = installation_root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let (key, artifact) = fixture("1.2.3", 7, native_machine());
            let digest = artifact.artifact_sha256();
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            manager
                .starting_observations
                .store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
            let supervisor = installer(&account, key, manager.clone());
            let request = supervisor.allocate_request().unwrap();
            let pending = supervisor
                .install(request, account.confirm_install(artifact))
                .await
                .unwrap();
            let path = account.anchor().join("requests");
            let mut journal: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            journal["reservations"][0]["install"]
                .as_object_mut()
                .unwrap()
                .remove("start_basis");
            journal.sort_all_objects();
            let bytes = serde_json::to_vec(&journal).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            manager
                .starting_observations
                .store(0, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(
                supervisor.inspect_install_request(request).unwrap(),
                (pending.clone(), digest)
            );
            assert_eq!(
                AccountBootstrapSetup::new(supervisor.clone())
                    .resolve(request, digest)
                    .await,
                Err(ServiceFailure::Pending)
            );
            assert_eq!(supervisor.resolve_install(request).await.unwrap(), pending);
            assert_eq!(std::fs::read(path).unwrap(), bytes);
            assert_eq!(
                manager.effects.lock().unwrap().as_slice(),
                ["configure", "start"]
            );
        })
        .await
        .expect("a legacy original must remain safely pending without effects");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn unrelated_unobservable_process_does_not_block_native_absence() {
        use spaceterm_core::authority::ServerEpoch;
        use spaceterm_host::account_service::{
            NativeServiceProcessTable, NativeServiceProcesses, ServiceFailure, ServiceProcesses,
        };
        use std::{
            io::{BufRead as _, BufReader},
            process::{Command, Stdio},
        };
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let root = installation_root();
        let source = root.path().join("unobservable.c");
        let executable = root.path().join("unobservable");
        std::fs::write(&source, b"#include <sys/prctl.h>\n#include <stdio.h>\n#include <unistd.h>\nint main(void) { if (prctl(PR_SET_DUMPABLE, 0) != 0) return 1; puts(\"ready\"); fflush(stdout); for (;;) pause(); }\n").unwrap();
        let mut compiler = spaceterm_host::process::spawn(
            Command::new("cc").arg(&source).arg("-o").arg(&executable),
        )
        .unwrap();
        assert!(compiler.wait().unwrap().success());
        let mut child = Child(
            spaceterm_host::process::spawn(Command::new(&executable).stdout(Stdio::piped()))
                .unwrap(),
        );
        let mut ready = String::new();
        BufReader::new(child.0.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "ready\n");
        assert_eq!(
            NativeServiceProcessTable::current()
                .unwrap()
                .observe(child.0.id()),
            Err(ServiceFailure::CleanupUnresolved)
        );
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let supervisor = installer(&account, key.clone(), manager.clone());
        let request = supervisor.allocate_request().unwrap();
        supervisor
            .install(request, account.confirm_install(artifact))
            .await
            .unwrap();
        let registration = supervisor.discover().unwrap().unwrap();
        let native = NativeServiceProcesses::current(std::sync::Arc::new(SignedInstallations(key)))
            .unwrap()
            .with_manager(manager.clone());
        assert_eq!(
            native
                .previous_instance_cleanup(&registration, ServerEpoch::new(0), &[])
                .await,
            Ok(Vec::new())
        );
        *manager.scope.lock().unwrap() = vec![child.0.id()];
        assert_eq!(
            native
                .previous_instance_cleanup(&registration, ServerEpoch::new(0), &[])
                .await,
            Err(ServiceFailure::CleanupUnresolved)
        );
    }

    #[tokio::test]
    async fn initial_install_creates_organization_store_before_service_readiness() {
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        manager
            .require_organization_store
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let supervisor = installer(&account, key, manager.clone());
        let request = supervisor.allocate_request().unwrap();
        supervisor
            .install(request, account.confirm_install(artifact))
            .await
            .unwrap();
        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            &["configure", "start"]
        );
    }

    #[tokio::test]
    async fn initial_install_refuses_foreign_organization_storage_without_starting_service() {
        use spaceterm_host::{account_service::ServiceFailure, storage::Database};
        let root = installation_root();
        let canonical = root.path().canonicalize().unwrap();
        let data = canonical.join("new-data");
        let runtime = canonical.join("new-runtime");
        let organization = data.join("organization");
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&organization).unwrap();
        for directory in [&data, &organization] {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        drop(Database::open_for_application(&organization, &[], 1).unwrap());
        let before = std::fs::read(organization.join("organization.sqlite3")).unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let supervisor = installer(&account, key, manager.clone());
        let request = supervisor.allocate_request().unwrap();
        let confirmation = account
            .assess_test_install(artifact)
            .with_roots(data, runtime)
            .unwrap()
            .confirm();
        assert_eq!(
            supervisor.install(request, confirmation).await,
            Err(ServiceFailure::InvalidRecord)
        );
        assert!(manager.effects.lock().unwrap().is_empty());
        assert_eq!(
            std::fs::read(organization.join("organization.sqlite3")).unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn concurrent_first_install_reuses_one_identity_and_first_approved_xdg_roots() {
        use spaceterm_host::account_service::{
            ServiceFailure, ServiceOutcome, ServiceRequestState,
        };
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, first) = fixture("1.2.3", 7, native_machine());
        let (_, second) = fixture("1.2.3", 7, native_machine());
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let left = installer(&account, key.clone(), manager.clone());
        let right = installer(&account, key, manager.clone());
        let left_id = left.allocate_request().unwrap();
        let right_id = right.allocate_request().unwrap();
        let roots = [
            (
                root.path()
                    .canonicalize()
                    .unwrap()
                    .join("first-xdg-data/server"),
                root.path()
                    .canonicalize()
                    .unwrap()
                    .join("first-xdg-runtime/server"),
            ),
            (
                root.path()
                    .canonicalize()
                    .unwrap()
                    .join("second-xdg-data/server"),
                root.path()
                    .canonicalize()
                    .unwrap()
                    .join("second-xdg-runtime/server"),
            ),
        ];
        let first = account
            .assess_test_install(first)
            .with_roots(roots[0].0.clone(), roots[0].1.clone())
            .unwrap()
            .confirm();
        let second = account
            .assess_test_install(second)
            .with_roots(roots[1].0.clone(), roots[1].1.clone())
            .unwrap()
            .confirm();
        let barrier = std::sync::Barrier::new(2);
        let (left_result, right_result) = std::thread::scope(|scope| {
            let left = left.clone();
            let right = right.clone();
            let barrier = &barrier;
            let first = scope.spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                barrier.wait();
                runtime.block_on(left.install(left_id, first))
            });
            let second = scope.spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                barrier.wait();
                runtime.block_on(right.install(right_id, second))
            });
            (first.join().unwrap(), second.join().unwrap())
        });
        let mut outcomes = Vec::new();
        for (provider, request, initial, root_index) in [
            (&left, left_id, left_result, 0),
            (&right, right_id, right_result, 1),
        ] {
            let result = match initial {
                Ok(result) => result,
                Err(ServiceFailure::Pending) => {
                    let (_, retry) = fixture("1.2.3", 7, native_machine());
                    provider
                        .install(
                            request,
                            account
                                .assess_test_install(retry)
                                .with_roots(
                                    roots[root_index].0.clone(),
                                    roots[root_index].1.clone(),
                                )
                                .unwrap()
                                .confirm(),
                        )
                        .await
                        .unwrap()
                }
                Err(error) => panic!("unexpected install classification {error:?}"),
            };
            let ServiceRequestState::Completed(ServiceOutcome::Installed {
                server,
                installation,
                reused,
            }) = result.state
            else {
                panic!("installation did not complete");
            };
            outcomes.push((server, installation, reused));
        }
        assert_eq!(outcomes[0].0, outcomes[1].0);
        assert_eq!(outcomes[0].1, outcomes[1].1);
        assert_ne!(outcomes[0].2, outcomes[1].2);
        let winner = usize::from(outcomes[0].2);
        let retained = left.discover().unwrap().unwrap();
        assert_eq!(retained.data_directory(), roots[winner].0);
        assert!(roots[winner].1.exists());
        assert!(!roots[1 - winner].0.exists());
        assert!(!roots[1 - winner].1.exists());
        assert_eq!(
            right.discover().unwrap().unwrap().data_directory(),
            roots[winner].0
        );

        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            &["configure", "start"]
        );
    }

    #[test]
    fn test_support_trust_never_changes_production_artifact_assessment() {
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let provider = installer(&account, key, manager);
        assert!(matches!(
            provider.assess_install(artifact),
            Err(spaceterm_host::account_service::ServiceFailure::ArtifactRejected)
        ));
        assert!(provider.discover().unwrap().is_none());
    }

    #[tokio::test]
    async fn install_reuses_stopped_registration_without_replacing_approval_or_starting() {
        use spaceterm_host::account_service::{
            InstallationStatus, ServiceOutcome, ServiceRequestState,
        };
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let original = InstallationStatus {
            installation: spaceterm_host::startup::InstallationId(
                std::num::NonZeroU64::new(9).unwrap(),
            ),
            version: BuildVersion::parse("1.2.2").unwrap(),
            artifact_sha256: [3; 32],
        };
        let registered = account
            .initialize_with_installation(DesiredServerState::Stopped, Some(original.clone()))
            .unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let supervisor = installer(&account, key, manager.clone());
        let request = supervisor.allocate_request().unwrap();
        let result = supervisor
            .install(request, account.confirm_install(artifact))
            .await
            .unwrap();
        assert!(
            matches!(result.state, ServiceRequestState::Completed(ServiceOutcome::Installed { reused: true, installation, .. }) if installation == original)
        );
        let reused = supervisor.discover().unwrap().unwrap();
        assert_eq!(reused.server_pin(), registered.server_pin());
        assert_eq!(reused.desired_state(), DesiredServerState::Stopped);
        assert!(manager.effects.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn install_resumes_original_request_after_interrupted_archive_extraction() {
        use spaceterm_host::account_service::{
            ServiceFailure, ServiceOutcome, ServiceRequestState,
        };
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let supervisor = installer(&account, key.clone(), manager.clone());
        let request = supervisor.allocate_request().unwrap();
        test_support::after_next_partial_copy(1, || {});
        assert_eq!(
            supervisor
                .install(request, account.confirm_install(artifact))
                .await,
            Err(ServiceFailure::StorageUnavailable)
        );
        let original_pin = supervisor.discover().unwrap().unwrap().server_pin();
        let reopened = installer(&account, key, manager.clone());
        let pending = reopened.resolve(request).await.unwrap();
        assert_eq!(pending.request, request);
        assert_eq!(pending.state, ServiceRequestState::Pending);
        let (_, artifact) = fixture("1.2.3", 7, native_machine());
        let completed = reopened
            .install(request, account.confirm_install(artifact))
            .await
            .unwrap();
        assert!(
            matches!(completed.state, ServiceRequestState::Completed(ServiceOutcome::Installed { reused: false, server, .. }) if server == original_pin)
        );
        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            &["configure", "start"]
        );
    }

    #[tokio::test]
    async fn interrupted_first_install_keeps_approved_roots_before_identity_creation() {
        use spaceterm_host::account_service::{ServiceFailure, ServicePhase, ServiceRequestState};
        let root = installation_root();
        let canonical = root.path().canonicalize().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let supervisor = installer(&account, key.clone(), manager.clone());
        let request = supervisor.allocate_request().unwrap();
        let data = canonical.join("approved-data/server");
        let runtime = canonical.join("approved-runtime/server");
        let confirmation = account
            .assess_test_install(artifact)
            .with_roots(data.clone(), runtime.clone())
            .unwrap()
            .confirm();
        spaceterm_host::account_service::test_support::interrupt_after_phase(
            ServicePhase::Accepted,
        );
        assert_eq!(
            supervisor.install(request, confirmation).await,
            Err(ServiceFailure::SupervisorUnavailable)
        );
        assert!(supervisor.discover().unwrap().is_none());
        assert!(!data.exists());
        let reopened = installer(&account, key, manager.clone());
        let pending = reopened.resolve_install(request).await.unwrap();
        assert_eq!(pending.state, ServiceRequestState::Pending);
        let (_, artifact) = fixture("1.2.3", 7, native_machine());
        let changed = canonical.join("replacement-data/server");
        let changed_runtime = canonical.join("replacement-runtime/server");
        let different = account
            .assess_test_install(artifact)
            .with_roots(changed.clone(), changed_runtime.clone())
            .unwrap()
            .confirm();
        assert_eq!(
            reopened.install(request, different).await,
            Err(ServiceFailure::RequestConflict)
        );
        assert!(!changed.exists());
        assert!(!changed_runtime.exists());
        assert!(manager.effects.lock().unwrap().is_empty());
        let (_, artifact) = fixture("1.2.3", 7, native_machine());
        let original = account
            .assess_test_install(artifact)
            .with_roots(data.clone(), runtime.clone())
            .unwrap()
            .confirm();
        let completed = reopened.install(request, original).await.unwrap();
        assert_eq!(completed.request, request);
        assert_eq!(completed.request_digest, pending.request_digest);
        assert_eq!(reopened.discover().unwrap().unwrap().data_directory(), data);
        assert!(runtime.exists());
        let (_, artifact) = fixture("1.2.3", 7, native_machine());
        let different = account
            .assess_test_install(artifact)
            .with_roots(changed, changed_runtime)
            .unwrap()
            .confirm();
        assert_eq!(
            reopened.install(request, different).await,
            Err(ServiceFailure::RequestConflict)
        );
        assert_eq!(reopened.resolve_install(request).await.unwrap(), completed);
        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            &["configure", "start"]
        );
    }

    #[tokio::test]
    async fn signed_install_and_stage_response_loss_resolve_original_request() {
        use spaceterm_host::account_service::{
            ServiceFailure, ServiceOutcome, ServicePhase, ServiceRequestState,
        };
        for phase in [ServicePhase::Publishing, ServicePhase::Completed] {
            let root = installation_root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let (key, artifact) = fixture("1.2.3", 7, native_machine());
            let artifact_sha256 = artifact.artifact_sha256();
            let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
            let supervisor = installer(&account, key.clone(), manager.clone());
            let request = supervisor.allocate_request().unwrap();
            spaceterm_host::account_service::test_support::interrupt_after_phase(phase);
            assert_eq!(
                supervisor
                    .install(request, account.confirm_install(artifact))
                    .await,
                Err(ServiceFailure::SupervisorUnavailable)
            );
            let original = supervisor.discover().unwrap().unwrap().server_pin();
            let reopened = installer(&account, key, manager.clone());
            let result = reopened.resolve(request).await.unwrap();
            let ServiceRequestState::Completed(ServiceOutcome::Installed {
                server,
                installation,
                reused: false,
            }) = &result.state
            else {
                panic!("original signed installation did not resolve");
            };
            assert_eq!(*server, original);
            assert_eq!(installation.artifact_sha256, artifact_sha256);
            assert_eq!(result.request, request);
            assert_eq!(reopened.resolve_install(request).await.unwrap(), result);
            assert_eq!(
                manager.effects.lock().unwrap().as_slice(),
                &["configure", "start"]
            );
        }
    }

    struct SignedInstallations(UpdateKey);
    impl spaceterm_host::account_service::ServiceInstallations for SignedInstallations {
        fn approved_executable(
            &self,
            registration: &spaceterm_host::account_service::RegisteredServer,
        ) -> Result<std::path::PathBuf, spaceterm_host::account_service::ServiceFailure> {
            let selected = test_support::existing_store(registration, self.0.clone())?
                .retain_compatible_approved(1)?;
            let status = selected.status();
            let expected = registration
                .approved_installation()
                .ok_or(spaceterm_host::account_service::ServiceFailure::Missing)?;
            if status.installation != expected.installation
                || status.artifact_sha256 != expected.artifact_sha256
                || status.version != expected.version
            {
                return Err(spaceterm_host::account_service::ServiceFailure::IdentityMismatch);
            }
            Ok(selected.path().to_owned())
        }
        fn uninstall(
            &self,
            _: &spaceterm_host::account_service::RegisteredServer,
        ) -> Result<(), spaceterm_host::account_service::ServiceFailure> {
            Err(spaceterm_host::account_service::ServiceFailure::ConfirmationRequired)
        }
    }

    struct PreviousRuntime {
        expected: Vec<spaceterm_core::authority::SessionKey>,
        observations: Vec<spaceterm_core::retirement::SessionCleanupObservation>,
    }
    impl spaceterm_host::account_service::ServiceProcesses for PreviousRuntime {
        fn assess<'a>(
            &'a self,
            _: &'a spaceterm_host::account_service::RegisteredServer,
        ) -> spaceterm_host::account_service::ServiceFuture<
            'a,
            spaceterm_host::account_service::RuntimeAssessment,
        > {
            Box::pin(async {
                Err(spaceterm_host::account_service::ServiceFailure::SupervisorUnavailable)
            })
        }
        fn revalidate<'a>(
            &'a self,
            _: &'a spaceterm_host::account_service::RegisteredServer,
            _: &'a spaceterm_host::account_service::RuntimeAssessment,
        ) -> spaceterm_host::account_service::ServiceFuture<'a, ()> {
            Box::pin(async {
                Err(spaceterm_host::account_service::ServiceFailure::SupervisorUnavailable)
            })
        }
        fn abort_reservation<'a>(
            &'a self,
            _: &'a spaceterm_host::account_service::RegisteredServer,
            _: &'a spaceterm_host::account_service::RuntimeAssessment,
        ) -> spaceterm_host::account_service::ServiceFuture<'a, ()> {
            Box::pin(async {
                Err(spaceterm_host::account_service::ServiceFailure::SupervisorUnavailable)
            })
        }
        fn quiesce<'a>(
            &'a self,
            _: &'a spaceterm_host::account_service::RegisteredServer,
            _: spaceterm_core::native_management::NativeManagementRequestRef,
            _: spaceterm_core::native_management::NativeRetirementAction,
            _: &'a spaceterm_host::account_service::RuntimeAssessment,
        ) -> spaceterm_host::account_service::ServiceFuture<
            'a,
            Vec<spaceterm_core::retirement::SessionCleanupObservation>,
        > {
            Box::pin(async {
                Err(spaceterm_host::account_service::ServiceFailure::SupervisorUnavailable)
            })
        }
        fn verify_absent<'a>(
            &'a self,
            _: &'a spaceterm_host::account_service::RegisteredServer,
        ) -> spaceterm_host::account_service::ServiceFuture<'a, ()> {
            Box::pin(async {
                Err(spaceterm_host::account_service::ServiceFailure::CleanupUnresolved)
            })
        }
        fn previous_instance_cleanup<'a>(
            &'a self,
            _: &'a spaceterm_host::account_service::RegisteredServer,
            epoch: spaceterm_core::authority::ServerEpoch,
            sessions: &'a [spaceterm_core::authority::SessionKey],
        ) -> spaceterm_host::account_service::ServiceFuture<
            'a,
            Vec<spaceterm_core::retirement::SessionCleanupObservation>,
        > {
            Box::pin(async move {
                assert_eq!(sessions, self.expected);
                assert_eq!(epoch, self.expected[0].epoch);
                Ok(self.observations.clone())
            })
        }
    }

    #[tokio::test]
    async fn trusted_previous_instance_cleanup_matches_every_old_session() {
        use spaceterm_core::{
            authority::{ServerEpoch, SessionGeneration, SessionKey},
            identity::{PaneId, ServerId},
            organization::{FailureClass, RetirementReason},
            retirement::{CleanupResult, ConfirmedCleanupClass, SessionCleanupObservation},
        };
        use spaceterm_host::account_service::{
            NativeServiceProcesses, ServiceFailure, ServiceProcesses,
        };
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let supervisor = installer(&account, key.clone(), manager.clone());
        let request = supervisor.allocate_request().unwrap();
        supervisor
            .install(request, account.confirm_install(artifact))
            .await
            .unwrap();
        let registration = supervisor.discover().unwrap().unwrap();
        let server = ServerId::new(registration.server_pin().identity);
        let epoch = ServerEpoch::new(4);
        let sessions: Vec<_> = [1, 2]
            .map(|pane| SessionKey {
                epoch,
                pane: PaneId::new(server, pane).unwrap(),
                generation: SessionGeneration::new(3),
            })
            .into();
        let observations: Vec<_> = sessions
            .iter()
            .map(|session| {
                SessionCleanupObservation::new(
                    *session,
                    RetirementReason::ServerRecovery,
                    CleanupResult::Confirmed(
                        ConfirmedCleanupClass::AlreadyAbsentWithConfirmedOwnership,
                    ),
                )
                .unwrap()
            })
            .collect();
        let installations = std::sync::Arc::new(SignedInstallations(key));
        let missing_runtime = NativeServiceProcesses::current(installations.clone())
            .unwrap()
            .with_manager(manager.clone());
        assert_eq!(
            missing_runtime
                .previous_instance_cleanup(&registration, epoch, &sessions)
                .await,
            Err(ServiceFailure::CleanupUnresolved)
        );
        assert_eq!(
            missing_runtime
                .previous_instance_cleanup(&registration, epoch, &[])
                .await,
            Err(ServiceFailure::CleanupUnresolved)
        );
        assert_eq!(
            missing_runtime
                .previous_instance_cleanup(&registration, ServerEpoch::new(0), &[])
                .await,
            Ok(Vec::new())
        );
        let failed = SessionCleanupObservation::new(
            sessions[1],
            RetirementReason::ServerRecovery,
            CleanupResult::Failed(FailureClass::RuntimeUnavailable),
        )
        .unwrap();
        let changed = SessionCleanupObservation::new(
            SessionKey {
                generation: SessionGeneration::new(4),
                ..sessions[1]
            },
            RetirementReason::ServerRecovery,
            CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
        )
        .unwrap();
        for returned in [
            vec![],
            vec![observations[0]],
            vec![observations[0], observations[0]],
            vec![observations[1], observations[0]],
            vec![observations[0], changed],
            vec![observations[0], failed],
        ] {
            let native = NativeServiceProcesses::current(installations.clone())
                .unwrap()
                .with_manager(manager.clone())
                .with_runtime(std::sync::Arc::new(PreviousRuntime {
                    expected: sessions.clone(),
                    observations: returned,
                }));
            assert_eq!(
                native
                    .previous_instance_cleanup(&registration, epoch, &sessions)
                    .await,
                Err(ServiceFailure::CleanupUnresolved)
            );
        }
        let native = NativeServiceProcesses::current(installations)
            .unwrap()
            .with_manager(manager.clone())
            .with_runtime(std::sync::Arc::new(PreviousRuntime {
                expected: sessions.clone(),
                observations: observations.clone(),
            }));
        assert_eq!(
            native
                .previous_instance_cleanup(&registration, epoch, &sessions)
                .await
                .unwrap(),
            observations
        );
        assert_eq!(
            native
                .previous_instance_cleanup(&registration, ServerEpoch::new(5), &sessions)
                .await,
            Err(ServiceFailure::InvalidRecord)
        );
        assert_eq!(
            native
                .previous_instance_cleanup(&registration, epoch, &[sessions[0], sessions[0]])
                .await,
            Err(ServiceFailure::InvalidRecord)
        );
        assert_eq!(
            native
                .previous_instance_cleanup(&registration, epoch, &[sessions[1], sessions[0]])
                .await,
            Err(ServiceFailure::InvalidRecord)
        );
        let endpoint = registration.endpoint().unwrap();
        let listener = std::os::unix::net::UnixListener::bind(endpoint.path()).unwrap();
        assert_eq!(
            native
                .previous_instance_cleanup(&registration, epoch, &sessions)
                .await,
            Err(ServiceFailure::CleanupUnresolved)
        );
        drop(listener);
        assert_eq!(
            native
                .previous_instance_cleanup(&registration, epoch, &sessions)
                .await
                .unwrap(),
            observations
        );
    }

    #[test]
    fn confirmed_uninstall_removes_approved_and_staged_code_but_retains_identity() {
        use spaceterm_host::server_installation::{
            InstallationAdmission, InstallationOperation, InstallationScope,
        };
        struct Confirmed;
        impl InstallationAdmission for Confirmed {
            fn verify(
                &self,
                _: &spaceterm_host::account_service::RegisteredServer,
                _: InstallationOperation,
            ) -> Result<(), spaceterm_host::account_service::ServiceFailure> {
                Ok(())
            }
        }
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let pin = registered.server_pin();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let id = store.stage(&artifact).unwrap();
        store
            .activate(id, store.snapshot().unwrap().generation, 1)
            .unwrap();
        let path = store.retain_approved().unwrap().path().to_owned();
        let (_, next) = fixture("1.2.4", 7, native_machine());
        store.stage(&next).unwrap();
        let scope = InstallationScope::new(&registered, &Confirmed).unwrap();
        let inner = test_support::existing_store(&registered, key).unwrap();
        inner.uninstall(&scope).unwrap();
        assert!(!path.exists());
        assert_eq!(inner.snapshot().unwrap().approved, None);
        assert_eq!(inner.snapshot().unwrap().staged, None);
        assert_eq!(
            account.reopen().discover().unwrap().unwrap().server_pin(),
            pin
        );
        inner.uninstall(&scope).unwrap();
    }

    #[test]
    fn staging_does_not_activate_or_start_a_stopped_registration() {
        use std::os::unix::fs::PermissionsExt;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let id = store.stage(&artifact).unwrap();
        let snapshot = store.snapshot().unwrap();
        assert_eq!(snapshot.staged, Some(id));
        assert_eq!(snapshot.approved, None);
        assert_eq!(registered.desired_state(), DesiredServerState::Stopped);
        let versions = registered.data_directory().join("installations");
        assert_eq!(
            std::fs::metadata(&versions).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let executable = versions
            .join(format!("version-{}", id.0.get()))
            .join(&artifact.manifest().entry_points.server);
        assert_eq!(
            std::fs::metadata(executable).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn stored_packaging_feed_with_build_commit_reopens_compatible_approved_executable() {
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Running).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let feed: serde_json::Value = serde_json::from_slice(artifact.feed().bytes()).unwrap();
        assert_eq!(
            feed["build_commit"],
            "0101010101010101010101010101010101010101"
        );
        let store = test_support::store(&registered, key.clone()).unwrap();
        let approved = store.stage(&artifact).unwrap();
        store
            .activate(approved, store.snapshot().unwrap().generation, 1)
            .unwrap();
        drop(store);
        let selected = test_support::existing_store(&registered, key)
            .unwrap()
            .retain_compatible_approved(1)
            .unwrap();
        assert_eq!(selected.status().installation, approved);
        assert_eq!(
            selected.status().artifact_sha256,
            artifact.artifact_sha256()
        );
    }

    #[test]
    fn signed_staging_preserves_approved_code_resources_and_intent() {
        use std::os::unix::fs::MetadataExt as _;
        for desired in [DesiredServerState::Running, DesiredServerState::Stopped] {
            let root = installation_root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let registered = account.initialize(desired).unwrap();
            let pin = registered.server_pin();
            let (key, original) = fixture("1.2.3", 7, native_machine());
            let store = test_support::store(&registered, key.clone()).unwrap();
            let approved = store.stage(&original).unwrap();
            store
                .activate(approved, store.snapshot().unwrap().generation, 1)
                .unwrap();
            let executable = store.retain_compatible_approved(1).unwrap();
            let before = std::fs::metadata(executable.path()).unwrap();
            let original_status = executable.status();
            let (_, candidate) = fixture("1.2.4", 7, native_machine());
            let candidate_id = store.stage(&candidate).unwrap();
            assert_ne!(candidate_id, approved);
            let staged = store.staged_status().unwrap().unwrap();
            assert_eq!(staged.installation, candidate_id);
            assert_eq!(staged.version, BuildVersion::parse("1.2.4").unwrap());
            assert_eq!(staged.artifact_sha256, candidate.artifact_sha256());
            assert_ne!(staged.artifact_sha256, original_status.artifact_sha256);
            drop(store);
            let reopened = test_support::existing_store(&registered, key).unwrap();
            let selected = reopened.retain_compatible_approved(1).unwrap();
            assert_eq!(selected.status(), original_status);
            assert_eq!(reopened.snapshot().unwrap().approved, Some(approved));
            assert_eq!(reopened.snapshot().unwrap().staged, Some(candidate_id));
            let after = std::fs::metadata(selected.path()).unwrap();
            assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
            let tree = registered
                .data_directory()
                .join("installations")
                .join(format!("version-{}", approved.0.get()));
            for file in &original.manifest().files {
                let bytes = std::fs::read(tree.join(&file.name)).unwrap();
                assert_eq!(bytes.len() as u64, file.size);
                assert_eq!(<[u8; 32]>::from(Sha256::digest(bytes)), file.sha256);
            }
            let discovered = account.reopen().discover().unwrap().unwrap();
            assert_eq!(discovered.server_pin(), pin);
            assert_eq!(discovered.desired_state(), desired);
        }
    }

    #[test]
    fn activation_and_rollback_select_exact_verified_versions_and_retain_the_previous() {
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, first) = fixture("1.2.3", 7, native_machine());
        let (_, second) = fixture("1.2.4", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let first_id = store.stage(&first).unwrap();
        store
            .activate(first_id, store.snapshot().unwrap().generation, 1)
            .unwrap();
        let second_id = store.stage(&second).unwrap();
        assert_eq!(store.retain_approved().unwrap().installation(), first_id);
        store
            .activate(second_id, store.snapshot().unwrap().generation, 1)
            .unwrap();
        let snapshot = store.snapshot().unwrap();
        assert_eq!(snapshot.approved, Some(second_id));
        assert_eq!(snapshot.previous, Some(first_id));
        drop(store);
        let reopened = test_support::store(&registered, key).unwrap();
        reopened.rollback(snapshot.generation, 1).unwrap();
        assert_eq!(reopened.retain_approved().unwrap().installation(), first_id);
        assert_eq!(reopened.snapshot().unwrap().previous, Some(second_id));
        assert_eq!(
            account
                .reopen()
                .discover()
                .unwrap()
                .unwrap()
                .desired_state(),
            DesiredServerState::Stopped
        );
    }

    #[test]
    fn stale_selection_and_incompatible_storage_cannot_activate_a_candidate() {
        use spaceterm_host::account_service::ServiceFailure;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Running).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let old = store.snapshot().unwrap();
        let id = store.stage(&artifact).unwrap();
        assert_eq!(
            store.activate(id, old.generation, 1),
            Err(ServiceFailure::RequestConflict)
        );
        let staged = store.snapshot().unwrap();
        assert_eq!(
            store.activate(id, staged.generation, 2),
            Err(ServiceFailure::IncompatibleStorage)
        );
        assert_eq!(store.snapshot().unwrap(), staged);
    }

    #[test]
    fn actual_wrong_architecture_is_rejected_and_the_original_preparation_can_be_discarded() {
        use spaceterm_host::account_service::ServiceFailure;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, 0);
        let store = test_support::store(&registered, key.clone()).unwrap();
        assert_eq!(
            store.stage(&artifact),
            Err(ServiceFailure::ArtifactRejected)
        );
        drop(store);
        let reopened = test_support::store(&registered, key).unwrap();
        let snapshot = reopened.snapshot().unwrap();
        assert_eq!(snapshot.approved, None);
        let preparing = snapshot.preparing.unwrap();
        assert_eq!(reopened.stage(&artifact), Err(ServiceFailure::Pending));
        reopened
            .discard_preparing(preparing, snapshot.generation)
            .unwrap();
        reopened.cleanup().unwrap();
        assert!(reopened.snapshot().unwrap().cleanup_pending.is_none());
        assert!(
            !registered
                .data_directory()
                .join("installations")
                .join(format!("version-{}", preparing.0.get()))
                .exists()
        );
    }

    #[test]
    fn an_alternate_update_key_and_other_build_identity_cannot_publish_code() {
        use spaceterm_host::account_service::ServiceFailure;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let (_, other) = fixture("1.2.3", 8, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        assert_eq!(store.stage(&other), Err(ServiceFailure::ArtifactRejected));
        assert_eq!(
            {
                struct Admission;
                impl spaceterm_host::server_installation::InstallationAdmission for Admission {
                    fn verify(
                        &self,
                        _: &spaceterm_host::account_service::RegisteredServer,
                        _: spaceterm_host::server_installation::InstallationOperation,
                    ) -> Result<(), ServiceFailure> {
                        Ok(())
                    }
                }
                let scope = spaceterm_host::server_installation::InstallationScope::new(
                    &registered,
                    &Admission,
                )
                .unwrap();
                spaceterm_host::server_installation::ServerInstallationStore::open(&scope)
                    .unwrap()
                    .stage(&scope, &artifact)
            },
            Err(ServiceFailure::ArtifactRejected)
        );
        for build in [BuildIdentity::Release, BuildIdentity::Preflight] {
            let account = AccountServiceFixture::new(root.path(), build).unwrap();
            let registered = account.initialize(DesiredServerState::Stopped).unwrap();
            let store = test_support::store(&registered, key.clone()).unwrap();
            assert_eq!(
                store.stage(&artifact),
                Err(ServiceFailure::ArtifactRejected)
            );
        }
    }

    #[test]
    fn startup_rejects_tampered_or_linked_installed_code() {
        use std::os::unix::fs::symlink;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let id = store.stage(&artifact).unwrap();
        store
            .activate(id, store.snapshot().unwrap().generation, 1)
            .unwrap();
        let retained = store.retain_approved().unwrap();
        let path = retained.path().to_owned();
        let original = std::fs::read(&path).unwrap();
        std::fs::write(&path, b"tampered").unwrap();
        assert!(store.retain_approved().is_err());
        std::fs::write(&path, original).unwrap();
        let other = path.with_extension("link-target");
        std::fs::rename(&path, &other).unwrap();
        symlink(&other, &path).unwrap();
        assert!(store.retain_approved().is_err());
    }

    #[test]
    fn cleanup_removes_only_the_superseded_version_and_preserves_private_account_data() {
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, first) = fixture("1.2.3", 7, native_machine());
        let (_, second) = fixture("1.2.4", 7, native_machine());
        let (_, third) = fixture("1.2.5", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let mut ids = Vec::new();
        for artifact in [&first, &second, &third] {
            let id = store.stage(artifact).unwrap();
            store
                .activate(id, store.snapshot().unwrap().generation, 1)
                .unwrap();
            ids.push(id);
        }
        let versions = registered.data_directory().join("installations");
        let old = versions.join(format!("version-{}", ids[0].0.get()));
        let unlisted = old.join("unlisted");
        std::fs::write(&unlisted, b"do not delete").unwrap();
        assert!(store.cleanup().is_err());
        assert!(unlisted.exists());
        assert_eq!(store.snapshot().unwrap().cleanup_pending, Some(ids[0]));
        std::fs::remove_file(unlisted).unwrap();
        store.cleanup().unwrap();
        assert!(!old.exists());
        assert!(
            !versions
                .join(format!("metadata-{}", ids[0].0.get()))
                .exists()
        );
        assert_eq!(store.snapshot().unwrap().previous, Some(ids[1]));
        assert_eq!(store.retain_approved().unwrap().installation(), ids[2]);
        assert_eq!(
            account.reopen().discover().unwrap().unwrap().server_pin(),
            registered.server_pin()
        );
    }

    #[test]
    fn activation_sync_failure_closes_the_failed_handle_and_recovers_exact_new_selection() {
        use spaceterm_host::account_service::ServiceFailure;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let id = store.stage(&artifact).unwrap();
        let generation = store.snapshot().unwrap().generation;
        test_support::fail_next_publication_durability();
        assert_eq!(
            store.activate(id, generation, 1),
            Err(ServiceFailure::Indeterminate)
        );
        assert_eq!(store.snapshot(), Err(ServiceFailure::Indeterminate));
        let reopened = test_support::store(&registered, key).unwrap();
        assert_eq!(reopened.snapshot().unwrap().approved, Some(id));
        assert!(
            registered
                .data_directory()
                .join("installations")
                .join(format!("version-{}", id.0.get()))
                .exists()
        );
    }

    #[test]
    fn final_staging_publication_recovery_requires_directory_durability_before_acknowledgment() {
        use spaceterm_host::account_service::ServiceFailure;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        test_support::after_next_metadata_publication(
            test_support::fail_next_publication_durability,
        );
        assert_eq!(store.stage(&artifact), Err(ServiceFailure::Indeterminate));
        assert_eq!(store.stage(&artifact), Err(ServiceFailure::Indeterminate));
        drop(store);
        let publication = registered
            .data_directory()
            .join("installations/selection-publication");
        let original_intent = std::fs::read(&publication).unwrap();

        test_support::fail_next_tree_sync();
        assert!(matches!(
            test_support::store(&registered, key.clone()),
            Err(ServiceFailure::Indeterminate)
        ));
        assert_eq!(std::fs::read(&publication).unwrap(), original_intent);
        let reopened = test_support::store(&registered, key).unwrap();
        assert!(!publication.exists());
        let id = reopened.stage(&artifact).unwrap();
        assert_eq!(id.0.get(), 1);
        let snapshot = reopened.snapshot().unwrap();
        assert_eq!(snapshot.staged, Some(id));
        assert_eq!(snapshot.preparing, None);
        assert_eq!(snapshot.approved, None);
    }

    #[test]
    fn final_staging_publication_recovers_process_exit_before_directory_sync() {
        use spaceterm_host::account_service::ServiceFailure;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        drop(test_support::store(&registered, key.clone()).unwrap());
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "account_installation::preparation_process_fixture",
            ])
            .env("SPACETERM_PREPARATION_CRASH_ROOT", root.path())
            .env("SPACETERM_PREPARATION_CRASH_PHASE", "staged");
        let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(86));
        test_support::fail_next_tree_sync();
        assert!(matches!(
            test_support::store(&registered, key.clone()),
            Err(ServiceFailure::Indeterminate)
        ));
        let reopened = test_support::store(&registered, key).unwrap();
        let id = reopened.stage(&artifact).unwrap();
        assert_eq!(id.0.get(), 1);
        assert_eq!(reopened.snapshot().unwrap().staged, Some(id));
        assert_eq!(reopened.snapshot().unwrap().approved, None);
    }

    #[test]
    fn activation_recovery_sync_failure_never_exposes_approved_code() {
        use spaceterm_host::account_service::ServiceFailure;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let id = store.stage(&artifact).unwrap();
        test_support::fail_next_publication_durability();
        assert_eq!(
            store.activate(id, store.snapshot().unwrap().generation, 1),
            Err(ServiceFailure::Indeterminate)
        );
        assert!(matches!(
            store.retain_approved(),
            Err(ServiceFailure::Indeterminate)
        ));
        drop(store);
        let publication = registered
            .data_directory()
            .join("installations/selection-publication");
        let original_intent = std::fs::read(&publication).unwrap();

        // Startup must establish the original publication's durability before
        // returning a handle that can expose newly approved executable code.
        for _ in 0..2 {
            test_support::fail_next_tree_sync();
            assert!(matches!(
                test_support::existing_store(&registered, key.clone()),
                Err(ServiceFailure::Indeterminate)
            ));
            assert_eq!(std::fs::read(&publication).unwrap(), original_intent);
        }
        let reopened = test_support::existing_store(&registered, key).unwrap();
        assert!(!publication.exists());
        assert_eq!(reopened.retain_approved().unwrap().installation(), id);
        assert_eq!(reopened.snapshot().unwrap().approved, Some(id));
    }

    #[test]
    fn selection_publication_recovery_rejects_a_same_content_replacement_inode() {
        use spaceterm_host::account_service::ServiceFailure;
        use std::os::unix::fs::PermissionsExt;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let id = store.stage(&artifact).unwrap();
        test_support::fail_next_publication_durability();
        assert_eq!(
            store.activate(id, store.snapshot().unwrap().generation, 1),
            Err(ServiceFailure::Indeterminate)
        );
        drop(store);
        let installations = registered.data_directory().join("installations");
        let selection = installations.join("selection");
        let displaced = installations.join("original-selection");
        let bytes = std::fs::read(&selection).unwrap();
        std::fs::rename(&selection, &displaced).unwrap();
        std::fs::write(&selection, &bytes).unwrap();
        std::fs::set_permissions(&selection, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            test_support::existing_store(&registered, key.clone()),
            Err(ServiceFailure::CleanupUnresolved)
        ));
        assert_eq!(std::fs::read(&selection).unwrap(), bytes);
        assert!(displaced.exists());
        assert!(installations.join("selection-publication").exists());
        std::fs::remove_file(&selection).unwrap();
        std::fs::rename(displaced, &selection).unwrap();
        let reopened = test_support::existing_store(&registered, key).unwrap();
        assert_eq!(reopened.retain_approved().unwrap().installation(), id);
    }

    #[test]
    fn activation_process_fixture() {
        let Ok(root) = std::env::var("SPACETERM_INSTALLATION_CRASH_ROOT") else {
            return;
        };
        let account =
            AccountServiceFixture::new(std::path::Path::new(&root), BuildIdentity::Development)
                .unwrap();
        let registered = account.reopen().discover().unwrap().unwrap();
        let (key, _) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let snapshot = store.snapshot().unwrap();
        if std::env::var_os("SPACETERM_SELECTION_PREPUBLICATION").is_some() {
            test_support::at_cleanup_boundary(15, || std::process::exit(86));
        } else {
            test_support::after_next_publication(|| std::process::exit(86));
        }
        store
            .activate(snapshot.staged.unwrap(), snapshot.generation, 1)
            .unwrap();
        panic!("activation interruption did not fire");
    }

    #[test]
    fn process_exit_during_activation_recovers_the_original_selected_version() {
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let id = store.stage(&artifact).unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "account_installation::activation_process_fixture",
                "--nocapture",
            ])
            .env("SPACETERM_INSTALLATION_CRASH_ROOT", root.path());
        let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(86));
        test_support::fail_next_tree_sync();
        assert_eq!(
            store.snapshot(),
            Err(spaceterm_host::account_service::ServiceFailure::Indeterminate)
        );
        assert!(matches!(
            store.retain_approved(),
            Err(spaceterm_host::account_service::ServiceFailure::Indeterminate)
        ));
        let reopened = test_support::store(&registered, key).unwrap();
        assert_eq!(reopened.snapshot().unwrap().approved, Some(id));
        assert!(
            registered
                .data_directory()
                .join("installations")
                .join(format!("version-{}", id.0.get()))
                .exists()
        );
    }
    #[test]
    fn unlisted_files_and_replaced_version_directories_never_become_startup_authority() {
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let id = store.stage(&artifact).unwrap();
        store
            .activate(id, store.snapshot().unwrap().generation, 1)
            .unwrap();
        let path = registered
            .data_directory()
            .join("installations")
            .join(format!("version-{}", id.0.get()));
        let extra = path.join("unlisted");
        std::fs::write(&extra, b"unexpected executable resource").unwrap();
        for _ in 0..2 {
            assert!(store.retain_approved().is_err());
        }
        std::fs::remove_file(extra).unwrap();
        assert_eq!(
            store.retain_approved().unwrap().status().artifact_sha256,
            artifact.artifact_sha256()
        );
        let displaced = path.with_extension("displaced");
        std::fs::rename(&path, &displaced).unwrap();
        std::fs::create_dir(&path).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(store.retain_approved().is_err());
        assert!(displaced.exists());
    }
    #[test]
    fn cleanup_never_deletes_substituted_payload_bytes() {
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, first) = fixture("1.2.3", 7, native_machine());
        let (_, second) = fixture("1.2.4", 7, native_machine());
        let (_, third) = fixture("1.2.5", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let mut first_id = None;
        for artifact in [&first, &second, &third] {
            let id = store.stage(artifact).unwrap();
            first_id.get_or_insert(id);
            store
                .activate(id, store.snapshot().unwrap().generation, 1)
                .unwrap();
        }
        let original = registered
            .data_directory()
            .join("installations")
            .join(format!("version-{}", first_id.unwrap().0.get()))
            .join(&first.manifest().entry_points.server);
        std::fs::write(&original, b"unverified replacement").unwrap();
        assert!(store.cleanup().is_err());
        assert_eq!(std::fs::read(original).unwrap(), b"unverified replacement");
        assert!(store.snapshot().unwrap().cleanup_pending.is_some());
    }
    #[test]
    fn an_unsettled_preparation_blocks_new_selection_until_original_discard() {
        use spaceterm_host::account_service::ServiceFailure;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, first) = fixture("1.2.3", 7, native_machine());
        let (_, second) = fixture("1.2.4", 7, native_machine());
        let (_, bad) = fixture("1.2.5", 7, 0);
        let store = test_support::store(&registered, key).unwrap();
        for artifact in [&first, &second] {
            let id = store.stage(artifact).unwrap();
            store
                .activate(id, store.snapshot().unwrap().generation, 1)
                .unwrap();
        }
        assert_eq!(store.stage(&bad), Err(ServiceFailure::ArtifactRejected));
        let pending = store.snapshot().unwrap();
        assert_eq!(
            store.rollback(pending.generation, 1),
            Err(ServiceFailure::Pending)
        );
        assert_eq!(store.snapshot().unwrap(), pending);
        store
            .discard_preparing(pending.preparing.unwrap(), pending.generation)
            .unwrap();
        store.cleanup().unwrap();
        store
            .rollback(store.snapshot().unwrap().generation, 1)
            .unwrap();
        assert_eq!(store.snapshot().unwrap().approved, pending.previous);
    }
    #[test]
    fn an_unrelated_child_cannot_retain_a_released_installer_lock() {
        use std::{
            io::Write,
            os::{fd::AsRawFd, unix::net::UnixStream},
        };
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, _) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let (mut control, parked) = UnixStream::pair().unwrap();
        let control_fd = control.as_raw_fd();
        let parked_fd = parked.as_raw_fd();
        let child = test_support::with_installation_lock(&store, || {
            // SAFETY: the child uses only async-signal-safe libc calls and never touches Rust runtime state.
            let pid = unsafe { libc::fork() };
            if pid == 0 {
                let mut byte = 0_u8;
                // SAFETY: the child owns these inherited descriptors and exits after one bounded parent-controlled read.
                unsafe {
                    libc::close(control_fd);
                    libc::read(parked_fd, (&mut byte as *mut u8).cast(), 1);
                    libc::_exit(0);
                }
            }
            pid
        })
        .unwrap();
        drop(parked);
        let snapshot = store.snapshot();
        let release = control.write_all(b"x");
        drop(control);
        let mut status = 0;
        // SAFETY: this exact child belongs to the test and releases its inherited descriptors before exiting.
        let reaped = if child > 0 {
            unsafe { libc::waitpid(child, &mut status, 0) }
        } else {
            -1
        };
        assert!(child > 0);
        assert_eq!(reaped, child);
        assert_eq!(status, 0);
        release.unwrap();
        assert!(
            snapshot.is_ok(),
            "released lock was retained by the unrelated child: {snapshot:?}"
        );
    }

    fn partial_extraction_keeps_cleanup_closed(process_exit: bool) {
        use spaceterm_host::account_service::ServiceFailure;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        if process_exit {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "account_installation::preparation_process_fixture",
                ])
                .env("SPACETERM_PREPARATION_CRASH_ROOT", root.path())
                .env("SPACETERM_PREPARATION_CRASH_PHASE", "copy");
            let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
            assert_eq!(child.wait().unwrap().code(), Some(86));
        } else {
            test_support::after_next_partial_copy(1, || {});
            assert_eq!(
                store.stage(&artifact),
                Err(ServiceFailure::StorageUnavailable)
            );
        }
        drop(store);
        let store = test_support::store(&registered, key).unwrap();
        let snapshot = store.snapshot().unwrap();
        let id = snapshot.preparing.unwrap();
        let payload = registered
            .data_directory()
            .join("installations")
            .join(format!("version-{}", id.0.get()))
            .join(&artifact.manifest().entry_points.server);
        assert_eq!(std::fs::metadata(&payload).unwrap().len(), 1);
        store.discard_preparing(id, snapshot.generation).unwrap();
        // A creation record authorizes this original incomplete inode, not a
        // replacement at the same manifest pathname.
        replace_from_other_process(&payload, false);
        assert_eq!(store.cleanup(), Err(ServiceFailure::CleanupUnresolved));
        assert_eq!(
            std::fs::metadata(payload.with_extension("displaced"))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(std::fs::read(&payload).unwrap(), b"unrelated replacement");
        assert_eq!(
            store.stage(&artifact),
            Err(ServiceFailure::CleanupUnresolved)
        );
        assert_eq!(store.snapshot().unwrap().cleanup_pending, Some(id));
        assert!(store.snapshot().unwrap().approved.is_none());
    }

    #[test]
    fn a_failed_partial_copy_never_authorizes_deleting_a_replacement_inode() {
        partial_extraction_keeps_cleanup_closed(false);
    }

    #[test]
    fn a_terminated_partial_copy_never_authorizes_deleting_a_replacement_inode() {
        partial_extraction_keeps_cleanup_closed(true);
    }

    #[test]
    fn preparation_process_fixture() {
        let Ok(root) = std::env::var("SPACETERM_PREPARATION_CRASH_ROOT") else {
            return;
        };
        let account =
            AccountServiceFixture::new(std::path::Path::new(&root), BuildIdentity::Development)
                .unwrap();
        let registered = account.reopen().discover().unwrap().unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        match std::env::var("SPACETERM_PREPARATION_CRASH_PHASE")
            .unwrap()
            .as_str()
        {
            "mkdir" => test_support::after_next_version_creation(|| std::process::exit(86)),
            "metadata" => test_support::after_next_metadata_publication(|| std::process::exit(86)),
            "staged" => test_support::after_next_metadata_publication(|| {
                test_support::after_next_publication(|| std::process::exit(86));
            }),
            "copy" => test_support::after_next_partial_copy(1, || std::process::exit(86)),
            "root-created" => test_support::at_cleanup_boundary(12, || std::process::exit(86)),
            "root-journaled" => test_support::at_cleanup_boundary(9, || std::process::exit(86)),
            "file-created" => test_support::at_cleanup_boundary(13, || std::process::exit(86)),
            "file-journaled" => test_support::at_cleanup_boundary(11, || std::process::exit(86)),
            _ => panic!("unknown preparation interruption"),
        }
        store.stage(&artifact).unwrap();
        panic!("preparation interruption did not fire");
    }

    fn preparation_is_retained_before_external_effect(phase: &str) {
        use spaceterm_host::account_service::ServiceFailure;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "account_installation::preparation_process_fixture",
            ])
            .env("SPACETERM_PREPARATION_CRASH_ROOT", root.path())
            .env("SPACETERM_PREPARATION_CRASH_PHASE", phase);
        let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(86));
        let reopened = test_support::store(&registered, key).unwrap();
        let snapshot = reopened.snapshot().unwrap();
        let id = snapshot
            .preparing
            .expect("native creation lost its durable candidate reservation");
        assert!(snapshot.approved.is_none());
        assert!(snapshot.staged.is_none());
        assert_eq!(reopened.stage(&artifact), Err(ServiceFailure::Pending));
        assert_eq!(store.snapshot().unwrap(), snapshot);
        let version = registered
            .data_directory()
            .join("installations")
            .join(format!("version-{}", id.0.get()));
        assert!(version.is_dir());
        replace_from_other_process(&version, true);
        assert_eq!(
            reopened.discard_preparing(id, snapshot.generation),
            Err(ServiceFailure::CleanupUnresolved)
        );
        assert!(version.is_dir());
        assert!(version.with_extension("displaced").is_dir());
        assert_eq!(reopened.snapshot().unwrap(), snapshot);
    }

    #[test]
    fn preparation_is_durably_reserved_before_version_mkdir() {
        preparation_is_retained_before_external_effect("mkdir");
    }

    #[test]
    fn preparation_is_durably_reserved_before_metadata_publication() {
        preparation_is_retained_before_external_effect("metadata");
    }

    #[test]
    fn cleanup_replacement_process_fixture() {
        let Ok(original) = std::env::var("SPACETERM_CLEANUP_REPLACEMENT_PATH") else {
            return;
        };
        let original = std::path::PathBuf::from(original);
        let replacement = original.with_extension("replacement");
        let displaced = original.with_extension("displaced");
        std::fs::rename(&original, displaced).unwrap();
        if std::env::var_os("SPACETERM_CLEANUP_REPLACE_DIRECTORY").is_some() {
            std::fs::create_dir(&replacement).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o700)).unwrap();
        } else {
            std::fs::write(&replacement, b"unrelated replacement").unwrap();
            use std::os::unix::fs::PermissionsExt;
            let mode = if std::env::var_os("SPACETERM_CLEANUP_REPLACE_METADATA").is_some() {
                0o600
            } else {
                0o700
            };
            std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        std::fs::rename(replacement, &original).unwrap();
    }

    fn replace_from_other_process(path: &std::path::Path, directory: bool) {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "account_installation::cleanup_replacement_process_fixture",
            ])
            .env("SPACETERM_CLEANUP_REPLACEMENT_PATH", path);
        if directory {
            command.env("SPACETERM_CLEANUP_REPLACE_DIRECTORY", "1");
        }
        let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
        assert!(child.wait().unwrap().success());
    }

    fn cleanup_replacement_survives(directory: bool) {
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, first) = fixture("1.2.3", 7, native_machine());
        let (_, second) = fixture("1.2.4", 7, native_machine());
        let (_, third) = fixture("1.2.5", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let mut first_id = None;
        for artifact in [&first, &second, &third] {
            let id = store.stage(artifact).unwrap();
            first_id.get_or_insert(id);
            store
                .activate(id, store.snapshot().unwrap().generation, 1)
                .unwrap();
        }
        let version = registered
            .data_directory()
            .join("installations")
            .join(format!("version-{}", first_id.unwrap().0.get()));
        let original = if directory {
            version
        } else {
            version.join(&first.manifest().entry_points.server)
        };
        let hook_path = original.clone();
        if directory {
            test_support::before_next_version_removal(move || {
                replace_from_other_process(&hook_path, true)
            });
        } else {
            test_support::before_next_file_removal(move || {
                replace_from_other_process(&hook_path, false)
            });
        }
        let result = store.cleanup();
        assert!(
            original.exists(),
            "cleanup deleted a replacement after its final identity check"
        );
        if !directory {
            assert_eq!(std::fs::read(original).unwrap(), b"unrelated replacement");
        }
        assert!(result.is_err());
        assert_eq!(store.snapshot().unwrap().cleanup_pending, first_id);
    }

    fn replace_metadata_from_other_process(path: &std::path::Path) {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "account_installation::cleanup_replacement_process_fixture",
            ])
            .env("SPACETERM_CLEANUP_REPLACEMENT_PATH", path)
            .env("SPACETERM_CLEANUP_REPLACE_METADATA", "1");
        let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn metadata_cleanup_process_fixture() {
        let Ok(root) = std::env::var("SPACETERM_METADATA_CLEANUP_CRASH_ROOT") else {
            return;
        };
        let account =
            AccountServiceFixture::new(std::path::Path::new(&root), BuildIdentity::Development)
                .unwrap();
        let registered = account.reopen().discover().unwrap().unwrap();
        let (key, _) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        test_support::after_next_version_removal(|| std::process::exit(86));
        store.cleanup().unwrap();
        panic!("metadata cleanup interruption did not fire");
    }

    fn cleanup_preserves_replaced_metadata(version_already_removed: bool) {
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let id = store.stage(&artifact).unwrap();
        store
            .discard_staged(id, store.snapshot().unwrap().generation)
            .unwrap();
        let directory = registered.data_directory().join("installations");
        let metadata = directory.join(format!("metadata-{}", id.0.get()));
        let version = directory.join(format!("version-{}", id.0.get()));
        if version_already_removed {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "account_installation::metadata_cleanup_process_fixture",
                ])
                .env("SPACETERM_METADATA_CLEANUP_CRASH_ROOT", root.path());
            let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
            assert_eq!(child.wait().unwrap().code(), Some(86));
            assert!(!version.exists());
            replace_metadata_from_other_process(&metadata);
        } else {
            let path = metadata.clone();
            test_support::before_next_version_removal(move || {
                replace_metadata_from_other_process(&path)
            });
        }
        let result = store.cleanup();
        assert!(
            metadata.exists(),
            "cleanup deleted unauthenticated metadata"
        );
        assert_eq!(std::fs::read(&metadata).unwrap(), b"unrelated replacement");
        assert!(result.is_err());
        assert_eq!(store.snapshot().unwrap().cleanup_pending, Some(id));
        assert!(!version.exists());
        drop(store);
        let reopened = test_support::store(&registered, key).unwrap();
        assert!(reopened.cleanup().is_err());
        assert_eq!(std::fs::read(metadata).unwrap(), b"unrelated replacement");
        assert_eq!(reopened.snapshot().unwrap().cleanup_pending, Some(id));
    }

    #[test]
    fn cleanup_preserves_metadata_replaced_after_manifest_authentication() {
        cleanup_preserves_replaced_metadata(false);
    }

    #[test]
    fn cleanup_authenticates_metadata_after_interrupted_version_removal() {
        cleanup_preserves_replaced_metadata(true);
    }

    #[test]
    fn cleanup_preserves_a_leaf_replaced_after_the_final_identity_check() {
        cleanup_replacement_survives(false);
    }

    #[test]
    fn cleanup_preserves_a_version_replaced_after_the_final_identity_check() {
        cleanup_replacement_survives(true);
    }

    #[test]
    fn review_probe_cleanup_process_fixture() {
        let Ok(root) = std::env::var("SPACETERM_REVIEW_CLEANUP_ROOT") else {
            return;
        };
        let account =
            AccountServiceFixture::new(std::path::Path::new(&root), BuildIdentity::Development)
                .unwrap();
        let registered = account.reopen().discover().unwrap().unwrap();
        let (key, _) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let boundary = std::env::var("SPACETERM_REVIEW_CLEANUP_BOUNDARY")
            .unwrap()
            .parse()
            .unwrap();
        test_support::at_cleanup_boundary(boundary, || std::process::exit(86));
        store.cleanup().unwrap();
        panic!("cleanup interruption did not fire");
    }

    fn cleanup_recovery_probe(boundary: u8, sync_error: bool) {
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let id = store.stage(&artifact).unwrap();
        store
            .discard_staged(id, store.snapshot().unwrap().generation)
            .unwrap();
        if sync_error {
            test_support::at_cleanup_boundary(2, test_support::fail_next_tree_sync);
            assert_eq!(
                store.cleanup(),
                Err(spaceterm_host::account_service::ServiceFailure::Indeterminate)
            );
        } else {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "account_installation::review_probe_cleanup_process_fixture",
                ])
                .env("SPACETERM_REVIEW_CLEANUP_ROOT", root.path())
                .env("SPACETERM_REVIEW_CLEANUP_BOUNDARY", boundary.to_string());
            let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
            assert_eq!(child.wait().unwrap().code(), Some(86));
        }
        drop(store);
        let reopened = test_support::store(&registered, key).unwrap();
        assert_eq!(reopened.snapshot().unwrap().cleanup_pending, Some(id));
        reopened
            .cleanup()
            .expect("original quarantine must be recoverable");
        assert!(reopened.stage(&artifact).unwrap().0.get() > id.0.get());
    }

    fn recovered_removal_publication_requires_durability(
        process_exit: bool,
        sync_fault: impl FnOnce(&std::path::Path),
    ) {
        use spaceterm_host::account_service::ServiceFailure;
        use std::os::unix::fs::MetadataExt;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let id = store.stage(&artifact).unwrap();
        store
            .discard_staged(id, store.snapshot().unwrap().generation)
            .unwrap();
        let installations = registered.data_directory().join("installations");
        let version = installations.join(format!("version-{}", id.0.get()));
        let payload = version.join(&artifact.manifest().files[0].name);
        let original_bytes = std::fs::read(&payload).unwrap();
        let original = std::fs::metadata(&payload).unwrap();
        let objects = installations.join(format!("objects-{}", id.0.get()));
        let original_inventory = std::fs::read(&objects).unwrap();

        // This is a nonlegacy installation. Boundary 16 interrupts publication
        // of its first removal intent, not acquisition of a legacy inventory.
        if process_exit {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "account_installation::review_probe_cleanup_process_fixture",
                ])
                .env("SPACETERM_REVIEW_CLEANUP_ROOT", root.path())
                .env("SPACETERM_REVIEW_CLEANUP_BOUNDARY", "16");
            let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
            assert_eq!(child.wait().unwrap().code(), Some(86));
        } else {
            test_support::at_cleanup_boundary(16, test_support::fail_next_tree_sync);
            assert_eq!(store.cleanup(), Err(ServiceFailure::Indeterminate));
        }
        drop(store);
        let removal = installations.join(format!("removal-{}", id.0.get()));
        let original_intent = std::fs::read(&removal).unwrap();
        assert_eq!(std::fs::read(&payload).unwrap(), original_bytes);
        assert_eq!(std::fs::read(&objects).unwrap(), original_inventory);
        let reopened = test_support::store(&registered, key.clone()).unwrap();
        let before = reopened.snapshot().unwrap();
        sync_fault(&removal);
        assert_eq!(reopened.cleanup(), Err(ServiceFailure::Indeterminate));
        assert_eq!(
            std::fs::read(&payload).expect("recovery must persist its intent before capture"),
            original_bytes
        );
        let unchanged = std::fs::metadata(&payload).unwrap();
        assert_eq!(
            (unchanged.dev(), unchanged.ino()),
            (original.dev(), original.ino())
        );
        assert_eq!(std::fs::read(&objects).unwrap(), original_inventory);
        assert_eq!(std::fs::read(&removal).unwrap(), original_intent);
        assert_eq!(
            reopened.stage(&artifact),
            Err(ServiceFailure::Indeterminate)
        );
        drop(reopened);
        let recovered = test_support::store(&registered, key).unwrap();
        assert_eq!(recovered.snapshot().unwrap(), before);
        recovered.cleanup().unwrap();
        assert!(!version.exists());
        assert_eq!(recovered.snapshot().unwrap().cleanup_pending, None);
        assert!(recovered.stage(&artifact).unwrap().0.get() > id.0.get());
    }

    #[test]
    fn recovered_removal_intent_is_durable_before_capture_after_process_exit() {
        recovered_removal_publication_requires_durability(true, |_| {
            test_support::fail_next_tree_sync();
        });
    }

    #[test]
    fn recovered_removal_intent_is_durable_before_capture_after_sync_failure() {
        recovered_removal_publication_requires_durability(false, |_| {
            test_support::fail_next_tree_sync();
        });
    }

    #[test]
    fn recovered_removal_record_bytes_are_durable_before_capture() {
        use std::os::unix::fs::MetadataExt;
        recovered_removal_publication_requires_durability(true, |removal| {
            let record = std::fs::metadata(removal).unwrap();
            test_support::fail_next_file_sync_for(record.dev(), record.ino());
        });
    }

    #[test]
    fn recovered_creation_log_is_durable_before_removal_authority_is_used() {
        use spaceterm_host::account_service::ServiceFailure;
        use std::os::unix::fs::MetadataExt;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let id = store.stage(&artifact).unwrap();
        store
            .discard_staged(id, store.snapshot().unwrap().generation)
            .unwrap();
        drop(store);
        let installations = registered.data_directory().join("installations");
        let objects = installations.join(format!("objects-{}", id.0.get()));
        let inventory = std::fs::metadata(&objects).unwrap();
        let original_inventory = std::fs::read(&objects).unwrap();
        let version = installations.join(format!("version-{}", id.0.get()));
        let payload = version.join(&artifact.manifest().files[0].name);
        let original_bytes = std::fs::read(&payload).unwrap();
        let reopened = test_support::store(&registered, key.clone()).unwrap();
        test_support::fail_next_file_sync_for(inventory.dev(), inventory.ino());
        assert_eq!(reopened.cleanup(), Err(ServiceFailure::Indeterminate));
        assert_eq!(std::fs::read(&payload).unwrap(), original_bytes);
        assert_eq!(std::fs::read(&objects).unwrap(), original_inventory);
        assert!(
            !installations
                .join(format!("removal-{}", id.0.get()))
                .exists()
        );
        drop(reopened);
        let recovered = test_support::store(&registered, key).unwrap();
        recovered.cleanup().unwrap();
        assert!(!version.exists());
        assert_eq!(recovered.snapshot().unwrap().cleanup_pending, None);
    }

    #[test]
    fn review_probe_cleanup_recovers_after_capture() {
        cleanup_recovery_probe(1, false);
    }
    #[test]
    fn review_probe_cleanup_recovers_after_capture_sync() {
        cleanup_recovery_probe(3, false);
    }
    #[test]
    fn review_probe_cleanup_recovers_after_unlink() {
        cleanup_recovery_probe(4, false);
    }
    #[test]
    fn review_probe_cleanup_recovers_after_capture_sync_error() {
        cleanup_recovery_probe(2, true);
    }

    fn preparation_recovery_probe(phase: &str) {
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        drop(test_support::store(&registered, key.clone()).unwrap());
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "account_installation::preparation_process_fixture",
            ])
            .env("SPACETERM_PREPARATION_CRASH_ROOT", root.path())
            .env("SPACETERM_PREPARATION_CRASH_PHASE", phase);
        let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(86));
        let reopened = test_support::store(&registered, key).unwrap();
        let snapshot = reopened.snapshot().unwrap();
        let id = snapshot.preparing.unwrap();
        reopened
            .discard_preparing(id, snapshot.generation)
            .expect("original preparation must be recoverable");
        reopened
            .cleanup()
            .expect("original incomplete extraction must be cleanable");
        assert!(reopened.stage(&artifact).unwrap().0.get() > id.0.get());
    }
    #[test]
    fn review_probe_preparation_recovers_mkdir() {
        preparation_recovery_probe("mkdir");
    }
    #[test]
    fn review_probe_preparation_recovers_metadata() {
        preparation_recovery_probe("metadata");
    }
    #[test]
    fn review_probe_partial_extraction_recovers_process_exit() {
        preparation_recovery_probe("copy");
    }
    #[test]
    fn review_probe_partial_extraction_recovers_write_failure() {
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        test_support::after_next_partial_copy(1, || {});
        assert_eq!(
            store.stage(&artifact),
            Err(spaceterm_host::account_service::ServiceFailure::StorageUnavailable)
        );
        drop(store);
        let reopened = test_support::store(&registered, key).unwrap();
        let snapshot = reopened.snapshot().unwrap();
        let id = snapshot.preparing.unwrap();
        reopened.discard_preparing(id, snapshot.generation).unwrap();
        reopened
            .cleanup()
            .expect("original incomplete extraction must be cleanable");
        assert!(reopened.stage(&artifact).unwrap().0.get() > id.0.get());
    }

    fn activation_recovery_probe(process_exit: bool) {
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let id = store.stage(&artifact).unwrap();
        if process_exit {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "account_installation::activation_process_fixture",
                ])
                .env("SPACETERM_INSTALLATION_CRASH_ROOT", root.path());
            let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
            assert_eq!(child.wait().unwrap().code(), Some(86));
        } else {
            test_support::fail_next_publication_durability();
            assert_eq!(
                store.activate(id, store.snapshot().unwrap().generation, 1),
                Err(spaceterm_host::account_service::ServiceFailure::Indeterminate)
            );
        }
        drop(store);
        let reopened =
            test_support::store(&registered, key).expect("original activation must admit recovery");
        assert_eq!(reopened.snapshot().unwrap().approved, Some(id));
    }
    #[test]
    fn review_probe_activation_recovers_sync_failure() {
        activation_recovery_probe(false);
    }
    #[test]
    fn review_probe_activation_recovers_process_exit() {
        activation_recovery_probe(true);
    }

    fn capture_replacement_probe(directory: bool) {
        use std::sync::{Arc, Mutex};
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        let id = store.stage(&artifact).unwrap();
        store
            .discard_staged(id, store.snapshot().unwrap().generation)
            .unwrap();
        let installations = registered.data_directory().join("installations");
        let parent = if directory {
            installations
        } else {
            installations
                .join(format!("version-{}", id.0.get()))
                .join(&artifact.manifest().files[0].name)
                .parent()
                .unwrap()
                .to_owned()
        };
        let captured = Arc::new(Mutex::new(None));
        let captured_hook = captured.clone();
        test_support::at_cleanup_boundary(if directory { 5 } else { 2 }, move || {
            let mut paths = std::fs::read_dir(&parent)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .starts_with(".cleanup-")
                });
            let path = paths.next().expect("original entry was not captured");
            assert!(paths.next().is_none());
            replace_from_other_process(&path, directory);
            *captured_hook.lock().unwrap() = Some(path);
        });
        let result = store.cleanup();
        let captured = captured
            .lock()
            .unwrap()
            .take()
            .expect("final authentication hook did not run");
        assert!(captured.with_extension("displaced").exists());
        assert!(
            captured.exists(),
            "cleanup deleted the replacement at its authenticated capture name"
        );
        if !directory {
            assert_eq!(std::fs::read(captured).unwrap(), b"unrelated replacement");
        }
        assert!(result.is_err());
        assert_eq!(store.snapshot().unwrap().cleanup_pending, Some(id));
    }
    #[test]
    fn review_probe_cleanup_preserves_replaced_captured_file() {
        capture_replacement_probe(false);
    }
    #[test]
    fn review_probe_cleanup_preserves_replaced_captured_version() {
        capture_replacement_probe(true);
    }
    #[test]
    fn revoked_lifecycle_admission_cannot_stage_or_activate_code() {
        use spaceterm_host::{
            account_service::{RegisteredServer, ServiceFailure},
            server_installation::{
                InstallationAdmission, InstallationOperation, InstallationScope,
            },
        };
        use std::cell::Cell;
        struct Gate(Cell<bool>);
        impl InstallationAdmission for Gate {
            fn verify(
                &self,
                _: &RegisteredServer,
                operation: InstallationOperation,
            ) -> Result<(), ServiceFailure> {
                if self.0.get() && operation != InstallationOperation::Open {
                    Err(ServiceFailure::StaleAssessment)
                } else {
                    Ok(())
                }
            }
        }
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let gate = Gate(Cell::new(false));
        let scope = InstallationScope::new(&registered, &gate).unwrap();
        let store = test_support::store_with_scope(&scope, key).unwrap();
        let before = store.snapshot().unwrap();
        gate.0.set(true);
        assert_eq!(
            store.stage(&scope, &artifact),
            Err(ServiceFailure::StaleAssessment)
        );
        assert_eq!(store.snapshot().unwrap(), before);
        gate.0.set(false);
        let id = store.stage(&scope, &artifact).unwrap();
        let staged = store.snapshot().unwrap();
        gate.0.set(true);
        assert_eq!(
            store.activate(&scope, id, staged.generation, 1),
            Err(ServiceFailure::StaleAssessment)
        );
        assert_eq!(store.snapshot().unwrap(), staged);
        assert_eq!(registered.desired_state(), DesiredServerState::Stopped);
    }
    #[test]
    fn cleanup_recovers_after_metadata_removal() {
        cleanup_recovery_probe(7, false);
    }
    #[test]
    fn cleanup_recovers_after_creation_evidence_removal() {
        cleanup_recovery_probe(8, false);
    }
    #[test]
    fn preparation_recovers_a_journaled_root_before_namespace_publication() {
        preparation_recovery_probe("root-journaled");
    }
    #[test]
    fn preparation_recovers_a_journaled_file_before_namespace_publication() {
        preparation_recovery_probe("file-journaled");
    }

    fn unexposed_creation_recovers_prior_selection(phase: &str) {
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        drop(test_support::store(&registered, key.clone()).unwrap());
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "account_installation::preparation_process_fixture",
            ])
            .env("SPACETERM_PREPARATION_CRASH_ROOT", root.path())
            .env("SPACETERM_PREPARATION_CRASH_PHASE", phase);
        let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(86));
        let installations = registered.data_directory().join("installations");
        let unrecorded: Vec<_> = std::fs::read_dir(&installations)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with(".cleanup-")
            })
            .collect();
        assert_eq!(unrecorded.len(), 1);
        let store = test_support::store(&registered, key).unwrap();
        let snapshot = store.snapshot().unwrap();
        let id = snapshot.preparing.unwrap();
        store.discard_preparing(id, snapshot.generation).unwrap();
        store.cleanup().unwrap();
        assert!(
            unrecorded[0].exists(),
            "no durable creation identity authorizes removing this private temporary"
        );
        assert!(
            !installations
                .join(format!("version-{}", id.0.get()))
                .exists()
        );
        assert!(store.snapshot().unwrap().approved.is_none());
        assert!(store.stage(&artifact).unwrap().0.get() > id.0.get());
    }
    #[test]
    fn interrupted_unexposed_root_does_not_grant_removal_authority() {
        unexposed_creation_recovers_prior_selection("root-created");
    }
    #[test]
    fn interrupted_unexposed_file_does_not_grant_removal_authority() {
        unexposed_creation_recovers_prior_selection("file-created");
    }
    #[test]
    fn activation_before_publication_recovers_the_exact_prior_selection() {
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, first) = fixture("1.2.3", 7, native_machine());
        let (_, second) = fixture("1.2.4", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let id = store.stage(&first).unwrap();
        store
            .activate(id, store.snapshot().unwrap().generation, 1)
            .unwrap();
        store.stage(&second).unwrap();
        let before = store.snapshot().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "account_installation::activation_process_fixture",
            ])
            .env("SPACETERM_INSTALLATION_CRASH_ROOT", root.path())
            .env("SPACETERM_SELECTION_PREPUBLICATION", "1");
        let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(86));
        drop(store);
        test_support::fail_next_tree_sync();
        assert!(matches!(
            test_support::existing_store(&registered, key.clone()),
            Err(spaceterm_host::account_service::ServiceFailure::Indeterminate)
        ));
        let reopened = test_support::store(&registered, key).unwrap();
        assert_eq!(reopened.snapshot().unwrap(), before);
        assert_eq!(reopened.retain_approved().unwrap().installation(), id);
    }

    #[test]
    fn retained_complete_installations_without_creation_records_use_the_same_cleanup_journal() {
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let id = store.stage(&artifact).unwrap();
        store
            .discard_staged(id, store.snapshot().unwrap().generation)
            .unwrap();
        drop(store);
        let installations = registered.data_directory().join("installations");
        std::fs::remove_file(installations.join(format!("objects-{}", id.0.get()))).unwrap();
        let selection = installations.join("selection");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&selection).unwrap()).unwrap();
        value["cleanup"]
            .as_object_mut()
            .unwrap()
            .remove("incomplete");
        std::fs::write(selection, serde_json::to_vec(&value).unwrap()).unwrap();
        let store = test_support::store(&registered, key).unwrap();
        store.cleanup().unwrap();
        assert!(store.snapshot().unwrap().cleanup_pending.is_none());
        assert!(
            !installations
                .join(format!("version-{}", id.0.get()))
                .exists()
        );
        assert!(
            !installations
                .join(format!("metadata-{}", id.0.get()))
                .exists()
        );
    }
    fn legacy_inventory_recovery_probe(process_exit: bool, replace_file: bool) {
        use spaceterm_host::account_service::ServiceFailure;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let id = store.stage(&artifact).unwrap();
        store
            .discard_staged(id, store.snapshot().unwrap().generation)
            .unwrap();
        drop(store);
        let installations = registered.data_directory().join("installations");
        std::fs::remove_file(installations.join(format!("objects-{}", id.0.get()))).unwrap();
        let selection = installations.join("selection");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&selection).unwrap()).unwrap();
        value["cleanup"]
            .as_object_mut()
            .unwrap()
            .remove("incomplete");
        std::fs::write(selection, serde_json::to_vec(&value).unwrap()).unwrap();
        let store = test_support::store(&registered, key.clone()).unwrap();

        // Interrupt the first atomic inventory publication, after its rename
        // but before directory synchronization or any payload removal.
        if process_exit {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "account_installation::review_probe_cleanup_process_fixture",
                ])
                .env("SPACETERM_REVIEW_CLEANUP_ROOT", root.path())
                .env("SPACETERM_REVIEW_CLEANUP_BOUNDARY", "16");
            let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
            assert_eq!(child.wait().unwrap().code(), Some(86));
        } else {
            test_support::at_cleanup_boundary(16, test_support::fail_next_tree_sync);
            assert_eq!(store.cleanup(), Err(ServiceFailure::Indeterminate));
        }
        drop(store);
        let version = installations.join(format!("version-{}", id.0.get()));
        let executable = version.join(&artifact.manifest().entry_points.server);
        let original_bytes = std::fs::read(&executable).unwrap();
        let reopened = test_support::store(&registered, key.clone()).unwrap();
        assert_eq!(reopened.snapshot().unwrap().cleanup_pending, Some(id));
        if replace_file {
            // The complete inventory predates this same-content replacement.
            // Content authentication alone must not grant removal authority.
            let displaced = installations.join("retained-original-executable");
            std::fs::rename(&executable, &displaced).unwrap();
            std::fs::write(&executable, &original_bytes).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(reopened.cleanup(), Err(ServiceFailure::CleanupUnresolved));
            assert_eq!(std::fs::read(&executable).unwrap(), original_bytes);
            assert!(displaced.exists());
            assert_eq!(reopened.snapshot().unwrap().cleanup_pending, Some(id));
            std::fs::remove_file(&executable).unwrap();
            std::fs::rename(displaced, &executable).unwrap();
        }
        reopened
            .cleanup()
            .expect("the original complete inventory must permit recovery");
        assert_eq!(reopened.snapshot().unwrap().cleanup_pending, None);
        assert!(!version.exists());
        assert!(
            !installations
                .join(format!("metadata-{}", id.0.get()))
                .exists()
        );
        assert!(reopened.stage(&artifact).unwrap().0.get() > id.0.get());
    }

    #[test]
    fn legacy_inventory_publication_recovers_after_process_exit() {
        legacy_inventory_recovery_probe(true, false);
    }

    #[test]
    fn legacy_inventory_publication_recovers_after_directory_sync_failure() {
        legacy_inventory_recovery_probe(false, false);
    }

    #[test]
    fn legacy_inventory_publication_recovery_preserves_replacement_entries() {
        legacy_inventory_recovery_probe(true, true);
    }

    #[test]
    fn missing_creation_evidence_never_reports_incomplete_cleanup_complete() {
        use spaceterm_host::account_service::ServiceFailure;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let store = test_support::store(&registered, key).unwrap();
        test_support::after_next_partial_copy(1, || {});
        assert_eq!(
            store.stage(&artifact),
            Err(ServiceFailure::StorageUnavailable)
        );
        let snapshot = store.snapshot().unwrap();
        let id = snapshot.preparing.unwrap();
        store.discard_preparing(id, snapshot.generation).unwrap();
        let installations = registered.data_directory().join("installations");
        std::fs::remove_file(installations.join(format!("objects-{}", id.0.get()))).unwrap();
        std::fs::remove_file(installations.join(format!("metadata-{}", id.0.get()))).unwrap();
        assert_eq!(store.cleanup(), Err(ServiceFailure::CleanupUnresolved));
        assert_eq!(store.snapshot().unwrap().cleanup_pending, Some(id));
        assert!(
            installations
                .join(format!("version-{}", id.0.get()))
                .exists()
        );
    }
    #[test]
    fn existing_code_storage_never_initializes_missing_installation_state() {
        use spaceterm_host::account_service::ServiceFailure;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registered = account.initialize(DesiredServerState::Stopped).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let installations = registered.data_directory().join("installations");
        assert_eq!(
            test_support::existing_store(&registered, key.clone()).unwrap_err(),
            ServiceFailure::Missing
        );
        assert!(!installations.exists());
        let store = test_support::store(&registered, key.clone()).unwrap();
        let id = store.stage(&artifact).unwrap();
        store
            .activate(id, store.snapshot().unwrap().generation, 1)
            .unwrap();
        let before = store.snapshot().unwrap();
        drop(store);
        let existing = test_support::existing_store(&registered, key.clone()).unwrap();
        assert_eq!(existing.snapshot().unwrap(), before);
        assert_eq!(existing.retain_approved().unwrap().installation(), id);
        drop(existing);
        std::fs::remove_file(installations.join("selection")).unwrap();
        assert_eq!(
            test_support::existing_store(&registered, key).unwrap_err(),
            ServiceFailure::Missing
        );
        assert!(!installations.join("selection").exists());
        assert_eq!(registered.desired_state(), DesiredServerState::Stopped);
    }
    #[test]
    fn bootstrap_transfer_rejects_replacement_truncation_and_wrong_build() {
        use spaceterm_host::bootstrap::artifact::{receive_artifact_with_key, write_artifact};
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let build = artifact.feed().document().build_identity;
        let platform = artifact.feed().document().platform;
        let mut transfer = Vec::new();
        write_artifact(&artifact, &mut transfer).unwrap();
        let received = receive_artifact_with_key(
            &mut transfer.as_slice(),
            &key,
            build,
            platform,
            artifact.artifact_sha256(),
            1,
        )
        .unwrap();
        assert_eq!(received.artifact_sha256(), artifact.artifact_sha256());
        drop(received);
        for cut in [0, 7, 24, transfer.len() - 1] {
            assert!(
                receive_artifact_with_key(
                    &mut &transfer[..cut],
                    &key,
                    build,
                    platform,
                    artifact.artifact_sha256(),
                    1
                )
                .is_err()
            );
        }
        let mut replaced = transfer.clone();
        *replaced.last_mut().unwrap() ^= 1;
        assert!(
            receive_artifact_with_key(
                &mut replaced.as_slice(),
                &key,
                build,
                platform,
                artifact.artifact_sha256(),
                1
            )
            .is_err()
        );
        assert!(
            receive_artifact_with_key(
                &mut transfer.as_slice(),
                &key,
                BuildChannel::Release,
                platform,
                artifact.artifact_sha256(),
                1
            )
            .is_err()
        );
        let (_, replacement) = fixture("1.2.4", 7, native_machine());
        let mut replacement_transfer = Vec::new();
        write_artifact(&replacement, &mut replacement_transfer).unwrap();
        assert!(
            receive_artifact_with_key(
                &mut replacement_transfer.as_slice(),
                &key,
                build,
                platform,
                artifact.artifact_sha256(),
                1
            )
            .is_err()
        );
        let mut appended = transfer;
        appended.push(0);
        assert!(
            receive_artifact_with_key(
                &mut appended.as_slice(),
                &key,
                build,
                platform,
                artifact.artifact_sha256(),
                1
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn bootstrap_remote_account_composition() {
        use spaceterm_host::bootstrap::setup::{
            AccountBootstrapSetup, BootstrapAccountRegistration,
        };
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let digest = artifact.artifact_sha256();
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let setup = AccountBootstrapSetup::new(installer(&account, key, manager.clone()));
        assert!(matches!(
            setup.inspect().await.unwrap().registration,
            BootstrapAccountRegistration::Missing
        ));
        assert!(!account.anchor().exists());
        let request = setup.reserve_request().unwrap();
        setup
            .install(request, account.confirm_install(artifact))
            .await
            .unwrap();
        let facts = setup.inspect().await.unwrap();
        assert!(
            matches!(facts.registration, BootstrapAccountRegistration::Running { artifact_identity, protocol_majors, .. } if artifact_identity == digest && protocol_majors == [1, 2])
        );
        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            ["configure", "start"]
        );
    }

    #[tokio::test]
    async fn bootstrap_lost_remote_reply_resolves_original_request_after_restart() {
        use spaceterm_host::bootstrap::setup::AccountBootstrapSetup;
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let digest = artifact.artifact_sha256();
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let setup = AccountBootstrapSetup::new(installer(&account, key.clone(), manager.clone()));
        let request = setup.reserve_request().unwrap();
        setup
            .install(request, account.confirm_install(artifact))
            .await
            .unwrap();
        drop(setup);
        let reopened = AccountBootstrapSetup::new(installer(&account, key, manager.clone()));
        let first = reopened.resolve(request, digest).await.unwrap();
        assert_eq!(reopened.resolve(request, digest).await.unwrap(), first);
        assert!(reopened.resolve(request, [9; 32]).await.is_err());
        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            ["configure", "start"]
        );
        let reserved = reopened.reserve_request().unwrap();
        assert!(reopened.resolve(reserved, digest).await.is_err());
        assert_eq!(manager.effects.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn bootstrap_native_convergence_preserves_stopped() {
        use spaceterm_host::bootstrap::setup::{
            AccountBootstrapSetup, BootstrapAccountRegistration,
        };
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let original = account
            .initialize_with_installation(
                DesiredServerState::Stopped,
                Some(spaceterm_host::account_service::InstallationStatus {
                    installation: spaceterm_host::startup::InstallationId(
                        std::num::NonZeroU64::new(9).unwrap(),
                    ),
                    version: BuildVersion::parse("1.2.2").unwrap(),
                    artifact_sha256: [3; 32],
                }),
            )
            .unwrap();
        let pin = original.server_pin();
        let setup = AccountBootstrapSetup::new(installer(&account, key, manager.clone()));
        assert!(
            matches!(setup.inspect().await.unwrap().registration, BootstrapAccountRegistration::Stopped { pin: observed, .. } if observed == pin)
        );
        let request = setup.reserve_request().unwrap();
        setup
            .install(request, account.confirm_install(artifact))
            .await
            .unwrap();
        assert_eq!(
            account
                .reopen()
                .discover()
                .unwrap()
                .unwrap()
                .desired_state(),
            DesiredServerState::Stopped
        );
        assert!(manager.effects.lock().unwrap().is_empty());
        assert!(matches!(
            setup.inspect().await.unwrap().registration,
            BootstrapAccountRegistration::Stopped { .. }
        ));
    }
    #[tokio::test]
    async fn bootstrap_lookup_of_interrupted_install_never_resumes_native_effects() {
        use spaceterm_host::{
            account_service::{ServiceFailure, ServicePhase},
            bootstrap::setup::AccountBootstrapSetup,
        };
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let digest = artifact.artifact_sha256();
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let setup = AccountBootstrapSetup::new(installer(&account, key.clone(), manager.clone()));
        let request = setup.reserve_request().unwrap();
        spaceterm_host::account_service::test_support::interrupt_after_phase(
            ServicePhase::Accepted,
        );
        assert_eq!(
            setup
                .install(request, account.confirm_install(artifact))
                .await,
            Err(ServiceFailure::SupervisorUnavailable)
        );
        let reopened = AccountBootstrapSetup::new(installer(&account, key, manager.clone()));
        assert_eq!(
            reopened.resolve(request, digest).await,
            Err(ServiceFailure::Pending)
        );
        assert!(account.reopen().discover().unwrap().is_none());
        assert!(manager.effects.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn bootstrap_owner_lookup_retains_original_native_request_and_exact_claim() {
        use spaceterm_core::{
            identity::{DeviceId, ServerId},
            pairing::{EnrollmentProposal, EnrollmentProposalParts, PairingKeys, PairingKeysParts},
        };
        use spaceterm_host::{
            account_service::ServiceFailure, bootstrap::setup::AccountBootstrapSetup,
        };
        let root = installation_root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = fixture("1.2.3", 7, native_machine());
        let digest = artifact.artifact_sha256();
        let manager = std::sync::Arc::new(InstallManager::new(key.clone()));
        let supervisor = installer(&account, key.clone(), manager.clone());
        let setup = AccountBootstrapSetup::new(supervisor.clone());
        let install = setup.reserve_request().unwrap();
        setup
            .install(install, account.confirm_install(artifact))
            .await
            .unwrap();
        let expected = setup.inspect().await.unwrap().registration;
        let pin = account.reopen().discover().unwrap().unwrap().server_pin();
        let proposal = |nonce| {
            EnrollmentProposal::new(EnrollmentProposalParts {
                server: ServerId::new(pin.identity),
                device: DeviceId::new([7; 16]),
                keys: PairingKeys::new(PairingKeysParts {
                    device_identity_public_key: [8; 32],
                    pairing_authentication_public_key: [9; 32],
                })
                .unwrap(),
                claim_nonce: [nonce; 32],
            })
            .unwrap()
        };
        let request = setup.reserve_request().unwrap();
        let original = setup
            .authorize_owner(request, &expected, &proposal(1))
            .await
            .unwrap();
        assert_eq!(
            setup
                .authorize_owner(request, &expected, &proposal(1))
                .await
                .unwrap(),
            original
        );
        assert_eq!(
            setup
                .authorize_owner(request, &expected, &proposal(2))
                .await,
            Err(ServiceFailure::RequestConflict)
        );
        supervisor.acknowledge_request(request).unwrap();
        let reopened = AccountBootstrapSetup::new(installer(&account, key, manager.clone()));
        assert_eq!(
            reopened
                .resolve(request, digest)
                .await
                .unwrap()
                .registration,
            expected
        );
        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            ["configure", "start"]
        );
    }
}
