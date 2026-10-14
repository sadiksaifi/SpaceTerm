#[path = "support/native_client_fixture.rs"]
mod native_client_fixture;
use native_client_fixture::{Directory, Fixture, Key, Verifier};

use spaceterm_client::{
    failure::{ClientFailure, ClientFailureClass},
    journal::{ClientJournal, JournalLimits, PairingClientJournal},
};
use spaceterm_client_native::journal::NativeClientJournal;
use spaceterm_core::{operation_ledger::canonical::CanonicalCodec, pairing::SignedPairingReceipt};
use spaceterm_protocol::pairing_adapter::ReceiptCodec;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

impl Directory {
    fn new() -> Self {
        // Linux /tmp can be tmpfs, which native storage rejects. Use persistent
        // temporary storage outside the build tree to isolate journal syncs from
        // compiler I/O. Native filesystem and durability checks remain unchanged.
        let parent = if cfg!(target_os = "linux") {
            PathBuf::from("/var/tmp")
        } else {
            std::env::var_os("CARGO_TARGET_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"))
        };
        Self::in_parent(parent)
    }
}

#[tokio::test]
async fn pairing_facade_uses_one_native_journal_and_one_scheduler() {
    let directory = Directory::new();
    let journal = directory.create().await;
    let native = Arc::new(journal);
    let fixture = Fixture::new(1);
    fixture.seed(native.as_ref()).await;
    let journal = controller(native.clone()).await;
    assert!(std::ptr::eq(
        journal.authority_fence(),
        native.authority_fence()
    ));
    assert!(Arc::ptr_eq(
        &journal.admission_latch(),
        &native.admission_latch()
    ));
    let store = JournalPairingStore::new(journal.clone());
    let snapshot = store.recover().await.unwrap();
    let stale = PairingJournalTransaction {
        expected_revision: snapshot.revision,
        change: PairingJournalChange::RecordCredentialPrepared {
            generation: fixture.binding.target().incarnation(),
            credential: fixture.credential.clone(),
        },
    };
    journal
        .transact(vec![ClientJournalChange::ReconcileGrants {
            local_pairing: fixture.binding.target().local_pairing().unwrap(),
            epoch: spaceterm_core::authority::ServerEpoch::new(1),
            revision: spaceterm_core::authority::OrganizationRevision::new(1),
            grants: vec![],
        }])
        .await
        .unwrap();
    assert_eq!(
        store.commit(&stale).await,
        PairingCommitDisposition::NotApplied(PairingFailure::StaleAssessment)
    );
    assert_eq!(
        store.recover().await.unwrap().revision,
        native.recover().await.unwrap().revision().get()
    );
    let tasks = PendingTasks::new(journal.clone(), Arc::new(NoEffects));
    let runtime = tasks.start_scheduler(Arc::new(NoEffects)).unwrap();
    let other_controller = controller(native).await;
    assert!(
        PendingTasks::new(other_controller, Arc::new(NoEffects))
            .start_scheduler(Arc::new(NoEffects))
            .is_err()
    );
    assert!(
        PendingTasks::new(journal, Arc::new(NoEffects))
            .start_scheduler(Arc::new(NoEffects))
            .is_err()
    );
    runtime.shutdown().await.unwrap();
}

use spaceterm_client::{
    api::{ConnectionTarget, PairedServerBinding, RouteDescription, RouteParts},
    journal::{
        ClientJournalChange, ClientJournalTransaction, JournalController, JournalRevocation,
        PairingCommitDisposition, SavedBinding,
    },
    pairing::{
        credentials::*,
        journal::*,
        revocation::{ConfirmedSelfUnpair, PairingController, VerifiedRevocationReceipt},
    },
    pending::{
        CredentialCleanupDisposition, LocalUseInvalidator, PendingTasks, RevocationDriver,
        RevocationResolution,
    },
    ports::ClientFuture,
};
use spaceterm_core::{
    authority::{GrantedControl, StoreRevision},
    identity::*,
    operations::*,
    pairing::*,
};
use spaceterm_protocol::{ProtocolError, authentication::SigningKey};

impl Fixture {
    async fn remove(&self, store: JournalPairingStore<JournalController>) -> OperationId {
        let authority = store
            .ordinary_authority(&self.binding.target().local_pairing().unwrap())
            .await
            .unwrap();
        let snapshot = store.recover().await.unwrap();
        let operation = OperationId::new(
            snapshot.saved[0].client().parts().self_revocation,
            OperationSequence::new(1).unwrap(),
        );
        let controller = PairingController::new(store, Arc::new(CanonicalCodec));
        controller
            .self_unpair(
                &authority,
                ConfirmedSelfUnpair::confirm(
                    self.binding.target().local_pairing().unwrap(),
                    operation,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(authority.lease().validate(&self.credential.scope).is_err());
        operation
    }
    fn revocation(&self) -> JournalRevocation {
        use spaceterm_client::journal::{RevocationPhase as CanonicalPhase, SavedTarget};
        use spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest;
        let key = self.binding.target().local_pairing().unwrap();
        let operation = OperationId::new(
            self.saved.streams().parts().self_revocation,
            OperationSequence::new(1).unwrap(),
        );
        let request = CanonicalOperationRequest::new(
            operation,
            Command::Pairing(PairingCommand::SelfRevoke {
                target: PairingTarget::new(PairingTargetParts {
                    server: key.parts().server,
                    pairing: key.parts().pairing,
                    device: self.binding.target().device(),
                })
                .unwrap(),
            }),
            &CanonicalCodec,
        )
        .unwrap();
        let task = PendingRevocationTask::new(PendingRevocationTaskParts {
            original: PendingSelfRevocation::new(PendingParts {
                original_server: key.parts().server,
                original_pairing: key.parts().pairing,
                device: self.binding.target().device(),
                client: self.binding.target().client(),
                operation,
                route: self.route,
                credential: self.credential.scope.reference(),
                notice_dismissed: false,
            })
            .unwrap(),
            local_pairing: key,
            pinned_server_public_key: self.binding.target().pin().public_key,
            request_digest: request.binding().digest,
        })
        .unwrap();
        JournalRevocation {
            task,
            target: SavedTarget::from_target(self.binding.target()),
            phase: CanonicalPhase::AwaitingConfirmation,
            notice_dismissed: false,
            attempts: 0,
        }
    }
    fn receipt(&self, task: &PendingRevocationTask) -> SignedPairingReceipt {
        ReceiptCodec::new(self.binding.target().pin())
            .sign(
                PairingReceiptBody::Revocation {
                    receipt: RevocationReceipt {
                        server: self.binding.target().server(),
                        pairing: self.binding.target().pairing(),
                        device: self.binding.target().device(),
                        operation: task.parts().original.operation(),
                        revision: StoreRevision::new(2),
                        receipt_digest: RequestDigest::from_bytes([0; 32]),
                    },
                    authentication_key_digest: RequestDigest::from_bytes(
                        ring::digest::digest(&ring::digest::SHA256, &self.credential.public_key)
                            .as_ref()
                            .try_into()
                            .unwrap(),
                    ),
                    revocation_event: [10; 16],
                },
                &self.server,
            )
            .unwrap()
    }
}
async fn controller(journal: Arc<NativeClientJournal>) -> Arc<JournalController> {
    Arc::new(
        JournalController::recover(
            journal,
            Arc::new(CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap(),
    )
}
struct NoEffects;
impl LocalUseInvalidator for NoEffects {
    fn invalidate_workspace(
        &self,
        _: &LocalPairingKey,
        _: WorkspaceId,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        Ok(vec![])
    }
    fn invalidate_attachment(
        &self,
        _: &LocalPairingKey,
        _: spaceterm_client::view::WorkspaceAttachment,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        Err(ClientFailure::new(ClientFailureClass::StaleAuthority))
    }
    fn invalidate_pairing(&self, _: &LocalPairingKey) {}
    fn remove_pairing<'a>(&'a self, _: &'a LocalPairingKey) -> ClientFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
impl RevocationDriver for NoEffects {
    fn resolve<'a>(&'a self, _: &'a JournalRevocation) -> ClientFuture<'a, RevocationResolution> {
        Box::pin(async { panic!("no pending revocation") })
    }
    fn acknowledge<'a>(&'a self, _: &'a JournalRevocation) -> ClientFuture<'a, ()> {
        Box::pin(async { panic!("no pending revocation") })
    }
    fn cleanup<'a>(
        &'a self,
        _: &'a JournalRevocation,
    ) -> ClientFuture<'a, CredentialCleanupDisposition> {
        Box::pin(async { panic!("no pending revocation") })
    }
}

#[tokio::test]
async fn native_receipt_commit_crash_resumes_only_exact_credential_cleanup() {
    let directory = Directory::new();
    let native = Arc::new(directory.create().await);
    let first = Fixture::new(1);
    first.seed(native.as_ref()).await;
    let journal = controller(native.clone()).await;
    let store = JournalPairingStore::new(journal.clone());
    let operation = first.remove(store).await;
    let store = JournalPairingStore::new(journal.clone());
    let snapshot = store.recover().await.unwrap();
    let task = &snapshot.revocations[0].task;
    let receipt =
        VerifiedRevocationReceipt::verify(task, &first.credential, first.receipt(task), false)
            .unwrap();
    let tx = PairingJournalTransaction {
        expected_revision: snapshot.revision,
        change: PairingJournalChange::RecordReceipt {
            operation,
            receipt: Box::new(receipt),
        },
    };
    require_durable(store.commit(&tx).await, tx.expected_revision).unwrap();
    // A later pairing must survive cleanup of this original credential.
    let second = Fixture::new(2);
    second.seed(journal.as_ref()).await;
    drop(store);
    drop(journal);
    drop(native);
    let native = Arc::new(
        NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap(),
    );
    let journal = controller(native).await;
    let store = JournalPairingStore::new(journal.clone());
    let snapshot = store.recover().await.unwrap();
    assert_eq!(snapshot.revocations[0].credential, first.credential);
    assert!(matches!(
        snapshot.revocations[0].phase,
        RevocationPhase::ReceiptRecorded {
            acknowledged: false,
            ..
        }
    ));
    let wrong = PairingJournalTransaction {
        expected_revision: snapshot.revision,
        change: PairingJournalChange::FinishCleanup {
            operation,
            expected_credential: second.credential.clone(),
        },
    };
    assert_eq!(
        store.commit(&wrong).await,
        PairingCommitDisposition::NotApplied(PairingFailure::CredentialIntegrity)
    );
    let ack = PairingJournalTransaction {
        expected_revision: snapshot.revision,
        change: PairingJournalChange::RecordAcknowledgement { operation },
    };
    require_durable(store.commit(&ack).await, ack.expected_revision).unwrap();
    let finish = PairingJournalTransaction {
        expected_revision: store.recover().await.unwrap().revision,
        change: PairingJournalChange::FinishCleanup {
            operation,
            expected_credential: first.credential,
        },
    };
    require_durable(store.commit(&finish).await, finish.expected_revision).unwrap();
    let state = journal.snapshot().unwrap();
    assert!(state.revocations().is_empty());
    assert!(state.ordinary_use_allowed(&second.binding));
    assert!(!state.ordinary_use_allowed(&first.binding));
}

struct DelayedRead {
    journal: Arc<JournalController>,
    delay: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}
impl ClientJournal for DelayedRead {
    fn admission_latch(&self) -> Arc<spaceterm_client::journal::JournalAdmission> {
        self.journal.admission_latch()
    }
    fn scheduler_claim(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.journal.scheduler_claim()
    }
    fn authority_fence(&self) -> &PairingAuthorityFence {
        self.journal.authority_fence()
    }
    fn recover(&self) -> ClientFuture<'_, spaceterm_client::journal::RecoveredClientState> {
        Box::pin(async move {
            let state = self.journal.snapshot()?;
            if self.delay.swap(false, Ordering::AcqRel) {
                self.entered.notify_one();
                self.resume.notified().await;
            }
            Ok(state)
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a ClientJournalTransaction,
    ) -> ClientFuture<'a, spaceterm_client::journal::JournalCommitDisposition> {
        self.journal.commit(tx)
    }
}
#[tokio::test]
async fn recovered_suppression_denies_delayed_first_authority_issuance_in_real_client_journal() {
    let directory = Directory::new();
    let native = Arc::new(directory.create().await);
    let fixture = Fixture::new(1);
    fixture.seed(native.as_ref()).await;
    let journal = controller(native).await;
    let delayed = Arc::new(DelayedRead {
        journal: journal.clone(),
        delay: true.into(),
        entered: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
    });
    let worker = tokio::spawn({
        let delayed = delayed.clone();
        let key = fixture.binding.target().local_pairing().unwrap();
        async move {
            JournalPairingStore::new(delayed)
                .ordinary_authority(&key)
                .await
        }
    });
    delayed.entered.notified().await;
    fixture
        .remove(JournalPairingStore::new(journal.clone()))
        .await;
    // Recovery observes real durable suppression before the delayed read is released.
    assert!(journal.recover_pairings().await.unwrap().saved.is_empty());
    delayed.resume.notify_one();
    assert!(worker.await.unwrap().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 5)]
async fn authority_capacity_is_reclaimed_after_completed_pairing_lifetimes_in_real_client_journal()
{
    let directory = Directory::new();
    let native = Arc::new(directory.create().await);
    let journal = controller(native.clone()).await;
    let store = JournalPairingStore::new(journal.clone());
    let batch_size = JournalLimits::default().saved_servers;
    let lifetimes = u64::try_from(MAX_PENDING_REVOCATIONS).unwrap() + 1;
    let server = |lifetime: u64| {
        ServerId::new(
            u128::from((lifetime - 1) % u64::try_from(batch_size).unwrap() + 1).to_be_bytes(),
        )
    };
    // Every lifetime issues a live gate after durable admission, then retires it
    // durably. Independent Servers keep both batches within the existing
    // 64-binding and 256-change limits.
    for first in (1..=lifetimes).step_by(batch_size) {
        let fixtures: Vec<_> = (first
            ..=lifetimes.min(first + u64::try_from(batch_size).unwrap() - 1))
            .map(|lifetime| Fixture::on_server(lifetime, server(lifetime)))
            .collect();
        journal
            .transact(fixtures.iter().flat_map(Fixture::seed_changes).collect())
            .await
            .unwrap();
        // Issuance performs real receipt verification. Independent reads run
        // concurrently; every mutation still goes through one serial journal.
        let issuances: Vec<_> = fixtures
            .iter()
            .map(|fixture| {
                let journal = journal.clone();
                let key = fixture.binding.target().local_pairing().unwrap();
                let scope = fixture.credential.scope.clone();
                tokio::spawn(async move {
                    let authority = JournalPairingStore::new(journal)
                        .ordinary_authority(&key)
                        .await
                        .unwrap();
                    authority.lease().validate(&scope).unwrap();
                    authority
                })
            })
            .collect();
        let mut authorities = Vec::new();
        for issuance in issuances {
            authorities.push(issuance.await.unwrap());
        }
        // No wire acknowledgement or native deletion occurs in this capacity
        // test. Revocation and its verified settlement bookkeeping can share a
        // durable transaction. Driver tests cover barriers around those effects.
        journal
            .transact(
                fixtures
                    .iter()
                    .flat_map(|fixture| {
                        let record = fixture.revocation();
                        let receipt = VerifiedRevocationReceipt::verify(
                            &record.task,
                            &fixture.credential,
                            fixture.receipt(&record.task),
                            false,
                        )
                        .unwrap();
                        let operation = record.task.parts().original.operation();
                        [
                            ClientJournalChange::BeginRevocation(Box::new(record)),
                            ClientJournalChange::ConfirmRevocation {
                                operation,
                                receipt: Box::new(receipt.proof().clone()),
                            },
                            ClientJournalChange::ConfirmRevocationAcknowledgement(operation),
                            ClientJournalChange::FinishCredentialCleanup(operation),
                        ]
                    })
                    .collect(),
            )
            .await
            .unwrap();
        for (authority, fixture) in authorities.iter().zip(&fixtures) {
            assert!(
                authority
                    .lease()
                    .validate(&fixture.credential.scope)
                    .is_err()
            );
        }
        let removed = store.recover().await.unwrap();
        assert!(removed.saved.is_empty());
        assert!(removed.revocations.is_empty());
    }
    assert_eq!(
        journal.snapshot().unwrap().tombstones().len(),
        MAX_PENDING_REVOCATIONS + 1
    );
    let next = Fixture::on_server(lifetimes + 1, server(lifetimes + 1));
    next.seed(journal.as_ref()).await;
    store
        .ordinary_authority(&next.binding.target().local_pairing().unwrap())
        .await
        .unwrap()
        .lease()
        .validate(&next.credential.scope)
        .unwrap();
    let expected = journal.snapshot().unwrap();
    drop(store);
    drop(journal);
    drop(native);
    let reopened = NativeClientJournal::open_existing(
        directory.0.clone(),
        Arc::new(CanonicalCodec),
        Arc::new(Verifier),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    assert_eq!(reopened.recover().await.unwrap(), expected);
}

use spaceterm_client::view::*;
fn retained_attachment(fixture: &Fixture, incarnation: u64) -> PersistedView {
    let server = fixture.binding.target().server();
    let workspace = WorkspaceId::new(server, 1).unwrap();
    let tab = TabId::new(server, 1).unwrap();
    let pane = PaneId::new(server, 1).unwrap();
    let attachment = WorkspaceAttachment::new(
        workspace,
        fixture.binding.target().pairing(),
        fixture.binding.target().client(),
        AttachmentIncarnation::new(incarnation).unwrap(),
    )
    .unwrap();
    PersistedView::new(PersistedViewParts {
        revision: ViewRevision::new(1),
        incarnation_high_water: incarnation,
        active: Some(workspace),
        workspaces: vec![PersistedWorkspace {
            attachment,
            availability: ViewAvailability::Available,
            tabs: TabOrderRecord {
                order: vec![tab],
                active: Some(tab),
                activation_history: vec![tab],
            },
            arrangements: vec![ArrangementRecord {
                root: 0,
                nodes: vec![ArrangementNode::Pane(pane)],
                split_high_water: 0,
                focused: pane,
                focus_history: vec![pane],
                zoomed: None,
            }],
        }],
    })
    .unwrap()
}
struct ViewFile(Option<PersistedView>);
impl ViewStore for ViewFile {
    type Version = u64;
    fn load(&mut self, _: usize) -> Result<Option<StoredView<u64>>, ViewStoreFailure> {
        Ok(self.0.as_ref().map(|view| StoredView {
            version: 1,
            bytes: view.encode().unwrap().into_bytes(),
        }))
    }
    fn replace(&mut self, _: Option<&u64>, _: &EncodedView) -> ViewCommit<u64> {
        panic!("launch does not publish personal state")
    }
}
#[tokio::test]
async fn detach_restart_preserves_explicit_reattachment_incarnation() {
    use spaceterm_client::journal::{DetachTask, JournalAttachment};
    let directory = Directory::new();
    let fixture = Fixture::new(1);
    let native = Arc::new(directory.create().await);
    fixture.seed(native.as_ref()).await;
    let journal = controller(native.clone()).await;
    let old = retained_attachment(&fixture, 7);
    let attachment = old.parts().workspaces[0].attachment;
    let key = fixture.binding.target().local_pairing().unwrap();
    journal
        .record_attachment(key.clone(), attachment)
        .await
        .unwrap();
    journal
        .transact(vec![
            ClientJournalChange::BeginDetach(DetachTask {
                local_pairing: key.clone(),
                workspace: attachment.workspace(),
                grants: vec![],
                dependencies: vec![],
                releases: vec![],
            }),
            ClientJournalChange::SuppressAttachment(
                JournalAttachment::new(key.clone(), attachment).unwrap(),
            ),
            ClientJournalChange::FinishDetach {
                local_pairing: key.clone(),
                workspace: attachment.workspace(),
            },
        ])
        .await
        .unwrap();
    let new = retained_attachment(&fixture, 8);
    let new_attachment = new.parts().workspaces[0].attachment;
    journal
        .record_attachment(key, new_attachment)
        .await
        .unwrap();
    drop(journal);
    drop(native);
    let native = Arc::new(
        NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap(),
    );
    let journal = controller(native).await;
    let (restored, _) = launch::restore(
        &mut ViewPersistence::new(ViewFile(Some(new))),
        &journal,
        &[],
    )
    .unwrap();
    assert_eq!(restored.attachments(), &[new_attachment]);
    let (old_restored, _) = launch::restore(
        &mut ViewPersistence::new(ViewFile(Some(old))),
        &journal,
        &[],
    )
    .unwrap();
    assert!(old_restored.attachments().is_empty());
    let (older_restored, _) = launch::restore(
        &mut ViewPersistence::new(ViewFile(Some(retained_attachment(&fixture, 6)))),
        &journal,
        &[],
    )
    .unwrap();
    assert!(older_restored.attachments().is_empty());
    assert_eq!(
        old_restored
            .persisted()
            .unwrap()
            .parts()
            .incarnation_high_water,
        8
    );
    let (missing, _) =
        launch::restore(&mut ViewPersistence::new(ViewFile(None)), &journal, &[]).unwrap();
    assert_eq!(
        missing.persisted().unwrap().parts().incarnation_high_water,
        8
    );
}

// The physical journal applies the transaction; this boundary fault withholds
// its receipt. It exercises uncertain native completion, not a SQLite VFS fault.
struct LostNativeReceipt {
    native: Arc<NativeClientJournal>,
    lose: std::sync::atomic::AtomicBool,
}
impl ClientJournal for LostNativeReceipt {
    fn admission_latch(&self) -> Arc<spaceterm_client::journal::JournalAdmission> {
        self.native.admission_latch()
    }
    fn authority_fence(&self) -> &PairingAuthorityFence {
        self.native.authority_fence()
    }
    fn scheduler_claim(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.native.scheduler_claim()
    }
    fn recover(&self) -> ClientFuture<'_, spaceterm_client::journal::RecoveredClientState> {
        self.native.recover()
    }
    fn commit<'a>(
        &'a self,
        tx: &'a ClientJournalTransaction,
    ) -> ClientFuture<'a, spaceterm_client::journal::JournalCommitDisposition> {
        Box::pin(async move {
            let result = self.native.commit(tx).await?;
            if self.lose.swap(false, Ordering::AcqRel) {
                assert!(matches!(
                    result,
                    spaceterm_client::journal::JournalCommitDisposition::Durable(_)
                ));
                Ok(spaceterm_client::journal::JournalCommitDisposition::Indeterminate)
            } else {
                Ok(result)
            }
        })
    }
}
use spaceterm_client::{pane_channel::*, projection::ProjectionBudgets};
#[derive(Default)]
struct PanePort {
    sent: AtomicU64,
    queued: std::sync::Mutex<Vec<(EffectPermit, FencedRuntimeCommand)>>,
}
impl PanePort {
    fn flush(&self) -> u64 {
        let mut queued = self.queued.lock().unwrap();
        for (permit, _) in queued.drain(..) {
            if permit.is_current() {
                self.sent.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.sent.load(Ordering::Relaxed)
    }
}
impl PaneConnectionPort for PanePort {
    fn grid_limits(
        &self,
        _: spaceterm_client::api::ConnectedInfo,
    ) -> Result<spaceterm_core::geometry::GridLimits, ClientFailure> {
        Ok(spaceterm_core::geometry::SUPPORTED_GRID_LIMITS)
    }
    fn explicit_use(&self, _: PaneUseRequest) -> ClientFuture<'_, PaneUseReply> {
        Box::pin(async { panic!("health test never requests startup") })
    }
    fn complete_intent(
        &self,
        _: WorkspaceId,
        _: spaceterm_core::control::MeasuredStartupMeasurement,
    ) -> ClientFuture<'_, spaceterm_client::operations::OperationTicket> {
        Box::pin(async { panic!("health test never completes startup") })
    }
    fn cancel_intent(&self, _: spaceterm_core::control::StartupIntent) {}
    fn take_over(
        &self,
        _: WorkspaceId,
        _: spaceterm_core::control::TakeOverRequest,
    ) -> ClientFuture<'_, spaceterm_client::operations::OperationTicket> {
        Box::pin(async { panic!("health test never takes over") })
    }
    fn try_effect(
        &self,
        permit: EffectPermit,
        command: FencedRuntimeCommand,
    ) -> Result<EffectAdmission, ClientFailure> {
        if !permit.is_current() {
            return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
        }
        self.queued.lock().unwrap().push((permit, command));
        Ok(EffectAdmission::Queued)
    }
    fn read(
        &self,
        _: EffectPermit,
        _: PaneReadRequest,
    ) -> ClientFuture<'_, Box<dyn PaneReadSource>> {
        Box::pin(async { panic!("health test never reads content") })
    }
}
fn pane_snapshot(fixture: &Fixture) -> spaceterm_core::organization::OrganizationSnapshot {
    use spaceterm_core::{authority::*, organization::*, organization_retained::*};
    let server = fixture.binding.target().server();
    let workspace = WorkspaceId::new(server, 1).unwrap();
    let tab = TabId::new(server, 1).unwrap();
    let pane = PaneId::new(server, 1).unwrap();
    OrganizationSnapshot::new(
        ServerEpoch::new(1),
        OrganizationState {
            server,
            revision: OrganizationRevision::new(1),
            retained: Some(
                OrganizationRetainedFacts::new(OrganizationRetainedParts {
                    server,
                    workspace_names: vec![WorkspaceNameBasis::new(
                        workspace,
                        NameText::new("Workspace".into()).unwrap(),
                        NameText::new("Workspace".into()).unwrap(),
                    )],
                    creation_defaults: vec![TabCreationDefault::new(tab, vec![pane]).unwrap()],
                    pane_generations: vec![PaneGenerationHighWater::new(
                        pane,
                        SessionGeneration::new(1),
                        ControlGeneration::new(1),
                    )],
                    tab_membership_versions: vec![TabMembershipVersion::new(
                        tab,
                        FieldVersion::new(1),
                    )],
                    repository_inventory_versions: vec![],
                    worktree_inventory: vec![],
                    remote_access: Versioned {
                        version: FieldVersion::new(1),
                        value: spaceterm_core::remote_access::RemoteAccessPolicy::disabled(server),
                    },
                })
                .unwrap(),
            ),
            workspaces: vec![WorkspaceRecord {
                id: workspace,
                name: Versioned {
                    version: FieldVersion::new(1),
                    value: WorkspaceName {
                        mode: NameMode::Custom,
                        display: NameText::new("Workspace".into()).unwrap(),
                    },
                },
                pinned_directory: Versioned {
                    version: FieldVersion::new(1),
                    value: None,
                },
                root_tab: tab,
                creation_ordinal: CreationOrdinal::new(1),
            }],
            tabs: vec![TabRecord {
                id: tab,
                workspace,
                worktree: None,
                root_pane: pane,
                creation_ordinal: CreationOrdinal::new(2),
            }],
            panes: vec![PaneRecord {
                id: pane,
                tab,
                creation_ordinal: CreationOrdinal::new(3),
                creation: PaneCreation::Initial,
                starting_directory: ServerDirectory::new(server, "/server/home".into()).unwrap(),
                confirmed_restoration_directory: Versioned {
                    version: FieldVersion::new(1),
                    value: None,
                },
            }],
            repositories: vec![],
            worktrees: vec![],
            settings: ServerSettingsRecord {
                worktree_location: Versioned {
                    version: FieldVersion::new(1),
                    value: WorktreeLocationTemplate::new("{branch}".into()).unwrap(),
                },
            },
        },
        vec![PaneRuntimeRecord {
            pane,
            session_generation: SessionGeneration::new(1),
            phase: SessionPhase::Running {
                generation: SessionGeneration::new(1),
            },
            control_generation: ControlGeneration::new(1),
            control: ControlRecord::Held {
                client: fixture.binding.target().client(),
                generation: ControlGeneration::new(1),
            },
        }],
    )
    .unwrap()
}
#[tokio::test]
async fn indeterminate_native_journal_revokes_live_pane_dispatch() {
    check_indeterminate_dispatch(0).await;
}

#[tokio::test]
async fn native_health_latch_revokes_dispatch_before_controller_receives_commit_result() {
    check_indeterminate_dispatch(1).await;
}

async fn check_indeterminate_dispatch(mode: u8) {
    use spaceterm_client::{
        api::{ConnectedInfo, ConnectionCapabilities},
        journal::{JournalAttachment, JournalHealth},
        limits::ClientLimits,
        replica::{OrganizationResidency, Replica},
    };
    use spaceterm_core::authority::*;
    let directory = Directory::new();
    let native = Arc::new(directory.create().await);
    let fixture = Fixture::new(1);
    fixture.seed(native.as_ref()).await;
    let storage = Arc::new(LostNativeReceipt {
        native: native.clone(),
        lose: true.into(),
    });
    let journal = JournalController::recover(
        storage,
        Arc::new(CanonicalCodec),
        Arc::new(Verifier),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    let port = Arc::new(PanePort::default());
    let channels =
        PaneChannels::new(port.clone(), ProjectionBudgets::supported().unwrap()).unwrap();
    let info = ConnectedInfo {
        server: fixture.binding.target().server(),
        pairing: fixture.binding.target().pairing(),
        client: fixture.binding.target().client(),
        epoch: ServerEpoch::new(1),
        binding: ConnectionBinding::new(
            fixture.binding.target().client(),
            ConnectionGeneration::new(1),
        ),
        major: 2,
        limits: Default::default(),
        capabilities: ConnectionCapabilities::NONE,
        organization_revision: OrganizationRevision::new(1),
        attempt: 1,
    };
    let mut replica = Replica::new(
        info.server,
        Arc::new(OrganizationResidency::new(ClientLimits::default()).unwrap()),
    );
    let token = replica.begin_resync(info.epoch).unwrap();
    replica
        .install_snapshot(token, pane_snapshot(&fixture))
        .unwrap();
    let snapshot = replica.subscribe().borrow().retained().unwrap().clone();
    channels.publish(info, snapshot.clone()).unwrap();
    let attachment = retained_attachment(&fixture, 1).parts().workspaces[0].attachment;
    let lease = channels
        .attach(attachment.workspace(), attachment.incarnation())
        .unwrap();
    let pane = lease.pane(PaneId::new(info.server, 1).unwrap()).unwrap();
    pane.update_context(PaneContextFacts {
        focused_pane: true,
        terminal_input_focus: true,
        presentable: true,
    })
    .unwrap();
    journal.bind_pane_channels(channels.clone()).unwrap();
    let authority = JournalPairingStore::new(Arc::new(journal.clone()))
        .ordinary_authority(&fixture.binding.target().local_pairing().unwrap())
        .await
        .unwrap();
    let session = SessionKey {
        epoch: info.epoch,
        pane: pane.pane(),
        generation: SessionGeneration::new(1),
    };
    let fence = EffectFence::new(
        session,
        ControlGrant::new(pane.pane(), info.client, ControlGeneration::new(1)).unwrap(),
        info.binding,
        pane.state().borrow().context,
        None,
    )
    .unwrap();
    pane.publish_content_base(
        fence,
        spaceterm_core::content::ContentBase {
            session,
            revision: ContentRevision::new(1),
            screen: spaceterm_core::content::Screen::Primary,
            screen_generation: 1,
            topology_revision: 1,
        },
    )
    .unwrap();
    let read = pane.permit(EffectKind::Read).unwrap();
    let stamp = pane
        .query_stamp(
            &read,
            spaceterm_core::content_queries::RequestGeneration::new(1),
        )
        .unwrap();
    let mut page = spaceterm_core::content::ContentPageParts::new(stamp.base(), 1);
    page.rows.push(
        spaceterm_core::content::ContentRow::new(
            spaceterm_core::content::RowId {
                screen: stamp.base().screen,
                screen_generation: 1,
                node_serial: 1,
                page_row: 0,
            },
            1,
            false,
            vec![spaceterm_core::content::StyledCell::new(
                spaceterm_core::content::CellContent::grapheme(
                    "a".into(),
                    spaceterm_core::content::CellWidth::Single,
                )
                .unwrap(),
                Default::default(),
                None,
            )],
        )
        .unwrap(),
    );
    pane.projection()
        .lock()
        .unwrap()
        .accept_page(
            stamp,
            spaceterm_core::content::ContentPage::new(page).unwrap(),
        )
        .unwrap();
    let frame = pane.current_frame().unwrap();
    assert!(frame.is_paintable() && frame.permits_native_content_use());
    let request = spaceterm_core::native_requests::NativeRequest::new(
        fence,
        spaceterm_core::native_requests::NativeRequestId::new(1).unwrap(),
        spaceterm_core::observations::MonotonicTick::new(0),
        spaceterm_core::observations::MonotonicTick::new(0),
        Some(spaceterm_core::observations::MonotonicTick::new(30000)),
        spaceterm_core::native_requests::NativeRequestKind::ClipboardWrite(
            spaceterm_core::native_requests::NativeText::new("fixture".into()).unwrap(),
        ),
    )
    .unwrap();
    let mut events = pane.events().unwrap();
    pane.receive_native(
        request.clone(),
        NativeTiming {
            not_before: std::time::Duration::ZERO,
            expires_after: Some(std::time::Duration::from_secs(30)),
        },
    )
    .unwrap();
    let PaneEvent::Native {
        request: native_request,
        ..
    } = events.next().await.unwrap().unwrap()
    else {
        panic!("native request was not delivered");
    };
    assert!(native_request.is_current());
    let focus = pane.permit(EffectKind::FocusReport).unwrap();
    pane.try_effect(&focus, RuntimeEffect::FocusReport(true))
        .unwrap();
    let permit = pane.permit(EffectKind::ImeCommit).unwrap();
    let native_permit = pane.permit(EffectKind::NativeRead).unwrap();
    if mode == 2 {
        JournalPairingStore::new(native.clone())
            .seal_ordinary(&fixture.binding.target().local_pairing().unwrap());
    } else {
        if mode == 1 {
            // Native uncertainty must revoke before an async commit waiter resumes.
            native.admission_latch().close();
        }
        journal
            .transact(vec![ClientJournalChange::RecordAttachment(
                JournalAttachment::new(
                    fixture.binding.target().local_pairing().unwrap(),
                    attachment,
                )
                .unwrap(),
            )])
            .await
            .unwrap_err();
        assert_eq!(*journal.health().borrow(), JournalHealth::RecoveryRequired);
        assert_eq!(
            native.recover().await.unwrap_err().class(),
            ClientFailureClass::RecoveryRequired
        );
    }
    let _ = channels.publish(info, snapshot);
    let _ = pane.publish_content_base(
        fence,
        spaceterm_core::content::ContentBase {
            session,
            revision: ContentRevision::new(1),
            screen: spaceterm_core::content::Screen::Primary,
            screen_generation: 1,
            topology_revision: 1,
        },
    );
    assert!(!frame.is_paintable());
    assert!(!frame.permits_native_content_use());
    assert!(!native_request.is_current());
    assert!(
        pane.receive_native(
            request,
            NativeTiming {
                not_before: std::time::Duration::ZERO,
                expires_after: Some(std::time::Duration::from_secs(30))
            }
        )
        .is_err()
    );
    assert!(!permit.is_current());
    assert!(!native_permit.is_current());
    assert!(pane.permit(EffectKind::NativeRead).is_err());
    assert!(
        port.try_effect(
            permit.clone(),
            FencedRuntimeCommand {
                fence,
                command: RuntimeEffect::ClearScreenAndScrollback
            }
        )
        .is_err()
    );
    assert!(pane.permit(EffectKind::ImeCommit).is_err());
    assert!(
        authority
            .lease()
            .validate(&fixture.credential.scope)
            .is_err()
    );
    assert_eq!(port.flush(), 0);
}

#[tokio::test]
async fn native_pairing_facade_transfers_preparation_and_enrollment_ack_in_one_aggregate() {
    use spaceterm_protocol::{
        pairing_adapter::{PublicEnrollmentReservation, operation_to_wire, target_to_wire},
        wire::{common, pairing},
    };
    let directory = Directory::new();
    let journal = controller(Arc::new(directory.create().await)).await;
    let store = JournalPairingStore::new(journal.clone());
    let fixture = Fixture::new(1);
    let device = PreparedCredential {
        scope: CredentialScope::new(CredentialScopeParts {
            namespace: fixture.credential.scope.parts().namespace.clone(),
            purpose: CredentialPurpose::DeviceIdentity,
            reference: CredentialReference::new([55; 16]),
        })
        .unwrap(),
        public_key: Key::new(3).public_key(),
    };
    for credential in [&device, &fixture.credential] {
        let reservation = reserve_native_credential(&store, credential.scope.clone())
            .await
            .unwrap();
        record_prepared_credential(&store, reservation.generation(), credential.clone())
            .await
            .unwrap();
    }
    let generation = LocalPairingGeneration::new(2).unwrap();
    journal
        .transact(vec![ClientJournalChange::BindCredentialRoute {
            reference: fixture.credential.scope.reference(),
            route_reference: fixture.route,
            route: fixture.binding.target().route().clone(),
        }])
        .await
        .unwrap();
    let target = PairingTarget::new(PairingTargetParts {
        server: fixture.binding.target().server(),
        pairing: fixture.binding.target().pairing(),
        device: fixture.binding.target().device(),
    })
    .unwrap();
    let keys = PairingKeys::new(PairingKeysParts {
        device_identity_public_key: device.public_key,
        pairing_authentication_public_key: fixture.credential.public_key,
    })
    .unwrap();
    let enrollment = OperationBinding {
        id: OperationId::new(
            OperationStreamId::new(
                OperationStreamKey::new(target.parts().server, 5).unwrap(),
                target.parts().pairing,
                fixture.binding.target().client(),
            )
            .unwrap(),
            OperationSequence::new(1).unwrap(),
        ),
        digest: RequestDigest::from_bytes([11; 32]),
    };
    let public = PublicEnrollmentReservation::from_wire(&pairing::Reservation {
        ticket: Some(pairing::Ticket {
            server: Some(common::Identity {
                value: target.parts().server.bytes().to_vec(),
            }),
            ticket_nonce: vec![12; 16],
        }),
        target: Some(target_to_wire(&target)),
        keys: Some(pairing::Keys {
            device_identity_public_key: device.public_key.to_vec(),
            pairing_authentication_public_key: fixture.credential.public_key.to_vec(),
        }),
        initial_client: Some(common::Entity {
            server: Some(common::Identity {
                value: target.parts().server.bytes().to_vec(),
            }),
            local_id: fixture.binding.target().client().local(),
        }),
        enrollment_operation: Some(operation_to_wire(enrollment.id)),
        claim_digest: vec![13; 32],
        enrollment_request_digest: enrollment.digest.bytes().to_vec(),
    })
    .unwrap();
    let prepared = PreparedEnrollment {
        local_pairing: LocalPairingKey::new(LocalPairingKeyParts {
            server: target.parts().server,
            pairing: target.parts().pairing,
            generation,
        })
        .unwrap(),
        device: target.parts().device,
        pin: fixture.binding.target().pin(),
        keys: keys.clone(),
        client: fixture.binding.target().client(),
        credential: fixture.credential.clone(),
        route: fixture.route,
        enrollment,
        reservation: Some(public),
    };
    let tx = PairingJournalTransaction {
        expected_revision: store.recover().await.unwrap().revision,
        change: PairingJournalChange::PrepareEnrollment(Box::new(prepared.clone())),
    };
    require_durable(store.commit(&tx).await, tx.expected_revision).unwrap();
    let proof = ReceiptCodec::new(prepared.pin)
        .sign(
            PairingReceiptBody::Enrollment {
                target,
                keys,
                client: fixture.saved.streams().clone(),
                enrollment,
                revision: StoreRevision::new(3),
            },
            &fixture.server,
        )
        .unwrap();
    let saved =
        spaceterm_client::pairing::enrollment::verify_binding(&prepared, proof.clone(), false)
            .unwrap();
    let tx = PairingJournalTransaction {
        expected_revision: store.recover().await.unwrap().revision,
        change: PairingJournalChange::SaveEnrollment {
            saved: Box::new(saved),
            acknowledgement: Box::new(EnrollmentAcknowledgementWork {
                prepared: prepared.clone(),
                device,
                receipt: proof,
            }),
        },
    };
    require_durable(store.commit(&tx).await, tx.expected_revision).unwrap();
    let snapshot = store.recover().await.unwrap();
    assert!(snapshot.prepared.is_empty());
    assert_eq!(snapshot.saved.len(), 1);
    assert_eq!(snapshot.enrollment_acknowledgements[0].prepared, prepared);
    assert_eq!(journal.snapshot().unwrap().bindings().len(), 1);
    assert_eq!(
        snapshot.revision,
        journal.snapshot().unwrap().revision().get()
    );
    let tx = PairingJournalTransaction {
        expected_revision: snapshot.revision,
        change: PairingJournalChange::FinishEnrollmentAcknowledgement {
            operation: enrollment.id,
        },
    };
    require_durable(store.commit(&tx).await, tx.expected_revision).unwrap();
    assert!(
        store
            .recover()
            .await
            .unwrap()
            .enrollment_acknowledgements
            .is_empty()
    );
}

#[tokio::test]
async fn canonical_native_self_unpair_seals_pairing_facade_cached_authority() {
    let directory = Directory::new();
    let native = Arc::new(directory.create().await);
    let fixture = Fixture::new(1);
    fixture.seed(native.as_ref()).await;
    let key = fixture.binding.target().local_pairing().unwrap();
    let authority = JournalPairingStore::new(native.clone())
        .ordinary_authority(&key)
        .await
        .unwrap();
    let tx = ClientJournalTransaction {
        expected_revision: native.recover().await.unwrap().revision(),
        changes: vec![ClientJournalChange::BeginRevocation(Box::new(
            fixture.revocation(),
        ))],
    };
    assert!(matches!(
        native.commit(&tx).await.unwrap(),
        spaceterm_client::journal::JournalCommitDisposition::Durable(_)
    ));
    assert!(
        authority
            .lease()
            .validate(&fixture.credential.scope)
            .is_err()
    );
    assert!(
        !native
            .recover()
            .await
            .unwrap()
            .ordinary_use_allowed(&fixture.binding)
    );
}

struct CleanupCredentials(Arc<std::sync::Mutex<Vec<PreparedCredential>>>);
impl NativeDeviceCredentials for CleanupCredentials {
    fn create(
        &self,
        _: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, CredentialFailure>> {
        Box::pin(async { panic!("recorded revocation cannot enroll") })
    }
    fn recover_preparation<'a>(
        &'a self,
        _: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, CredentialFailure>> {
        Box::pin(async { panic!("recorded revocation cannot prepare a credential") })
    }
    fn signing_key<'a>(
        &'a self,
        _: &'a PreparedCredential,
        _: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, CredentialFailure>> {
        Box::pin(async { panic!("acknowledged revocation needs no signing") })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, CredentialFailure>> {
        Box::pin(async move {
            self.0.lock().unwrap().push(credential.clone());
            Ok(CredentialDeletion::Deleted)
        })
    }
}
struct NoRestrictedConnection;
impl spaceterm_client::pairing::revocation::RestrictedConnector for NoRestrictedConnection {
    fn open<'a>(
        &'a self,
        _: &'a PendingRevocationTask,
    ) -> PairingFuture<
        'a,
        Result<Box<dyn spaceterm_client::pairing::enrollment::PairingTransport>, PairingFailure>,
    > {
        Box::pin(async { panic!("acknowledged receipt resumes cleanup, not revocation") })
    }
}

#[tokio::test]
async fn native_recorded_revocation_uses_real_pairing_driver_and_one_scheduler() {
    let directory = Directory::new();
    let native = Arc::new(directory.create().await);
    let first = Fixture::new(1);
    first.seed(native.as_ref()).await;
    let journal = controller(native.clone()).await;
    let operation = first
        .remove(JournalPairingStore::new(journal.clone()))
        .await;
    let store = JournalPairingStore::new(journal.clone());
    let snapshot = store.recover().await.unwrap();
    let task = &snapshot.revocations[0].task;
    let receipt =
        VerifiedRevocationReceipt::verify(task, &first.credential, first.receipt(task), false)
            .unwrap();
    for change in [
        PairingJournalChange::RecordReceipt {
            operation,
            receipt: Box::new(receipt),
        },
        PairingJournalChange::RecordAcknowledgement { operation },
    ] {
        let revision = store.recover().await.unwrap().revision;
        require_durable(
            store
                .commit(&PairingJournalTransaction {
                    expected_revision: revision,
                    change,
                })
                .await,
            revision,
        )
        .unwrap();
    }
    let second = Fixture::new(2);
    second.seed(journal.as_ref()).await;
    drop(store);
    drop(journal);
    drop(native);
    let native = Arc::new(
        NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap(),
    );
    let journal = controller(native).await;
    let deleted = Arc::new(std::sync::Mutex::new(vec![]));
    let driver = Arc::new(
        spaceterm_client::pairing::revocation::RevocationDriver::new(
            CleanupCredentials(deleted.clone()),
            NoRestrictedConnection,
            Arc::new(CanonicalCodec),
            spaceterm_protocol::authentication::HandshakeConfig::default(),
        ),
    );
    let tasks = PendingTasks::new(journal.clone(), Arc::new(NoEffects));
    let runtime = tasks.start_pairing_scheduler(driver.clone()).unwrap();
    assert!(tasks.start_pairing_scheduler(driver).is_err());
    assert!(tasks.start_scheduler(Arc::new(NoEffects)).is_err());
    // This watchdog bounds a stalled test, not the native disk's commit latency.
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let mut revisions = journal.revisions();
        while !journal.snapshot().unwrap().revocations().is_empty() {
            revisions.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    runtime.shutdown().await.unwrap();
    assert_eq!(*deleted.lock().unwrap(), vec![first.credential]);
    assert!(journal.ordinary_use_allowed(&second.binding).unwrap());
    assert!(!journal.ordinary_use_allowed(&first.binding).unwrap());
}

struct ClosingViewLoad {
    journal: JournalController,
    file: ViewFile,
}
impl ViewStore for ClosingViewLoad {
    type Version = u64;
    fn load(&mut self, maximum_bytes: usize) -> Result<Option<StoredView<u64>>, ViewStoreFailure> {
        self.journal.admission_latch().close();
        self.file.load(maximum_bytes)
    }
    fn replace(&mut self, _: Option<&u64>, _: &EncodedView) -> ViewCommit<u64> {
        panic!("launch cannot overwrite the existing view")
    }
}

#[tokio::test]
async fn launch_rechecks_native_journal_health_after_personal_view_load() {
    let fixture = Fixture::new(1);
    for document in [None, Some(retained_attachment(&fixture, 1))] {
        let directory = Directory::new();
        let native = Arc::new(directory.create().await);
        fixture.seed(native.as_ref()).await;
        let journal = controller(native).await;
        let mut persistence = ViewPersistence::new(ClosingViewLoad {
            journal: journal.as_ref().clone(),
            file: ViewFile(document),
        });
        assert!(matches!(
            launch::restore(&mut persistence, journal.as_ref(), &[]),
            Err(launch::LaunchFailure::Journal(error)) if error.class() == ClientFailureClass::RecoveryRequired
        ));
    }
}

#[tokio::test]
async fn native_pairing_seal_denies_delayed_publication_and_final_pane_dequeue() {
    check_indeterminate_dispatch(2).await;
}

struct HeldAttachmentReceipt {
    native: Arc<NativeClientJournal>,
    first: std::sync::atomic::AtomicBool,
    durable: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
}
impl ClientJournal for HeldAttachmentReceipt {
    fn authority_fence(&self) -> &PairingAuthorityFence {
        self.native.authority_fence()
    }
    fn admission_latch(&self) -> Arc<spaceterm_client::journal::JournalAdmission> {
        self.native.admission_latch()
    }
    fn scheduler_claim(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.native.scheduler_claim()
    }
    fn recover(&self) -> ClientFuture<'_, spaceterm_client::journal::RecoveredClientState> {
        self.native.recover()
    }
    fn commit<'a>(
        &'a self,
        tx: &'a ClientJournalTransaction,
    ) -> ClientFuture<'a, spaceterm_client::journal::JournalCommitDisposition> {
        Box::pin(async move {
            let result = self.native.commit(tx).await?;
            if self.first.swap(false, Ordering::AcqRel) {
                assert!(matches!(
                    result,
                    spaceterm_client::journal::JournalCommitDisposition::Durable(_)
                ));
                self.durable.add_permits(1);
                self.release.acquire().await.unwrap().forget();
            }
            Ok(result)
        })
    }
}

#[tokio::test]
async fn cancelled_native_attachment_reservation_retries_with_new_incarnation_in_the_same_view() {
    use spaceterm_client::{
        api::{ConnectedInfo, ConnectionCapabilities},
        limits::ClientLimits,
        replica::{OrganizationResidency, Replica},
        view::*,
    };
    use spaceterm_core::authority::*;
    let directory = Directory::new();
    let native = Arc::new(directory.create().await);
    let fixture = Fixture::new(1);
    fixture.seed(native.as_ref()).await;
    let storage = Arc::new(HeldAttachmentReceipt {
        native: native.clone(),
        first: true.into(),
        durable: tokio::sync::Semaphore::new(0),
        release: tokio::sync::Semaphore::new(0),
    });
    let journal = JournalController::recover(
        storage.clone(),
        Arc::new(CanonicalCodec),
        Arc::new(Verifier),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    let channels = PaneChannels::new(
        Arc::new(PanePort::default()),
        ProjectionBudgets::supported().unwrap(),
    )
    .unwrap();
    journal.bind_pane_channels(channels.clone()).unwrap();
    let snapshot = pane_snapshot(&fixture);
    let info = ConnectedInfo {
        server: fixture.binding.target().server(),
        pairing: fixture.binding.target().pairing(),
        client: fixture.binding.target().client(),
        epoch: snapshot.epoch(),
        binding: ConnectionBinding::new(
            fixture.binding.target().client(),
            ConnectionGeneration::new(1),
        ),
        major: 2,
        limits: Default::default(),
        capabilities: ConnectionCapabilities::NONE,
        organization_revision: snapshot.state().revision,
        attempt: 1,
    };
    let replica = ReplicaObservation::new(
        &snapshot,
        &[],
        ReplicaFacts {
            pairing: info.pairing,
            client: info.client,
            connection: Some(info.binding),
            readiness: ReplicaReadiness::Ready,
            status: ServerViewStatus::Connected,
        },
    )
    .unwrap();
    let mut publisher = Replica::new(
        info.server,
        Arc::new(OrganizationResidency::new(ClientLimits::default()).unwrap()),
    );
    let token = publisher.begin_resync(info.epoch).unwrap();
    publisher.install_snapshot(token, snapshot.clone()).unwrap();
    channels
        .publish(
            info,
            publisher.subscribe().borrow().retained().unwrap().clone(),
        )
        .unwrap();
    let (mut view, _) = ClientViewState::restore(None, &journal.snapshot().unwrap()).unwrap();
    let workspace = snapshot.state().workspaces[0].id;
    let mut changes = journal.revisions();
    let mut cancelled = Box::pin(launch::attach(
        &mut view,
        &replica,
        workspace,
        &fixture.binding,
        &journal,
        &channels,
    ));
    tokio::select! { _ = &mut cancelled => panic!("receipt is withheld"), permit = storage.durable.acquire() => permit.unwrap().forget() }
    drop(cancelled);
    assert!(view.attachments().is_empty());
    assert_eq!(
        native
            .recover()
            .await
            .unwrap()
            .incarnation_floor()
            .unwrap()
            .get(),
        1
    );
    storage.release.add_permits(1);
    changes.changed().await.unwrap();
    let (lease, _) = launch::attach(
        &mut view,
        &replica,
        workspace,
        &fixture.binding,
        &journal,
        &channels,
    )
    .await
    .unwrap();
    assert_eq!(lease.incarnation().get(), 2);
    assert!(lease.pane(snapshot.state().panes[0].id).is_ok());
    assert_eq!(view.attachments()[0].incarnation(), lease.incarnation());
}

fn fixture_on_route(route: RouteDescription) -> Fixture {
    let mut fixture = Fixture::new(1);
    let old = fixture.binding.target();
    let target = ConnectionTarget::new(
        old.pin(),
        old.device(),
        old.pairing(),
        old.client(),
        route,
        old.credential(),
        old.incarnation(),
    )
    .unwrap();
    fixture.binding = PairedServerBinding::new(target);
    fixture.saved = SavedBinding::verified(
        fixture.binding.clone(),
        fixture.saved.streams().clone(),
        fixture.saved.proof().clone(),
        &Verifier,
    )
    .unwrap();
    fixture
}

#[tokio::test]
async fn restricted_native_connector_uses_original_journal_task_after_reopen() {
    use spaceterm_client::{
        journal::JournalCommitDisposition, pairing::revocation::RestrictedConnector,
    };
    use spaceterm_client_native::{
        restricted_routes::NativePairingConnector,
        route::{LocalRouteAuthority, NativeRouteConnector},
    };
    use spaceterm_host::{
        ipc::{IpcListener, PrivateIpcDirectory},
        transport::{AdmissionLimits, MessageLimit, MessageTransport},
    };
    for major in [1, 2] {
        let directory = Directory::new();
        let sockets = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(sockets.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let endpoint = PrivateIpcDirectory::open(&sockets.path().canonicalize().unwrap())
            .unwrap()
            .endpoint("server")
            .unwrap();
        let authority = LocalRouteAuthority::new(endpoint.clone()).unwrap();
        let fixture = fixture_on_route(authority.description().clone());
        let journal = directory.create().await;
        fixture.seed(&journal).await;
        let record = fixture.revocation();
        let transaction = ClientJournalTransaction {
            expected_revision: journal.recover().await.unwrap().revision(),
            changes: vec![ClientJournalChange::BeginRevocation(Box::new(
                record.clone(),
            ))],
        };
        assert!(matches!(
            journal.commit(&transaction).await.unwrap(),
            JournalCommitDisposition::Durable(_)
        ));
        drop(journal);
        let journal = NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        assert!(journal.recover().await.unwrap().bindings().is_empty());
        let listener = IpcListener::bind(
            endpoint,
            MessageLimit::envelope(),
            AdmissionLimits::new(2, std::time::Duration::from_secs(20)).unwrap(),
        )
        .unwrap();
        let receipt = fixture.receipt(&record.task);
        let expected_receipt = receipt.clone();
        let pin = fixture.binding.target().pin();
        let task = record.task.clone();
        let server_key: Arc<dyn SigningKey> = Arc::new(Key::new(1));
        let worker = tokio::spawn(async move {
            use spaceterm_protocol::{
                authentication::{
                    Established, HandshakeConfig, PairingLookup, PairingRecord, Principal,
                    ServerHandshake,
                },
                pairing_adapter::{
                    PairingRestrictedCodec, operation_to_wire, receipt_to_wire, target_to_wire,
                },
                wire::pairing,
            };
            struct Lookup;
            impl PairingLookup for Lookup {
                fn lookup(
                    &self,
                    server: [u8; 16],
                    pairing: [u8; 16],
                    client: [u8; 16],
                ) -> Result<PairingRecord, ProtocolError> {
                    Ok(PairingRecord {
                        principal: Principal {
                            server,
                            device: [3; 16],
                            pairing,
                            client,
                        },
                        device_public_key: Key::new(2).public_key(),
                        active: false,
                    })
                }
            }
            let pending = listener.accept().await.unwrap();
            let hello = pending.recv().await.unwrap().unwrap();
            let (handshake, reply) = ServerHandshake::accept(
                pin.identity,
                1,
                HandshakeConfig {
                    majors: vec![major],
                    ..HandshakeConfig::default()
                },
                server_key,
                &Lookup,
                &hello,
            )
            .unwrap();
            pending.send(&reply).await.unwrap();
            let proof = pending.recv().await.unwrap().unwrap();
            let (established, ready) = handshake.receive_device(&Lookup, &proof).unwrap();
            let Established::Restricted(mut restricted) = established else {
                panic!("restricted route gained ordinary authority")
            };
            assert_eq!(restricted.major(), major);
            pending.send(&ready).await.unwrap();
            let transport = pending.finish_authentication().unwrap();
            let codec = PairingRestrictedCodec::new(pin);
            let request = transport.recv().await.unwrap().unwrap();
            let request = restricted
                .decode_pairing_extension(&request, &codec)
                .unwrap();
            assert!(matches!(
                request.kind,
                Some(pairing::restricted_extension::Kind::Status(_))
            ));
            let original = &task.parts().original;
            let target = PairingTarget::new(PairingTargetParts {
                server: original.server(),
                pairing: original.pairing(),
                device: original.device(),
            })
            .unwrap();
            let result = pairing::RestrictedExtension {
                kind: Some(pairing::restricted_extension::Kind::Result(
                    pairing::RestrictedResult {
                        request_operation: Some(operation_to_wire(original.operation())),
                        request_digest: task.parts().request_digest.bytes().to_vec(),
                        target: Some(target_to_wire(&target)),
                        disposition: 2,
                        receipt: Some(receipt_to_wire(&receipt)),
                    },
                )),
            };
            transport
                .send(
                    &restricted
                        .encode_pairing_extension(&result, &codec)
                        .unwrap(),
                )
                .await
                .unwrap();
            transport.close().await.unwrap();
        });
        let connector = NativePairingConnector::new(
            Arc::new(journal),
            NativeRouteConnector::new(vec![authority], None).unwrap(),
        );
        assert!(matches!(
            spaceterm_client::pairing::enrollment::EnrollmentConnector::open(
                &connector,
                pin,
                fixture.route
            )
            .await,
            Err(PairingFailure::RouteChanged)
        ));
        let mut transport = RestrictedConnector::open(&connector, &record.task)
            .await
            .unwrap();
        use spaceterm_protocol::{
            authentication::{ClientHandshake, Established, HandshakeConfig, Principal, Purpose},
            pairing_adapter::{PairingRestrictedCodec, operation_to_wire, target_to_wire},
            wire::pairing,
        };
        let local = |value: u64| {
            let mut bytes = [0; 16];
            bytes[8..].copy_from_slice(&value.to_be_bytes());
            bytes
        };
        let original = &record.task.parts().original;
        let principal = Principal {
            server: pin.identity,
            device: original.device().bytes(),
            pairing: local(original.pairing().local()),
            client: local(original.client().local()),
        };
        let (handshake, hello) = ClientHandshake::begin(
            pin,
            principal,
            Purpose::RevocationStatus,
            HandshakeConfig {
                majors: vec![major],
                ..HandshakeConfig::default()
            },
            Arc::new(Key::new(2)),
        )
        .unwrap();
        transport.send(&hello).await.unwrap();
        let (ready, proof) = handshake
            .receive_server(&transport.receive().await.unwrap())
            .unwrap();
        transport.send(&proof).await.unwrap();
        let Established::Restricted(mut connection) = ready
            .receive_ready(&transport.receive().await.unwrap())
            .unwrap()
        else {
            panic!("ordinary connection")
        };
        let target = PairingTarget::new(PairingTargetParts {
            server: original.server(),
            pairing: original.pairing(),
            device: original.device(),
        })
        .unwrap();
        let request = pairing::RestrictedExtension {
            kind: Some(pairing::restricted_extension::Kind::Status(
                pairing::ReceiptStatus {
                    operation: Some(operation_to_wire(original.operation())),
                    request_digest: record.task.parts().request_digest.bytes().to_vec(),
                    target: Some(target_to_wire(&target)),
                },
            )),
        };
        let codec = PairingRestrictedCodec::new(pin);
        transport
            .send(
                &connection
                    .encode_pairing_extension(&request, &codec)
                    .unwrap(),
            )
            .await
            .unwrap();
        let reply = connection
            .decode_pairing_extension(&transport.receive().await.unwrap(), &codec)
            .unwrap();
        let Some(pairing::restricted_extension::Kind::Result(reply)) = reply.kind else {
            panic!("missing receipt")
        };
        let receipt = ReceiptCodec::new(pin)
            .from_wire(reply.receipt.as_ref().unwrap())
            .unwrap();
        assert_eq!(receipt, expected_receipt);
        ReceiptCodec::new(pin).verify(&receipt).unwrap();
        worker.await.unwrap();
    }
}

#[tokio::test]
async fn old_uncaptured_ssh_journal_records_fail_closed_without_spawning() {
    use spaceterm_client::pairing::{
        enrollment::EnrollmentConnector, revocation::RestrictedConnector,
    };
    use spaceterm_client_native::{
        restricted_routes::NativePairingConnector, route::NativeRouteConnector,
    };
    let directory = Directory::new();
    let journal = directory.create().await;
    let fixture = fixture_on_route(
        RouteDescription::new(RouteParts::Ssh {
            destination: "old-alias".into(),
            remote_socket: "/server".into(),
        })
        .unwrap(),
    );
    fixture.seed(&journal).await;
    let connector = NativePairingConnector::new(
        Arc::new(journal.clone()),
        NativeRouteConnector::new(vec![], None).unwrap(),
    );
    assert!(matches!(
        EnrollmentConnector::open(&connector, fixture.binding.target().pin(), fixture.route).await,
        Err(PairingFailure::RouteChanged)
    ));
    let record = fixture.revocation();
    let transaction = ClientJournalTransaction {
        expected_revision: journal.recover().await.unwrap().revision(),
        changes: vec![ClientJournalChange::BeginRevocation(Box::new(
            record.clone(),
        ))],
    };
    assert!(matches!(
        journal.commit(&transaction).await.unwrap(),
        spaceterm_client::journal::JournalCommitDisposition::Durable(_)
    ));
    assert!(matches!(
        RestrictedConnector::open(&connector, &record.task).await,
        Err(PairingFailure::RouteChanged)
    ));
    assert!(matches!(
        RestrictedConnector::open(&connector, &Fixture::new(2).revocation().task).await,
        Err(PairingFailure::IdentityMismatch)
    ));
}

#[tokio::test]
async fn captured_ssh_binding_survives_journal_reopen_and_rejects_redirected_retry() {
    use spaceterm_client::{
        journal::JournalCommitDisposition, pairing::revocation::RestrictedConnector,
    };
    use spaceterm_client_native::{
        restricted_routes::NativePairingConnector,
        route::{NativeRouteConnector, SshRouteConfiguration, TunnelEndpoints},
    };
    use spaceterm_host::ipc::PrivateIpcDirectory;
    use std::os::unix::fs::PermissionsExt;
    let directory = Directory::new();
    let runtime = tempfile::tempdir().unwrap();
    std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let runtime_path = runtime.path().canonicalize().unwrap();
    let configuration = runtime_path.join("ssh-config");
    let original = "Host original\n HostName 127.0.0.1\n User owner\n";
    std::fs::write(&configuration, original).unwrap();
    let ssh = || {
        SshRouteConfiguration::new(
            "/usr/bin/ssh".into(),
            configuration.clone(),
            runtime_path.clone(),
            vec![("PATH".into(), "/usr/bin:/bin".into())],
            TunnelEndpoints::new(PrivateIpcDirectory::open(&runtime_path).unwrap()).unwrap(),
            None,
        )
        .unwrap()
    };
    let fixture = fixture_on_route(
        RouteDescription::new(RouteParts::Ssh {
            destination: "original".into(),
            remote_socket: "/server".into(),
        })
        .unwrap(),
    );
    let journal = directory.create().await;
    let mut preparation = fixture.seed_changes();
    preparation.pop(); // Capture before the exact binding becomes ordinarily usable.
    let tx = ClientJournalTransaction {
        expected_revision: journal.recover().await.unwrap().revision(),
        changes: preparation,
    };
    assert!(matches!(
        journal.commit(&tx).await.unwrap(),
        JournalCommitDisposition::Durable(_)
    ));
    let owner = controller(Arc::new(journal.clone())).await;
    let connector = NativePairingConnector::new(
        owner.clone(),
        NativeRouteConnector::new(vec![], Some(ssh())).unwrap(),
    );
    connector
        .capture_route(fixture.credential.scope.reference())
        .await
        .unwrap();
    assert!(
        owner
            .snapshot()
            .unwrap()
            .pairing()
            .slot(fixture.credential.scope.reference())
            .unwrap()
            .ssh_capture
            .is_some()
    );
    owner
        .transact(vec![ClientJournalChange::SaveBinding(Box::new(
            fixture.saved.clone(),
        ))])
        .await
        .unwrap();
    drop(connector);
    drop(owner);
    drop(journal);
    let journal = NativeClientJournal::open_existing(
        directory.0.clone(),
        Arc::new(CanonicalCodec),
        Arc::new(Verifier),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    let state = journal.recover().await.unwrap();
    let slot = state
        .pairing()
        .slot(fixture.credential.scope.reference())
        .unwrap();
    assert!(slot.route.is_none());
    assert!(
        slot.ssh_capture
            .as_ref()
            .unwrap()
            .configuration()
            .contains("hostname 127.0.0.1")
    );
    let record = fixture.revocation();
    let tx = ClientJournalTransaction {
        expected_revision: state.revision(),
        changes: vec![ClientJournalChange::BeginRevocation(Box::new(
            record.clone(),
        ))],
    };
    assert!(matches!(
        journal.commit(&tx).await.unwrap(),
        JournalCommitDisposition::Durable(_)
    ));
    std::fs::write(&configuration, original.replace("127.0.0.1", "127.0.0.2")).unwrap();
    let connector = NativePairingConnector::new(
        Arc::new(journal.clone()),
        NativeRouteConnector::new(vec![], Some(ssh())).unwrap(),
    );
    assert!(matches!(
        RestrictedConnector::open(&connector, &record.task).await,
        Err(PairingFailure::RouteChanged)
    ));
    assert!(matches!(
        connector
            .capture_route(fixture.credential.scope.reference())
            .await,
        Err(PairingFailure::RouteChanged)
    ));
    let state = journal.recover().await.unwrap();
    assert_eq!(state.revocations().len(), 1);
    assert!(state.bindings().is_empty());
    let operation = record.task.parts().original.operation();
    let tx = ClientJournalTransaction {
        expected_revision: state.revision(),
        changes: vec![
            ClientJournalChange::ConfirmRevocation {
                operation,
                receipt: Box::new(fixture.receipt(&record.task)),
            },
            ClientJournalChange::ConfirmRevocationAcknowledgement(operation),
            ClientJournalChange::FinishCredentialCleanup(operation),
        ],
    };
    assert!(matches!(
        journal.commit(&tx).await.unwrap(),
        JournalCommitDisposition::Durable(_)
    ));
    let state = journal.recover().await.unwrap();
    assert!(state.revocations().is_empty());
    assert!(
        state
            .pairing()
            .slot(fixture.credential.scope.reference())
            .unwrap()
            .ssh_capture
            .is_none()
    );
    assert!(state.bindings().is_empty());
}

#[tokio::test]
async fn restricted_ipc_rejects_impostor_pin_before_sending_pairing_proof() {
    use spaceterm_client::{
        journal::JournalCommitDisposition, pairing::revocation::RestrictedConnector,
    };
    use spaceterm_client_native::{
        restricted_routes::NativePairingConnector,
        route::{LocalRouteAuthority, NativeRouteConnector},
    };
    use spaceterm_host::{
        ipc::{IpcListener, PrivateIpcDirectory},
        transport::{AdmissionLimits, MessageLimit, MessageTransport},
    };
    use spaceterm_protocol::authentication::{
        ClientHandshake, HandshakeConfig, PairingLookup, PairingRecord, Principal, Purpose,
        ServerHandshake,
    };
    use std::{os::unix::fs::PermissionsExt, time::Duration};
    for major in [1, 2] {
        let directory = Directory::new();
        let sockets = tempfile::tempdir().unwrap();
        std::fs::set_permissions(sockets.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let endpoint = PrivateIpcDirectory::open(&sockets.path().canonicalize().unwrap())
            .unwrap()
            .endpoint("server")
            .unwrap();
        let authority = LocalRouteAuthority::new(endpoint.clone()).unwrap();
        let fixture = fixture_on_route(authority.description().clone());
        let journal = directory.create().await;
        fixture.seed(&journal).await;
        let record = fixture.revocation();
        let tx = ClientJournalTransaction {
            expected_revision: journal.recover().await.unwrap().revision(),
            changes: vec![ClientJournalChange::BeginRevocation(Box::new(
                record.clone(),
            ))],
        };
        assert!(matches!(
            journal.commit(&tx).await.unwrap(),
            JournalCommitDisposition::Durable(_)
        ));
        let original = &record.task.parts().original;
        let local = |value: u64| {
            let mut bytes = [0; 16];
            bytes[8..].copy_from_slice(&value.to_be_bytes());
            bytes
        };
        let pin = fixture.binding.target().pin();
        let principal = Principal {
            server: pin.identity,
            device: original.device().bytes(),
            pairing: local(original.pairing().local()),
            client: local(original.client().local()),
        };
        struct Lookup(Principal);
        impl PairingLookup for Lookup {
            fn lookup(
                &self,
                server: [u8; 16],
                pairing: [u8; 16],
                client: [u8; 16],
            ) -> Result<PairingRecord, ProtocolError> {
                assert_eq!(
                    (server, pairing, client),
                    (self.0.server, self.0.pairing, self.0.client)
                );
                Ok(PairingRecord {
                    principal: self.0,
                    device_public_key: Key::new(2).public_key(),
                    active: false,
                })
            }
        }
        let listener = IpcListener::bind(
            endpoint,
            MessageLimit::envelope(),
            AdmissionLimits::new(2, Duration::from_secs(10)).unwrap(),
        )
        .unwrap();
        let worker = tokio::spawn(async move {
            let pending = listener.accept().await.unwrap();
            let hello = pending.recv().await.unwrap().unwrap();
            let (_, reply) = ServerHandshake::accept(
                pin.identity,
                1,
                HandshakeConfig {
                    majors: vec![major],
                    ..HandshakeConfig::default()
                },
                Arc::new(Key::new(9)),
                &Lookup(principal),
                &hello,
            )
            .unwrap();
            pending.send(&reply).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_secs(5), pending.recv())
                    .await
                    .unwrap()
                    .unwrap()
                    .is_none(),
                "pairing proof reached an impostor"
            );
        });
        let connector = NativePairingConnector::new(
            Arc::new(journal.clone()),
            NativeRouteConnector::new(vec![authority], None).unwrap(),
        );
        let mut transport = RestrictedConnector::open(&connector, &record.task)
            .await
            .unwrap();
        let (handshake, hello) = ClientHandshake::begin(
            pin,
            principal,
            Purpose::RevocationStatus,
            HandshakeConfig {
                majors: vec![major],
                ..HandshakeConfig::default()
            },
            Arc::new(Key::new(2)),
        )
        .unwrap();
        transport.send(&hello).await.unwrap();
        assert!(matches!(
            handshake.receive_server(&transport.receive().await.unwrap()),
            Err(ProtocolError::Authentication)
        ));
        drop(transport);
        worker.await.unwrap();
        assert_eq!(journal.recover().await.unwrap().revocations().len(), 1);
        assert!(journal.recover().await.unwrap().bindings().is_empty());
    }
}

#[tokio::test]
async fn restricted_wss_pin_mismatch_preserves_pending_task_without_protocol_bytes() {
    use spaceterm_client::{
        journal::JournalCommitDisposition, pairing::revocation::RestrictedConnector,
    };
    use spaceterm_client_native::{
        restricted_routes::NativePairingConnector, route::NativeRouteConnector,
    };
    use spaceterm_host::{
        account_service::test_support::AccountServiceFixture,
        server_keys::ServerKeyStore,
        startup::{BuildIdentity, DesiredServerState},
        transport::{AdmissionLimits, MessageLimit},
        wss::{RemoteAccessEnablement, WssListener, server_tls_config},
    };
    use std::{
        net::{Ipv4Addr, SocketAddr},
        time::Duration,
    };
    let host_root = tempfile::tempdir().unwrap();
    let host = AccountServiceFixture::new(host_root.path(), BuildIdentity::Development).unwrap();
    let registration = host.initialize(DesiredServerState::Running).unwrap();
    let identity = ServerKeyStore::open(&registration)
        .unwrap()
        .load_identity(registration.server_pin())
        .unwrap();
    let enabled = RemoteAccessEnablement::from_durable_setting(
        registration.server_pin().identity,
        vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 0))],
    )
    .unwrap();
    let listener = WssListener::bind(
        Some(&enabled),
        server_tls_config(identity).unwrap(),
        MessageLimit::envelope(),
        AdmissionLimits::new(2, Duration::from_secs(2)).unwrap(),
    )
    .await
    .unwrap();
    let address = listener.local_addresses().await.unwrap()[0];
    // The saved journal deliberately pins a different key from this listener.
    // All saved request identities still match their original journal capture.
    let fixture = fixture_on_route(
        RouteDescription::new(RouteParts::Wss {
            address,
            server_name: "localhost".into(),
        })
        .unwrap(),
    );
    let directory = Directory::new();
    let journal = directory.create().await;
    fixture.seed(&journal).await;
    let record = fixture.revocation();
    let tx = ClientJournalTransaction {
        expected_revision: journal.recover().await.unwrap().revision(),
        changes: vec![ClientJournalChange::BeginRevocation(Box::new(
            record.clone(),
        ))],
    };
    assert!(matches!(
        journal.commit(&tx).await.unwrap(),
        JournalCommitDisposition::Durable(_)
    ));
    let connector = NativePairingConnector::new(
        Arc::new(journal.clone()),
        NativeRouteConnector::new(vec![], None).unwrap(),
    );
    let (client, server) = tokio::join!(
        RestrictedConnector::open(&connector, &record.task),
        listener.accept()
    );
    assert!(matches!(client, Err(PairingFailure::AuthorizationDenied)));
    assert!(
        server.is_err(),
        "an unpinned TLS peer received protocol reachability"
    );
    let state = journal.recover().await.unwrap();
    assert!(state.bindings().is_empty());
    assert_eq!(state.revocations()[0].task, record.task);
    listener.disable().await;
}

#[tokio::test]
async fn locked_pairing_credential_never_opens_a_native_restricted_route() {
    use spaceterm_client_native::{
        restricted_routes::NativePairingConnector,
        route::{LocalRouteAuthority, NativeRouteConnector},
    };
    use spaceterm_host::{
        ipc::{IpcListener, PrivateIpcDirectory},
        transport::{AdmissionLimits, MessageLimit},
    };
    use std::{os::unix::fs::PermissionsExt, time::Duration};
    struct Locked;
    impl NativeDeviceCredentials for Locked {
        fn create(
            &self,
            _: CredentialCreationReservation,
        ) -> PairingFuture<'_, Result<PreparedCredential, CredentialFailure>> {
            Box::pin(async { panic!("restricted retry cannot create credentials") })
        }
        fn recover_preparation<'a>(
            &'a self,
            _: &'a CredentialScope,
        ) -> PairingFuture<'a, Result<Option<PreparedCredential>, CredentialFailure>> {
            Box::pin(async { panic!("restricted retry cannot repair credentials") })
        }
        fn signing_key<'a>(
            &'a self,
            _: &'a PreparedCredential,
            _: &'a CredentialUse,
        ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, CredentialFailure>> {
            Box::pin(async { Err(PairingFailure::CredentialUnavailable) })
        }
        fn delete_pairing_key<'a>(
            &'a self,
            _: &'a PreparedCredential,
        ) -> PairingFuture<'a, Result<CredentialDeletion, CredentialFailure>> {
            Box::pin(async { panic!("locked unconfirmed retry cannot delete credentials") })
        }
    }
    let directory = Directory::new();
    let sockets = tempfile::tempdir().unwrap();
    std::fs::set_permissions(sockets.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let endpoint = PrivateIpcDirectory::open(&sockets.path().canonicalize().unwrap())
        .unwrap()
        .endpoint("server")
        .unwrap();
    let authority = LocalRouteAuthority::new(endpoint.clone()).unwrap();
    let fixture = fixture_on_route(authority.description().clone());
    let journal = Arc::new(directory.create().await);
    fixture.seed(journal.as_ref()).await;
    let controller = controller(journal).await;
    let store = JournalPairingStore::new(controller.clone());
    let operation = fixture
        .remove(JournalPairingStore::new(controller.clone()))
        .await;
    let connector = NativePairingConnector::new(
        controller.clone(),
        NativeRouteConnector::new(vec![authority], None).unwrap(),
    );
    let driver = spaceterm_client::pairing::revocation::RevocationDriver::new(
        Locked,
        connector,
        Arc::new(CanonicalCodec),
        spaceterm_protocol::authentication::HandshakeConfig::default(),
    );
    let listener = IpcListener::bind(
        endpoint,
        MessageLimit::envelope(),
        AdmissionLimits::new(2, Duration::from_secs(10)).unwrap(),
    )
    .unwrap();
    assert_eq!(
        driver.run_once(&store, operation).await,
        Err(PairingFailure::CredentialUnavailable)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
    let state = controller.snapshot().unwrap();
    assert!(state.bindings().is_empty());
    assert_eq!(state.revocations().len(), 1);
    assert!(matches!(
        state.revocations()[0].phase,
        spaceterm_client::journal::RevocationPhase::AwaitingConfirmation
    ));
}
