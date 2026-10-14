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
