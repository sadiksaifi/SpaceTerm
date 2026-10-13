#[path = "operation_recovery/support.rs"]
mod support;
use spaceterm_client::{
    failure::{ClientFailure, ClientFailureClass},
    journal::*,
    operations::OperationTracker,
    pending::*,
    ports::ClientFuture,
};
use spaceterm_core::{
    authority::{
        ControlGeneration, ControlGrant, GrantedControl, OrganizationRevision, ServerEpoch,
        StoreRevision,
    },
    control::CompareAcquireRequest,
    identity::PaneId,
    operations::*,
    pairing::*,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use support::*;

#[derive(Default)]
struct UseGate {
    attachment: Mutex<Option<spaceterm_client::view::WorkspaceAttachment>>,
    grants: Mutex<Vec<GrantedControl>>,
    invalidated: AtomicUsize,
    removed: AtomicUsize,
}
impl LocalUseInvalidator for UseGate {
    fn invalidate_workspace(
        &self,
        _: &LocalPairingKey,
        _: spaceterm_core::identity::WorkspaceId,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        self.invalidated.fetch_add(1, Ordering::SeqCst);
        Ok(self.grants.lock().unwrap().clone())
    }
    fn invalidate_attachment(
        &self,
        pairing: &LocalPairingKey,
        attachment: spaceterm_client::view::WorkspaceAttachment,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        let mut current = self.attachment.lock().unwrap();
        if current.is_some_and(|current| current != attachment) {
            return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
        }
        *current = None;
        self.invalidate_workspace(pairing, attachment.workspace())
    }
    fn invalidate_pairing(&self, _: &LocalPairingKey) {
        self.invalidated.fetch_add(1, Ordering::SeqCst);
    }
    fn remove_pairing<'a>(&'a self, _: &'a LocalPairingKey) -> ClientFuture<'a, ()> {
        Box::pin(async {
            self.removed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
fn grant(generation: u64, epoch: u64) -> GrantedControl {
    GrantedControl::new(
        ServerEpoch::new(epoch),
        ControlGrant::new(
            PaneId::new(server(), 1).unwrap(),
            spaceterm_core::identity::ClientId::new(server(), 1).unwrap(),
            ControlGeneration::new(generation),
        )
        .unwrap(),
    )
    .unwrap()
}
fn revocation(f: &Fixture) -> PendingRevocationTask {
    let state = f.journal.snapshot().unwrap();
    let saved = &state.bindings()[0];
    let original = PendingSelfRevocation::new(PendingParts {
        original_server: server(),
        original_pairing: f.binding.target().pairing(),
        device: f.binding.target().device(),
        client: f.binding.target().client(),
        operation: OperationId::new(
            saved.streams().parts().self_revocation,
            OperationSequence::new(1).unwrap(),
        ),
        route: RevocationRouteReference::new([9; 16]),
        credential: f.binding.target().credential(),
        notice_dismissed: false,
    })
    .unwrap();
    let command = Command::Pairing(PairingCommand::SelfRevoke {
        target: PairingTarget::new(PairingTargetParts {
            server: original.server(),
            pairing: original.pairing(),
            device: original.device(),
        })
        .unwrap(),
    });
    let digest = spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::new(
        original.operation(),
        command,
        f.codec.as_ref(),
    )
    .unwrap()
    .binding()
    .digest;
    PendingRevocationTask::new(PendingRevocationTaskParts {
        original,
        local_pairing: f.binding.target().local_pairing().unwrap(),
        pinned_server_public_key: f.binding.target().pin().public_key,
        request_digest: digest,
    })
    .unwrap()
}
fn receipt(task: &PendingRevocationTask) -> SignedPairingReceipt {
    let original = task.parts().original;
    SignedPairingReceipt::new(SignedPairingReceiptParts {
        schema: 1,
        claims: PairingReceiptBody::Revocation {
            receipt: RevocationReceipt {
                server: original.server(),
                pairing: original.pairing(),
                device: original.device(),
                operation: original.operation(),
                revision: StoreRevision::new(9),
                receipt_digest: RequestDigest::from_bytes([10; 32]),
            },
            authentication_key_digest: RequestDigest::from_bytes([11; 32]),
            revocation_event: [12; 16],
        },
        canonical_body: vec![13],
        signature: vec![1; 64],
    })
    .unwrap()
}
struct Driver {
    proof: SignedPairingReceipt,
    resolved: AtomicUsize,
    cleaned: Mutex<Vec<JournalRevocation>>,
    pending: bool,
}
impl RevocationDriver for Driver {
    fn resolve<'a>(&'a self, _: &'a JournalRevocation) -> ClientFuture<'a, RevocationResolution> {
        Box::pin(async {
            self.resolved.fetch_add(1, Ordering::SeqCst);
            if self.pending {
                Ok(RevocationResolution::Pending(
                    RevocationFailure::Unreachable,
                ))
            } else {
                Ok(RevocationResolution::Confirmed(Box::new(
                    self.proof.clone(),
                )))
            }
        })
    }
    fn acknowledge<'a>(&'a self, _: &'a JournalRevocation) -> ClientFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn cleanup<'a>(
        &'a self,
        task: &'a JournalRevocation,
    ) -> ClientFuture<'a, CredentialCleanupDisposition> {
        Box::pin(async move {
            self.cleaned.lock().unwrap().push(task.clone());
            Ok(CredentialCleanupDisposition::Removed)
        })
    }
}

#[tokio::test]
async fn offline_detach_revokes_lease_before_network_release() {
    let f = Fixture::new().await;
    let gate = Arc::new(UseGate::default());
    gate.grants.lock().unwrap().push(grant(1, 4));
    let tasks = PendingTasks::new(f.journal.clone(), gate.clone());
    let receipt = tasks
        .detach_workspace(&f.binding, workspace())
        .await
        .unwrap();
    assert_eq!(gate.invalidated.load(Ordering::SeqCst), 1);
    assert_eq!(receipt.workspace, workspace());
    assert!(f.peer.sent.lock().unwrap().is_empty());
    assert_eq!(f.storage.state().detaches()[0].grants[0], grant(1, 4));
}

#[tokio::test]
async fn detach_persistence_failure_keeps_local_use_blocked() {
    let f = Fixture::new().await;
    let gate = Arc::new(UseGate::default());
    let tasks = PendingTasks::new(f.journal.clone(), gate.clone());
    f.storage.fail_next(JournalCommitDisposition::NotApplied(
        JournalFailure::Unavailable,
    ));
    assert!(
        tasks
            .detach_workspace(&f.binding, workspace())
            .await
            .is_err()
    );
    assert_eq!(gate.invalidated.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn detach_resolves_inflight_acquire_and_releases_its_original_epoch() {
    let f = Fixture::new().await;
    let tracker = OperationTracker::new(f.journal.clone());
    let operation = tracker
        .submit(
            &f.binding,
            Command::CompareAcquire(CompareAcquireRequest {
                epoch: ServerEpoch::new(4),
                pane: grant(1, 4).grant().pane(),
                client: f.binding.target().client(),
                expected_generation: ControlGeneration::new(0),
            }),
            Some(workspace()),
            &f.peer,
        )
        .await
        .unwrap();
    let tasks = PendingTasks::new(f.journal.clone(), Arc::new(UseGate::default()));
    tasks
        .detach_workspace(&f.binding, workspace())
        .await
        .unwrap();
    f.peer.results.lock().unwrap().push((
        operation.id(),
        OperationLookup::Completed {
            result: Outcome::ControlGranted(grant(1, 4)),
            publication_revision: OrganizationRevision::new(2),
        },
    ));
    assert!(
        !tasks
            .drain_detach(&f.binding, workspace(), &f.peer)
            .await
            .unwrap()
    );
    let sent = f.peer.sent.lock().unwrap();
    assert!(
        matches!(sent.last().unwrap().command(), Command::ReleaseControl(release) if release.epoch == ServerEpoch::new(4) && release.grant == grant(1,4).grant())
    );
}

#[tokio::test]
async fn detach_release_ack_loss_reuses_original_operation() {
    let f = Fixture::new().await;
    let gate = Arc::new(UseGate::default());
    gate.grants.lock().unwrap().push(grant(1, 4));
    let tasks = PendingTasks::new(f.journal.clone(), gate);
    tasks
        .detach_workspace(&f.binding, workspace())
        .await
        .unwrap();
    assert!(
        !tasks
            .drain_detach(&f.binding, workspace(), &f.peer)
            .await
            .unwrap()
    );
    let release = f.peer.sent.lock().unwrap()[0].binding();
    f.peer
        .results
        .lock()
        .unwrap()
        .push((release.id, OperationLookup::NeverAdmitted));
    let reopened = f.reopen().await;
    let tasks = PendingTasks::new(reopened, Arc::new(UseGate::default()));
    assert!(
        !tasks
            .drain_detach(&f.binding, workspace(), &f.peer)
            .await
            .unwrap()
    );
    assert_eq!(f.peer.sent.lock().unwrap()[1].binding(), release);
}

#[tokio::test]
async fn reattach_does_not_restore_detached_grants() {
    let f = Fixture::new().await;
    let gate = Arc::new(UseGate::default());
    gate.grants.lock().unwrap().push(grant(1, 4));
    let tasks = PendingTasks::new(f.journal.clone(), gate);
    tasks
        .detach_workspace(&f.binding, workspace())
        .await
        .unwrap();
    let tracker = OperationTracker::new(f.journal.clone());
    assert!(
        tracker
            .submit(
                &f.binding,
                Command::TakeOver(spaceterm_core::control::TakeOverRequest {
                    epoch: ServerEpoch::new(4),
                    pane: grant(1, 4).grant().pane(),
                    client: f.binding.target().client()
                }),
                Some(workspace()),
                &f.peer
            )
            .await
            .is_err()
    );
    assert_eq!(
        f.journal.snapshot().unwrap().suppressed_workspaces().len(),
        1
    );
}

#[tokio::test]
async fn self_unpair_persists_task_before_local_removal() {
    let f = Fixture::new().await;
    let gate = Arc::new(UseGate::default());
    let tasks = PendingTasks::new(f.journal.clone(), gate.clone());
    let task = revocation(&f);
    tasks
        .record_self_revocation(&f.binding, task.clone())
        .await
        .unwrap();
    let durable = f.storage.state();
    assert_eq!(durable.revocations()[0].task, task);
    assert!(!durable.ordinary_use_allowed(&f.binding));
    assert_eq!(gate.removed.load(Ordering::SeqCst), 1);
    assert_eq!(durable.tombstones().len(), 1);
}

#[tokio::test]
async fn indeterminate_self_unpair_never_reports_local_removal_success() {
    let f = Fixture::new().await;
    let gate = Arc::new(UseGate::default());
    let tasks = PendingTasks::new(f.journal.clone(), gate.clone());
    let task = revocation(&f);
    f.storage.fail_next(JournalCommitDisposition::Indeterminate);
    assert_eq!(
        tasks
            .record_self_revocation(&f.binding, task)
            .await
            .unwrap_err()
            .class(),
        ClientFailureClass::RecoveryRequired
    );
    assert_eq!(gate.removed.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn dismissal_hides_notice_without_cancelling_retry() {
    let f = Fixture::new().await;
    let task = revocation(&f);
    let tasks = PendingTasks::new(f.journal.clone(), Arc::new(UseGate::default()));
    tasks
        .record_self_revocation(&f.binding, task.clone())
        .await
        .unwrap();
    tasks
        .dismiss_revocation(task.parts().original.operation())
        .await
        .unwrap();
    let driver = Driver {
        proof: receipt(&task),
        resolved: AtomicUsize::new(0),
        cleaned: Mutex::new(vec![]),
        pending: true,
    };
    assert!(
        !tasks
            .retry_revocation(task.parts().original.operation(), &driver)
            .await
            .unwrap()
    );
    assert_eq!(driver.resolved.load(Ordering::SeqCst), 1);
    assert!(f.journal.snapshot().unwrap().revocations()[0].notice_dismissed);
}

#[tokio::test]
async fn old_revocation_completion_preserves_new_pairing() {
    let f = Fixture::new().await;
    let task = revocation(&f);
    let tasks = PendingTasks::new(f.journal.clone(), Arc::new(UseGate::default()));
    tasks
        .record_self_revocation(&f.binding, task.clone())
        .await
        .unwrap();
    let new_binding = binding(2);
    f.journal
        .transact(vec![ClientJournalChange::SaveBinding(Box::new(saved(
            new_binding.clone(),
        )))])
        .await
        .unwrap();
    let driver = Driver {
        proof: receipt(&task),
        resolved: AtomicUsize::new(0),
        cleaned: Mutex::new(vec![]),
        pending: false,
    };
    assert!(
        tasks
            .retry_revocation(task.parts().original.operation(), &driver)
            .await
            .unwrap()
    );
    assert!(
        f.journal
            .snapshot()
            .unwrap()
            .ordinary_use_allowed(&new_binding)
    );
    assert_eq!(
        driver.cleaned.lock().unwrap()[0].task.parts().local_pairing,
        task.parts().local_pairing
    );
}

#[tokio::test]
async fn ordinary_acknowledgement_cannot_use_restricted_self_revocation_stream() {
    let f = Fixture::new().await;
    let task = revocation(&f);
    let id = task.parts().original.operation();
    let tasks = PendingTasks::new(f.journal.clone(), Arc::new(UseGate::default()));
    tasks
        .record_self_revocation(&f.binding, task.clone())
        .await
        .unwrap();
    f.journal
        .transact(vec![ClientJournalChange::ConfirmRevocation {
            operation: id,
            receipt: Box::new(receipt(&task)),
        }])
        .await
        .unwrap();
    let tracker = OperationTracker::new(f.journal.clone());
    assert_eq!(
        tracker
            .acknowledge(id.stream(), &f.peer)
            .await
            .unwrap_err()
            .class(),
        ClientFailureClass::AuthorizationDenied
    );
    assert!(f.peer.acks.lock().unwrap().is_empty());
    assert_eq!(
        f.journal
            .snapshot()
            .unwrap()
            .stream(id.stream())
            .unwrap()
            .confirmed_prefix,
        0
    );
}

#[tokio::test]
async fn ordinary_acknowledgement_cannot_use_removed_original_pairing() {
    let f = Fixture::new().await;
    let tracker = OperationTracker::new(f.journal.clone());
    let operation = tracker
        .submit(&f.binding, rename(1), Some(workspace()), &f.peer)
        .await
        .unwrap();
    f.peer.fail_ack.store(true, Ordering::SeqCst);
    assert!(
        tracker
            .resolve(
                operation.id(),
                OperationLookup::Completed {
                    result: Outcome::Applied,
                    publication_revision: OrganizationRevision::new(2),
                },
                &f.peer,
            )
            .await
            .is_err()
    );
    let task = revocation(&f);
    let tasks = PendingTasks::new(f.journal.clone(), Arc::new(UseGate::default()));
    tasks
        .record_self_revocation(&f.binding, task)
        .await
        .unwrap();
    assert_eq!(
        tracker
            .acknowledge(operation.id().stream(), &f.peer)
            .await
            .unwrap_err()
            .class(),
        ClientFailureClass::AuthorizationDenied
    );
    assert!(f.peer.acks.lock().unwrap().is_empty());
}

#[tokio::test]
async fn receipt_commit_crash_resumes_only_exact_credential_cleanup() {
    let f = Fixture::new().await;
    let task = revocation(&f);
    let tasks = PendingTasks::new(f.journal.clone(), Arc::new(UseGate::default()));
    tasks
        .record_self_revocation(&f.binding, task.clone())
        .await
        .unwrap();
    f.journal
        .transact(vec![ClientJournalChange::ConfirmRevocation {
            operation: task.parts().original.operation(),
            receipt: Box::new(receipt(&task)),
        }])
        .await
        .unwrap();
    let reopened = f.reopen().await;
    let tasks = PendingTasks::new(reopened, Arc::new(UseGate::default()));
    let driver = Driver {
        proof: receipt(&task),
        resolved: AtomicUsize::new(0),
        cleaned: Mutex::new(vec![]),
        pending: false,
    };
    assert!(
        tasks
            .retry_revocation(task.parts().original.operation(), &driver)
            .await
            .unwrap()
    );
    assert_eq!(driver.resolved.load(Ordering::SeqCst), 0);
    assert_eq!(driver.cleaned.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn detach_resolves_inflight_create_and_releases_its_exact_grant() {
    let f = Fixture::new().await;
    let tracker = OperationTracker::new(f.journal.clone());
    let operation = tracker
        .submit(
            &f.binding,
            Command::CreateWorkspace {
                server: server(),
                source: WorkspaceCreationSource::ServerHome { server: server() },
                name: spaceterm_core::organization::NameDraft::Automatic,
            },
            None,
            &f.peer,
        )
        .await
        .unwrap();
    let tasks = PendingTasks::new(f.journal.clone(), Arc::new(UseGate::default()));
    tasks
        .detach_workspace(&f.binding, workspace())
        .await
        .unwrap();
    assert!(
        !tasks
            .drain_detach(&f.binding, workspace(), &f.peer)
            .await
            .unwrap()
    );
    f.peer.results.lock().unwrap().push((
        operation.id(),
        OperationLookup::Completed {
            result: Outcome::CreatedWithControl(
                CreatedOrganization::new(
                    CreatedEntities::Workspace {
                        workspace: workspace(),
                        tab: spaceterm_core::identity::TabId::new(server(), 1).unwrap(),
                        pane: grant(1, 4).grant().pane(),
                    },
                    grant(1, 4),
                )
                .unwrap(),
            ),
            publication_revision: OrganizationRevision::new(2),
        },
    ));
    assert!(
        !tasks
            .drain_detach(&f.binding, workspace(), &f.peer)
            .await
            .unwrap()
    );
    assert!(
        matches!(f.peer.sent.lock().unwrap().last().unwrap().command(), Command::ReleaseControl(release) if release.epoch == ServerEpoch::new(4) && release.grant == grant(1,4).grant())
    );
}

#[tokio::test]
async fn malformed_control_outcome_cannot_redirect_deferred_release() {
    let f = Fixture::new().await;
    let tracker = OperationTracker::new(f.journal.clone());
    let operation = tracker
        .submit(
            &f.binding,
            Command::CompareAcquire(CompareAcquireRequest {
                epoch: ServerEpoch::new(4),
                pane: grant(1, 4).grant().pane(),
                client: f.binding.target().client(),
                expected_generation: ControlGeneration::new(0),
            }),
            Some(workspace()),
            &f.peer,
        )
        .await
        .unwrap();
    let result = tracker
        .resolve(
            operation.id(),
            OperationLookup::Completed {
                result: Outcome::ControlGranted(grant(1, 5)),
                publication_revision: OrganizationRevision::new(2),
            },
            &f.peer,
        )
        .await;
    assert!(result.is_err());
    assert!(f.peer.acks.lock().unwrap().is_empty());
}

#[tokio::test]
async fn ordinary_journal_saturation_preserves_cleanup_reservations() {
    let f = Fixture::new().await;
    let limits = JournalLimits {
        operations_per_stream: 1,
        ..Default::default()
    };
    let journal = Arc::new(
        JournalController::recover(
            f.storage.clone(),
            f.codec.clone(),
            Arc::new(Verifier),
            limits,
        )
        .await
        .unwrap(),
    );
    let tracker = OperationTracker::new(journal.clone());
    tracker
        .submit(&f.binding, rename(1), Some(workspace()), &f.peer)
        .await
        .unwrap();
    assert!(
        tracker
            .submit(&f.binding, rename(1), Some(workspace()), &f.peer)
            .await
            .is_err()
    );
    let gate = Arc::new(UseGate::default());
    gate.grants.lock().unwrap().push(grant(1, 4));
    let tasks = PendingTasks::new(journal.clone(), gate);
    tasks
        .detach_workspace(&f.binding, workspace())
        .await
        .unwrap();
    assert!(
        !tasks
            .drain_detach(&f.binding, workspace(), &f.peer)
            .await
            .unwrap()
    );
    assert!(matches!(
        f.peer.sent.lock().unwrap().last().unwrap().command(),
        Command::ReleaseControl(_)
    ));
    let task = revocation(&f);
    tasks
        .record_self_revocation(&f.binding, task)
        .await
        .unwrap();
    assert!(!journal.snapshot().unwrap().ordinary_use_allowed(&f.binding));
}

#[tokio::test]
async fn retained_byte_saturation_transfers_reserved_capacity_to_exact_release() {
    let f = Fixture::new().await;
    let limits = JournalLimits {
        cleanup_grants: 1,
        retained_bytes: 98_000,
        ..Default::default()
    };
    let journal = Arc::new(
        JournalController::recover(
            f.storage.clone(),
            f.codec.clone(),
            Arc::new(Verifier),
            limits,
        )
        .await
        .unwrap(),
    );
    let tracker = OperationTracker::new(journal.clone());
    let ordinary = tracker
        .submit(&f.binding, rename(1), Some(workspace()), &f.peer)
        .await
        .unwrap();
    tracker
        .resolve(
            ordinary.id(),
            OperationLookup::Completed {
                result: Outcome::Applied,
                publication_revision: OrganizationRevision::new(2),
            },
            &f.peer,
        )
        .await
        .unwrap();
    assert_eq!(
        tracker
            .submit(&f.binding, rename(2), Some(workspace()), &f.peer)
            .await
            .unwrap_err()
            .class(),
        ClientFailureClass::ResourceLimit
    );
    let gate = Arc::new(UseGate::default());
    gate.grants.lock().unwrap().push(grant(1, 4));
    let tasks = PendingTasks::new(journal, gate);
    tasks
        .detach_workspace(&f.binding, workspace())
        .await
        .unwrap();
    assert!(
        !tasks
            .drain_detach(&f.binding, workspace(), &f.peer)
            .await
            .unwrap()
    );
    let release = f.peer.sent.lock().unwrap().last().unwrap().clone();
    assert!(matches!(
        release.command(),
        Command::ReleaseControl(request)
            if request.epoch == ServerEpoch::new(4) && request.grant == grant(1, 4).grant()
    ));
    let recovered = JournalController::recover(
        f.storage.clone(),
        f.codec.clone(),
        Arc::new(Verifier),
        limits,
    )
    .await
    .unwrap();
    let retained = recovered.snapshot().unwrap();
    assert_eq!(retained.detaches()[0].releases, vec![release.binding()]);
    assert!(matches!(
        retained.operation(ordinary.id()).unwrap().state,
        RetainedOperationState::Completed {
            consumed: false,
            ..
        }
    ));
    let tasks = PendingTasks::new(Arc::new(recovered), Arc::new(UseGate::default()));
    f.peer.results.lock().unwrap().push((
        release.binding().id,
        OperationLookup::Completed {
            result: Outcome::Applied,
            publication_revision: OrganizationRevision::new(3),
        },
    ));
    assert!(
        tasks
            .drain_detach(&f.binding, workspace(), &f.peer)
            .await
            .unwrap()
    );
    assert!(f.storage.state().detaches().is_empty());
    assert!(f.storage.state().operation(ordinary.id()).is_some());
}

#[tokio::test]
async fn completed_acquire_detach_metadata_transfers_reserved_bytes_at_saturation() {
    reserved_marker_detach(false).await;
}

#[tokio::test]
async fn incarnation_detach_uses_the_reserved_marker_at_ordinary_saturation() {
    reserved_marker_detach(true).await;
}

async fn reserved_marker_detach(incarnation: bool) {
    let f = Fixture::new().await;
    let limits = JournalLimits {
        cleanup_grants: 1,
        retained_bytes: 97_400,
        ..Default::default()
    };
    let journal = Arc::new(
        JournalController::recover(
            f.storage.clone(),
            f.codec.clone(),
            Arc::new(Verifier),
            limits,
        )
        .await
        .unwrap(),
    );
    let attachment = incarnation.then(|| {
        spaceterm_client::view::WorkspaceAttachment::new(
            workspace(),
            f.binding.target().pairing(),
            f.binding.target().client(),
            spaceterm_client::view::AttachmentIncarnation::new(1).unwrap(),
        )
        .unwrap()
    });
    if let Some(attachment) = attachment {
        journal
            .record_attachment(f.binding.target().local_pairing().unwrap(), attachment)
            .await
            .unwrap();
    }
    let tracker = OperationTracker::new(journal.clone());
    let acquisition = tracker
        .submit(
            &f.binding,
            Command::CompareAcquire(CompareAcquireRequest {
                epoch: ServerEpoch::new(4),
                pane: grant(1, 4).grant().pane(),
                client: f.binding.target().client(),
                expected_generation: ControlGeneration::new(0),
            }),
            Some(workspace()),
            &f.peer,
        )
        .await
        .unwrap();
    tracker
        .resolve(
            acquisition.id(),
            OperationLookup::Completed {
                result: Outcome::ControlGranted(grant(1, 4)),
                publication_revision: OrganizationRevision::new(2),
            },
            &f.peer,
        )
        .await
        .unwrap();
    let gate = Arc::new(UseGate::default());
    let tasks = PendingTasks::new(journal, gate.clone());
    let removal = match attachment {
        Some(attachment) => tasks.detach_attachment(&f.binding, attachment).await,
        None => tasks.detach_workspace(&f.binding, workspace()).await,
    };
    removal.expect("admitted grant must retain its detach marker without new byte capacity");
    assert_eq!(gate.invalidated.load(Ordering::SeqCst), 1);

    let recovered = Arc::new(
        JournalController::recover(
            f.storage.clone(),
            f.codec.clone(),
            Arc::new(Verifier),
            limits,
        )
        .await
        .unwrap(),
    );
    let state = recovered.snapshot().unwrap();
    assert_eq!(state.detaches()[0].grants, vec![grant(1, 4)]);
    assert_reserved_marker(&state, attachment);
    assert!(matches!(
        state.operation(acquisition.id()).unwrap().state,
        RetainedOperationState::Completed {
            consumed: false,
            ..
        }
    ));
    let tasks = PendingTasks::new(recovered.clone(), Arc::new(UseGate::default()));
    assert!(
        !tasks
            .drain_detach(&f.binding, workspace(), &f.peer)
            .await
            .unwrap()
    );
    let release = f.peer.sent.lock().unwrap().last().unwrap().clone();
    assert!(matches!(
        release.command(),
        Command::ReleaseControl(request)
            if request.epoch == ServerEpoch::new(4) && request.grant == grant(1, 4).grant()
    ));
    f.peer.results.lock().unwrap().push((
        release.binding().id,
        OperationLookup::Completed {
            result: Outcome::Applied,
            publication_revision: OrganizationRevision::new(3),
        },
    ));
    assert!(
        tasks
            .drain_detach(&f.binding, workspace(), &f.peer)
            .await
            .unwrap()
    );
    OperationTracker::new(recovered.clone())
        .confirm_acknowledgement(release.binding().id.stream(), 2)
        .await
        .unwrap();
    let state = recovered.snapshot().unwrap();
    assert!(state.detaches().is_empty());
    assert!(state.operation(release.binding().id).is_none());
    assert_reserved_marker(&state, attachment);
    assert!(matches!(
        state.operation(acquisition.id()).unwrap().state,
        RetainedOperationState::Completed {
            consumed: false,
            ..
        }
    ));
}

fn assert_reserved_marker(
    state: &RecoveredClientState,
    attachment: Option<spaceterm_client::view::WorkspaceAttachment>,
) {
    use spaceterm_client::view::AttachmentSuppression;
    if let Some(attachment) = attachment {
        assert!(state.suppressed_workspaces().is_empty());
        assert!(state.suppresses(&attachment));
    } else {
        assert_eq!(state.suppressed_workspaces().len(), 1);
        assert_eq!(state.suppressed_workspaces()[0].workspace, workspace());
    }
}

#[tokio::test]
async fn settled_detach_marker_cannot_spend_a_different_workspaces_reservation() {
    let f = Fixture::new().await;
    let journal = Arc::new(
        JournalController::recover(
            f.storage.clone(),
            f.codec.clone(),
            Arc::new(Verifier),
            JournalLimits {
                cleanup_grants: 1,
                retained_bytes: 97_400,
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    let tasks = PendingTasks::new(journal.clone(), Arc::new(UseGate::default()));
    tasks
        .detach_workspace(&f.binding, workspace())
        .await
        .unwrap();
    assert!(
        tasks
            .drain_detach(&f.binding, workspace(), &f.peer)
            .await
            .unwrap()
    );
    assert_eq!(journal.snapshot().unwrap().suppressed_workspaces().len(), 1);
    let tracker = OperationTracker::new(journal);
    for (command, target) in [
        (
            Command::CompareAcquire(CompareAcquireRequest {
                epoch: ServerEpoch::new(4),
                pane: PaneId::new(server(), 2).unwrap(),
                client: f.binding.target().client(),
                expected_generation: ControlGeneration::new(0),
            }),
            Some(spaceterm_core::identity::WorkspaceId::new(server(), 2).unwrap()),
        ),
        (
            Command::CreateWorkspace {
                server: server(),
                source: WorkspaceCreationSource::ServerHome { server: server() },
                name: spaceterm_core::organization::NameDraft::Automatic,
            },
            None,
        ),
    ] {
        assert_eq!(
            tracker
                .submit(&f.binding, command, target, &f.peer)
                .await
                .unwrap_err()
                .class(),
            ClientFailureClass::ResourceLimit
        );
    }
    assert!(f.peer.sent.lock().unwrap().is_empty());
    assert!(f.storage.state().operations().is_empty());
    // A new attachment to the same Workspace can reuse its retained marker slot.
    tracker
        .submit(
            &f.binding,
            Command::CompareAcquire(CompareAcquireRequest {
                epoch: ServerEpoch::new(4),
                pane: grant(1, 4).grant().pane(),
                client: f.binding.target().client(),
                expected_generation: ControlGeneration::new(0),
            }),
            Some(workspace()),
            &f.peer,
        )
        .await
        .unwrap();
    assert_eq!(f.peer.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn grant_admission_reserves_release_request_and_bookkeeping_before_sending() {
    let f = Fixture::new().await;
    let journal = Arc::new(
        JournalController::recover(
            f.storage.clone(),
            f.codec.clone(),
            Arc::new(Verifier),
            JournalLimits {
                cleanup_grants: 1,
                retained_bytes: 97_100,
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    let tracker = OperationTracker::new(journal);
    assert_eq!(
        tracker
            .submit(
                &f.binding,
                Command::CompareAcquire(CompareAcquireRequest {
                    epoch: ServerEpoch::new(4),
                    pane: grant(1, 4).grant().pane(),
                    client: f.binding.target().client(),
                    expected_generation: ControlGeneration::new(0),
                }),
                Some(workspace()),
                &f.peer,
            )
            .await
            .unwrap_err()
            .class(),
        ClientFailureClass::ResourceLimit
    );
    assert!(f.peer.sent.lock().unwrap().is_empty());
    assert!(f.storage.state().operations().is_empty());
}

#[tokio::test]
async fn retained_release_capacity_cannot_be_borrowed_by_a_later_grant() {
    let f = Fixture::new().await;
    let journal = Arc::new(
        JournalController::recover(
            f.storage.clone(),
            f.codec.clone(),
            Arc::new(Verifier),
            JournalLimits {
                cleanup_grants: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    let gate = Arc::new(UseGate::default());
    gate.grants.lock().unwrap().push(grant(1, 4));
    let tasks = PendingTasks::new(journal.clone(), gate);
    tasks
        .detach_workspace(&f.binding, workspace())
        .await
        .unwrap();
    tasks
        .drain_detach(&f.binding, workspace(), &f.peer)
        .await
        .unwrap();
    let release = f.peer.sent.lock().unwrap()[0].binding();
    f.peer.results.lock().unwrap().push((
        release.id,
        OperationLookup::Completed {
            result: Outcome::Applied,
            publication_revision: OrganizationRevision::new(2),
        },
    ));
    assert!(
        tasks
            .drain_detach(&f.binding, workspace(), &f.peer)
            .await
            .unwrap()
    );
    let command = Command::CompareAcquire(CompareAcquireRequest {
        epoch: ServerEpoch::new(4),
        pane: PaneId::new(server(), 2).unwrap(),
        client: f.binding.target().client(),
        expected_generation: ControlGeneration::new(0),
    });
    let tracker = OperationTracker::new(journal.clone());
    assert_eq!(
        tracker
            .submit(&f.binding, command.clone(), Some(workspace()), &f.peer)
            .await
            .unwrap_err()
            .class(),
        ClientFailureClass::ResourceLimit
    );
    assert_eq!(f.peer.sent.lock().unwrap().len(), 1);
    tracker
        .confirm_acknowledgement(release.id.stream(), 1)
        .await
        .unwrap();
    assert!(journal.snapshot().unwrap().operation(release.id).is_none());
    tracker
        .submit(&f.binding, command, Some(workspace()), &f.peer)
        .await
        .unwrap();
    assert_eq!(f.peer.sent.lock().unwrap().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn pending_revocation_scheduler_retries_dismissed_task_with_bounded_backoff() {
    let f = Fixture::new().await;
    let task = revocation(&f);
    let tasks = PendingTasks::new(f.journal.clone(), Arc::new(UseGate::default()));
    tasks
        .record_self_revocation(&f.binding, task.clone())
        .await
        .unwrap();
    tasks
        .dismiss_revocation(task.parts().original.operation())
        .await
        .unwrap();
    let driver = Arc::new(Driver {
        proof: receipt(&task),
        resolved: AtomicUsize::new(0),
        cleaned: Mutex::new(vec![]),
        pending: true,
    });
    let runtime = tasks.start_scheduler(driver.clone()).unwrap();
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert_eq!(driver.resolved.load(Ordering::SeqCst), 1);
    tokio::time::advance(std::time::Duration::from_millis(200)).await;
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert_eq!(driver.resolved.load(Ordering::SeqCst), 1);
    tokio::time::advance(std::time::Duration::from_millis(800)).await;
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert_eq!(driver.resolved.load(Ordering::SeqCst), 2);
    assert!(tasks.start_scheduler(driver).is_err());
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn deferred_release_does_not_release_later_self_grant() {
    let f = Fixture::new().await;
    let gate = Arc::new(UseGate::default());
    gate.grants.lock().unwrap().push(grant(1, 4));
    let tasks = PendingTasks::new(f.journal.clone(), gate);
    tasks
        .detach_workspace(&f.binding, workspace())
        .await
        .unwrap();
    tasks
        .drain_detach(&f.binding, workspace(), &f.peer)
        .await
        .unwrap();
    let release = f.peer.sent.lock().unwrap().last().unwrap().binding();
    f.peer.results.lock().unwrap().push((
        release.id,
        OperationLookup::Completed {
            result: Outcome::Failed(OperationFailure::StaleAuthority),
            publication_revision: OrganizationRevision::new(9),
        },
    ));
    assert!(
        tasks
            .drain_detach(&f.binding, workspace(), &f.peer)
            .await
            .unwrap()
    );
    assert_eq!(f.peer.sent.lock().unwrap().len(), 1);
    assert!(
        matches!(f.peer.sent.lock().unwrap()[0].command(),Command::ReleaseControl(request) if request.grant == grant(1,4).grant())
    );
}

#[tokio::test]
async fn pending_revocation_cannot_be_bypassed_by_new_client_of_same_pairing() {
    use spaceterm_client::api::{ConnectionTarget, PairedServerBinding};
    let f = Fixture::new().await;
    let task = revocation(&f);
    let tasks = PendingTasks::new(f.journal.clone(), Arc::new(UseGate::default()));
    tasks
        .record_self_revocation(&f.binding, task)
        .await
        .unwrap();
    let old = f.binding.target();
    let replacement = PairedServerBinding::new(
        ConnectionTarget::new(
            old.pin(),
            old.device(),
            old.pairing(),
            spaceterm_core::identity::ClientId::new(server(), 2).unwrap(),
            old.route().clone(),
            CredentialReference::new([19; 16]),
            LocalPairingGeneration::new(2).unwrap(),
        )
        .unwrap(),
    );
    assert!(
        f.journal
            .transact(vec![ClientJournalChange::SaveBinding(Box::new(saved(
                replacement
            )))])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn completed_acquire_detaches_with_one_cleanup_reservation_and_unconsumed_result() {
    let f = Fixture::new().await;
    let limits = JournalLimits {
        cleanup_grants: 1,
        ..Default::default()
    };
    let journal = Arc::new(
        JournalController::recover(
            f.storage.clone(),
            f.codec.clone(),
            Arc::new(Verifier),
            limits,
        )
        .await
        .unwrap(),
    );
    let tracker = OperationTracker::new(journal.clone());
    let operation = tracker
        .submit(
            &f.binding,
            Command::CompareAcquire(CompareAcquireRequest {
                epoch: ServerEpoch::new(4),
                pane: grant(1, 4).grant().pane(),
                client: f.binding.target().client(),
                expected_generation: ControlGeneration::new(0),
            }),
            Some(workspace()),
            &f.peer,
        )
        .await
        .unwrap();
    tracker
        .resolve(
            operation.id(),
            OperationLookup::Completed {
                result: Outcome::ControlGranted(grant(1, 4)),
                publication_revision: OrganizationRevision::new(2),
            },
            &f.peer,
        )
        .await
        .unwrap();
    let tasks = PendingTasks::new(journal, Arc::new(UseGate::default()));
    tasks
        .detach_workspace(&f.binding, workspace())
        .await
        .unwrap();

    let recovered = JournalController::recover(
        f.storage.clone(),
        f.codec.clone(),
        Arc::new(Verifier),
        limits,
    )
    .await
    .unwrap()
    .snapshot()
    .unwrap();
    assert_eq!(recovered.detaches()[0].grants, vec![grant(1, 4)]);
    assert!(recovered.detaches()[0].dependencies.is_empty());
    assert!(matches!(
        recovered.operation(operation.id()).unwrap().state,
        RetainedOperationState::Completed {
            consumed: false,
            ..
        }
    ));
}

#[tokio::test]
async fn acknowledging_grant_result_does_not_spend_its_cleanup_reservation() {
    let f = Fixture::new().await;
    let journal = Arc::new(
        JournalController::recover(
            f.storage.clone(),
            f.codec.clone(),
            Arc::new(Verifier),
            JournalLimits {
                cleanup_grants: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    let tracker = OperationTracker::new(journal.clone());
    let first = tracker
        .submit(
            &f.binding,
            Command::CompareAcquire(CompareAcquireRequest {
                epoch: ServerEpoch::new(4),
                pane: grant(1, 4).grant().pane(),
                client: f.binding.target().client(),
                expected_generation: ControlGeneration::new(0),
            }),
            Some(workspace()),
            &f.peer,
        )
        .await
        .unwrap();
    tracker
        .resolve(
            first.id(),
            OperationLookup::Completed {
                result: Outcome::ControlGranted(grant(1, 4)),
                publication_revision: OrganizationRevision::new(2),
            },
            &f.peer,
        )
        .await
        .unwrap();
    tracker
        .confirm_acknowledgement(first.id().stream(), 1)
        .await
        .unwrap();
    journal
        .transact(vec![ClientJournalChange::ConsumeResult(first.id())])
        .await
        .unwrap();
    assert!(journal.snapshot().unwrap().operations().is_empty());
    let second = tracker
        .submit(
            &f.binding,
            Command::CompareAcquire(CompareAcquireRequest {
                epoch: ServerEpoch::new(4),
                pane: PaneId::new(server(), 2).unwrap(),
                client: f.binding.target().client(),
                expected_generation: ControlGeneration::new(0),
            }),
            Some(workspace()),
            &f.peer,
        )
        .await;
    assert!(second.is_err());
    assert_eq!(f.peer.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn detach_settlement_preserves_later_grant_cleanup_reservation() {
    let f = Fixture::new().await;
    let key = f.binding.target().local_pairing().unwrap();
    f.journal
        .transact(vec![ClientJournalChange::ReconcileGrants {
            local_pairing: key.clone(),
            epoch: ServerEpoch::new(4),
            revision: OrganizationRevision::new(1),
            grants: vec![(workspace(), grant(1, 4))],
        }])
        .await
        .unwrap();
    let tasks = PendingTasks::new(f.journal.clone(), Arc::new(UseGate::default()));
    tasks
        .detach_workspace(&f.binding, workspace())
        .await
        .unwrap();
    tasks
        .drain_detach(&f.binding, workspace(), &f.peer)
        .await
        .unwrap();
    let release = f.peer.sent.lock().unwrap()[0].binding();
    f.journal
        .transact(vec![ClientJournalChange::ReconcileGrants {
            local_pairing: key,
            epoch: ServerEpoch::new(4),
            revision: OrganizationRevision::new(9),
            grants: vec![(workspace(), grant(2, 4))],
        }])
        .await
        .unwrap();
    f.peer.results.lock().unwrap().push((
        release.id,
        OperationLookup::Completed {
            result: Outcome::Failed(OperationFailure::StaleAuthority),
            publication_revision: OrganizationRevision::new(10),
        },
    ));
    assert!(
        tasks
            .drain_detach(&f.binding, workspace(), &f.peer)
            .await
            .unwrap()
    );
    assert_eq!(
        f.journal.snapshot().unwrap().grants()[0].control,
        grant(2, 4)
    );
}

#[tokio::test]
async fn unverified_revocation_receipt_cannot_start_native_credential_cleanup() {
    let f = Fixture::new().await;
    let task = revocation(&f);
    let tasks = PendingTasks::new(f.journal.clone(), Arc::new(UseGate::default()));
    tasks
        .record_self_revocation(&f.binding, task.clone())
        .await
        .unwrap();
    let mut parts = receipt(&task).parts().clone();
    parts.signature = vec![2; 64];
    let driver = Driver {
        proof: SignedPairingReceipt::new(parts).unwrap(),
        resolved: AtomicUsize::new(0),
        cleaned: Mutex::new(vec![]),
        pending: false,
    };
    assert!(
        tasks
            .retry_revocation(task.parts().original.operation(), &driver)
            .await
            .is_err()
    );
    assert!(driver.cleaned.lock().unwrap().is_empty());
    assert!(matches!(
        f.journal.snapshot().unwrap().revocations()[0].phase,
        RevocationPhase::AwaitingConfirmation
    ));
}

#[tokio::test]
async fn detach_transaction_cannot_omit_a_retained_original_grant() {
    let f = Fixture::new().await;
    let key = f.binding.target().local_pairing().unwrap();
    f.journal
        .transact(vec![ClientJournalChange::ReconcileGrants {
            local_pairing: key.clone(),
            epoch: ServerEpoch::new(4),
            revision: OrganizationRevision::new(1),
            grants: vec![(workspace(), grant(1, 4))],
        }])
        .await
        .unwrap();
    assert!(
        f.journal
            .transact(vec![ClientJournalChange::BeginDetach(DetachTask {
                local_pairing: key,
                workspace: workspace(),
                grants: vec![],
                dependencies: vec![],
                releases: vec![],
            })])
            .await
            .is_err()
    );
    assert!(f.journal.snapshot().unwrap().detaches().is_empty());
}

#[tokio::test]
async fn detach_cleanup_identity_cannot_target_a_later_grant() {
    let f = Fixture::new().await;
    let key = f.binding.target().local_pairing().unwrap();
    let gate = Arc::new(UseGate::default());
    gate.grants.lock().unwrap().push(grant(1, 4));
    PendingTasks::new(f.journal.clone(), gate)
        .detach_workspace(&f.binding, workspace())
        .await
        .unwrap();
    let stream = f.journal.snapshot().unwrap().bindings()[0]
        .streams()
        .parts()
        .ordinary;
    let request = spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::new(
        OperationId::new(stream, OperationSequence::new(1).unwrap()),
        Command::ReleaseControl(spaceterm_core::control::ReleaseControlRequest {
            epoch: ServerEpoch::new(4),
            grant: grant(2, 4).grant(),
        }),
        f.codec.as_ref(),
    )
    .unwrap();
    assert!(
        f.journal
            .transact(vec![
                ClientJournalChange::Prepare(JournalOperation {
                    binding: request.binding(),
                    request: request.encode(f.codec.as_ref()).unwrap(),
                    workspace: Some(workspace()),
                    reserves_grant: false,
                    cleanup: true,
                    state: RetainedOperationState::Pending,
                }),
                ClientJournalChange::AddDetachRelease {
                    local_pairing: key,
                    workspace: workspace(),
                    operation: request.binding(),
                },
            ])
            .await
            .is_err()
    );
    assert!(f.journal.snapshot().unwrap().operations().is_empty());
    assert!(f.peer.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn delayed_detach_of_old_incarnation_cannot_capture_reattached_control() {
    use spaceterm_client::view::{AttachmentIncarnation, WorkspaceAttachment};
    let f = Fixture::new().await;
    let gate = Arc::new(UseGate::default());
    let tasks = PendingTasks::new(f.journal.clone(), gate.clone());
    let attachment = |incarnation| {
        WorkspaceAttachment::new(
            workspace(),
            f.binding.target().pairing(),
            f.binding.target().client(),
            AttachmentIncarnation::new(incarnation).unwrap(),
        )
        .unwrap()
    };
    let old = attachment(7);
    *gate.attachment.lock().unwrap() = Some(old);
    f.journal
        .record_attachment(f.binding.target().local_pairing().unwrap(), old)
        .await
        .unwrap();
    tasks.detach_attachment(&f.binding, old).await.unwrap();
    assert!(
        tasks
            .drain_detach(&f.binding, workspace(), &f.peer)
            .await
            .unwrap()
    );
    let current = attachment(8);
    *gate.attachment.lock().unwrap() = Some(current);
    f.journal
        .record_attachment(f.binding.target().local_pairing().unwrap(), current)
        .await
        .unwrap();
    gate.grants.lock().unwrap().push(grant(2, 4));
    let revision = f.journal.snapshot().unwrap().revision();
    let invalidations = gate.invalidated.load(Ordering::SeqCst);
    assert!(tasks.detach_attachment(&f.binding, old).await.is_err());
    assert_eq!(gate.invalidated.load(Ordering::SeqCst), invalidations);
    assert_eq!(f.journal.snapshot().unwrap().revision(), revision);
    assert!(f.journal.snapshot().unwrap().detaches().is_empty());
    assert!(f.peer.sent.lock().unwrap().is_empty());
}
