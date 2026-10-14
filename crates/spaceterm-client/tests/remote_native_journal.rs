#[path = "operation_recovery/support.rs"]
mod support;

use spaceterm_client::{
    api::PairedServerBinding, failure::ClientFailureClass, journal::*,
    pairing::credentials::PairingAuthorityFence, ports::ClientFuture,
};
use spaceterm_core::{
    identity::ClientId,
    operation_ledger::canonical::CanonicalCodec,
    pairing::{LocalPairingGeneration, LocalPairingKey, LocalPairingKeyParts},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use support::{MemoryJournal, Verifier, binding, saved};
use tokio::sync::Notify;

const REQUEST: [u8; 16] = [9; 16];
const SCOPE: &[u8] = b"original-public-stop-scope-and-capture";
const INTENT: &[u8] = b"original-public-stop-action";
const ASSESSMENT: &[u8] = b"original-public-assessment-expires-at-17";
const RESULT: &[u8] = b"original-public-terminal-progress";

struct Fixture {
    journal: JournalController,
    storage: Arc<MemoryJournal>,
    binding: PairedServerBinding,
}
impl Fixture {
    async fn new(limits: JournalLimits) -> Self {
        let storage = Arc::new(MemoryJournal::new(Arc::new(CanonicalCodec)));
        let journal = reopen(storage.clone(), limits).await;
        let binding = binding(1);
        journal
            .transact(vec![ClientJournalChange::SaveBinding(Box::new(saved(
                binding.clone(),
            )))])
            .await
            .unwrap();
        Self {
            journal,
            storage,
            binding,
        }
    }
    fn pairing(&self) -> LocalPairingKey {
        self.binding.target().local_pairing().unwrap()
    }
    fn client(&self) -> ClientId {
        self.binding.target().client()
    }
    fn original(&self, request: [u8; 16]) -> RemoteNativeRequest {
        RemoteNativeRequest::new(self.pairing(), self.client(), request, SCOPE.to_vec()).unwrap()
    }
    async fn assess(&self, request: [u8; 16]) {
        self.journal
            .retain_remote_native_assessment(
                self.pairing(),
                self.client(),
                request,
                SCOPE.to_vec(),
                ASSESSMENT.to_vec(),
            )
            .await
            .unwrap();
    }
    async fn submit(&self, request: [u8; 16]) {
        self.journal
            .mark_remote_native_submitted(
                self.pairing(),
                self.client(),
                request,
                SCOPE.to_vec(),
                Some(ASSESSMENT.to_vec()),
            )
            .await
            .unwrap();
    }
}
async fn reopen(storage: Arc<dyn ClientJournal>, limits: JournalLimits) -> JournalController {
    JournalController::recover(
        storage,
        Arc::new(CanonicalCodec),
        Arc::new(Verifier),
        limits,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn original_scope_and_lost_assessment_reply_recover_without_refresh() {
    let fixture = Fixture::new(JournalLimits::default()).await;
    fixture
        .journal
        .retain_remote_native_request(fixture.original(REQUEST))
        .await
        .unwrap();
    fixture
        .journal
        .retain_remote_native_intent(
            fixture.pairing(),
            fixture.client(),
            REQUEST,
            SCOPE.to_vec(),
            INTENT.to_vec(),
        )
        .await
        .unwrap();
    // The caller loses the assessment reply after its admitted journal write.
    fixture.assess(REQUEST).await;
    let recovered = reopen(fixture.storage.clone(), JournalLimits::default()).await;
    recovered
        .retain_remote_native_request(fixture.original(REQUEST))
        .await
        .unwrap();
    let records = recovered
        .recover_remote_native_requests(&fixture.pairing(), fixture.client())
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].request_id(), REQUEST);
    assert_eq!(records[0].original_scope(), SCOPE);
    assert_eq!(records[0].intent(), Some(INTENT));
    assert_eq!(records[0].assessment(), Some(ASSESSMENT));
    assert!(!records[0].submitted());
    assert!(records[0].result().is_none());
}

#[tokio::test]
async fn original_intent_is_durable_before_capture_and_accepts_only_exact_replay() {
    let fixture = Fixture::new(JournalLimits::default()).await;
    fixture
        .journal
        .retain_remote_native_request(fixture.original(REQUEST))
        .await
        .unwrap();
    let receipt = fixture
        .journal
        .retain_remote_native_intent(
            fixture.pairing(),
            fixture.client(),
            REQUEST,
            SCOPE.to_vec(),
            INTENT.to_vec(),
        )
        .await
        .unwrap();
    // The native caller has not reached Capture. Its durable journal already
    // contains the original selected action for this exact guest-issued ID.
    let state = fixture.storage.state();
    assert_eq!(state.revision(), receipt.revision());
    assert_eq!(state.remote_native_requests()[0].intent(), Some(INTENT));
    fixture.assess(REQUEST).await;
    fixture.submit(REQUEST).await;
    fixture
        .journal
        .retain_remote_native_result(
            fixture.pairing(),
            fixture.client(),
            REQUEST,
            SCOPE.to_vec(),
            RESULT.to_vec(),
        )
        .await
        .unwrap();
    let journal = reopen(fixture.storage.clone(), JournalLimits::default()).await;
    journal
        .retain_remote_native_intent(
            fixture.pairing(),
            fixture.client(),
            REQUEST,
            SCOPE.to_vec(),
            INTENT.to_vec(),
        )
        .await
        .unwrap();
    let revision = fixture.storage.state().revision();
    for (scope, intent) in [
        (SCOPE.to_vec(), b"different-public-action".to_vec()),
        (b"different-public-scope".to_vec(), INTENT.to_vec()),
        (SCOPE.to_vec(), Vec::new()),
        (SCOPE.to_vec(), vec![1; MAX_REMOTE_NATIVE_BYTES + 1]),
    ] {
        assert!(
            journal
                .retain_remote_native_intent(
                    fixture.pairing(),
                    fixture.client(),
                    REQUEST,
                    scope,
                    intent,
                )
                .await
                .is_err()
        );
    }
    assert_eq!(fixture.storage.state().revision(), revision);
    let records = journal
        .recover_remote_native_requests(&fixture.pairing(), fixture.client())
        .unwrap();
    assert_eq!(records[0].original_scope(), SCOPE);
    assert_eq!(records[0].intent(), Some(INTENT));
    assert_eq!(records[0].assessment(), Some(ASSESSMENT));
    assert_eq!(records[0].result(), Some(RESULT));
}

#[tokio::test]
async fn first_intent_cannot_be_added_after_original_assessment_submission_or_result() {
    for phase in 0..3 {
        let fixture = Fixture::new(JournalLimits::default()).await;
        fixture
            .journal
            .retain_remote_native_request(fixture.original(REQUEST))
            .await
            .unwrap();
        match phase {
            0 => fixture.assess(REQUEST).await,
            1 => {
                fixture
                    .journal
                    .mark_remote_native_submitted(
                        fixture.pairing(),
                        fixture.client(),
                        REQUEST,
                        SCOPE.to_vec(),
                        None,
                    )
                    .await
                    .unwrap();
            }
            _ => {
                fixture
                    .journal
                    .retain_remote_native_result(
                        fixture.pairing(),
                        fixture.client(),
                        REQUEST,
                        SCOPE.to_vec(),
                        RESULT.to_vec(),
                    )
                    .await
                    .unwrap();
            }
        }
        let revision = fixture.storage.state().revision();
        assert!(
            fixture
                .journal
                .retain_remote_native_intent(
                    fixture.pairing(),
                    fixture.client(),
                    REQUEST,
                    SCOPE.to_vec(),
                    INTENT.to_vec(),
                )
                .await
                .is_err()
        );
        let records = fixture
            .journal
            .recover_remote_native_requests(&fixture.pairing(), fixture.client())
            .unwrap();
        assert_eq!(records[0].intent(), None);
        assert_eq!(records[0].original_scope(), SCOPE);
        assert_eq!(fixture.storage.state().revision(), revision);
    }
}

#[tokio::test]
async fn retained_scope_assessment_submission_and_result_cannot_be_replaced() {
    let fixture = Fixture::new(JournalLimits::default()).await;
    fixture
        .journal
        .retain_remote_native_request(fixture.original(REQUEST))
        .await
        .unwrap();
    fixture.assess(REQUEST).await;
    fixture.submit(REQUEST).await;
    fixture
        .journal
        .retain_remote_native_result(
            fixture.pairing(),
            fixture.client(),
            REQUEST,
            SCOPE.to_vec(),
            RESULT.to_vec(),
        )
        .await
        .unwrap();
    let revision = fixture.storage.state().revision();
    let altered = RemoteNativeRequest::new(
        fixture.pairing(),
        fixture.client(),
        REQUEST,
        b"different-public-scope".to_vec(),
    )
    .unwrap();
    assert!(
        fixture
            .journal
            .retain_remote_native_request(altered)
            .await
            .is_err()
    );
    assert!(
        fixture
            .journal
            .retain_remote_native_assessment(
                fixture.pairing(),
                fixture.client(),
                REQUEST,
                SCOPE.to_vec(),
                b"fresh-assessment".to_vec(),
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .journal
            .mark_remote_native_submitted(
                fixture.pairing(),
                fixture.client(),
                REQUEST,
                SCOPE.to_vec(),
                Some(b"fresh-assessment".to_vec()),
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .journal
            .retain_remote_native_result(
                fixture.pairing(),
                fixture.client(),
                REQUEST,
                SCOPE.to_vec(),
                b"different-result".to_vec(),
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .journal
            .retire_remote_native_request(
                fixture.pairing(),
                fixture.client(),
                REQUEST,
                b"different-public-scope".to_vec(),
                RESULT.to_vec(),
            )
            .await
            .is_err()
    );
    assert_eq!(fixture.storage.state().revision(), revision);
    let state = fixture.storage.state();
    let record = &state.remote_native_requests()[0];
    assert_eq!(record.original_scope(), SCOPE);
    assert_eq!(record.assessment(), Some(ASSESSMENT));
    assert_eq!(record.result(), Some(RESULT));
    fixture.journal.ensure_healthy().unwrap();
}

#[tokio::test]
async fn terminal_result_requires_a_separate_durable_commit_before_retirement() {
    let fixture = Fixture::new(JournalLimits::default()).await;
    fixture
        .journal
        .retain_remote_native_request(fixture.original(REQUEST))
        .await
        .unwrap();
    assert!(
        fixture
            .journal
            .transact(vec![
                ClientJournalChange::CompleteRemoteNativeRequest {
                    pairing: fixture.pairing(),
                    client: fixture.client(),
                    request: REQUEST,
                    original_scope: SCOPE.to_vec(),
                    result: RESULT.to_vec(),
                },
                ClientJournalChange::RetireRemoteNativeRequest {
                    pairing: fixture.pairing(),
                    client: fixture.client(),
                    request: REQUEST,
                    original_scope: SCOPE.to_vec(),
                    result: RESULT.to_vec(),
                },
            ])
            .await
            .is_err()
    );
    assert!(
        fixture.storage.state().remote_native_requests()[0]
            .result()
            .is_none()
    );
    let completed = fixture
        .journal
        .retain_remote_native_result(
            fixture.pairing(),
            fixture.client(),
            REQUEST,
            SCOPE.to_vec(),
            RESULT.to_vec(),
        )
        .await
        .unwrap();
    let recovered = reopen(fixture.storage.clone(), JournalLimits::default()).await;
    assert_eq!(
        recovered
            .recover_remote_native_requests(&fixture.pairing(), fixture.client())
            .unwrap()[0]
            .result(),
        Some(RESULT)
    );
    assert!(
        recovered
            .retire_remote_native_request(
                fixture.pairing(),
                fixture.client(),
                REQUEST,
                SCOPE.to_vec(),
                b"wrong-result".to_vec(),
            )
            .await
            .is_err()
    );
    let retired = recovered
        .retire_remote_native_request(
            fixture.pairing(),
            fixture.client(),
            REQUEST,
            SCOPE.to_vec(),
            RESULT.to_vec(),
        )
        .await
        .unwrap();
    assert!(retired.revision().get() > completed.revision().get());
    assert!(
        reopen(fixture.storage, JournalLimits::default())
            .await
            .snapshot()
            .unwrap()
            .remote_native_requests()
            .is_empty()
    );
}

#[tokio::test]
async fn simultaneous_original_requests_keep_exact_results_and_reject_capacity_without_eviction() {
    let limits = JournalLimits {
        operations_per_stream: 2,
        ..JournalLimits::default()
    };
    let fixture = Fixture::new(limits).await;
    let first = fixture.original([1; 16]);
    let second = fixture.original([2; 16]);
    let (left, right) = tokio::join!(
        fixture.journal.retain_remote_native_request(first),
        fixture.journal.retain_remote_native_request(second),
    );
    left.unwrap();
    right.unwrap();
    assert_eq!(
        fixture
            .journal
            .retain_remote_native_request(fixture.original([3; 16]))
            .await
            .unwrap_err()
            .class(),
        ClientFailureClass::ResourceLimit
    );
    let (left, right) = tokio::join!(
        fixture.journal.retain_remote_native_result(
            fixture.pairing(),
            fixture.client(),
            [1; 16],
            SCOPE.to_vec(),
            b"result-one".to_vec()
        ),
        fixture.journal.retain_remote_native_result(
            fixture.pairing(),
            fixture.client(),
            [2; 16],
            SCOPE.to_vec(),
            b"result-two".to_vec()
        ),
    );
    left.unwrap();
    right.unwrap();
    let records = fixture
        .journal
        .recover_remote_native_requests(&fixture.pairing(), fixture.client())
        .unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(
        records
            .iter()
            .find(|r| r.request_id() == [1; 16])
            .unwrap()
            .result(),
        Some(b"result-one".as_slice())
    );
    assert_eq!(
        records
            .iter()
            .find(|r| r.request_id() == [2; 16])
            .unwrap()
            .result(),
        Some(b"result-two".as_slice())
    );
    fixture.journal.ensure_healthy().unwrap();
}

#[tokio::test]
async fn independent_servers_and_clients_can_retain_the_same_guest_request_id_without_mixing() {
    use spaceterm_client::api::ConnectionTarget;
    use spaceterm_core::identity::{PairingId, ServerId};
    use spaceterm_protocol::authentication::ServerPin;
    let fixture = Fixture::new(JournalLimits::default()).await;
    let target = fixture.binding.target();
    let server = ServerId::new([42; 16]);
    let other = PairedServerBinding::new(
        ConnectionTarget::new(
            ServerPin {
                identity: server.bytes(),
                public_key: [43; 32],
            },
            target.device(),
            PairingId::new(server, 1).unwrap(),
            ClientId::new(server, 2).unwrap(),
            target.route().clone(),
            target.credential(),
            target.incarnation(),
        )
        .unwrap(),
    );
    let other_pairing = other.target().local_pairing().unwrap();
    let other_client = other.target().client();
    fixture
        .journal
        .transact(vec![ClientJournalChange::SaveBinding(Box::new(saved(
            other,
        )))])
        .await
        .unwrap();
    let (first, second) = tokio::join!(
        fixture
            .journal
            .retain_remote_native_request(fixture.original(REQUEST)),
        fixture.journal.retain_remote_native_request(
            RemoteNativeRequest::new(
                other_pairing.clone(),
                other_client,
                REQUEST,
                b"other-public-scope".to_vec()
            )
            .unwrap()
        ),
    );
    first.unwrap();
    second.unwrap();
    let (first, second) = tokio::join!(
        fixture.journal.retain_remote_native_result(
            fixture.pairing(),
            fixture.client(),
            REQUEST,
            SCOPE.to_vec(),
            RESULT.to_vec()
        ),
        fixture.journal.retain_remote_native_result(
            other_pairing.clone(),
            other_client,
            REQUEST,
            b"other-public-scope".to_vec(),
            b"other-public-result".to_vec()
        ),
    );
    first.unwrap();
    second.unwrap();
    assert_eq!(
        fixture
            .journal
            .recover_remote_native_requests(&fixture.pairing(), fixture.client())
            .unwrap()[0]
            .result(),
        Some(RESULT)
    );
    assert_eq!(
        fixture
            .journal
            .recover_remote_native_requests(&other_pairing, other_client)
            .unwrap()[0]
            .result(),
        Some(b"other-public-result".as_slice())
    );
}

#[tokio::test]
async fn future_intent_assessment_and_result_capacity_is_reserved_before_first_effect() {
    let fixture = Fixture::new(JournalLimits::default()).await;
    let baseline = fixture.storage.state();
    let mut low = 1usize;
    let mut high = JournalLimits::default().retained_bytes;
    while low < high {
        let mid = low + (high - low) / 2;
        if baseline
            .validate(
                &CanonicalCodec,
                &Verifier,
                JournalLimits {
                    retained_bytes: mid,
                    ..JournalLimits::default()
                },
            )
            .is_ok()
        {
            high = mid;
        } else {
            low = mid + 1;
        }
    }
    let constrained = reopen(
        fixture.storage.clone(),
        JournalLimits {
            // The old three-field allowance cannot admit a request with a
            // fourth future intent field, even while its original scope is small.
            retained_bytes: low + 3 * MAX_REMOTE_NATIVE_BYTES * 4 + 1024,
            ..JournalLimits::default()
        },
    )
    .await;
    assert_eq!(
        constrained
            .retain_remote_native_request(fixture.original(REQUEST))
            .await
            .unwrap_err()
            .class(),
        ClientFailureClass::ResourceLimit
    );
    assert!(fixture.storage.state().remote_native_requests().is_empty());
    constrained.ensure_healthy().unwrap();
}

#[tokio::test]
async fn wrong_pairing_client_or_incarnation_cannot_recover_or_change_an_original() {
    let fixture = Fixture::new(JournalLimits::default()).await;
    fixture
        .journal
        .retain_remote_native_request(fixture.original(REQUEST))
        .await
        .unwrap();
    let wrong_client = ClientId::new(fixture.client().server(), 2).unwrap();
    let wrong_generation = LocalPairingKey::new(LocalPairingKeyParts {
        generation: LocalPairingGeneration::new(2).unwrap(),
        ..fixture.pairing().parts().clone()
    })
    .unwrap();
    let wrong_pairing = binding(2).target().local_pairing().unwrap();
    for (pairing, client) in [
        (fixture.pairing(), wrong_client),
        (wrong_generation, fixture.client()),
        (wrong_pairing, fixture.client()),
    ] {
        assert!(
            fixture
                .journal
                .recover_remote_native_requests(&pairing, client)
                .unwrap()
                .is_empty()
        );
        assert!(
            fixture
                .journal
                .retain_remote_native_result(
                    pairing.clone(),
                    client,
                    REQUEST,
                    SCOPE.to_vec(),
                    RESULT.to_vec(),
                )
                .await
                .is_err()
        );
        assert!(
            fixture
                .journal
                .retain_remote_native_request(
                    RemoteNativeRequest::new(pairing, client, REQUEST, SCOPE.to_vec()).unwrap(),
                )
                .await
                .is_err()
        );
    }
    assert_eq!(
        fixture
            .journal
            .recover_remote_native_requests(&fixture.pairing(), fixture.client())
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn unpair_and_new_binding_preserve_original_tasks_without_adopting_them() {
    use spaceterm_core::{
        operation_ledger_contracts::CanonicalOperationRequest,
        operations::{Command, OperationId, OperationSequence},
        pairing::*,
    };
    let fixture = Fixture::new(JournalLimits::default()).await;
    fixture
        .journal
        .retain_remote_native_request(fixture.original(REQUEST))
        .await
        .unwrap();
    fixture.assess(REQUEST).await;
    let snapshot = fixture.journal.snapshot().unwrap();
    let target = fixture.binding.target();
    let original = PendingSelfRevocation::new(PendingParts {
        original_server: target.server(),
        original_pairing: target.pairing(),
        device: target.device(),
        client: target.client(),
        operation: OperationId::new(
            snapshot.bindings()[0].streams().parts().self_revocation,
            OperationSequence::new(1).unwrap(),
        ),
        route: RevocationRouteReference::new([31; 16]),
        credential: target.credential(),
        notice_dismissed: false,
    })
    .unwrap();
    let digest = CanonicalOperationRequest::new(
        original.operation(),
        Command::Pairing(PairingCommand::SelfRevoke {
            target: PairingTarget::new(PairingTargetParts {
                server: target.server(),
                pairing: target.pairing(),
                device: target.device(),
            })
            .unwrap(),
        }),
        &CanonicalCodec,
    )
    .unwrap()
    .binding()
    .digest;
    let task = PendingRevocationTask::new(PendingRevocationTaskParts {
        original,
        local_pairing: fixture.pairing(),
        pinned_server_public_key: target.pin().public_key,
        request_digest: digest,
    })
    .unwrap();
    fixture
        .journal
        .transact(vec![ClientJournalChange::BeginRevocation(Box::new(
            JournalRevocation {
                task,
                target: SavedTarget::from_target(target),
                phase: RevocationPhase::AwaitingConfirmation,
                notice_dismissed: false,
                attempts: 0,
            },
        ))])
        .await
        .unwrap();
    let next = binding(2);
    let next_pairing = next.target().local_pairing().unwrap();
    let next_client = next.target().client();
    fixture
        .journal
        .transact(vec![ClientJournalChange::SaveBinding(Box::new(saved(
            next.clone(),
        )))])
        .await
        .unwrap();
    let journal = reopen(fixture.storage.clone(), JournalLimits::default()).await;
    assert_eq!(
        journal
            .recover_remote_native_requests(&fixture.pairing(), fixture.client())
            .unwrap()[0]
            .assessment(),
        Some(ASSESSMENT)
    );
    assert!(
        journal
            .recover_remote_native_requests(&next_pairing, next_client)
            .unwrap()
            .is_empty()
    );
    assert!(!journal.ordinary_use_allowed(&fixture.binding).unwrap());
    assert!(
        journal
            .mark_remote_native_submitted(
                fixture.pairing(),
                fixture.client(),
                REQUEST,
                SCOPE.to_vec(),
                Some(ASSESSMENT.to_vec())
            )
            .await
            .is_err()
    );
    journal
        .retain_remote_native_request(
            RemoteNativeRequest::new(
                next_pairing.clone(),
                next_client,
                REQUEST,
                b"new-public-scope".to_vec(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    journal
        .retain_remote_native_result(
            fixture.pairing(),
            fixture.client(),
            REQUEST,
            SCOPE.to_vec(),
            RESULT.to_vec(),
        )
        .await
        .unwrap();
    assert!(
        journal
            .recover_remote_native_requests(&next_pairing, next_client)
            .unwrap()[0]
            .result()
            .is_none()
    );
    assert_eq!(
        journal
            .recover_remote_native_requests(&fixture.pairing(), fixture.client())
            .unwrap()[0]
            .result(),
        Some(RESULT)
    );
}

#[tokio::test]
async fn uncertain_confirmation_and_completion_recover_only_the_original_durable_facts() {
    for uncertain_completion in [false, true] {
        let fixture = Fixture::new(JournalLimits::default()).await;
        fixture
            .journal
            .retain_remote_native_request(fixture.original(REQUEST))
            .await
            .unwrap();
        fixture.assess(REQUEST).await;
        if uncertain_completion {
            fixture.submit(REQUEST).await;
        }
        fixture.storage.apply_then_indeterminate();
        let result = if uncertain_completion {
            fixture
                .journal
                .retain_remote_native_result(
                    fixture.pairing(),
                    fixture.client(),
                    REQUEST,
                    SCOPE.to_vec(),
                    RESULT.to_vec(),
                )
                .await
        } else {
            fixture
                .journal
                .mark_remote_native_submitted(
                    fixture.pairing(),
                    fixture.client(),
                    REQUEST,
                    SCOPE.to_vec(),
                    Some(ASSESSMENT.to_vec()),
                )
                .await
        };
        assert_eq!(
            result.unwrap_err().class(),
            ClientFailureClass::RecoveryRequired
        );
        assert_eq!(
            fixture
                .journal
                .ordinary_use_allowed(&fixture.binding)
                .unwrap_err()
                .class(),
            ClientFailureClass::RecoveryRequired
        );
        let reopened = reopen(fixture.storage.clone(), JournalLimits::default()).await;
        let records = reopened
            .recover_remote_native_requests(&fixture.pairing(), fixture.client())
            .unwrap();
        assert_eq!(records[0].original_scope(), SCOPE);
        assert_eq!(records[0].assessment(), Some(ASSESSMENT));
        assert!(records[0].submitted());
        assert_eq!(
            records[0].result(),
            if uncertain_completion {
                Some(RESULT)
            } else {
                None
            }
        );
    }
}

#[tokio::test]
async fn schema_one_defaults_missing_remote_records_and_bounds_decode_and_validation() {
    let fixture = Fixture::new(JournalLimits::default()).await;
    fixture
        .journal
        .retain_remote_native_request(fixture.original(REQUEST))
        .await
        .unwrap();
    let state = fixture.storage.state();
    let mut old = serde_json::to_value(&state).unwrap();
    old.as_object_mut()
        .unwrap()
        .remove("remote_native_requests");
    let decoded: RecoveredClientState = serde_json::from_value(old).unwrap();
    decoded
        .validate(&CanonicalCodec, &Verifier, JournalLimits::default())
        .unwrap();
    assert!(decoded.remote_native_requests().is_empty());
    let mut old_pending = serde_json::to_value(&state).unwrap();
    old_pending["remote_native_requests"][0]
        .as_object_mut()
        .unwrap()
        .remove("intent");
    let decoded: RecoveredClientState = serde_json::from_value(old_pending).unwrap();
    decoded
        .validate(&CanonicalCodec, &Verifier, JournalLimits::default())
        .unwrap();
    assert_eq!(decoded.remote_native_requests()[0].intent(), None);
    assert_eq!(decoded.remote_native_requests()[0].original_scope(), SCOPE);
    for field in ["original_scope", "intent", "assessment", "result"] {
        let mut oversized = serde_json::to_value(&state).unwrap();
        oversized["remote_native_requests"][0][field] =
            serde_json::json!(vec![1u8; MAX_REMOTE_NATIVE_BYTES + 1]);
        assert!(serde_json::from_value::<RecoveredClientState>(oversized).is_err());
    }
    let mut duplicate = serde_json::to_value(&state).unwrap();
    let record = duplicate["remote_native_requests"][0].clone();
    duplicate["remote_native_requests"]
        .as_array_mut()
        .unwrap()
        .push(record.clone());
    let decoded: RecoveredClientState = serde_json::from_value(duplicate).unwrap();
    assert!(
        decoded
            .validate(&CanonicalCodec, &Verifier, JournalLimits::default())
            .is_err()
    );
    let mut too_many = serde_json::to_value(&state).unwrap();
    too_many["remote_native_requests"] =
        serde_json::json!(vec![record; MAX_REMOTE_NATIVE_REQUESTS + 1]);
    assert!(serde_json::from_value::<RecoveredClientState>(too_many).is_err());
    assert!(
        RemoteNativeRequest::new(fixture.pairing(), fixture.client(), [0; 16], SCOPE.to_vec())
            .is_err()
    );
}

// Inject only the storage completion edge. Model validation, controller serial,
// durable snapshot and the synchronous native uncertainty latch remain real.
struct UncertainStorage {
    inner: Arc<MemoryJournal>,
    latch: Arc<JournalAdmission>,
    armed: AtomicBool,
    writes: AtomicUsize,
    entered: Notify,
    release: Notify,
}
impl UncertainStorage {
    fn new(inner: Arc<MemoryJournal>) -> Self {
        Self {
            inner,
            latch: Arc::default(),
            armed: AtomicBool::new(true),
            writes: AtomicUsize::new(0),
            entered: Notify::new(),
            release: Notify::new(),
        }
    }
}
impl ClientJournal for UncertainStorage {
    fn authority_fence(&self) -> &PairingAuthorityFence {
        self.inner.authority_fence()
    }
    fn admission_latch(&self) -> Arc<JournalAdmission> {
        self.latch.clone()
    }
    fn scheduler_claim(&self) -> Arc<AtomicBool> {
        self.inner.scheduler_claim()
    }
    fn recover(&self) -> ClientFuture<'_, RecoveredClientState> {
        self.inner.recover()
    }
    fn commit<'a>(
        &'a self,
        transaction: &'a ClientJournalTransaction,
    ) -> ClientFuture<'a, JournalCommitDisposition> {
        Box::pin(async move {
            self.writes.fetch_add(1, Ordering::SeqCst);
            let result = self.inner.commit(transaction).await?;
            if self.armed.swap(false, Ordering::SeqCst) {
                assert!(matches!(result, JournalCommitDisposition::Durable(_)));
                self.latch.close();
                self.entered.notify_one();
                self.release.notified().await;
                Ok(JournalCommitDisposition::Indeterminate)
            } else {
                Ok(result)
            }
        })
    }
}

#[tokio::test]
async fn uncertain_intent_closes_pre_capture_admission_before_return_and_recovers_exact_intent() {
    let fixture = Fixture::new(JournalLimits::default()).await;
    fixture
        .journal
        .retain_remote_native_request(fixture.original(REQUEST))
        .await
        .unwrap();
    let storage = Arc::new(UncertainStorage::new(fixture.storage.clone()));
    let journal = reopen(storage.clone(), JournalLimits::default()).await;
    let health = journal.health();
    let writer = journal.clone();
    let pairing = fixture.pairing();
    let client = fixture.client();
    let pending = tokio::spawn(async move {
        writer
            .retain_remote_native_intent(pairing, client, REQUEST, SCOPE.to_vec(), INTENT.to_vec())
            .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        storage.entered.notified(),
    )
    .await
    .unwrap();
    assert_eq!(*health.borrow(), JournalHealth::RecoveryRequired);
    assert_eq!(
        journal.ensure_healthy().unwrap_err().class(),
        ClientFailureClass::RecoveryRequired
    );
    assert_eq!(
        journal
            .ordinary_use_allowed(&fixture.binding)
            .unwrap_err()
            .class(),
        ClientFailureClass::RecoveryRequired
    );
    assert!(
        journal
            .retain_remote_native_intent(
                fixture.pairing(),
                fixture.client(),
                REQUEST,
                SCOPE.to_vec(),
                INTENT.to_vec(),
            )
            .await
            .is_err()
    );
    assert_eq!(storage.writes.load(Ordering::SeqCst), 1);
    storage.release.notify_one();
    assert_eq!(
        pending.await.unwrap().unwrap_err().class(),
        ClientFailureClass::RecoveryRequired
    );
    let fresh = reopen(fixture.storage.clone(), JournalLimits::default()).await;
    let records = fresh
        .recover_remote_native_requests(&fixture.pairing(), fixture.client())
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].request_id(), REQUEST);
    assert_eq!(records[0].original_scope(), SCOPE);
    assert_eq!(records[0].intent(), Some(INTENT));
    assert!(records[0].assessment().is_none());
    assert!(!records[0].submitted());
    assert!(records[0].result().is_none());
}

#[tokio::test]
async fn native_uncertainty_closes_readiness_and_ordinary_use_before_returning_and_keeps_original()
{
    let fixture = Fixture::new(JournalLimits::default()).await;
    let storage = Arc::new(UncertainStorage::new(fixture.storage.clone()));
    let journal = reopen(storage.clone(), JournalLimits::default()).await;
    let health = journal.health();
    let original = fixture.original(REQUEST);
    let writer = journal.clone();
    let pending = tokio::spawn(async move { writer.retain_remote_native_request(original).await });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        storage.entered.notified(),
    )
    .await
    .unwrap();
    assert_eq!(*health.borrow(), JournalHealth::RecoveryRequired);
    assert_eq!(
        journal.ensure_healthy().unwrap_err().class(),
        ClientFailureClass::RecoveryRequired
    );
    assert_eq!(
        journal
            .ordinary_use_allowed(&fixture.binding)
            .unwrap_err()
            .class(),
        ClientFailureClass::RecoveryRequired
    );
    assert!(
        journal
            .recover_remote_native_requests(&fixture.pairing(), fixture.client())
            .is_err()
    );
    assert!(
        journal
            .retain_remote_native_request(fixture.original(REQUEST))
            .await
            .is_err()
    );
    assert_eq!(storage.writes.load(Ordering::SeqCst), 1);
    storage.release.notify_one();
    assert_eq!(
        pending.await.unwrap().unwrap_err().class(),
        ClientFailureClass::RecoveryRequired
    );
    let fresh = reopen(fixture.storage.clone(), JournalLimits::default()).await;
    let recovered = fresh
        .recover_remote_native_requests(&fixture.pairing(), fixture.client())
        .unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].request_id(), REQUEST);
    assert_eq!(recovered[0].original_scope(), SCOPE);
}

mod historical_original {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use sha2::{Digest, Sha256};
    use spaceterm_client::{
        api::{ConnectionTarget, RouteDescription, RouteParts},
        failure::ClientFailure,
        pending::{LocalUseInvalidator, PendingTasks},
    };
    use spaceterm_core::{
        authority::{GrantedControl, StoreRevision},
        identity::{OperationStreamId, OperationStreamKey, WorkspaceId},
        operation_ledger_contracts::CanonicalOperationRequest,
        operations::*,
        pairing::*,
    };

    use spaceterm_protocol::{
        ProtocolError,
        authentication::{ServerPin, SigningKey},
        pairing_adapter::ReceiptCodec,
    };

    const CAPTURE: &str = "hostname original.example\nuser original-user\nport 22\n";
    const ROUTE: RevocationRouteReference = RevocationRouteReference::new([8; 16]);

    // Slot-backed ordinary admission also validates the pairing projection's
    // receipt crypto. The older literal receipt-port fixture is insufficient.
    // This synthetic server key never reads native credential material.
    struct TestServerKey(Ed25519KeyPair);
    impl TestServerKey {
        fn new() -> Self {
            Self(Ed25519KeyPair::from_seed_unchecked(&[19; 32]).unwrap())
        }
    }
    impl SigningKey for TestServerKey {
        fn public_key(&self) -> [u8; 32] {
            self.0.public_key().as_ref().try_into().unwrap()
        }
        fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
            Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
        }
    }
    struct SignedReceiptVerifier;
    impl ReceiptVerifier for SignedReceiptVerifier {
        fn verify(
            &self,
            pin: ServerPin,
            receipt: &SignedPairingReceipt,
        ) -> Result<(), ClientFailure> {
            ReceiptCodec::new(pin)
                .verify(receipt)
                .map(|_| ())
                .map_err(|_| ClientFailure::new(ClientFailureClass::InvalidPeer))
        }
    }
    fn sign(pin: ServerPin, claims: PairingReceiptBody) -> SignedPairingReceipt {
        ReceiptCodec::new(pin)
            .sign(claims, &TestServerKey::new())
            .unwrap()
    }
    async fn reopen(storage: Arc<dyn ClientJournal>, limits: JournalLimits) -> JournalController {
        JournalController::recover(
            storage,
            Arc::new(CanonicalCodec),
            Arc::new(SignedReceiptVerifier),
            limits,
        )
        .await
        .unwrap()
    }
    fn binding(generation: u64) -> PairedServerBinding {
        let original = super::binding(generation);
        let old = original.target();
        PairedServerBinding::new(
            ConnectionTarget::new(
                ServerPin {
                    identity: old.pin().identity,
                    public_key: TestServerKey::new().public_key(),
                },
                old.device(),
                old.pairing(),
                old.client(),
                old.route().clone(),
                old.credential(),
                old.incarnation(),
            )
            .unwrap(),
        )
    }
    fn saved(binding: PairedServerBinding) -> SavedBinding {
        let target = binding.target();
        let stream = |key| {
            OperationStreamId::new(
                OperationStreamKey::new(target.server(), key).unwrap(),
                target.pairing(),
                target.client(),
            )
            .unwrap()
        };
        let streams = PairingClientStreams::new(PairingClientStreamsParts {
            client: ClientRecord {
                id: target.client(),
                pairing: target.pairing(),
            },
            ordinary: stream(target.incarnation().get() * 2),
            self_revocation: stream(target.incarnation().get() * 2 + 1),
        })
        .unwrap();
        let proof = sign(
            target.pin(),
            PairingReceiptBody::ClientBinding {
                target: PairingTarget::new(PairingTargetParts {
                    server: target.server(),
                    pairing: target.pairing(),
                    device: target.device(),
                })
                .unwrap(),
                keys: PairingKeys::new(PairingKeysParts {
                    device_identity_public_key: [5; 32],
                    pairing_authentication_public_key: [6; 32],
                })
                .unwrap(),
                client: streams.clone(),
                operation: OperationBinding {
                    id: OperationId::new(
                        streams.parts().ordinary,
                        OperationSequence::new(1).unwrap(),
                    ),
                    digest: RequestDigest::from_bytes([7; 32]),
                },
                revision: StoreRevision::new(1),
            },
        );
        SavedBinding::verified(binding, streams, proof, &SignedReceiptVerifier).unwrap()
    }

    fn ssh_binding(
        generation: u64,
        credential: [u8; 16],
        destination: &str,
    ) -> PairedServerBinding {
        let original = binding(generation);
        let old = original.target();
        PairedServerBinding::new(
            ConnectionTarget::new(
                old.pin(),
                old.device(),
                old.pairing(),
                old.client(),
                RouteDescription::new(RouteParts::Ssh {
                    destination: destination.into(),
                    remote_socket: "/original/server.sock".into(),
                })
                .unwrap(),
                CredentialReference::new(credential),
                old.incarnation(),
            )
            .unwrap(),
        )
    }

    // These fixtures exercise the canonical model, controller and real receipt
    // codec. Storage and deletion are injected; no native credentials, SSH,
    // actors, queries or native DTO validation run here.
    struct HistoricalFixture {
        journal: Arc<JournalController>,
        storage: Arc<MemoryJournal>,
        binding: PairedServerBinding,
    }
    impl HistoricalFixture {
        async fn new(capture: bool) -> Self {
            let storage = Arc::new(MemoryJournal::with_verifier(
                Arc::new(CanonicalCodec),
                Arc::new(SignedReceiptVerifier),
            ));
            let journal = Arc::new(reopen(storage.clone(), JournalLimits::default()).await);
            let binding = ssh_binding(2, [4; 16], "original-alias");
            let slot = |device: bool| JournalCredentialSlot {
                namespace: "dev.spaceterm.desktop".into(),
                server: (!device).then_some(binding.target().server()),
                reference: CredentialReference::new([if device { 9 } else { 4 }; 16]),
                public_key: None,
                route_reference: None,
                route: None,
                ssh_capture: None,
                generation: LocalPairingGeneration::new(if device { 1 } else { 2 }).unwrap(),
            };
            let device = slot(true);
            let pairing = slot(false);
            let mut changes = vec![
                ClientJournalChange::ReserveCredential(device.clone()),
                ClientJournalChange::RecordCredentialPublicKey {
                    reference: device.reference,
                    public_key: [5; 32],
                },
                ClientJournalChange::ReserveCredential(pairing.clone()),
                ClientJournalChange::BindCredentialRoute {
                    reference: pairing.reference,
                    route_reference: ROUTE,
                    route: binding.target().route().clone(),
                },
            ];
            if capture {
                changes.push(ClientJournalChange::CaptureRestrictedSsh {
                    reference: pairing.reference,
                    capture: JournalSshCapture::new(CAPTURE.into()).unwrap(),
                });
            }
            changes.push(ClientJournalChange::RecordCredentialPublicKey {
                reference: pairing.reference,
                public_key: [6; 32],
            });
            journal.transact(changes).await.unwrap();
            let streams = saved(binding.clone()).streams().clone();
            let keys = PairingKeys::new(PairingKeysParts {
                device_identity_public_key: [5; 32],
                pairing_authentication_public_key: [6; 32],
            })
            .unwrap();
            let enrollment = OperationBinding {
                id: OperationId::new(
                    OperationStreamId::new(
                        OperationStreamKey::new(binding.target().server(), 1).unwrap(),
                        binding.target().pairing(),
                        binding.target().client(),
                    )
                    .unwrap(),
                    OperationSequence::new(1).unwrap(),
                ),
                digest: RequestDigest::from_bytes([7; 32]),
            };
            let prepared = JournalEnrollment {
                target: SavedTarget::from_target(binding.target()),
                keys: keys.clone(),
                route_reference: ROUTE,
                enrollment,
                reservation: Some(JournalEnrollmentReservation {
                    ticket: EnrollmentTicketId::new(EnrollmentTicketParts {
                        server: binding.target().server(),
                        nonce: [10; 16],
                    })
                    .unwrap(),
                    claim_digest: RequestDigest::from_bytes([11; 32]),
                }),
            };
            journal
                .transact(vec![ClientJournalChange::PrepareEnrollment(Box::new(
                    prepared.clone(),
                ))])
                .await
                .unwrap();
            let proof = sign(
                binding.target().pin(),
                PairingReceiptBody::Enrollment {
                    target: PairingTarget::new(PairingTargetParts {
                        server: binding.target().server(),
                        pairing: binding.target().pairing(),
                        device: binding.target().device(),
                    })
                    .unwrap(),
                    keys,
                    client: streams.clone(),
                    enrollment,
                    revision: StoreRevision::new(5),
                },
            );
            let verified = SavedBinding::verified(
                binding.clone(),
                streams,
                proof.clone(),
                &SignedReceiptVerifier,
            )
            .unwrap();
            journal
                .transact(vec![ClientJournalChange::SaveEnrollment {
                    binding: Box::new(verified),
                    acknowledgement: Box::new(JournalEnrollmentAcknowledgement {
                        prepared,
                        device_credential: device.reference,
                        receipt: proof,
                    }),
                }])
                .await
                .unwrap();
            journal
                .transact(vec![ClientJournalChange::FinishEnrollmentAcknowledgement(
                    enrollment.id,
                )])
                .await
                .unwrap();
            assert!(journal.ordinary_use_allowed(&binding).unwrap());
            Self {
                journal,
                storage,
                binding,
            }
        }
        fn pairing(&self) -> LocalPairingKey {
            self.binding.target().local_pairing().unwrap()
        }
        fn client(&self) -> ClientId {
            self.binding.target().client()
        }
        async fn retain(&self, request: [u8; 16], intent: bool, assessment: bool, submitted: bool) {
            self.journal
                .retain_remote_native_request(
                    RemoteNativeRequest::new(
                        self.pairing(),
                        self.client(),
                        request,
                        SCOPE.to_vec(),
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
            if intent {
                self.journal
                    .retain_remote_native_intent(
                        self.pairing(),
                        self.client(),
                        request,
                        SCOPE.to_vec(),
                        INTENT.to_vec(),
                    )
                    .await
                    .unwrap();
            }
            if assessment {
                self.journal
                    .retain_remote_native_assessment(
                        self.pairing(),
                        self.client(),
                        request,
                        SCOPE.to_vec(),
                        ASSESSMENT.to_vec(),
                    )
                    .await
                    .unwrap();
            }
            if submitted {
                self.journal
                    .mark_remote_native_submitted(
                        self.pairing(),
                        self.client(),
                        request,
                        SCOPE.to_vec(),
                        assessment.then(|| ASSESSMENT.to_vec()),
                    )
                    .await
                    .unwrap();
            }
        }
        fn port(&self) -> HistoricalRemoteNativePort {
            self.journal
                .recover_remote_native_original(&self.pairing(), self.client(), REQUEST)
                .unwrap()
                .unwrap()
        }
        async fn remove(&self) -> PendingRevocationTask {
            let target = self.binding.target();
            let state = self.journal.snapshot().unwrap();
            let original = PendingSelfRevocation::new(PendingParts {
                original_server: target.server(),
                original_pairing: target.pairing(),
                device: target.device(),
                client: target.client(),
                operation: OperationId::new(
                    state.bindings()[0].streams().parts().self_revocation,
                    OperationSequence::new(1).unwrap(),
                ),
                route: ROUTE,
                credential: target.credential(),
                notice_dismissed: false,
            })
            .unwrap();
            let digest = CanonicalOperationRequest::new(
                original.operation(),
                Command::Pairing(PairingCommand::SelfRevoke {
                    target: PairingTarget::new(PairingTargetParts {
                        server: target.server(),
                        pairing: target.pairing(),
                        device: target.device(),
                    })
                    .unwrap(),
                }),
                &CanonicalCodec,
            )
            .unwrap()
            .binding()
            .digest;
            let task = PendingRevocationTask::new(PendingRevocationTaskParts {
                original,
                local_pairing: self.pairing(),
                pinned_server_public_key: target.pin().public_key,
                request_digest: digest,
            })
            .unwrap();
            self.journal
                .transact(vec![ClientJournalChange::BeginRevocation(Box::new(
                    JournalRevocation {
                        task: task.clone(),
                        target: SavedTarget::from_target(target),
                        phase: RevocationPhase::AwaitingConfirmation,
                        notice_dismissed: false,
                        attempts: 0,
                    },
                ))])
                .await
                .unwrap();
            task
        }
        async fn confirm_removal(&self, task: &PendingRevocationTask) {
            let original = task.parts().original;
            let proof = sign(
                self.binding.target().pin(),
                PairingReceiptBody::Revocation {
                    receipt: RevocationReceipt {
                        server: original.server(),
                        pairing: original.pairing(),
                        device: original.device(),
                        operation: original.operation(),
                        revision: StoreRevision::new(9),
                        receipt_digest: RequestDigest::from_bytes([10; 32]),
                    },
                    authentication_key_digest: RequestDigest::from_bytes(
                        Sha256::digest([6; 32]).into(),
                    ),
                    revocation_event: [12; 16],
                },
            );
            self.journal
                .transact(vec![ClientJournalChange::ConfirmRevocation {
                    operation: original.operation(),
                    receipt: Box::new(proof),
                }])
                .await
                .unwrap();
            self.journal
                .transact(vec![ClientJournalChange::ConfirmRevocationAcknowledgement(
                    original.operation(),
                )])
                .await
                .unwrap();
        }
    }

    struct NoLocalEffects;
    impl LocalUseInvalidator for NoLocalEffects {
        fn invalidate_workspace(
            &self,
            _: &LocalPairingKey,
            _: WorkspaceId,
        ) -> Result<Vec<GrantedControl>, ClientFailure> {
            panic!("historical enumeration cannot invalidate a workspace")
        }
        fn invalidate_attachment(
            &self,
            _: &LocalPairingKey,
            _: spaceterm_client::view::WorkspaceAttachment,
        ) -> Result<Vec<GrantedControl>, ClientFailure> {
            panic!("historical enumeration cannot invalidate an attachment")
        }
        fn invalidate_pairing(&self, _: &LocalPairingKey) {
            panic!("historical enumeration cannot invalidate a pairing")
        }
        fn remove_pairing<'a>(&'a self, _: &'a LocalPairingKey) -> ClientFuture<'a, ()> {
            panic!("historical enumeration cannot remove a pairing")
        }
    }
    fn pending(journal: &Arc<JournalController>) -> Vec<RemoteNativeOriginalPending> {
        PendingTasks::new(journal.clone(), Arc::new(NoLocalEffects))
            .remote_native_originals()
            .unwrap()
    }

    #[tokio::test]
    async fn fresh_reopen_after_local_removal_recovers_only_the_exact_original() {
        let f = HistoricalFixture::new(true).await;
        f.retain(REQUEST, true, true, true).await;
        let before_removal = f.port();
        f.remove().await;
        before_removal.validate().unwrap();
        // A current binding on the same server cannot supply original custody.
        let next = ssh_binding(3, [14; 16], "replacement-alias");
        f.journal
            .transact(vec![ClientJournalChange::SaveBinding(Box::new(saved(
                next.clone(),
            )))])
            .await
            .unwrap();
        let journal = Arc::new(reopen(f.storage.clone(), JournalLimits::default()).await);
        assert!(!journal.ordinary_use_allowed(&f.binding).unwrap());
        let port = journal
            .recover_remote_native_original(&f.pairing(), f.client(), REQUEST)
            .unwrap()
            .unwrap();
        assert_eq!(port.target(), f.binding.target());
        assert_eq!(port.credential().reference, f.binding.target().credential());
        assert_eq!(port.credential().route_reference, Some(ROUTE));
        assert_eq!(port.ssh_capture().configuration(), CAPTURE);
        assert_eq!(port.snapshot().unwrap().intent(), Some(INTENT));
        assert_eq!(port.snapshot().unwrap().assessment(), Some(ASSESSMENT));
        port.validate_journal(&journal).unwrap();
        assert_eq!(
            before_removal
                .validate_journal(&journal)
                .unwrap_err()
                .class(),
            ClientFailureClass::InvalidStorage
        );
        let rows = pending(&journal);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].local_pairing(), &f.pairing());
        assert_eq!(rows[0].client(), f.client());
        assert_eq!(rows[0].request(), REQUEST);
        assert_eq!(rows[0].state(), RemoteNativeOriginalState::Available);
        assert!(!rows[0].result_retained());
        assert!(
            journal
                .recover_remote_native_original(
                    &next.target().local_pairing().unwrap(),
                    next.target().client(),
                    REQUEST
                )
                .unwrap()
                .is_none()
        );
        let wrong_generation = LocalPairingKey::new(LocalPairingKeyParts {
            generation: LocalPairingGeneration::new(4).unwrap(),
            ..f.pairing().parts().clone()
        })
        .unwrap();
        assert!(
            journal
                .recover_remote_native_original(&wrong_generation, f.client(), REQUEST)
                .unwrap()
                .is_none()
        );
        assert!(
            journal
                .recover_remote_native_original(&f.pairing(), next.target().client(), REQUEST)
                .unwrap()
                .is_none()
        );
        assert!(
            journal
                .recover_remote_native_original(&f.pairing(), f.client(), [19; 16])
                .unwrap()
                .is_none()
        );
        // Recovery must not reopen foreground confirmation or fresh admission.
        assert!(
            journal
                .mark_remote_native_submitted(
                    f.pairing(),
                    f.client(),
                    REQUEST,
                    SCOPE.to_vec(),
                    Some(ASSESSMENT.to_vec())
                )
                .await
                .is_err()
        );
        assert!(
            journal
                .retain_remote_native_request(
                    RemoteNativeRequest::new(f.pairing(), f.client(), [20; 16], SCOPE.to_vec())
                        .unwrap()
                )
                .await
                .is_err()
        );
        assert_eq!(pending(&journal), rows);
    }

    #[tokio::test]
    async fn only_submitted_intent_and_assessment_qualify_and_blocked_rows_stay_visible() {
        let f = HistoricalFixture::new(true).await;
        f.retain([1; 16], true, true, false).await;
        f.retain([2; 16], false, true, true).await;
        f.retain([3; 16], true, false, true).await;
        f.retain(REQUEST, true, true, true).await;
        for request in [[1; 16], [2; 16], [3; 16], [4; 16]] {
            assert!(
                f.journal
                    .recover_remote_native_original(&f.pairing(), f.client(), request)
                    .unwrap()
                    .is_none()
            );
        }
        let rows = pending(&f.journal);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.request() != [1; 16]));
        assert_eq!(
            rows.iter()
                .find(|row| row.request() == [2; 16])
                .unwrap()
                .state(),
            RemoteNativeOriginalState::Blocked(RemoteNativeOriginalBlocker::MissingIntent)
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.request() == [3; 16])
                .unwrap()
                .state(),
            RemoteNativeOriginalState::Blocked(RemoteNativeOriginalBlocker::MissingAssessment)
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.request() == REQUEST)
                .unwrap()
                .state(),
            RemoteNativeOriginalState::Available
        );
        f.remove().await;
        assert_eq!(pending(&f.journal), rows);
        f.port().validate().unwrap();
    }

    #[tokio::test]
    async fn missing_original_capture_is_blocked_without_using_a_current_route() {
        let f = HistoricalFixture::new(false).await;
        f.retain(REQUEST, true, true, true).await;
        f.remove().await;
        assert!(
            f.journal
                .recover_remote_native_original(&f.pairing(), f.client(), REQUEST)
                .unwrap()
                .is_none()
        );
        let rows = pending(&f.journal);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].state(),
            RemoteNativeOriginalState::Blocked(
                RemoteNativeOriginalBlocker::MissingOriginalSshCapture
            )
        );
        assert_eq!(
            f.journal.snapshot().unwrap().remote_native_requests().len(),
            1
        );
    }

    #[tokio::test]
    async fn legacy_missing_slot_and_non_ssh_route_remain_typed_blocked_work() {
        for (binding, blocker) in [
            (
                ssh_binding(2, [4; 16], "original-alias"),
                RemoteNativeOriginalBlocker::MissingOriginalCredential,
            ),
            (
                binding(2),
                RemoteNativeOriginalBlocker::UnsupportedOriginalRoute,
            ),
        ] {
            let storage = Arc::new(MemoryJournal::with_verifier(
                Arc::new(CanonicalCodec),
                Arc::new(SignedReceiptVerifier),
            ));
            let journal = Arc::new(reopen(storage.clone(), JournalLimits::default()).await);
            journal
                .transact(vec![ClientJournalChange::SaveBinding(Box::new(saved(
                    binding.clone(),
                )))])
                .await
                .unwrap();
            let f = HistoricalFixture {
                journal,
                storage,
                binding,
            };
            f.retain(REQUEST, true, true, true).await;
            assert!(
                f.journal
                    .recover_remote_native_original(&f.pairing(), f.client(), REQUEST)
                    .unwrap()
                    .is_none()
            );
            let rows = pending(&f.journal);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].state(), RemoteNativeOriginalState::Blocked(blocker));
        }
    }

    #[tokio::test]
    async fn suppression_without_a_submitted_original_has_no_recovery_port_or_pending_row() {
        let f = HistoricalFixture::new(true).await;
        f.retain(REQUEST, true, true, false).await;
        let task = f.remove().await;
        f.confirm_removal(&task).await;
        let tasks = PendingTasks::new(f.journal.clone(), Arc::new(NoLocalEffects));
        let driver = CleanupKey::new(f.binding.target().credential());
        assert!(
            tasks
                .retry_revocation(task.parts().original.operation(), &driver)
                .await
                .unwrap()
        );
        assert!(!driver.present.load(Ordering::SeqCst));
        let journal = Arc::new(reopen(f.storage.clone(), JournalLimits::default()).await);
        assert!(journal.snapshot().unwrap().revocations().is_empty());
        assert!(
            journal
                .snapshot()
                .unwrap()
                .pairing()
                .slot(f.binding.target().credential())
                .unwrap()
                .ssh_capture
                .is_none()
        );
        assert!(!journal.ordinary_use_allowed(&f.binding).unwrap());
        assert!(
            journal
                .recover_remote_native_original(&f.pairing(), f.client(), REQUEST)
                .unwrap()
                .is_none()
        );
        assert!(pending(&journal).is_empty());
    }

    #[tokio::test]
    async fn exact_result_is_separately_durable_before_ack_retirement() {
        let f = HistoricalFixture::new(true).await;
        f.retain(REQUEST, true, true, true).await;
        f.retain([10; 16], true, true, true).await;
        f.remove().await;
        let port = f.port();
        let clone = port.clone();
        assert_eq!(
            port.retire_after_acknowledgement(RESULT)
                .await
                .unwrap_err()
                .class(),
            ClientFailureClass::StaleAuthority
        );
        assert!(port.retain_result(&[]).await.is_err());
        assert_eq!(
            port.retain_result(&vec![1; MAX_REMOTE_NATIVE_BYTES + 1])
                .await
                .unwrap_err()
                .class(),
            ClientFailureClass::ResourceLimit
        );
        let receipt = port.retain_result(RESULT).await.unwrap();
        assert_eq!(f.storage.state().revision(), receipt.revision());
        assert_eq!(clone.snapshot().unwrap().result(), Some(RESULT));
        assert!(
            pending(&f.journal)
                .iter()
                .find(|row| row.request() == REQUEST)
                .unwrap()
                .result_retained()
        );
        clone.retain_result(RESULT).await.unwrap();
        assert!(clone.retain_result(b"replacement-result").await.is_err());
        assert_eq!(
            clone
                .retire_after_acknowledgement(b"wrong-ack-result")
                .await
                .unwrap_err()
                .class(),
            ClientFailureClass::StaleAuthority
        );
        let reopened = Arc::new(reopen(f.storage.clone(), JournalLimits::default()).await);
        let recovered = reopened
            .recover_remote_native_original(&f.pairing(), f.client(), REQUEST)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.snapshot().unwrap().result(), Some(RESULT));
        recovered
            .retire_after_acknowledgement(RESULT)
            .await
            .unwrap();
        assert_eq!(
            recovered.validate().unwrap_err().class(),
            ClientFailureClass::StaleAuthority
        );
        let rows = pending(&reopened);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].request(), [10; 16]);
        assert!(!rows[0].result_retained());
        assert_eq!(f.storage.state().remote_native_requests().len(), 1);
    }

    // The external deletion boundary exposes disposition/order only. No real
    // credential is created, read or deleted by these portable journal tests.
    struct CleanupKey {
        credential: CredentialReference,
        present: AtomicBool,
        fail: AtomicBool,
        calls: AtomicUsize,
        changed: Notify,
    }
    impl CleanupKey {
        fn new(credential: CredentialReference) -> Self {
            Self {
                credential,
                present: AtomicBool::new(true),
                fail: AtomicBool::new(false),
                calls: AtomicUsize::new(0),
                changed: Notify::new(),
            }
        }
    }
    impl spaceterm_client::pending::RevocationDriver for CleanupKey {
        fn resolve<'a>(
            &'a self,
            _: &'a JournalRevocation,
        ) -> ClientFuture<'a, spaceterm_client::pending::RevocationResolution> {
            panic!("an already confirmed revocation cannot resend or resolve")
        }
        fn acknowledge<'a>(&'a self, _: &'a JournalRevocation) -> ClientFuture<'a, ()> {
            panic!("an already acknowledged revocation cannot reconnect for ACK")
        }
        fn cleanup<'a>(
            &'a self,
            task: &'a JournalRevocation,
        ) -> ClientFuture<'a, spaceterm_client::pending::CredentialCleanupDisposition> {
            Box::pin(async move {
                assert_eq!(task.task.parts().original.credential(), self.credential);
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.changed.notify_one();
                if self.fail.load(Ordering::SeqCst) {
                    return Err(ClientFailure::new(ClientFailureClass::Unavailable));
                }
                Ok(if self.present.swap(false, Ordering::SeqCst) {
                    spaceterm_client::pending::CredentialCleanupDisposition::Removed
                } else {
                    spaceterm_client::pending::CredentialCleanupDisposition::AlreadyAbsent
                })
            })
        }
    }

    #[tokio::test]
    async fn deleted_key_keeps_only_comparison_provenance_until_last_original_ack_and_cleanup_retry()
     {
        let f = HistoricalFixture::new(true).await;
        let device_before = f
            .journal
            .snapshot()
            .unwrap()
            .pairing()
            .slot(CredentialReference::new([9; 16]))
            .unwrap()
            .clone();
        f.retain(REQUEST, true, true, true).await;
        f.retain([10; 16], true, true, true).await;
        let port = f.port();
        port.retain_result(RESULT).await.unwrap();
        let task = f.remove().await;
        f.confirm_removal(&task).await;
        let next = ssh_binding(3, [14; 16], "replacement-alias");
        f.journal
            .transact(vec![ClientJournalChange::SaveBinding(Box::new(saved(
                next.clone(),
            )))])
            .await
            .unwrap();
        let other = HistoricalFixture {
            journal: f.journal.clone(),
            storage: f.storage.clone(),
            binding: next.clone(),
        };
        other.retain([11; 16], true, true, true).await;
        let driver = CleanupKey::new(f.binding.target().credential());
        let tasks = PendingTasks::new(f.journal.clone(), Arc::new(NoLocalEffects));
        driver.fail.store(true, Ordering::SeqCst);
        assert!(
            tasks
                .retry_revocation(task.parts().original.operation(), &driver)
                .await
                .is_err()
        );
        assert!(driver.present.load(Ordering::SeqCst));
        port.validate().unwrap();
        driver.fail.store(false, Ordering::SeqCst);
        assert!(
            !tasks
                .retry_revocation(task.parts().original.operation(), &driver)
                .await
                .unwrap()
        );
        assert!(!driver.present.load(Ordering::SeqCst));
        port.clone().validate().unwrap();
        assert!(!f.journal.ordinary_use_allowed(&f.binding).unwrap());
        assert!(f.journal.ordinary_use_allowed(&next).unwrap());
        let journal = Arc::new(reopen(f.storage.clone(), JournalLimits::default()).await);
        let tasks = PendingTasks::new(journal.clone(), Arc::new(NoLocalEffects));
        let recovered = journal
            .recover_remote_native_original(&f.pairing(), f.client(), REQUEST)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.ssh_capture().configuration(), CAPTURE);
        assert_eq!(recovered.snapshot().unwrap().result(), Some(RESULT));
        recovered
            .retire_after_acknowledgement(RESULT)
            .await
            .unwrap();
        // Another exact submitted request still needs status/result/ACK. The
        // AlreadyAbsent deletion disposition cannot release its provenance.
        assert!(
            !tasks
                .retry_revocation(task.parts().original.operation(), &driver)
                .await
                .unwrap()
        );
        let sibling = journal
            .recover_remote_native_original(&f.pairing(), f.client(), [10; 16])
            .unwrap()
            .unwrap();
        sibling.retain_result(RESULT).await.unwrap();
        sibling.retire_after_acknowledgement(RESULT).await.unwrap();
        assert_eq!(journal.snapshot().unwrap().revocations().len(), 1);
        // Native retirement never assumes key deletion ran. Only the existing
        // idempotent cleanup owner can now release the original metadata.
        assert!(
            tasks
                .retry_revocation(task.parts().original.operation(), &driver)
                .await
                .unwrap()
        );
        assert_eq!(driver.calls.load(Ordering::SeqCst), 4);
        let state = journal.snapshot().unwrap();
        assert!(state.revocations().is_empty());
        let slot = state
            .pairing()
            .slot(f.binding.target().credential())
            .unwrap();
        assert!(slot.ssh_capture.is_none());
        assert_eq!(slot.route_reference, Some(ROUTE));
        assert_eq!(slot.public_key, Some([6; 32]));
        assert_eq!(slot.generation, f.pairing().parts().generation);
        assert_eq!(
            state.pairing().slot(device_before.reference),
            Some(&device_before)
        );
        assert_eq!(
            state.bindings()[0].binding().unwrap().target(),
            next.target()
        );
        assert_eq!(
            recovered.validate().unwrap_err().class(),
            ClientFailureClass::StaleAuthority
        );
        // Unrelated current-pairing debt must not retain the old credential's
        // provenance or be removed by its cleanup.
        let rows = pending(&journal);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].local_pairing(),
            &next.target().local_pairing().unwrap()
        );
        assert_eq!(rows[0].request(), [11; 16]);
        assert!(!journal.ordinary_use_allowed(&f.binding).unwrap());
        assert!(journal.ordinary_use_allowed(&next).unwrap());
    }

    #[tokio::test]
    async fn blocked_submitted_original_alone_still_retains_existing_provenance() {
        for (intent, assessment, blocker) in [
            (false, true, RemoteNativeOriginalBlocker::MissingIntent),
            (true, false, RemoteNativeOriginalBlocker::MissingAssessment),
        ] {
            let f = HistoricalFixture::new(true).await;
            f.retain(REQUEST, intent, assessment, true).await;
            let task = f.remove().await;
            f.confirm_removal(&task).await;
            let driver = CleanupKey::new(f.binding.target().credential());
            let tasks = PendingTasks::new(f.journal.clone(), Arc::new(NoLocalEffects));
            assert!(
                !tasks
                    .retry_revocation(task.parts().original.operation(), &driver)
                    .await
                    .unwrap()
            );
            assert!(!driver.present.load(Ordering::SeqCst));
            assert_eq!(f.journal.snapshot().unwrap().revocations().len(), 1);
            assert!(
                f.journal
                    .snapshot()
                    .unwrap()
                    .pairing()
                    .slot(f.binding.target().credential())
                    .unwrap()
                    .ssh_capture
                    .is_some()
            );
            assert_eq!(
                pending(&f.journal)[0].state(),
                RemoteNativeOriginalState::Blocked(blocker)
            );
            assert!(
                f.journal
                    .recover_remote_native_original(&f.pairing(), f.client(), REQUEST)
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retained_cleanup_provenance_keeps_scheduler_backoff_without_network_replay() {
        let f = HistoricalFixture::new(true).await;
        f.retain(REQUEST, true, true, true).await;
        let task = f.remove().await;
        f.confirm_removal(&task).await;
        let driver = Arc::new(CleanupKey::new(f.binding.target().credential()));
        let tasks = PendingTasks::new(f.journal.clone(), Arc::new(NoLocalEffects));
        let runtime = tasks.start_scheduler(driver.clone()).unwrap();
        driver.changed.notified().await;
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
        assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
        assert!(!driver.present.load(Ordering::SeqCst));
        f.port().validate().unwrap();
        tokio::time::advance(std::time::Duration::from_millis(999)).await;
        assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        driver.changed.notified().await;
        assert_eq!(driver.calls.load(Ordering::SeqCst), 2);
        runtime.shutdown().await.unwrap();
        assert_eq!(
            pending(&f.journal)[0].state(),
            RemoteNativeOriginalState::Available
        );
    }

    struct LegacySnapshot {
        state: RecoveredClientState,
        fence: PairingAuthorityFence,
        latch: Arc<JournalAdmission>,
        scheduler: Arc<AtomicBool>,
    }
    impl ClientJournal for LegacySnapshot {
        fn authority_fence(&self) -> &PairingAuthorityFence {
            &self.fence
        }
        fn admission_latch(&self) -> Arc<JournalAdmission> {
            self.latch.clone()
        }
        fn scheduler_claim(&self) -> Arc<AtomicBool> {
            self.scheduler.clone()
        }
        fn recover(&self) -> ClientFuture<'_, RecoveredClientState> {
            Box::pin(async { Ok(self.state.clone()) })
        }
        fn commit<'a>(
            &'a self,
            _: &'a ClientJournalTransaction,
        ) -> ClientFuture<'a, JournalCommitDisposition> {
            panic!("legacy blocked enumeration cannot mutate its persisted input")
        }
    }

    #[tokio::test]
    async fn already_cleaned_legacy_snapshot_stays_blocked_without_reconstructing_original_provenance()
     {
        let f = HistoricalFixture::new(true).await;
        f.retain(REQUEST, true, true, true).await;
        f.port().retain_result(RESULT).await.unwrap();
        let task = f.remove().await;
        f.confirm_removal(&task).await;
        // Reproduce only the previous canonical writer's cleanup output through
        // its public serialized format. Recovery still runs the real canonical
        // validation; no private port constructor or alternate authority exists.
        let mut legacy = serde_json::to_value(f.journal.snapshot().unwrap()).unwrap();
        legacy["revocations"] = serde_json::json!([]);
        let reference = serde_json::to_value(f.binding.target().credential()).unwrap();
        let slot = legacy["pairing"]["slots"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|slot| slot["reference"] == reference)
            .unwrap();
        slot["ssh_capture"] = serde_json::Value::Null;
        let storage = Arc::new(LegacySnapshot {
            state: serde_json::from_value(legacy).unwrap(),
            fence: PairingAuthorityFence::default(),
            latch: Arc::default(),
            scheduler: Arc::default(),
        });
        let journal = Arc::new(reopen(storage, JournalLimits::default()).await);
        assert!(!journal.ordinary_use_allowed(&f.binding).unwrap());
        assert!(
            journal
                .recover_remote_native_original(&f.pairing(), f.client(), REQUEST)
                .unwrap()
                .is_none()
        );
        let rows = pending(&journal);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].state(),
            RemoteNativeOriginalState::Blocked(RemoteNativeOriginalBlocker::MissingOriginalTarget)
        );
        assert!(rows[0].result_retained());
        let state = journal.snapshot().unwrap();
        assert_eq!(state.remote_native_requests()[0].result(), Some(RESULT));
        assert_eq!(
            state
                .pairing()
                .slot(f.binding.target().credential())
                .unwrap()
                .route_reference,
            Some(ROUTE)
        );
    }

    // Pause the external persistence completion while the real controller owns
    // its serial lock. No canonical state or private port field is manufactured.
    struct PausedStorage {
        inner: Arc<MemoryJournal>,
        latch: Arc<JournalAdmission>,
        armed: AtomicBool,
        writes: AtomicUsize,
        entered: Notify,
        release: Notify,
    }
    impl ClientJournal for PausedStorage {
        fn authority_fence(&self) -> &PairingAuthorityFence {
            self.inner.authority_fence()
        }
        fn admission_latch(&self) -> Arc<JournalAdmission> {
            self.latch.clone()
        }
        fn scheduler_claim(&self) -> Arc<AtomicBool> {
            self.inner.scheduler_claim()
        }
        fn recover(&self) -> ClientFuture<'_, RecoveredClientState> {
            self.inner.recover()
        }
        fn commit<'a>(
            &'a self,
            transaction: &'a ClientJournalTransaction,
        ) -> ClientFuture<'a, JournalCommitDisposition> {
            Box::pin(async move {
                self.writes.fetch_add(1, Ordering::SeqCst);
                if self.armed.swap(false, Ordering::SeqCst) {
                    self.entered.notify_one();
                    self.release.notified().await;
                }
                self.inner.commit(transaction).await
            })
        }
    }

    #[tokio::test]
    async fn result_and_ack_writes_recheck_provenance_inside_the_serial_owner() {
        use std::{future::Future, task::Poll};
        for retire in [false, true] {
            let f = HistoricalFixture::new(true).await;
            f.retain(REQUEST, true, true, true).await;
            f.port().retain_result(RESULT).await.unwrap();
            let task = f.remove().await;
            f.confirm_removal(&task).await;
            let storage = Arc::new(PausedStorage {
                inner: f.storage.clone(),
                latch: Arc::default(),
                armed: AtomicBool::new(true),
                writes: AtomicUsize::new(0),
                entered: Notify::new(),
                release: Notify::new(),
            });
            let journal = Arc::new(reopen(storage.clone(), JournalLimits::default()).await);
            let port = journal
                .recover_remote_native_original(&f.pairing(), f.client(), REQUEST)
                .unwrap()
                .unwrap();
            let retiring = port.clone();
            let retirement =
                tokio::spawn(async move { retiring.retire_after_acknowledgement(RESULT).await });
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                storage.entered.notified(),
            )
            .await
            .unwrap();
            port.validate().unwrap();
            let mut mutation = Box::pin(async {
                if retire {
                    port.retire_after_acknowledgement(RESULT).await
                } else {
                    port.retain_result(RESULT).await
                }
            });
            // Poll once to queue the accepted mutation behind exact retirement,
            // while its original still exists. A pre-write check alone is stale.
            std::future::poll_fn(|cx| {
                assert!(mutation.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            port.validate().unwrap();
            storage.release.notify_one();
            retirement.await.unwrap().unwrap();
            assert_eq!(
                mutation.await.unwrap_err().class(),
                ClientFailureClass::StaleAuthority
            );
            assert_eq!(storage.writes.load(Ordering::SeqCst), 1);
            assert!(
                journal
                    .snapshot()
                    .unwrap()
                    .remote_native_requests()
                    .is_empty()
            );
            assert!(pending(&journal).is_empty());
            assert_eq!(
                port.validate().unwrap_err().class(),
                ClientFailureClass::StaleAuthority
            );
        }
    }

    #[tokio::test]
    async fn uncertain_result_publication_closes_every_port_until_fresh_recovery() {
        let f = HistoricalFixture::new(true).await;
        f.retain(REQUEST, true, true, true).await;
        f.remove().await;
        let storage = Arc::new(UncertainStorage::new(f.storage.clone()));
        let journal = Arc::new(reopen(storage.clone(), JournalLimits::default()).await);
        let port = journal
            .recover_remote_native_original(&f.pairing(), f.client(), REQUEST)
            .unwrap()
            .unwrap();
        let writer = port.clone();
        let task = tokio::spawn(async move { writer.retain_result(RESULT).await });
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            storage.entered.notified(),
        )
        .await
        .unwrap();
        assert_eq!(
            port.validate().unwrap_err().class(),
            ClientFailureClass::RecoveryRequired
        );
        assert_eq!(
            port.snapshot().unwrap_err().class(),
            ClientFailureClass::RecoveryRequired
        );
        assert_eq!(
            port.retire_after_acknowledgement(RESULT)
                .await
                .unwrap_err()
                .class(),
            ClientFailureClass::RecoveryRequired
        );
        assert_eq!(
            PendingTasks::new(journal.clone(), Arc::new(NoLocalEffects))
                .remote_native_originals()
                .unwrap_err()
                .class(),
            ClientFailureClass::RecoveryRequired
        );
        assert_eq!(storage.writes.load(Ordering::SeqCst), 1);
        storage.release.notify_one();
        assert_eq!(
            task.await.unwrap().unwrap_err().class(),
            ClientFailureClass::RecoveryRequired
        );
        let fresh = Arc::new(reopen(f.storage.clone(), JournalLimits::default()).await);
        let recovered = fresh
            .recover_remote_native_original(&f.pairing(), f.client(), REQUEST)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.snapshot().unwrap().result(), Some(RESULT));
        assert!(!fresh.ordinary_use_allowed(&f.binding).unwrap());
        assert_eq!(
            port.validate_journal(&fresh).unwrap_err().class(),
            ClientFailureClass::InvalidStorage
        );
        recovered
            .retire_after_acknowledgement(RESULT)
            .await
            .unwrap();
        assert!(pending(&fresh).is_empty());
    }
}
