use super::*;
use crate::platform::local_server_client::tests::Root;
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use spaceterm_client::{
    journal::{JournalLimits, ReceiptVerifier},
    pairing::credentials::PairingFuture,
    pairing::credentials::{
        CredentialCreationReservation, CredentialDeletion, CredentialScope, CredentialUse,
        PreparedCredential,
    },
};
use spaceterm_core::{
    identity::ServerId,
    operation_ledger::canonical::CanonicalCodec,
    pairing::{PairingFailure, PairingOutcome, SignedPairingReceipt},
};
use spaceterm_host::{
    account_service::{InstallationStatus, test_support::AccountServiceFixture},
    organization_store::{
        SqliteOrganizationStore, StoreIdentity, StoreLimits,
        bootstrap::{InitializationDisposition, NewServerOrigin, NewStoreSeed},
        migrations, schema,
    },
    shell_launch::ShellLaunchPlanner,
    startup::InstallationId,
    startup::{BuildIdentity, DesiredServerState},
    storage::Database,
    transport::MessageTransport,
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{ServerPin, SigningKey},
    pairing_adapter::ReceiptCodec,
};
use spaceterm_server::{
    account_startup::{
        AccountStartup, RecoveredAccountServer,
        test_support::{StartupAbsence, supervisor},
    },
    adapters::{store::production_services, terminal::AccountLaunchFacts},
};
use std::{os::unix::fs::DirBuilderExt as _, sync::Mutex};

struct Verifier;
impl ReceiptVerifier for Verifier {
    fn verify(&self, pin: ServerPin, receipt: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(Into::into)
    }
}
#[derive(Clone, Default)]
struct Credentials(
    Arc<Mutex<Vec<(PreparedCredential, u8)>>>,
    Arc<std::sync::atomic::AtomicUsize>,
    Arc<Mutex<Vec<CredentialScope>>>,
    Arc<Mutex<Vec<CredentialScope>>>,
    Arc<std::sync::atomic::AtomicUsize>,
    Arc<tokio::sync::Notify>,
);
struct Key(Ed25519KeyPair);
impl Key {
    fn new(seed: u8) -> Self {
        Self(Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap())
    }
}
impl SigningKey for Key {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}
impl NativeDeviceCredentials for Credentials {
    fn create(
        &self,
        reservation: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        Box::pin(async move {
            let mode = self.4.swap(0, std::sync::atomic::Ordering::SeqCst);
            if mode == 1 {
                return Err(PairingFailure::CredentialUnavailable);
            }
            let prepared = {
                let mut items = self.0.lock().unwrap();
                assert!(
                    !items
                        .iter()
                        .any(|(item, _)| item.scope == *reservation.scope()),
                    "native credential insertion must be exclusive"
                );
                let seed = u8::try_from(items.len() + 30).unwrap();
                let prepared = PreparedCredential {
                    scope: reservation.scope().clone(),
                    public_key: Key::new(seed).public_key(),
                };
                items.push((prepared.clone(), seed));
                prepared
            };
            if mode == 2 {
                self.5.notify_one();
                std::future::pending::<()>().await;
            }
            Ok(prepared)
        })
    }
    fn recover_preparation<'a>(
        &'a self,
        scope: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        Box::pin(async move {
            if self.2.lock().unwrap().contains(scope) {
                return Ok(None);
            }
            let remaining = self.1.load(std::sync::atomic::Ordering::SeqCst);
            if remaining > 0 && self.1.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                return Err(PairingFailure::CredentialUnavailable);
            }
            Ok(self
                .0
                .lock()
                .unwrap()
                .iter()
                .find(|(credential, _)| &credential.scope == scope)
                .map(|(credential, _)| credential.clone()))
        })
    }
    fn signing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
        authorization: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        Box::pin(async move {
            authorization.validate(&credential.scope)?;
            if self.2.lock().unwrap().contains(&credential.scope) {
                return Err(PairingFailure::CredentialUnavailable);
            }
            let items = self.0.lock().unwrap();
            let (_, seed) = items
                .iter()
                .find(|(prepared, _)| prepared == credential)
                .ok_or(PairingFailure::CredentialUnavailable)?;
            Ok(Arc::new(Key::new(*seed)) as Arc<dyn SigningKey>)
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        Box::pin(async move {
            assert!(
                self.3.lock().unwrap().contains(&credential.scope),
                "foreground enrollment must not delete another credential"
            );
            let mut items = self.0.lock().unwrap();
            let before = items.len();
            items.retain(|(item, _)| item != credential);
            Ok(if before == items.len() {
                CredentialDeletion::AlreadyMissing
            } else {
                CredentialDeletion::Deleted
            })
        })
    }
}

async fn journal(root: &Root) -> Arc<JournalController> {
    let directory = root.0.join("client");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let codec = Arc::new(CanonicalCodec);
    let verifier = Arc::new(Verifier);
    let storage = spaceterm_client_native::journal::NativeClientJournal::create(
        directory,
        codec.clone(),
        verifier.clone(),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    Arc::new(
        JournalController::recover(Arc::new(storage), codec, verifier, JournalLimits::default())
            .await
            .unwrap(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_foreground_owner_pairing_publishes_verified_binding_from_running_account_actor() {
    foreground_owner_pairing(false, 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_foreground_owner_pairing_provisions_dedicated_clients_from_running_account_actor() {
    foreground_owner_pairing(true, 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_pairing_repairs_consumers_after_receipt_storage_interruption() {
    foreground_owner_pairing(false, 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_pairing_repairs_partial_consumers_without_rebinding_saved_cli() {
    foreground_owner_pairing(false, 3).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_pairing_retries_original_origin_before_first_consumer_save() {
    foreground_owner_pairing(true, 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_pairing_retries_original_origin_after_cli_activation() {
    foreground_owner_pairing(true, 3).await;
}

async fn enroll_cli_with_credentials(
    native: &NativeClientProvider,
    credentials: Credentials,
    pin: ServerPin,
) -> Result<(), ClientFailure> {
    use spaceterm_client_native::{
        cli::{
            arguments::{CliCommand, parse},
            bindings::PairingCommandBindings,
        },
        native::consumer::{NativeConsumer, NativeConsumerKind},
    };
    let invocation = parse(
        [
            "pair",
            "enroll",
            "--server-fingerprint",
            &spaceterm_client_native::cli::output::hex(&pin.public_key),
        ]
        .into_iter()
        .map(std::ffi::OsString::from),
    )
    .unwrap();
    let CliCommand::PairEnroll { fingerprint, .. } = invocation.command else {
        panic!("pair enroll required");
    };
    NativeConsumer::new(native.management(), NativeConsumerKind::Cli)
        .with_credentials(Arc::new(credentials))
        .enroll(
            fingerprint,
            false,
            spaceterm_host::native_management::SudoMode::NonInteractive,
            &mut PairIo,
        )
        .await
        .map(|_| ())
        .map_err(classify)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_account_recovery_after_last_device_key_loss_in_both_majors() {
    account_recovery(false, false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_account_recovery_after_last_device_revocation_in_both_majors() {
    account_recovery(true, false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_account_recovery_resumes_original_attempt_after_receiving_failure_in_both_majors()
{
    account_recovery(false, true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_account_recovery_rolls_over_after_recovered_device_key_loss_in_both_majors() {
    account_recovery(false, false, true).await;
}

async fn account_recovery(revoke: bool, interrupt: bool, lose_recovered_device: bool) {
    use spaceterm_client::management_cli::ProtocolManagement;
    use spaceterm_client_native::{
        cli::{
            arguments::{CliCommand, parse},
            bindings::PairingCommandBindings,
        },
        native::consumer::{NativeConsumer, NativeConsumerKind},
    };
    for major in [1, 2] {
        let root = Root::new();
        let fixture = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
        let registration = fixture
            .initialize_with_installation(
                DesiredServerState::Running,
                Some(InstallationStatus {
                    installation: InstallationId(std::num::NonZeroU64::new(9).unwrap()),
                    version: spaceterm_distribution::BuildVersion::parse("9.0.0").unwrap(),
                    artifact_sha256: [8; 32],
                }),
            )
            .unwrap();
        let pin = registration.server_pin();
        let identity = StoreIdentity {
            server: ServerId::new(pin.identity),
            account: registration.key(),
            server_public_key: pin.public_key,
        };
        let home = spaceterm_core::organization::ServerDirectory::new(
            identity.server,
            root.0.as_os_str().as_encoded_bytes().to_vec(),
        )
        .unwrap();
        let database = Database::open_for_application(
            registration.organization_directory(),
            migrations(),
            schema::APPLICATION_ID,
        )
        .unwrap();
        assert!(matches!(
            SqliteOrganizationStore::initialize_new(
                database,
                NewStoreSeed {
                    identity,
                    origin: NewServerOrigin::LocalHeadless
                },
                StoreLimits::production(),
                production_services(identity, home).unwrap(),
            ),
            InitializationDisposition::Durable { .. }
        ));
        let supervisor = supervisor(&root.0, Arc::new(StartupAbsence)).unwrap();
        let server = RecoveredAccountServer::recover(
            supervisor.clone(),
            AccountStartup::admit(&supervisor).unwrap(),
            AccountLaunchFacts {
                uid: registration.key().account_uid,
                home: root.0.clone(),
                planner: ShellLaunchPlanner::for_test(root.0.join("shell"), root.0.clone()),
                machine: Default::default(),
            },
        )
        .await
        .unwrap();
        let handle = server.handle();
        let before = handle.snapshot();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let running = tokio::spawn(server.run_until(async {
            let _ = stopped.await;
        }));
        let native = NativeClientProvider::from_supervisor((*supervisor).clone());
        let credentials = Credentials::default();
        let consumer = || {
            NativeConsumer::new(native.management(), NativeConsumerKind::Cli)
                .with_credentials(Arc::new(credentials.clone()))
                .with_owner_handshake(spaceterm_protocol::authentication::HandshakeConfig {
                    majors: vec![major],
                    ..Default::default()
                })
        };
        let fingerprint = || {
            let invocation = parse(
                [
                    "pair",
                    "enroll",
                    "--server-fingerprint",
                    &spaceterm_client_native::cli::output::hex(&pin.public_key),
                ]
                .into_iter()
                .map(std::ffi::OsString::from),
            )
            .unwrap();
            let CliCommand::PairEnroll { fingerprint, .. } = invocation.command else {
                panic!("enrollment command required")
            };
            fingerprint
        };
        let initial = consumer();
        initial
            .enroll(
                fingerprint(),
                false,
                spaceterm_host::native_management::SudoMode::NonInteractive,
                &mut PairIo,
            )
            .await
            .unwrap();
        initial.shutdown().await.unwrap();
        let old = consumer();
        let runtime = old.pairing_runtime().await.unwrap();
        let inventory = runtime.inventory().await.unwrap();
        let [saved] = inventory.saved.as_slice() else {
            panic!("one CLI binding required")
        };
        let old_key = saved.key().clone();
        let old_device = saved.device();
        let old_keys = saved.receipt().parts().claims.clone();
        let old_authentication = saved.credential().clone();
        let shared_device = credentials.0.lock().unwrap()[0].0.clone();
        // The unrelated credential must survive both old cleanup and recovery.
        let sentinel = PreparedCredential {
            scope: DeviceCredentials::new_scope(
                shared_device.scope.parts().namespace.clone(),
                spaceterm_client::pairing::credentials::CredentialPurpose::DeviceIdentity,
            )
            .unwrap(),
            public_key: Key::new(99).public_key(),
        };
        credentials.0.lock().unwrap().push((sentinel.clone(), 99));
        if revoke {
            credentials
                .3
                .lock()
                .unwrap()
                .push(old_authentication.scope.clone());
            let assessment = runtime
                .assess_unpair(&old_key, saved.client().parts().client.id)
                .await
                .unwrap();
            let removal = runtime.self_unpair(assessment).await.unwrap();
            runtime
                .retry_revocation_once(removal.operation)
                .await
                .unwrap();
        } else {
            credentials
                .2
                .lock()
                .unwrap()
                .push(old_authentication.scope.clone());
        }
        drop(runtime);
        old.shutdown().await.unwrap();
        let remaining = credentials.0.lock().unwrap().len();
        let ordinary = consumer();
        assert!(
            ProtocolManagement::status(&ordinary).await.is_err(),
            "ordinary reconnect must not reenroll lost or revoked authority"
        );
        ordinary.shutdown().await.unwrap();
        assert_eq!(credentials.0.lock().unwrap().len(), remaining);
        let denied = consumer();
        let wrong = parse(
            ["pair", "enroll", "--server-fingerprint", &"ff".repeat(32)]
                .into_iter()
                .map(std::ffi::OsString::from),
        )
        .unwrap();
        let CliCommand::PairEnroll {
            fingerprint: wrong_pin,
            ..
        } = wrong.command
        else {
            panic!("enrollment required")
        };
        assert!(matches!(
            denied
                .enroll(
                    wrong_pin,
                    true,
                    spaceterm_host::native_management::SudoMode::NonInteractive,
                    &mut PairIo
                )
                .await,
            Err(spaceterm_client::management_cli::ManagementFailure::AuthorizationDenied)
        ));
        denied.shutdown().await.unwrap();
        assert_eq!(credentials.0.lock().unwrap().len(), remaining);
        let recovery = consumer();
        if interrupt {
            credentials.1.store(1, std::sync::atomic::Ordering::SeqCst);
        }
        let result = recovery
            .enroll(
                fingerprint(),
                true,
                spaceterm_host::native_management::SudoMode::NonInteractive,
                &mut PairIo,
            )
            .await;
        recovery.shutdown().await.unwrap();
        if interrupt {
            assert!(
                result.is_err(),
                "receiving credential boundary must interrupt recovery"
            );
        } else if let Err(failure) = result {
            let mut stages = Vec::new();
            for name in ["enrollment", "cli", "companion", "management"] {
                let codec = Arc::new(CanonicalCodec);
                let verifier = Arc::new(Verifier);
                let storage = spaceterm_client_native::journal::NativeClientJournal::open_existing(
                    native
                        .management()
                        .supervisor()
                        .client_connections_directory()
                        .join(name),
                    codec.clone(),
                    verifier.clone(),
                    JournalLimits::default(),
                )
                .await
                .unwrap();
                let journal = JournalController::recover(
                    Arc::new(storage),
                    codec,
                    verifier,
                    JournalLimits::default(),
                )
                .await
                .unwrap();
                let inventory = journal.snapshot().unwrap().pairing_snapshot().unwrap();
                stages.push((
                    name,
                    inventory.saved.len(),
                    inventory.prepared.len(),
                    inventory.enrollment_acknowledgements.len(),
                    inventory.revocations.len(),
                    inventory
                        .saved
                        .iter()
                        .any(|saved| saved.key().parts().pairing != old_key.parts().pairing),
                ));
            }
            panic!(
                "explicit recovery classification {failure:?}; stages (name,saved,prepared,acks,revocations,fresh) {stages:?}"
            );
        }
        let new_count = credentials.0.lock().unwrap().len();
        assert_eq!(
            new_count,
            remaining + 2,
            "recovery must create exactly two fresh keys"
        );
        if interrupt {
            let blocked = consumer();
            assert!(matches!(
                ProtocolManagement::status(&blocked).await,
                Err(spaceterm_client::management_cli::ManagementFailure::OutcomePending)
            ));
            blocked.shutdown().await.unwrap();
        }
        let retry = consumer();
        retry
            .enroll(
                fingerprint(),
                true,
                spaceterm_host::native_management::SudoMode::NonInteractive,
                &mut PairIo,
            )
            .await
            .unwrap();
        retry.shutdown().await.unwrap();
        assert_eq!(
            credentials.0.lock().unwrap().len(),
            new_count,
            "recovery retry must retain the original new attempt"
        );
        if lose_recovered_device {
            let admission = supervisor.admit_client_provisioning().unwrap();
            let attempt: serde_json::Value =
                serde_json::from_slice(&admission.owner_attempt("enrollment").unwrap().unwrap())
                    .unwrap();
            let recovered_device: PreparedCredential =
                serde_json::from_value(attempt["device"].clone()).unwrap();
            drop(admission);
            credentials.2.lock().unwrap().push(recovered_device.scope);
            let second = consumer();
            second
                .enroll(
                    fingerprint(),
                    true,
                    spaceterm_host::native_management::SudoMode::NonInteractive,
                    &mut PairIo,
                )
                .await
                .unwrap();
            second.shutdown().await.unwrap();
            assert_eq!(
                credentials.0.lock().unwrap().len(),
                new_count + 2,
                "loss of the recovered device key must create a fresh identity and pairing"
            );
            let replay = consumer();
            replay
                .enroll(
                    fingerprint(),
                    true,
                    spaceterm_host::native_management::SudoMode::NonInteractive,
                    &mut PairIo,
                )
                .await
                .unwrap();
            replay.shutdown().await.unwrap();
            assert_eq!(credentials.0.lock().unwrap().len(), new_count + 2);
        }
        for kind in [NativeConsumerKind::Cli, NativeConsumerKind::StatusCompanion] {
            let receiver = NativeConsumer::new(native.management(), kind)
                .with_credentials(Arc::new(credentials.clone()));
            if kind == NativeConsumerKind::Cli {
                let runtime = receiver.pairing_runtime().await.unwrap();
                let inventory = runtime.inventory().await.unwrap();
                let [saved] = inventory.saved.as_slice() else {
                    panic!("one fresh receiving binding required")
                };
                assert_ne!(saved.key().parts().pairing, old_key.parts().pairing);
                assert_ne!(saved.device(), old_device);
                assert!(saved.credential() != &old_authentication);
                assert!(saved.credential().public_key != old_authentication.public_key);
                let spaceterm_core::pairing::PairingReceiptBody::ClientBinding {
                    keys: old_pairing_keys,
                    ..
                } = &old_keys
                else {
                    panic!("old CLI binding receipt required")
                };
                let spaceterm_core::pairing::PairingReceiptBody::ClientBinding {
                    keys: new_pairing_keys,
                    ..
                } = &saved.receipt().parts().claims
                else {
                    panic!("new CLI binding receipt required")
                };
                assert!(
                    new_pairing_keys.parts().device_identity_public_key
                        != old_pairing_keys.parts().device_identity_public_key
                );
                assert!(inventory.is_suppressed(&old_key));
                assert!(
                    runtime
                        .restore(&old_key, saved.client().parts().client.id)
                        .await
                        .is_err()
                );
                drop(runtime);
            }
            assert!(ProtocolManagement::status(&receiver).await.is_ok());
            receiver.shutdown().await.unwrap();
        }
        for name in ["enrollment", "cli", "companion", "management"] {
            let codec = Arc::new(CanonicalCodec);
            let verifier = Arc::new(Verifier);
            let storage = spaceterm_client_native::journal::NativeClientJournal::open_existing(
                native
                    .management()
                    .supervisor()
                    .client_connections_directory()
                    .join(name),
                codec.clone(),
                verifier.clone(),
                JournalLimits::default(),
            )
            .await
            .unwrap();
            let journal = JournalController::recover(
                Arc::new(storage),
                codec,
                verifier,
                JournalLimits::default(),
            )
            .await
            .unwrap();
            let snapshot = journal.snapshot().unwrap();
            let inventory = snapshot.pairing_snapshot().unwrap();
            assert!(inventory.is_suppressed(&old_key));
            let [saved] = inventory.saved.as_slice() else {
                panic!("one recovered binding required")
            };
            assert_ne!(saved.key().parts().pairing, old_key.parts().pairing);
            assert!(
                journal
                    .ordinary_use_allowed(&snapshot.bindings()[0].binding().unwrap())
                    .unwrap()
            );
        }
        assert!(
            credentials
                .0
                .lock()
                .unwrap()
                .iter()
                .any(|(item, _)| item == &sentinel)
        );
        assert!(
            credentials
                .0
                .lock()
                .unwrap()
                .iter()
                .any(|(item, _)| item == &shared_device)
        );
        let after = handle.snapshot();
        assert_eq!(
            after.state(),
            before.state(),
            "account recovery must retain the organization"
        );
        assert_eq!(
            fixture.reopen().discover().unwrap().unwrap().server_pin(),
            pin
        );
        stop.send(()).unwrap();
        assert!(running.await.unwrap().unwrap().cleanup_confirmed);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_account_recovery_refuses_before_suppressing_unfinished_receiver_origin() {
    foreground_owner_pairing_with_preparation(true, 5, 4).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_account_recovery_preserves_unissued_desktop_key_preparation() {
    foreground_owner_pairing_with_preparation(false, 0, 5).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_owner_preparation_retries_original_scope_after_insertion_failure() {
    foreground_owner_pairing_with_preparation(true, 0, 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_owner_preparation_recovers_exact_device_key_after_cancellation() {
    foreground_owner_pairing_with_preparation(true, 0, 2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_enrollment_recovers_original_owner_metadata_publication_before_recovery_guard() {
    for mode in [6, 7] {
        foreground_owner_pairing_with_preparation(true, 0, mode).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_account_recovery_blocks_native_management_and_desktop_provisioning() {
    foreground_owner_pairing_with_preparation(true, 0, 3).await;
}

async fn foreground_owner_pairing(cli: bool, interrupt_recovery: usize) {
    foreground_owner_pairing_with_preparation(cli, interrupt_recovery, 0).await;
}

async fn foreground_owner_pairing_with_preparation(
    cli: bool,
    interrupt_recovery: usize,
    preparation_mode: usize,
) {
    let root = Root::new();
    let fixture = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
    let registration = fixture
        .initialize_with_installation(
            DesiredServerState::Running,
            Some(InstallationStatus {
                installation: InstallationId(std::num::NonZeroU64::new(9).unwrap()),
                version: spaceterm_distribution::BuildVersion::parse("9.0.0").unwrap(),
                artifact_sha256: [8; 32],
            }),
        )
        .unwrap();
    let pin = registration.server_pin();
    let identity = StoreIdentity {
        server: ServerId::new(pin.identity),
        account: registration.key(),
        server_public_key: pin.public_key,
    };
    let home = spaceterm_core::organization::ServerDirectory::new(
        identity.server,
        root.0.as_os_str().as_encoded_bytes().to_vec(),
    )
    .unwrap();
    let database = Database::open_for_application(
        registration.organization_directory(),
        migrations(),
        schema::APPLICATION_ID,
    )
    .unwrap();
    assert!(matches!(
        SqliteOrganizationStore::initialize_new(
            database,
            NewStoreSeed {
                identity,
                origin: NewServerOrigin::LocalHeadless
            },
            StoreLimits::production(),
            production_services(identity, home).unwrap(),
        ),
        InitializationDisposition::Durable { .. }
    ));
    let supervisor = supervisor(&root.0, Arc::new(StartupAbsence)).unwrap();
    let startup = AccountStartup::admit(&supervisor).unwrap();
    let server = RecoveredAccountServer::recover(
        supervisor.clone(),
        startup,
        AccountLaunchFacts {
            uid: registration.key().account_uid,
            home: root.0.clone(),
            planner: ShellLaunchPlanner::for_test(root.0.join("shell"), root.0.clone()),
            machine: Default::default(),
        },
    )
    .await
    .unwrap();
    let handle = server.handle();
    let before = handle.snapshot();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let running = tokio::spawn(server.run_until(async {
        let _ = stopped.await;
    }));
    let journal = journal(&root).await;
    let credentials = Credentials::default();
    let native = NativeClientProvider::from_supervisor((*supervisor).clone());
    credentials
        .1
        .store(interrupt_recovery, std::sync::atomic::Ordering::SeqCst);
    credentials.4.store(
        if preparation_mode == 5 {
            2
        } else if preparation_mode <= 2 {
            preparation_mode
        } else {
            0
        },
        std::sync::atomic::Ordering::SeqCst,
    );
    if preparation_mode == 2 || preparation_mode == 5 {
        let pending_native = NativeClientProvider::from_supervisor((*supervisor).clone());
        let pending_credentials = credentials.clone();
        let pending_journal = journal.clone();
        let preparing = tokio::spawn(async move {
            if cli {
                enroll_cli_with_credentials(&pending_native, pending_credentials, pin).await
            } else {
                enroll_with_credentials(&pending_native, pending_journal, pending_credentials).await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(15), credentials.5.notified())
            .await
            .unwrap();
        preparing.abort();
        assert!(preparing.await.unwrap_err().is_cancelled());
        assert_eq!(credentials.0.lock().unwrap().len(), 1);
        if preparation_mode == 5 {
            use spaceterm_client_native::cli::{
                arguments::{CliCommand, parse},
                bindings::PairingCommandBindings,
            };
            let before = supervisor
                .admit_client_provisioning()
                .unwrap()
                .owner_attempt("desktop-preparation")
                .unwrap()
                .unwrap();
            let invocation = parse(
                [
                    "pair",
                    "enroll",
                    "--server-fingerprint",
                    &spaceterm_client_native::cli::output::hex(&pin.public_key),
                ]
                .into_iter()
                .map(std::ffi::OsString::from),
            )
            .unwrap();
            let CliCommand::PairEnroll { fingerprint, .. } = invocation.command else {
                panic!("enroll required")
            };
            let recovery = spaceterm_client_native::native::consumer::NativeConsumer::new(
                native.management(),
                spaceterm_client_native::native::consumer::NativeConsumerKind::Cli,
            )
            .with_credentials(Arc::new(credentials.clone()));
            assert!(matches!(
                recovery
                    .enroll(
                        fingerprint,
                        true,
                        spaceterm_host::native_management::SudoMode::NonInteractive,
                        &mut PairIo
                    )
                    .await,
                Err(spaceterm_client::management_cli::ManagementFailure::OutcomePending)
            ));
            recovery.shutdown().await.unwrap();
            let admission = supervisor.admit_client_provisioning().unwrap();
            assert!(admission.owner_attempt("recovery").unwrap().is_none());
            assert!(admission.owner_attempt("desktop").unwrap().is_none());
            assert!(
                admission
                    .owner_attempt("desktop-preparation")
                    .unwrap()
                    .as_ref()
                    == Some(&before)
            );
            assert_eq!(credentials.0.lock().unwrap().len(), 1);
            assert!(
                journal
                    .snapshot()
                    .unwrap()
                    .pairing_snapshot()
                    .unwrap()
                    .suppressed
                    .is_empty()
            );
        }
    }
    let mut result = if cli {
        enroll_cli_with_credentials(&native, credentials.clone(), pin).await
    } else {
        enroll_with_credentials(&native, journal.clone(), credentials.clone()).await
    };
    if preparation_mode == 1 {
        assert!(
            result.is_err(),
            "native insertion must fail before receiving enrollment authority"
        );
        assert!(credentials.0.lock().unwrap().is_empty());
        let original = supervisor
            .admit_client_provisioning()
            .unwrap()
            .owner_attempt("enrollment-preparation")
            .unwrap()
            .unwrap();
        for _ in 0..3 {
            assert!(
                enroll_cli_with_credentials(&native, credentials.clone(), pin)
                    .await
                    .is_err(),
                "absence cannot prove the original insertion will not finish"
            );
            let admission = supervisor.admit_client_provisioning().unwrap();
            assert!(
                admission
                    .owner_attempt("enrollment-preparation")
                    .unwrap()
                    .as_ref()
                    == Some(&original)
            );
            assert!(admission.owner_attempt("enrollment").unwrap().is_none());
            assert!(credentials.0.lock().unwrap().is_empty());
        }
        use spaceterm_client_native::cli::{
            arguments::{CliCommand, parse},
            bindings::PairingCommandBindings,
        };
        let invocation = parse(
            [
                "pair",
                "enroll",
                "--server-fingerprint",
                &spaceterm_client_native::cli::output::hex(&pin.public_key),
            ]
            .into_iter()
            .map(std::ffi::OsString::from),
        )
        .unwrap();
        let CliCommand::PairEnroll { fingerprint, .. } = invocation.command else {
            panic!("enroll required")
        };
        let recovery = spaceterm_client_native::native::consumer::NativeConsumer::new(
            native.management(),
            spaceterm_client_native::native::consumer::NativeConsumerKind::Cli,
        )
        .with_credentials(Arc::new(credentials.clone()));
        assert!(matches!(
            recovery
                .enroll(
                    fingerprint,
                    true,
                    spaceterm_host::native_management::SudoMode::NonInteractive,
                    &mut PairIo
                )
                .await,
            Err(spaceterm_client::management_cli::ManagementFailure::OutcomePending)
        ));
        recovery.shutdown().await.unwrap();
        assert!(
            supervisor
                .admit_client_provisioning()
                .unwrap()
                .owner_attempt("recovery")
                .unwrap()
                .is_none()
        );
        assert!(credentials.0.lock().unwrap().is_empty());
        assert!(handle.snapshot().state() == before.state());
        stop.send(()).unwrap();
        assert!(running.await.unwrap().unwrap().cleanup_confirmed);
        return;
    }
    if preparation_mode == 6 || preparation_mode == 7 {
        use spaceterm_host::account_service::{ServiceFailure, test_support};
        assert!(
            result.is_ok(),
            "initial enrollment must finish before publication interruption"
        );
        let count = credentials.0.lock().unwrap().len();
        let admission = supervisor.admit_client_provisioning().unwrap();
        let original = admission.owner_attempt("enrollment").unwrap().unwrap();
        if preparation_mode == 7 {
            test_support::fail_next_publication_durability();
        } else {
            test_support::fail_next_publication_before_effect();
        }
        assert_eq!(
            admission.retain_owner_attempt("enrollment", Some(&original), &original),
            Err(ServiceFailure::Indeterminate)
        );
        drop(admission);
        assert_eq!(
            supervisor.retained_account_recovery(),
            Err(ServiceFailure::Indeterminate)
        );
        result = enroll_cli_with_credentials(&native, credentials.clone(), pin).await;
        assert!(
            result.is_ok(),
            "deliberate enrollment must recover the exact original publication"
        );
        assert_eq!(credentials.0.lock().unwrap().len(), count);
        assert_eq!(
            supervisor
                .admit_client_provisioning()
                .unwrap()
                .owner_attempt("enrollment")
                .unwrap(),
            Some(original)
        );
    }
    if preparation_mode == 3 {
        use spaceterm_client_native::native::runtime::{
            NativeRuntimeIdentity, RuntimeClientIdentity,
        };
        let codec = Arc::new(CanonicalCodec);
        let verifier = Arc::new(Verifier);
        let storage = spaceterm_client_native::journal::NativeClientJournal::open_existing(
            native
                .management()
                .supervisor()
                .client_connections_directory()
                .join("management"),
            codec.clone(),
            verifier.clone(),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let management_journal = Arc::new(
            JournalController::recover(
                Arc::new(storage),
                codec,
                verifier,
                JournalLimits::default(),
            )
            .await
            .unwrap(),
        );
        let identity =
            NativeRuntimeIdentity::from_journal(management_journal, Arc::new(credentials.clone()))
                .with_supervisor((*supervisor).clone());
        assert!(
            identity.access(&registration).await.is_ok(),
            "original management binding must be usable"
        );
        // Stage a marker-only interruption while every old journal is still
        // usable. The checkpoint uses only this fixture's owned public facts.
        let admission = supervisor.admit_client_provisioning().unwrap();
        let preparation: serde_json::Value = serde_json::from_slice(
            &admission
                .owner_attempt("enrollment-preparation")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let marker = serde_json::json!({
            "schema": 1, "server": preparation["server"], "server_public_key": preparation["server_public_key"],
            "previous_attempt": admission.owner_attempt("enrollment").unwrap(),
            "bindings": [], "preparation": preparation, "complete": false,
        });
        let checkpoint = serde_json::to_vec(&marker).unwrap();
        admission
            .retain_owner_attempt("recovery", None, &checkpoint)
            .unwrap();
        drop(admission);
        assert!(matches!(
            identity.access(&registration).await,
            Err(spaceterm_host::account_service::ServiceFailure::AuthorizationUnavailable)
        ));
        let foreground = spaceterm_client_native::native::consumer::NativeConsumer::new(
            native.management(),
            spaceterm_client_native::native::consumer::NativeConsumerKind::Cli,
        )
        .with_credentials(Arc::new(credentials.clone()));
        assert!(matches!(
            foreground.enroll_journal(journal.clone()).await,
            Err(spaceterm_client::management_cli::ManagementFailure::OutcomePending)
        ));
        assert!(matches!(
            foreground.provision_journal(journal.clone()).await,
            Err(spaceterm_client::management_cli::ManagementFailure::OutcomePending)
        ));
        assert_eq!(credentials.0.lock().unwrap().len(), 2);
        drop(identity);
        assert!(handle.snapshot().state() == before.state());
        stop.send(()).unwrap();
        assert!(running.await.unwrap().unwrap().cleanup_confirmed);
        return;
    }
    let desktop_journal = journal.clone();
    let mut journal = if cli && interrupt_recovery > 0 {
        let codec = Arc::new(CanonicalCodec);
        let verifier = Arc::new(Verifier);
        let storage = spaceterm_client_native::journal::NativeClientJournal::open_existing(
            native
                .management()
                .supervisor()
                .client_connections_directory()
                .join("enrollment"),
            codec.clone(),
            verifier.clone(),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        Arc::new(
            JournalController::recover(
                Arc::new(storage),
                codec,
                verifier,
                JournalLimits::default(),
            )
            .await
            .unwrap(),
        )
    } else {
        journal
    };
    let mut interrupted_client = None;
    if interrupt_recovery > 0 {
        assert!(
            result.is_err(),
            "credential boundary must interrupt provisioning"
        );
        let state = journal.snapshot().unwrap();
        interrupted_client = state.operations().iter().find_map(|record| {
            let spaceterm_client::journal::RetainedOperationState::Completed { outcome, .. } =
                &record.state
            else {
                return None;
            };
            let spaceterm_core::operations::Outcome::Pairing(outcome) =
                outcome.outcome(&CanonicalCodec).unwrap()
            else {
                return None;
            };
            let PairingOutcome::ClientBound(receipt) = *outcome else {
                return None;
            };
            let spaceterm_core::pairing::PairingReceiptBody::ClientBinding { client, .. } =
                &receipt.parts().claims
            else {
                return None;
            };
            Some(client.parts().client.id)
        });
        assert!(
            interrupted_client.is_some(),
            "origin must retain the issued receipt before receiving storage succeeds; failure {result:?}; bindings {}; prepared {}; acknowledgements {}; pending {}; completed {}",
            state.bindings().len(),
            state.pairing().prepared().len(),
            state.pairing().acknowledgements().len(),
            state
                .operations()
                .iter()
                .filter(|record| matches!(
                    record.state,
                    spaceterm_client::journal::RetainedOperationState::Pending
                ))
                .count(),
            state
                .operations()
                .iter()
                .filter(|record| matches!(
                    record.state,
                    spaceterm_client::journal::RetainedOperationState::Completed { .. }
                ))
                .count(),
        );
        if preparation_mode == 4 {
            use spaceterm_client_native::cli::{
                arguments::{CliCommand, parse},
                bindings::PairingCommandBindings,
            };
            let before = journal.snapshot().unwrap();
            assert_eq!(before.bindings().len(), 1);
            let native_consumer = spaceterm_client_native::native::consumer::NativeConsumer::new(
                native.management(),
                spaceterm_client_native::native::consumer::NativeConsumerKind::Cli,
            )
            .with_credentials(Arc::new(credentials.clone()));
            let invocation = parse(
                [
                    "pair",
                    "enroll",
                    "--server-fingerprint",
                    &spaceterm_client_native::cli::output::hex(&pin.public_key),
                ]
                .into_iter()
                .map(std::ffi::OsString::from),
            )
            .unwrap();
            let CliCommand::PairEnroll { fingerprint, .. } = invocation.command else {
                panic!("enroll required")
            };
            assert!(matches!(
                native_consumer
                    .enroll(
                        fingerprint,
                        true,
                        spaceterm_host::native_management::SudoMode::NonInteractive,
                        &mut PairIo
                    )
                    .await,
                Err(spaceterm_client::management_cli::ManagementFailure::OutcomePending)
            ));
            native_consumer.shutdown().await.unwrap();
            let admission = supervisor.admit_client_provisioning().unwrap();
            assert!(
                admission.owner_attempt("recovery").unwrap().is_none(),
                "unfinished receiver work must be refused before the recovery marker"
            );
            assert!(admission.provisioning_attempt("cli").unwrap().is_some());
            drop(admission);
            let after = journal.snapshot().unwrap();
            assert_eq!(after.bindings().len(), 1);
            assert!(after.pairing_snapshot().unwrap().suppressed.is_empty());
            assert!(
                journal
                    .ordinary_use_allowed(&after.bindings()[0].binding().unwrap())
                    .unwrap()
            );
            assert_eq!(credentials.0.lock().unwrap().len(), 2);
        }
        if cli {
            // A process restart releases the original journal before retrying
            // through the shipped lazy consumer constructor.
            drop(journal);
            result = enroll_cli_with_credentials(&native, credentials.clone(), pin).await;
            // Later assertions inspect only the separately provisioned journals.
            journal = desktop_journal.clone();
        } else {
            result = enroll_with_credentials(&native, journal.clone(), credentials.clone()).await;
        }
    }
    if result.is_ok() {
        use spaceterm_client::management_cli::{ProtocolManagement, StatusManagement};
        use spaceterm_client_native::native::consumer::{NativeConsumer, NativeConsumerKind};
        let cli = NativeConsumer::new(native.management(), NativeConsumerKind::Cli)
            .with_credentials(Arc::new(credentials.clone()));
        let restored = cli
            .pairing_runtime()
            .await
            .expect("dedicated CLI journal restoration");
        let restored_inventory = restored.inventory().await.unwrap();
        assert_eq!(restored_inventory.saved.len(), 1);
        assert!(restored_inventory.enrollment_acknowledgements.is_empty());
        drop(restored);
        assert_eq!(
            ProtocolManagement::status(&cli)
                .await
                .unwrap()
                .parts()
                .server
                .bytes(),
            pin.identity
        );
        cli.shutdown().await.unwrap();
        let companion =
            NativeConsumer::new(native.management(), NativeConsumerKind::StatusCompanion)
                .with_credentials(Arc::new(credentials.clone()));
        let mut subscription = StatusManagement::subscribe_status(&companion)
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while subscription.latest().unwrap().is_none() {
                subscription.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(
            subscription
                .latest()
                .unwrap()
                .unwrap()
                .parts()
                .server
                .bytes(),
            pin.identity
        );
        companion.shutdown().await.unwrap();
    }
    let _ = stop.send(());
    let report = running.await.unwrap();
    assert!(
        result.is_ok(),
        "owner enrollment {result:?}; joined server shutdown {report:?}"
    );
    let report = report.unwrap();
    assert!(report.cleanup_confirmed && report.listeners_closed && report.started_writes_finished);
    assert!(!report.recovery_required);
    if !cli {
        let snapshot = journal.snapshot().unwrap();
        assert_eq!(snapshot.bindings().len(), 1);
        let binding = snapshot.bindings()[0].binding().unwrap();
        assert_eq!(binding.target().pin(), pin);
        assert!(journal.ordinary_use_allowed(&binding).unwrap());
        assert!(snapshot.pairing().acknowledgements().is_empty());
    }
    let mut clients = std::collections::BTreeSet::new();
    for consumer in ["cli", "companion", "management"] {
        let codec = Arc::new(CanonicalCodec);
        let verifier = Arc::new(Verifier);
        let storage = spaceterm_client_native::journal::NativeClientJournal::open_existing(
            native
                .management()
                .supervisor()
                .client_connections_directory()
                .join(consumer),
            codec.clone(),
            verifier.clone(),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let journal = JournalController::recover(
            Arc::new(storage),
            codec,
            verifier,
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let snapshot = journal.snapshot().unwrap();
        assert_eq!(snapshot.bindings().len(), 1);
        let saved = &snapshot.bindings()[0];
        assert!(matches!(
            saved.proof().parts().claims,
            spaceterm_core::pairing::PairingReceiptBody::ClientBinding { .. }
        ));
        assert!(!saved.additional_client_acknowledgement_pending());
        let binding = saved.binding().unwrap();
        assert!(journal.ordinary_use_allowed(&binding).unwrap());
        assert_eq!(binding.target().pin(), pin);
        assert!(clients.insert(binding.target().client()));
    }
    if let Some(client) = interrupted_client {
        assert!(
            clients.contains(&client),
            "recovery must reuse the original issued Client"
        );
    }
    assert_eq!(credentials.0.lock().unwrap().len(), 2);
    if preparation_mode > 0 {
        let inventory = if cli {
            let codec = Arc::new(CanonicalCodec);
            let verifier = Arc::new(Verifier);
            let storage = spaceterm_client_native::journal::NativeClientJournal::open_existing(
                native
                    .management()
                    .supervisor()
                    .client_connections_directory()
                    .join("enrollment"),
                codec.clone(),
                verifier.clone(),
                JournalLimits::default(),
            )
            .await
            .unwrap();
            let journal = JournalController::recover(
                Arc::new(storage),
                codec,
                verifier,
                JournalLimits::default(),
            )
            .await
            .unwrap();
            journal.snapshot().unwrap().pairing_snapshot().unwrap()
        } else {
            desktop_journal
                .snapshot()
                .unwrap()
                .pairing_snapshot()
                .unwrap()
        };
        assert_eq!(
            inventory.credential_preparations.len(),
            1,
            "only the original shared device preparation may remain"
        );
        assert!(
            inventory.credential_preparations[0].credential.as_ref()
                == Some(&credentials.0.lock().unwrap()[0].0)
        );
    }
    assert_eq!(handle.snapshot().state(), before.state());
    assert_eq!(
        fixture
            .reopen()
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Running
    );
    assert_eq!(
        fixture.reopen().discover().unwrap().unwrap().server_pin(),
        pin
    );
}

struct TestChannel {
    send: tokio::sync::Mutex<Option<tokio::sync::mpsc::Sender<Vec<u8>>>>,
    completion: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    receive: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Vec<u8>>>,
    uid: u32,
    receive_fault: Arc<std::sync::atomic::AtomicUsize>,
}
impl MessageTransport for TestChannel {
    fn peer_facts(&self) -> spaceterm_host::transport::PeerFacts {
        spaceterm_host::transport::PeerFacts {
            route: spaceterm_host::transport::TransportRoute::PrivateIpc,
            local_uid: Some(self.uid),
            local_pid: None,
            pinned_server_key: None,
        }
    }
    fn send<'a>(&'a self, bytes: &'a [u8]) -> spaceterm_host::transport::TransportFuture<'a, ()> {
        Box::pin(async move {
            self.send
                .lock()
                .await
                .as_ref()
                .ok_or(spaceterm_host::transport::TransportError::Closed)?
                .send(bytes.to_vec())
                .await
                .map_err(|_| spaceterm_host::transport::TransportError::Closed)
        })
    }
    fn recv(&self) -> spaceterm_host::transport::TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async {
            let bytes = self.receive.lock().await.recv().await;
            let remaining = self.receive_fault.load(std::sync::atomic::Ordering::SeqCst);
            if bytes.is_some()
                && remaining > 0
                && self
                    .receive_fault
                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst)
                    == 1
            {
                return Err(spaceterm_host::transport::TransportError::Closed);
            }
            Ok(bytes)
        })
    }
    fn close(&self) -> spaceterm_host::transport::TransportFuture<'_, ()> {
        Box::pin(async {
            self.send.lock().await.take();
            if let Some(task) = self.completion.lock().await.take() {
                task.await
                    .map_err(|_| spaceterm_host::transport::TransportError::Unavailable)?;
            }
            Ok(())
        })
    }
}
struct InProcessOwner(
    Arc<spaceterm_host::account_service::ServiceSupervisor>,
    Arc<std::sync::atomic::AtomicUsize>,
);
impl OwnerChannelProvider for InProcessOwner {
    fn open<'a>(
        &'a self,
        registration: &'a spaceterm_host::account_service::RegisteredServer,
    ) -> spaceterm_client::ports::ClientFuture<'a, Arc<dyn MessageTransport>> {
        Box::pin(async move {
            let (first, first_receiver) = tokio::sync::mpsc::channel(1);
            let (second, second_receiver) = tokio::sync::mpsc::channel(1);
            let uid = registration.key().account_uid;
            let server = TestChannel {
                send: tokio::sync::Mutex::new(Some(first)),
                completion: tokio::sync::Mutex::new(None),
                receive: tokio::sync::Mutex::new(second_receiver),
                uid,
                receive_fault: Arc::default(),
            };
            let supervisor = self.0.clone();
            let task = tokio::spawn(async move {
                spaceterm_server::deployment::owner::execute(supervisor, &server)
                    .await
                    .unwrap();
            });
            Ok(Arc::new(TestChannel {
                send: tokio::sync::Mutex::new(Some(second)),
                completion: tokio::sync::Mutex::new(Some(task)),
                receive: tokio::sync::Mutex::new(first_receiver),
                uid,
                receive_fault: self.1.clone(),
            }) as Arc<dyn MessageTransport>)
        })
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_foreground_owner_pairing_preserves_stopped_server() {
    stopped_owner_pairing(false, 2, 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_foreground_owner_pairing_preserves_stopped_server_in_both_majors() {
    for major in [1, 2] {
        stopped_owner_pairing(true, major, 0).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_enrollment_recovers_consumed_receipt_before_local_save_in_both_majors() {
    for major in [1, 2] {
        stopped_owner_pairing(true, major, 5).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_enrollment_recovers_lost_acknowledgement_reply_in_both_majors() {
    for major in [1, 2] {
        stopped_owner_pairing(true, major, 6).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_enrollment_refuses_replacement_after_lost_authorization_delivery() {
    stopped_owner_pairing(true, 2, 2).await;
}

async fn stopped_owner_pairing(cli: bool, major: u32, receive_fault: usize) {
    let root = Root::new();
    let fixture = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
    let registration = fixture
        .initialize_with_installation(
            DesiredServerState::Stopped,
            Some(InstallationStatus {
                installation: InstallationId(std::num::NonZeroU64::new(9).unwrap()),
                version: spaceterm_distribution::BuildVersion::parse("9.0.0").unwrap(),
                artifact_sha256: [8; 32],
            }),
        )
        .unwrap();
    let pin = registration.server_pin();
    let identity = StoreIdentity {
        server: ServerId::new(pin.identity),
        account: registration.key(),
        server_public_key: pin.public_key,
    };
    let home = spaceterm_core::organization::ServerDirectory::new(
        identity.server,
        root.0.as_os_str().as_encoded_bytes().to_vec(),
    )
    .unwrap();
    let database = Database::open_for_application(
        registration.organization_directory(),
        migrations(),
        schema::APPLICATION_ID,
    )
    .unwrap();
    assert!(matches!(
        SqliteOrganizationStore::initialize_new(
            database,
            NewStoreSeed {
                identity,
                origin: NewServerOrigin::LocalHeadless
            },
            StoreLimits::production(),
            production_services(identity, home).unwrap(),
        ),
        InitializationDisposition::Durable { .. }
    ));
    let supervisor = supervisor(&root.0, Arc::new(StartupAbsence)).unwrap();
    let journal = journal(&root).await;
    let credentials = Credentials::default();
    let native = NativeClientProvider::from_supervisor((*supervisor).clone());
    let channels = Arc::new(InProcessOwner(
        supervisor.clone(),
        Arc::new(std::sync::atomic::AtomicUsize::new(receive_fault)),
    ));
    let journal = if cli {
        use spaceterm_client_native::{
            cli::bindings::PairingCommandBindings,
            native::consumer::{NativeConsumer, NativeConsumerKind},
        };
        let consumer = NativeConsumer::new(native.management(), NativeConsumerKind::Cli)
            .with_credentials(Arc::new(credentials.clone()))
            .with_owner_channels(channels.clone())
            .with_owner_handshake(spaceterm_protocol::authentication::HandshakeConfig {
                majors: vec![major],
                ..Default::default()
            });
        let invocation = spaceterm_client_native::cli::arguments::parse(
            [
                "pair",
                "enroll",
                "--server-fingerprint",
                &spaceterm_client_native::cli::output::hex(&pin.public_key),
            ]
            .into_iter()
            .map(std::ffi::OsString::from),
        )
        .unwrap();
        let spaceterm_client_native::cli::arguments::CliCommand::PairEnroll {
            fingerprint: Some(fingerprint),
            recover_account,
        } = invocation.command
        else {
            panic!("explicit fingerprint required");
        };
        let result = consumer
            .enroll(
                Some(fingerprint),
                recover_account,
                spaceterm_host::native_management::SudoMode::NonInteractive,
                &mut PairIo,
            )
            .await;
        if receive_fault > 0 {
            assert!(
                result.is_err(),
                "delivery interruption must prevent success"
            );
            let admission = supervisor.admit_client_provisioning().unwrap();
            let original = admission.owner_attempt("enrollment").unwrap().unwrap();
            drop(admission);
            let result = consumer
                .enroll(
                    Some(fingerprint),
                    recover_account,
                    spaceterm_host::native_management::SudoMode::NonInteractive,
                    &mut PairIo,
                )
                .await;
            let admission = supervisor.admit_client_provisioning().unwrap();
            assert_eq!(
                admission.owner_attempt("enrollment").unwrap().unwrap(),
                original
            );
            if receive_fault == 2 {
                assert!(matches!(
                    result,
                    Err(spaceterm_client::management_cli::ManagementFailure::OutcomePending)
                ));
                assert_eq!(credentials.0.lock().unwrap().len(), 2);
                assert_eq!(
                    std::fs::symlink_metadata(registration.endpoint().unwrap().path())
                        .unwrap_err()
                        .kind(),
                    std::io::ErrorKind::NotFound
                );
                assert_eq!(
                    supervisor.discover().unwrap().unwrap().desired_state(),
                    DesiredServerState::Stopped
                );
                return;
            }
            result.unwrap();
        } else {
            result.unwrap();
        }
        let codec = Arc::new(CanonicalCodec);
        let verifier = Arc::new(Verifier);
        let storage = spaceterm_client_native::journal::NativeClientJournal::open_existing(
            supervisor.client_connections_directory().join("enrollment"),
            codec.clone(),
            verifier.clone(),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        Arc::new(
            JournalController::recover(
                Arc::new(storage),
                codec,
                verifier,
                JournalLimits::default(),
            )
            .await
            .unwrap(),
        )
    } else {
        enroll_using(&native, journal.clone(), credentials.clone(), channels)
            .await
            .unwrap();
        journal
    };
    assert_eq!(
        std::fs::symlink_metadata(registration.endpoint().unwrap().path())
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );
    let snapshot = journal.snapshot().unwrap();
    assert_eq!(snapshot.bindings().len(), 1);
    let binding = snapshot.bindings()[0].binding().unwrap();
    assert_eq!(binding.target().pin(), pin);
    assert!(journal.ordinary_use_allowed(&binding).unwrap());
    assert!(snapshot.pairing().acknowledgements().is_empty());
    assert_eq!(credentials.0.lock().unwrap().len(), 2);
    assert_eq!(
        fixture
            .reopen()
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Stopped
    );
    assert_eq!(
        fixture.reopen().discover().unwrap().unwrap().server_pin(),
        pin
    );
}

struct PairIo;
impl spaceterm_client_native::cli::runner::CliIo for PairIo {
    fn write_stdout(&mut self, _: &[u8]) -> Result<(), spaceterm_client_native::cli::CliFailure> {
        Ok(())
    }
    fn write_stderr(&mut self, _: &[u8]) -> Result<(), spaceterm_client_native::cli::CliFailure> {
        Ok(())
    }
    fn read_confirmation(
        &mut self,
        _: &spaceterm_client::management_cli::ConfirmationPrompt,
    ) -> Result<
        spaceterm_client::management_cli::ConfirmationReply,
        spaceterm_client_native::cli::CliFailure,
    > {
        panic!("explicit fingerprint must not prompt")
    }
}
