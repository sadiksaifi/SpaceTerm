#[path = "operation_recovery/support.rs"]
mod durable;
mod support;

use spaceterm_client::{
    api::ConnectionTarget,
    directories::DirectoryClient,
    failure::ClientFailureClass,
    journal::{ClientJournalChange, JournalController, JournalLimits},
    registry::ClientRegistry,
};
use spaceterm_core::authority::ServerEpoch;
use spaceterm_core::server_directories::{DirectoryQueryAction, DirectoryQueryResult};
use spaceterm_protocol::{
    codec::Payload,
    convert::directories,
    limits::MAX_ACCOUNT_RUNTIME_BYTES,
    negotiation::{CAPABILITY_ACCOUNT_RUNTIME, CAPABILITY_REMOTE_NATIVE_LIFECYCLE},
    wire::common,
};
use std::sync::Arc;
use support::{peer::*, *};

fn directory_reply(payload: &Payload, target: &ConnectionTarget) -> Option<Payload> {
    let context = spaceterm_core::server_directories::DirectoryContext::new(
        spaceterm_core::server_directories::DirectoryContextParts {
            server: target.server(),
            pairing: target.pairing(),
            client: target.client(),
            epoch: ServerEpoch::new(1),
        },
    )
    .unwrap();
    let request = directories::decode_query(payload, context).ok()?;
    assert_eq!(request.query.parts().action, DirectoryQueryAction::Home);
    Some(
        directories::encode_response(
            payload.major(),
            &directories::DirectoryResponse {
                request: request.request,
                generation: request.generation,
                result: Ok(DirectoryQueryResult::Home(
                    spaceterm_core::organization::ServerDirectory::new(
                        target.server(),
                        b"/owning-server/home".to_vec(),
                    )
                    .unwrap(),
                )),
            },
            context,
        )
        .unwrap(),
    )
}

async fn setup(
    major: u32,
    reply: Option<fn(&Payload, &ConnectionTarget) -> Option<Payload>>,
) -> (
    spaceterm_client::registry::ServerHandle,
    spaceterm_client::registry::ClientRuntime,
    Arc<JournalController>,
    Arc<PeerPlan>,
) {
    setup_with_capabilities(
        major,
        reply,
        &[
            spaceterm_protocol::negotiation::CAPABILITY_SERVER_DIRECTORIES,
            spaceterm_protocol::negotiation::CAPABILITY_SERVER_GIT,
            spaceterm_protocol::negotiation::CAPABILITY_ACCOUNT_RUNTIME,
        ],
    )
    .await
}

async fn setup_with_capabilities(
    major: u32,
    reply: Option<QueryHandler>,
    capabilities: &[u32],
) -> (
    spaceterm_client::registry::ServerHandle,
    spaceterm_client::registry::ClientRuntime,
    Arc<JournalController>,
    Arc<PeerPlan>,
) {
    let binding = binding(1);
    let codec = Arc::new(spaceterm_core::operation_ledger::canonical::CanonicalCodec);
    let storage = Arc::new(durable::MemoryJournal::new(codec.clone()));
    let journal = Arc::new(
        JournalController::recover(
            storage,
            codec,
            Arc::new(durable::Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap(),
    );
    journal
        .transact(vec![ClientJournalChange::SaveBinding(Box::new(
            durable::saved(binding.clone()),
        ))])
        .await
        .unwrap();
    let mut plan = PeerPlan::new(vec![major]);
    plan.capabilities.extend_from_slice(capabilities);
    plan.capabilities.sort_unstable();
    plan.query_handler = reply;
    let plan = Arc::new(plan);
    let mut services = services(
        vec![binding.clone()],
        Arc::new(Peers::one(&binding, plan.clone())),
    );
    services.journal = journal.clone();
    services.additional_capabilities = capabilities.to_vec();
    let (registry, runtime) = ClientRegistry::recover(services).await.unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.connect().await.unwrap();
    settle().await;
    (server, runtime, journal, plan)
}

#[tokio::test(start_paused = true)]
async fn directory_client_uses_authenticated_server_worker_in_both_majors() {
    for major in [1, 2] {
        let (server, runtime, journal, plan) = setup(major, Some(directory_reply)).await;
        let reads = server.queries(journal).unwrap();
        let client = DirectoryClient::new(server.identity(), Arc::new(reads)).unwrap();
        let home = client.home().await.unwrap();
        assert_eq!(home.bytes(), b"/owning-server/home");
        assert_eq!(home.server(), server.identity());
        assert_eq!(plan.opens.load(std::sync::atomic::Ordering::SeqCst), 1);
        runtime.shutdown().await.unwrap();
    }
}

use spaceterm_client::{
    directories::ClientDirectoryFailure,
    git::rpc::{GitOperationState, GitSubscriptionEvent},
    management_cli::{
        bindings::StatusOnlyManagement,
        ports::{ManagementClientKind, StatusBackend, StatusManagement},
    },
    operations::OperationTracker,
};
use spaceterm_core::{
    identity::{PaneId, WorktreeId},
    management::{NativeVerification, ServerStatus, ServerStatusParts, StatusPublication},
    operations::{OperationLookup, Outcome},
    organization::Versioned,
    remote_access::RemoteAccessPolicy,
    worktrees::{
        GitAssessmentKind, GitAssessmentToken, GitAssessmentTokenParts, GitCommand,
        GitStatusTarget,
        queries::{GitQueryResponse, GitQueryResult, GitStatusFacts},
    },
};
use spaceterm_protocol::{
    convert::{git, management},
    wire::{v1, v2},
};
use std::{num::NonZeroU64, sync::atomic::Ordering, time::Duration};

fn git_reply(payload: &Payload, target: &ConnectionTarget) -> Option<Payload> {
    let request = git::decode_query(payload, target.server()).ok()?;
    Some(
        git::encode_response(
            payload.major(),
            &GitQueryResponse {
                correlation: request.correlation,
                scope: None,
                result: Ok(GitQueryResult::Status(GitStatusFacts {
                    revision: NonZeroU64::new(5).unwrap(),
                    branch: None,
                    changes: vec![],
                    dirty: false,
                    unchecked: false,
                    pull_request: None,
                    upstream: None,
                    operation: None,
                    counts: None,
                })),
            },
            target.server(),
        )
        .unwrap(),
    )
}
fn status(server: spaceterm_core::identity::ServerId, revision: u64) -> ServerStatus {
    ServerStatus::new(ServerStatusParts {
        runtime: Default::default(),
        server,
        epoch: ServerEpoch::new(1),
        revision: NonZeroU64::new(revision).unwrap(),
        running: None,
        approved: None,
        staged: None,
        native_verification: NativeVerification::Unverified,
        capabilities: vec![],
        remote_access: Versioned {
            version: spaceterm_core::authority::FieldVersion::new(1),
            value: RemoteAccessPolicy::disabled(server),
        },
        listeners: vec![],
        progress: vec![],
    })
    .unwrap()
}
fn publication(major: u32, value: StatusPublication) -> Payload {
    match major {
        1 => Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::StatusPublication(
                management::v1::encode_publication(&value),
            )),
        }),
        2 => Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::StatusPublication(
                management::v2::encode_publication(&value),
            )),
        }),
        _ => unreachable!(),
    }
}
fn status_request(payload: &Payload) -> Option<(u64, u64, bool)> {
    match payload {
        Payload::Major1(message) => match &message.kind {
            Some(v1::message::Kind::StatusQuery(query)) => {
                Some((query.request, query.request_generation, query.cancel))
            }
            _ => None,
        },
        Payload::Major2(message) => match &message.kind {
            Some(v2::message::Kind::StatusQuery(query)) => {
                Some((query.request, query.request_generation, query.cancel))
            }
            _ => None,
        },
    }
}
fn status_reply(payload: &Payload, target: &ConnectionTarget) -> Option<Payload> {
    let (request, generation, cancel) = status_request(payload)?;
    if cancel {
        return None;
    }
    Some(publication(
        payload.major(),
        StatusPublication::Snapshot {
            request: NonZeroU64::new(request).unwrap(),
            generation,
            status: status(target.server(), 1),
        },
    ))
}

#[tokio::test(start_paused = true)]
async fn git_reads_and_mutations_share_the_server_connection_and_canonical_tracker() {
    for major in [1, 2] {
        let (server, runtime, journal, plan) = setup(major, Some(git_reply)).await;
        let peer = Arc::new(durable::Peer::default());
        let reads = server.queries(journal.clone()).unwrap();
        let client = reads
            .git(OperationTracker::new(journal.clone()), peer.clone())
            .unwrap();
        let target = GitStatusTarget::Pane(PaneId::new(server.identity(), 1).unwrap());
        let mut subscription = client.subscribe(target).unwrap();
        assert!(
            matches!(subscription.next().await, Some(Ok(GitSubscriptionEvent::Status(read))) if matches!(&read.result, GitQueryResult::Status(facts) if facts.revision.get() == 5))
        );
        let command = GitCommand::RemoveConfirmed {
            assessment: GitAssessmentToken::new(GitAssessmentTokenParts {
                server: server.identity(),
                epoch: ServerEpoch::new(1),
                pairing: binding(1).target().pairing(),
                client: binding(1).target().client(),
                kind: GitAssessmentKind::Remove,
                random: [8; 32],
            })
            .unwrap(),
        };
        peer.fail_send.store(true, Ordering::SeqCst);
        let operation = client.submit(command).await.unwrap();
        let original = operation.id();
        assert_eq!(
            operation.state().await.unwrap(),
            GitOperationState::Unresolved
        );
        assert_eq!(peer.sent.lock().unwrap().len(), 1);
        let removed = WorktreeId::new(server.identity(), 7).unwrap();
        let result = spaceterm_core::worktrees::GitMutationOutcome::new(
            spaceterm_core::worktrees::GitMutationOutcomeParts::Removed {
                worktree: removed,
                closed_tabs: 0,
                ended_sessions: 0,
                revision: spaceterm_core::authority::OrganizationRevision::new(9),
            },
        )
        .unwrap();
        peer.results.lock().unwrap().push((
            original,
            OperationLookup::Completed {
                result: Outcome::Git(result.clone()),
                publication_revision: spaceterm_core::authority::OrganizationRevision::new(9),
            },
        ));
        let restored = client.resume(original).await.unwrap();
        assert_eq!(restored.id(), original);
        assert_eq!(
            restored.state().await.unwrap(),
            GitOperationState::Completed(result)
        );
        assert_eq!(peer.sent.lock().unwrap().len(), 1);
        assert_eq!(
            peer.acks.lock().unwrap().as_slice(),
            &[(original.stream(), 1)]
        );
        assert_eq!(journal.snapshot().unwrap().operations().len(), 1);
        assert_eq!(plan.opens.load(Ordering::SeqCst), 1);
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn management_status_uses_authenticated_context_and_cancels_only_its_read_interest() {
    for major in [1, 2] {
        let (server, runtime, journal, plan) = setup(major, Some(status_reply)).await;
        let reads = server.queries(journal.clone()).unwrap();
        let backend = reads.status_backend(ManagementClientKind::StatusCompanion);
        let context = backend.context().unwrap();
        assert_eq!(context.major(), major);
        assert_eq!(
            context.connection_binding().unwrap(),
            match *server.state().borrow() {
                spaceterm_client::api::ConnectionState::Connected { info } => info.binding,
                _ => panic!("server is not connected"),
            }
        );
        let facade = StatusOnlyManagement::new(backend);
        assert_eq!(facade.status().await.unwrap().parts().revision.get(), 1);
        let mut subscription = facade.subscribe_status().await.unwrap();
        subscription.changed().await.unwrap();
        assert_eq!(
            subscription.latest().unwrap().unwrap().parts().server,
            server.identity()
        );
        let clone = facade.subscribe_status().await.unwrap();
        settle().await;
        assert_eq!(
            plan.messages
                .lock()
                .unwrap()
                .iter()
                .filter_map(status_request)
                .count(),
            2
        );
        drop(subscription);
        settle().await;
        assert!(
            !plan
                .messages
                .lock()
                .unwrap()
                .iter()
                .any(|payload| status_request(payload).is_some_and(|(_, _, cancel)| cancel))
        );
        drop(clone);
        settle().await;
        assert_eq!(
            plan.messages
                .lock()
                .unwrap()
                .iter()
                .filter(|payload| status_request(payload).is_some_and(|(_, _, cancel)| cancel))
                .count(),
            1
        );
        assert!(journal.snapshot().unwrap().operations().is_empty());
        assert_eq!(plan.opens.load(Ordering::SeqCst), 1);
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_directory_reply_cannot_satisfy_replacement_and_read_capacity_is_reclaimed() {
    let (server, runtime, journal, plan) = setup(2, None).await;
    let reads = server.queries(journal).unwrap();
    let client = DirectoryClient::new(server.identity(), Arc::new(reads)).unwrap();
    let mut calls = vec![];
    for _ in 0..4 {
        let client = client.clone();
        calls.push(tokio::spawn(async move { client.home().await }));
    }
    settle().await;
    assert_eq!(
        client.home().await,
        Err(ClientDirectoryFailure::Remote(
            spaceterm_core::server_directories::DirectoryFailure::Capacity
        ))
    );
    let old = plan
        .messages
        .lock()
        .unwrap()
        .iter()
        .find(|payload| directory_reply(payload, binding(1).target()).is_some())
        .unwrap()
        .clone();
    for call in calls.drain(..) {
        call.abort();
        let _ = call.await;
    }
    let replacement = {
        let client = client.clone();
        tokio::spawn(async move { client.home().await })
    };
    settle().await;
    plan.control(PeerControl::publication(
        directory_reply(&old, binding(1).target()).unwrap(),
    ))
    .await;
    settle().await;
    assert!(!replacement.is_finished());
    let current = plan
        .messages
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|payload| directory_reply(payload, binding(1).target()).is_some())
        .unwrap()
        .clone();
    assert_ne!(current, old);
    plan.control(PeerControl::publication(
        directory_reply(&current, binding(1).target()).unwrap(),
    ))
    .await;
    assert_eq!(
        replacement.await.unwrap().unwrap().bytes(),
        b"/owning-server/home"
    );
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn connection_loss_finishes_pending_reads_without_retrying_or_reopening_another_route() {
    let (server, runtime, journal, plan) = setup(1, None).await;
    let reads = server.queries(journal).unwrap();
    let client = DirectoryClient::new(server.identity(), Arc::new(reads)).unwrap();
    let call = tokio::spawn(async move { client.home().await });
    settle().await;
    plan.control(PeerControl::Disconnect).await;
    settle().await;
    assert!(call.await.unwrap().is_err());
    assert_eq!(plan.opens.load(Ordering::SeqCst), 1);
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn directory_read_deadline_does_not_leave_a_live_request_or_mutation() {
    let (server, runtime, journal, plan) = setup(2, None).await;
    let reads = server.queries(journal.clone()).unwrap();
    let client = DirectoryClient::new(server.identity(), Arc::new(reads)).unwrap();
    let call = tokio::spawn(async move { client.home().await });
    settle().await;
    tokio::time::advance(Duration::from_secs(6)).await;
    assert_eq!(call.await.unwrap(), Err(ClientDirectoryFailure::Offline));
    assert!(journal.snapshot().unwrap().operations().is_empty());
    assert_eq!(plan.opens.load(Ordering::SeqCst), 1);
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn status_gap_coalesces_resync_and_rejects_a_snapshot_below_the_observed_floor() {
    for major in [1, 2] {
        let (server, runtime, journal, plan) = setup(major, None).await;
        let reads = server.queries(journal).unwrap();
        let subscription = reads.subscribe_status().await.unwrap();
        let original = plan
            .messages
            .lock()
            .unwrap()
            .iter()
            .find_map(status_request)
            .unwrap();
        let publish = |revision| {
            publication(
                major,
                StatusPublication::Snapshot {
                    request: NonZeroU64::new(original.0).unwrap(),
                    generation: original.1,
                    status: status(server.identity(), revision),
                },
            )
        };
        plan.control(PeerControl::publication(publish(1))).await;
        settle().await;
        assert_eq!(
            subscription
                .latest()
                .unwrap()
                .unwrap()
                .parts()
                .revision
                .get(),
            1
        );
        for revision in [6, 7] {
            plan.control(PeerControl::publication(publication(
                major,
                StatusPublication::Changed {
                    request: NonZeroU64::new(original.0).unwrap(),
                    generation: original.1,
                    base: NonZeroU64::new(5).unwrap(),
                    status: status(server.identity(), revision),
                },
            )))
            .await;
        }
        settle().await;
        assert!(subscription.needs_resync().unwrap());
        assert_eq!(
            plan.messages
                .lock()
                .unwrap()
                .iter()
                .filter_map(status_request)
                .count(),
            2
        );
        plan.control(PeerControl::publication(publish(1))).await;
        settle().await;
        assert!(subscription.needs_resync().unwrap());
        assert_eq!(
            subscription
                .latest()
                .unwrap()
                .unwrap()
                .parts()
                .revision
                .get(),
            1
        );
        assert_eq!(
            plan.messages
                .lock()
                .unwrap()
                .iter()
                .filter_map(status_request)
                .count(),
            3
        );
        plan.control(PeerControl::publication(publish(7))).await;
        settle().await;
        assert!(!subscription.needs_resync().unwrap());
        assert_eq!(
            subscription
                .latest()
                .unwrap()
                .unwrap()
                .parts()
                .revision
                .get(),
            7
        );
        drop(subscription);
        settle().await;
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn server_query_ports_reject_a_second_journal_with_the_same_saved_binding() {
    let (server, runtime, journal, _) = setup(2, Some(directory_reply)).await;
    let codec = Arc::new(spaceterm_core::operation_ledger::canonical::CanonicalCodec);
    let other = Arc::new(
        JournalController::recover(
            Arc::new(durable::MemoryJournal::new(codec.clone())),
            codec,
            Arc::new(durable::Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap(),
    );
    other
        .transact(vec![ClientJournalChange::SaveBinding(Box::new(
            durable::saved(binding(1)),
        ))])
        .await
        .unwrap();
    let queries = server.queries(journal.clone()).unwrap();
    assert!(queries.validate_owner(&binding(1), &journal).is_ok());
    assert!(
        queries
            .validate_recovery_owner(&binding(1), &journal)
            .is_ok()
    );
    assert_eq!(
        queries
            .validate_recovery_owner(&binding(1), &other)
            .unwrap_err()
            .class(),
        ClientFailureClass::InvalidStorage
    );
    assert_eq!(
        queries
            .validate_recovery_owner(&binding(2), &journal)
            .unwrap_err()
            .class(),
        ClientFailureClass::StaleAuthority
    );
    assert_eq!(
        queries
            .validate_owner(&binding(1), &other)
            .unwrap_err()
            .class(),
        ClientFailureClass::InvalidStorage
    );
    assert_eq!(
        queries
            .validate_owner(&binding(2), &journal)
            .unwrap_err()
            .class(),
        ClientFailureClass::StaleAuthority
    );
    assert!(server.queries(other).is_err());
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn initial_status_resync_response_recovers_once_in_both_majors() {
    for major in [1, 2] {
        let (server, runtime, journal, plan) = setup(major, None).await;
        let subscription = server
            .queries(journal)
            .unwrap()
            .subscribe_status()
            .await
            .unwrap();
        let query = plan
            .messages
            .lock()
            .unwrap()
            .iter()
            .find_map(status_request)
            .unwrap();
        plan.control(PeerControl::publication(publication(
            major,
            StatusPublication::ResyncRequired {
                request: NonZeroU64::new(query.0).unwrap(),
                generation: query.1,
            },
        )))
        .await;
        settle().await;
        assert_eq!(
            plan.messages
                .lock()
                .unwrap()
                .iter()
                .filter_map(status_request)
                .count(),
            2
        );
        plan.control(PeerControl::publication(publication(
            major,
            StatusPublication::Snapshot {
                request: NonZeroU64::new(query.0).unwrap(),
                generation: query.1,
                status: status(server.identity(), 1),
            },
        )))
        .await;
        settle().await;
        assert!(!subscription.needs_resync().unwrap());
        assert_eq!(
            subscription
                .latest()
                .unwrap()
                .unwrap()
                .parts()
                .revision
                .get(),
            1
        );
        tokio::time::advance(Duration::from_secs(6)).await;
        settle().await;
        assert_eq!(
            plan.messages
                .lock()
                .unwrap()
                .iter()
                .filter_map(status_request)
                .count(),
            2
        );
        drop(subscription);
        settle().await;
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn status_resync_retries_after_ordinary_egress_saturation_in_both_majors() {
    use spaceterm_client::{api::ConnectionState, failure::ClientFailureClass};
    use spaceterm_protocol::limits::{MAX_EGRESS_ENVELOPES, RESERVED_LIFECYCLE_ENVELOPES};

    for major in [1, 2] {
        let (server, runtime, journal, plan) = setup(major, None).await;
        let subscription = server
            .queries(journal)
            .unwrap()
            .subscribe_status()
            .await
            .unwrap();
        let original = plan
            .messages
            .lock()
            .unwrap()
            .iter()
            .find(|message| status_request(message).is_some())
            .unwrap()
            .clone();
        let (request, generation, _) = status_request(&original).unwrap();
        let query_count = || {
            plan.messages
                .lock()
                .unwrap()
                .iter()
                .filter_map(status_request)
                .filter(|&(id, _, cancel)| id == request && !cancel)
                .count()
        };
        let info = match *server.state().borrow() {
            ConnectionState::Connected { info } => info,
            _ => panic!("not ready"),
        };
        plan.client_send_gate
            .forget_permits(plan.client_send_gate.available_permits());
        let ordinary_capacity = MAX_EGRESS_ENVELOPES - RESERVED_LIFECYCLE_ENVELOPES;
        let mut ordinary = tokio::task::JoinSet::new();
        for offset in 0..=ordinary_capacity {
            let mut message = original.clone();
            match &mut message {
                Payload::Major1(message) => {
                    if let Some(v1::message::Kind::StatusQuery(query)) = &mut message.kind {
                        query.request = 100 + u64::try_from(offset).unwrap();
                    }
                }
                Payload::Major2(message) => {
                    if let Some(v2::message::Kind::StatusQuery(query)) = &mut message.kind {
                        query.request = 100 + u64::try_from(offset).unwrap();
                    }
                }
            }
            let server = server.clone();
            ordinary.spawn(async move { server.dispatch(info, message).await });
        }
        settle().await;
        assert_eq!(
            ordinary
                .join_next()
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
                .class(),
            ClientFailureClass::ResourceLimit
        );
        plan.control(PeerControl::publication(publication(
            major,
            StatusPublication::ResyncRequired {
                request: NonZeroU64::new(request).unwrap(),
                generation,
            },
        )))
        .await;
        settle().await;
        assert!(subscription.needs_resync().unwrap());
        assert_eq!(query_count(), 1);
        for revision in [2, 3] {
            plan.control(PeerControl::publication(publication(
                major,
                StatusPublication::Changed {
                    request: NonZeroU64::new(request).unwrap(),
                    generation,
                    base: NonZeroU64::new(revision - 1).unwrap(),
                    status: status(server.identity(), revision),
                },
            )))
            .await;
        }
        tokio::time::advance(Duration::from_secs(5)).await;
        settle().await;
        assert_eq!(query_count(), 1);
        assert!(matches!(
            *server.state().borrow(),
            ConnectionState::Connected { .. }
        ));

        plan.client_send_gate.add_permits(MAX_EGRESS_ENVELOPES);
        while let Some(result) = ordinary.join_next().await {
            result.unwrap().unwrap();
        }
        settle().await;
        assert_eq!(query_count(), 1);
        tokio::time::advance(Duration::from_secs(5)).await;
        settle().await;
        assert_eq!(
            query_count(),
            2,
            "status recovery was stranded after queue drain, major {major}"
        );
        plan.control(PeerControl::publication(publication(
            major,
            StatusPublication::Snapshot {
                request: NonZeroU64::new(request).unwrap(),
                generation,
                status: status(server.identity(), 3),
            },
        )))
        .await;
        settle().await;
        assert!(!subscription.needs_resync().unwrap());
        assert_eq!(
            subscription
                .latest()
                .unwrap()
                .unwrap()
                .parts()
                .revision
                .get(),
            3
        );
        tokio::time::advance(Duration::from_secs(6)).await;
        settle().await;
        assert_eq!(query_count(), 2);
        drop(subscription);
        settle().await;
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn status_recovery_stops_when_subscription_or_connection_is_invalidated() {
    for major in [1, 2] {
        for stopped in [false, true] {
            let (server, runtime, journal, plan) = setup(major, None).await;
            let subscription = server
                .queries(journal)
                .unwrap()
                .subscribe_status()
                .await
                .unwrap();
            let (request, generation, _) = plan
                .messages
                .lock()
                .unwrap()
                .iter()
                .find_map(status_request)
                .unwrap();
            plan.control(PeerControl::publication(publication(
                major,
                StatusPublication::ResyncRequired {
                    request: NonZeroU64::new(request).unwrap(),
                    generation,
                },
            )))
            .await;
            settle().await;
            assert_eq!(
                plan.messages
                    .lock()
                    .unwrap()
                    .iter()
                    .filter_map(status_request)
                    .count(),
                2
            );
            let retained = if stopped {
                plan.control(PeerControl::Stopped).await;
                settle().await;
                assert!(matches!(
                    *server.state().borrow(),
                    spaceterm_client::api::ConnectionState::Stopped
                ));
                Some(subscription)
            } else {
                drop(subscription);
                None
            };
            settle().await;
            tokio::time::advance(Duration::from_secs(11)).await;
            settle().await;
            let queries: Vec<_> = plan
                .messages
                .lock()
                .unwrap()
                .iter()
                .filter_map(status_request)
                .collect();
            assert_eq!(queries.iter().filter(|&&(_, _, cancel)| !cancel).count(), 2);
            assert_eq!(
                queries.iter().filter(|&&(_, _, cancel)| cancel).count(),
                usize::from(!stopped)
            );
            assert_eq!(plan.opens.load(Ordering::SeqCst), 1);
            drop(retained);
            settle().await;
            runtime.shutdown().await.unwrap();
        }
    }
}

#[tokio::test(start_paused = true)]
async fn status_initial_and_resync_response_deadlines_schedule_one_replacement() {
    for major in [1, 2] {
        let (server, runtime, journal, plan) = setup(major, None).await;
        let subscription = server
            .queries(journal)
            .unwrap()
            .subscribe_status()
            .await
            .unwrap();
        let query = plan
            .messages
            .lock()
            .unwrap()
            .iter()
            .find_map(status_request)
            .unwrap();
        for expected in [2, 3] {
            tokio::time::advance(Duration::from_secs(5)).await;
            settle().await;
            assert!(subscription.needs_resync().unwrap());
            assert_eq!(
                plan.messages
                    .lock()
                    .unwrap()
                    .iter()
                    .filter_map(status_request)
                    .count(),
                expected
            );
        }
        plan.control(PeerControl::publication(publication(
            major,
            StatusPublication::Snapshot {
                request: NonZeroU64::new(query.0).unwrap(),
                generation: query.1,
                status: status(server.identity(), 1),
            },
        )))
        .await;
        settle().await;
        tokio::time::advance(Duration::from_secs(6)).await;
        settle().await;
        assert_eq!(
            plan.messages
                .lock()
                .unwrap()
                .iter()
                .filter_map(status_request)
                .count(),
            3
        );
        assert!(!subscription.needs_resync().unwrap());
        drop(subscription);
        settle().await;
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn companion_status_decodes_other_client_and_compacted_operation_progress_in_both_majors() {
    use spaceterm_core::{identity::*, management::*, operations::*};
    for major in [1, 2] {
        for compacted in [false, true] {
            let (server, runtime, journal, plan) = setup(major, None).await;
            let operation = if compacted {
                let peer = durable::Peer::default();
                let tracker = OperationTracker::new(journal.clone());
                let ticket = tracker
                    .submit(
                        &binding(1),
                        Command::AssessEndAllSessions {
                            server: server.identity(),
                        },
                        None,
                        &peer,
                    )
                    .await
                    .unwrap();
                tracker
                    .resolve(
                        ticket.id(),
                        OperationLookup::Completed {
                            result: Outcome::Applied,
                            publication_revision:
                                spaceterm_core::authority::OrganizationRevision::new(1),
                        },
                        &peer,
                    )
                    .await
                    .unwrap();
                tracker
                    .confirm_acknowledgement(ticket.id().stream(), ticket.id().sequence().get())
                    .await
                    .unwrap();
                journal
                    .transact(vec![ClientJournalChange::ConsumeResult(ticket.id())])
                    .await
                    .unwrap();
                ticket.binding()
            } else {
                let stream = OperationStreamId::new(
                    OperationStreamKey::new(server.identity(), 99).unwrap(),
                    binding(1).target().pairing(),
                    ClientId::new(server.identity(), 99).unwrap(),
                )
                .unwrap();
                spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::new(
                    OperationId::new(stream, OperationSequence::new(1).unwrap()),
                    Command::AssessEndAllSessions {
                        server: server.identity(),
                    },
                    &spaceterm_core::operation_ledger::canonical::CanonicalCodec,
                )
                .unwrap()
                .binding()
            };
            assert!(
                journal
                    .snapshot()
                    .unwrap()
                    .operation(operation.id)
                    .is_none()
            );
            let backend = server
                .queries(journal)
                .unwrap()
                .status_backend(ManagementClientKind::StatusCompanion);
            if !compacted {
                assert_ne!(
                    operation.id.stream().client(),
                    backend
                        .context()
                        .unwrap()
                        .connection_binding()
                        .unwrap()
                        .client
                );
            }
            let subscription = backend.subscribe_status().await.unwrap();
            let query = plan
                .messages
                .lock()
                .unwrap()
                .iter()
                .find_map(status_request)
                .unwrap();
            let mut parts = status(server.identity(), 1).parts().clone();
            parts.progress.push(ManagementProgress {
                request: ManagementRequestReference::OperationReference(operation.id),
                state: ManagementProgressState::Draining,
                completed_units: 1,
                total_units: 2,
            });
            let value = ServerStatus::new(parts).unwrap();
            plan.control(PeerControl::publication(publication(
                major,
                StatusPublication::Snapshot {
                    request: NonZeroU64::new(query.0).unwrap(),
                    generation: query.1,
                    status: value.clone(),
                },
            )))
            .await;
            settle().await;
            assert_eq!(subscription.latest().unwrap(), Some(value));
            assert!(matches!(
                *server.state().borrow(),
                spaceterm_client::api::ConnectionState::Connected { .. }
            ));
            drop(subscription);
            settle().await;
            runtime.shutdown().await.unwrap();
        }
    }
}

fn account_runtime_request(payload: &Payload) -> Option<&common::AccountRuntimeMessage> {
    match payload {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::AccountRuntimeRequest(message)),
        })
        | Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::AccountRuntimeRequest(message)),
        }) => Some(message),
        _ => None,
    }
}

fn account_runtime_reply(major: u32, request: u64, generation: u64, body: &[u8]) -> Payload {
    let message = common::AccountRuntimeMessage {
        request,
        request_generation: generation,
        body: body.to_vec(),
    };
    match major {
        1 => Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::AccountRuntimeReply(message)),
        }),
        2 => Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::AccountRuntimeReply(message)),
        }),
        _ => unreachable!(),
    }
}

fn echo_account_runtime(payload: &Payload, _: &ConnectionTarget) -> Option<Payload> {
    let message = account_runtime_request(payload)?;
    Some(account_runtime_reply(
        payload.major(),
        message.request,
        message.request_generation,
        &message.body,
    ))
}

fn poll_pending(future: std::pin::Pin<&mut impl std::future::Future>) {
    assert!(
        future
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
}

fn runtime_requests(plan: &PeerPlan) -> Vec<common::AccountRuntimeMessage> {
    plan.messages
        .lock()
        .unwrap()
        .iter()
        .filter_map(account_runtime_request)
        .cloned()
        .collect()
}

#[tokio::test(start_paused = true)]
async fn account_runtime_uses_the_existing_worker_and_canonical_journal_in_both_majors() {
    for major in [1, 2] {
        let (server, runtime, journal, plan) = setup(major, Some(echo_account_runtime)).await;
        let queries = server.queries(journal.clone()).unwrap();
        let info = match *server.state().borrow() {
            spaceterm_client::api::ConnectionState::Connected { info } => info,
            _ => panic!("not connected"),
        };
        assert_eq!(queries.server_epoch().unwrap(), info.epoch);
        assert!(info.capabilities.supports(CAPABILITY_ACCOUNT_RUNTIME));
        assert!(
            queries
                .capabilities()
                .unwrap()
                .supports(CAPABILITY_ACCOUNT_RUNTIME)
        );
        let revision = journal.snapshot().unwrap().revision();
        for body in [vec![1], vec![7; MAX_ACCOUNT_RUNTIME_BYTES]] {
            assert_eq!(queries.account_runtime(&body).await.unwrap(), body);
        }
        let requests = runtime_requests(&plan);
        assert_eq!(requests.len(), 2);
        assert!(
            requests.iter().all(|request| {
                request.request != 0 && request.request_generation == info.attempt
            })
        );
        assert_ne!(requests[0].request, requests[1].request);
        assert_eq!(journal.snapshot().unwrap().revision(), revision);
        assert!(journal.snapshot().unwrap().operations().is_empty());
        assert_eq!(plan.opens.load(Ordering::SeqCst), 1);
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn account_runtime_isolates_callers_and_requires_exact_request_and_generation() {
    for major in [1, 2] {
        let (server, runtime, journal, plan) = setup(major, None).await;
        let queries = server.queries(journal).unwrap();
        let mut first = Box::pin(queries.account_runtime(b"first"));
        let mut second = Box::pin(queries.account_runtime(b"second"));
        poll_pending(first.as_mut());
        poll_pending(second.as_mut());
        settle().await;
        let requests = runtime_requests(&plan);
        let first_request = requests
            .iter()
            .find(|value| value.body == b"first")
            .unwrap();
        let second_request = requests
            .iter()
            .find(|value| value.body == b"second")
            .unwrap();
        assert_ne!(first_request.request, second_request.request);
        for (request, generation) in [
            (
                first_request.request + 100,
                first_request.request_generation,
            ),
            (first_request.request, 0),
            (first_request.request, first_request.request_generation + 1),
        ] {
            plan.control(PeerControl::publication(account_runtime_reply(
                major, request, generation, b"wrong",
            )))
            .await;
        }
        settle().await;
        poll_pending(first.as_mut());
        poll_pending(second.as_mut());
        plan.control(PeerControl::publication(account_runtime_reply(
            major,
            second_request.request,
            second_request.request_generation,
            b"second result",
        )))
        .await;
        settle().await;
        assert_eq!(second.await.unwrap(), b"second result");
        poll_pending(first.as_mut());
        plan.control(PeerControl::publication(account_runtime_reply(
            major,
            first_request.request,
            first_request.request_generation,
            b"first result",
        )))
        .await;
        assert_eq!(first.await.unwrap(), b"first result");
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn account_runtime_bounds_precede_shared_read_admission_and_cancellation_reclaims_capacity() {
    for major in [1, 2] {
        let (server, runtime, journal, plan) = setup(major, None).await;
        let queries = server.queries(journal).unwrap();
        let mut calls = Vec::new();
        for _ in 0..3 {
            let mut call = Box::pin(queries.account_runtime(b"pending"));
            poll_pending(call.as_mut());
            calls.push(call);
        }
        let client = DirectoryClient::new(server.identity(), Arc::new(queries.clone())).unwrap();
        let mut directory = Box::pin(client.home());
        poll_pending(directory.as_mut());
        settle().await;
        assert_eq!(
            queries.account_runtime(b"").await.unwrap_err().class(),
            ClientFailureClass::InvalidPeer
        );
        assert_eq!(
            queries
                .account_runtime(&vec![0; MAX_ACCOUNT_RUNTIME_BYTES + 1])
                .await
                .unwrap_err()
                .class(),
            ClientFailureClass::ResourceLimit
        );
        assert_eq!(
            queries.account_runtime(b"fifth").await.unwrap_err().class(),
            ClientFailureClass::ResourceLimit
        );
        let old = runtime_requests(&plan);
        assert_eq!(old.len(), 3);
        drop(calls);
        drop(directory);
        let mut replacement = Box::pin(queries.account_runtime(b"replacement"));
        poll_pending(replacement.as_mut());
        settle().await;
        for request in old {
            plan.control(PeerControl::publication(account_runtime_reply(
                major,
                request.request,
                request.request_generation,
                b"cancelled",
            )))
            .await;
        }
        settle().await;
        poll_pending(replacement.as_mut());
        let requests = runtime_requests(&plan);
        assert_eq!(requests.len(), 4);
        let current = requests.last().unwrap();
        plan.control(PeerControl::publication(account_runtime_reply(
            major,
            current.request,
            current.request_generation,
            b"current",
        )))
        .await;
        assert_eq!(replacement.await.unwrap(), b"current");
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn account_runtime_requires_the_negotiated_capability_without_sending() {
    for major in [1, 2] {
        let (server, runtime, journal, plan) = setup_with_capabilities(major, None, &[]).await;
        let queries = server.queries(journal).unwrap();
        assert_eq!(
            queries
                .account_runtime(b"request")
                .await
                .unwrap_err()
                .class(),
            ClientFailureClass::MissingCapability
        );
        assert!(runtime_requests(&plan).is_empty());
        assert_eq!(plan.opens.load(Ordering::SeqCst), 1);
        runtime.shutdown().await.unwrap();
    }
}

async fn block_runtime_writer(
    server: &spaceterm_client::registry::ServerHandle,
    plan: &PeerPlan,
) -> tokio::task::JoinHandle<Result<(), spaceterm_client::failure::ClientFailure>> {
    let info = match *server.state().borrow() {
        spaceterm_client::api::ConnectionState::Connected { info } => info,
        _ => panic!("not connected"),
    };
    plan.client_send_gate
        .forget_permits(plan.client_send_gate.available_permits());
    let before = plan.sends_started.load(Ordering::SeqCst);
    let message = match info.major {
        1 => Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::StatusQuery(v1::StatusQuery {
                request: 9000,
                ..Default::default()
            })),
        }),
        2 => Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::StatusQuery(v2::StatusQuery {
                request: 9000,
                ..Default::default()
            })),
        }),
        _ => unreachable!(),
    };
    let server = server.clone();
    let blocker = tokio::spawn(async move { server.dispatch(info, message).await });
    settle().await;
    assert_eq!(plan.sends_started.load(Ordering::SeqCst), before + 1);
    blocker
}

#[tokio::test(start_paused = true)]
async fn cancelled_account_runtime_calls_never_reach_transmission() {
    for major in [1, 2] {
        let (server, runtime, journal, plan) = setup(major, None).await;
        let queries = server.queries(journal).unwrap();
        drop(queries.account_runtime(b"cancelled before admission"));
        let blocker = block_runtime_writer(&server, &plan).await;
        let mut queued = Box::pin(queries.account_runtime(b"cancelled while queued"));
        poll_pending(queued.as_mut());
        settle().await;
        drop(queued);
        plan.client_send_gate.add_permits(10);
        blocker.await.unwrap().unwrap();
        settle().await;
        assert!(runtime_requests(&plan).is_empty());
        assert_eq!(plan.opens.load(Ordering::SeqCst), 1);
        runtime.shutdown().await.unwrap();
    }
}

async fn revoke_runtime_pairing(journal: &JournalController) {
    use spaceterm_client::journal::{JournalRevocation, RevocationPhase, SavedTarget};
    use spaceterm_core::{operations::*, pairing::*};
    let binding = binding(1);
    let state = journal.snapshot().unwrap();
    let saved = &state.bindings()[0];
    let original = PendingSelfRevocation::new(PendingParts {
        original_server: binding.target().server(),
        original_pairing: binding.target().pairing(),
        device: binding.target().device(),
        client: binding.target().client(),
        operation: OperationId::new(
            saved.streams().parts().self_revocation,
            OperationSequence::new(1).unwrap(),
        ),
        route: RevocationRouteReference::new([9; 16]),
        credential: binding.target().credential(),
        notice_dismissed: false,
    })
    .unwrap();
    let request = spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::new(
        original.operation(),
        Command::Pairing(PairingCommand::SelfRevoke {
            target: PairingTarget::new(PairingTargetParts {
                server: original.server(),
                pairing: original.pairing(),
                device: original.device(),
            })
            .unwrap(),
        }),
        &spaceterm_core::operation_ledger::canonical::CanonicalCodec,
    )
    .unwrap();
    let task = PendingRevocationTask::new(PendingRevocationTaskParts {
        original,
        local_pairing: binding.target().local_pairing().unwrap(),
        pinned_server_public_key: binding.target().pin().public_key,
        request_digest: request.binding().digest,
    })
    .unwrap();
    journal
        .transact(vec![ClientJournalChange::BeginRevocation(Box::new(
            JournalRevocation {
                task,
                target: SavedTarget::from_target(binding.target()),
                phase: RevocationPhase::AwaitingConfirmation,
                notice_dismissed: false,
                attempts: 0,
            },
        ))])
        .await
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn account_runtime_rechecks_revocation_at_final_transmission_and_result_release() {
    for major in [1, 2] {
        for queued in [false, true] {
            let (server, runtime, journal, plan) = setup(major, None).await;
            let queries = server.queries(journal.clone()).unwrap();
            let blocker = if queued {
                Some(block_runtime_writer(&server, &plan).await)
            } else {
                None
            };
            let mut call = Box::pin(queries.account_runtime(b"request"));
            poll_pending(call.as_mut());
            settle().await;
            if !queued {
                let requests = runtime_requests(&plan);
                let request = &requests[0];
                plan.control(PeerControl::publication(account_runtime_reply(
                    major,
                    request.request,
                    request.request_generation,
                    b"withheld result",
                )))
                .await;
                settle().await;
            }
            revoke_runtime_pairing(&journal).await;
            assert!(
                queries
                    .validate_recovery_owner(&binding(1), &journal)
                    .is_ok()
            );
            assert_eq!(
                queries
                    .validate_owner(&binding(1), &journal)
                    .unwrap_err()
                    .class(),
                ClientFailureClass::AuthorizationDenied
            );
            assert_eq!(
                queries
                    .account_runtime(b"new after revocation")
                    .await
                    .unwrap_err()
                    .class(),
                ClientFailureClass::AuthorizationDenied
            );
            if let Some(blocker) = blocker {
                plan.client_send_gate.add_permits(10);
                blocker.await.unwrap().unwrap();
            }
            assert!(matches!(
                call.await.unwrap_err().class(),
                ClientFailureClass::StaleAuthority | ClientFailureClass::AuthorizationDenied
            ));
            assert_eq!(runtime_requests(&plan).len(), usize::from(!queued));
            assert_eq!(plan.opens.load(Ordering::SeqCst), 1);
            runtime.shutdown().await.unwrap();
        }
    }
}

#[tokio::test(start_paused = true)]
async fn account_runtime_reconnect_cannot_complete_an_old_call_or_accept_its_reply() {
    for major in [1, 2] {
        for received_before_reconnect in [false, true] {
            let (server, runtime, journal, plan) = setup(major, None).await;
            let queries = server.queries(journal).unwrap();
            let mut old_call = Box::pin(queries.account_runtime(b"old"));
            poll_pending(old_call.as_mut());
            settle().await;
            let old = runtime_requests(&plan).pop().unwrap();
            if received_before_reconnect {
                plan.control(PeerControl::publication(account_runtime_reply(
                    major,
                    old.request,
                    old.request_generation,
                    b"old result",
                )))
                .await;
                settle().await;
            }
            plan.control(PeerControl::Disconnect).await;
            settle().await;
            tokio::time::advance(Duration::from_secs(1)).await;
            settle().await;
            assert_eq!(plan.opens.load(Ordering::SeqCst), 2);
            assert_eq!(
                old_call.await.unwrap_err().class(),
                ClientFailureClass::StaleAuthority
            );
            let mut current_call = Box::pin(queries.account_runtime(b"current"));
            poll_pending(current_call.as_mut());
            settle().await;
            let current = runtime_requests(&plan).pop().unwrap();
            assert_ne!(current.request_generation, old.request_generation);
            assert_ne!(current.request, old.request);
            for (request, generation) in [
                (old.request, old.request_generation),
                (old.request, current.request_generation),
                (current.request, old.request_generation),
            ] {
                plan.control(PeerControl::publication(account_runtime_reply(
                    major, request, generation, b"stale",
                )))
                .await;
            }
            settle().await;
            poll_pending(current_call.as_mut());
            plan.control(PeerControl::publication(account_runtime_reply(
                major,
                current.request,
                current.request_generation,
                b"current result",
            )))
            .await;
            assert_eq!(current_call.await.unwrap(), b"current result");
            runtime.shutdown().await.unwrap();
        }
    }
}

#[tokio::test(start_paused = true)]
async fn native_account_runtime_fences_queued_calls_and_rechecks_capabilities_after_reconnect() {
    for major in [1, 2] {
        for queued_before_reconnect in [false, true] {
            let (server, runtime, journal, plan) = setup_with_capabilities(
                major,
                Some(echo_account_runtime),
                &[
                    CAPABILITY_ACCOUNT_RUNTIME,
                    CAPABILITY_REMOTE_NATIVE_LIFECYCLE,
                ],
            )
            .await;
            let queries = server.queries(journal).unwrap();
            let epoch = queries.server_epoch().unwrap();
            assert!(
                queries
                    .capabilities()
                    .unwrap()
                    .supports(CAPABILITY_REMOTE_NATIVE_LIFECYCLE)
            );
            assert_eq!(
                queries.native_account_runtime(b"supported").await.unwrap(),
                b"supported"
            );
            let before = runtime_requests(&plan).len();
            let blocker = if queued_before_reconnect {
                Some(block_runtime_writer(&server, &plan).await)
            } else {
                None
            };
            let mut queued = Box::pin(queries.native_account_runtime(b"old queued request"));
            if queued_before_reconnect {
                poll_pending(queued.as_mut());
                settle().await;
            }
            *plan.next_capabilities.lock().unwrap() = Some(
                plan.capabilities
                    .iter()
                    .copied()
                    .filter(|capability| *capability != CAPABILITY_REMOTE_NATIVE_LIFECYCLE)
                    .collect(),
            );
            plan.control(PeerControl::Disconnect).await;
            settle().await;
            plan.client_send_gate.add_permits(100);
            tokio::time::advance(Duration::from_secs(1)).await;
            settle().await;
            if queued_before_reconnect {
                assert!(matches!(
                    queued.await.unwrap_err().class(),
                    ClientFailureClass::StaleAuthority | ClientFailureClass::Cancelled
                ));
            } else {
                drop(queued);
            }
            if let Some(blocker) = blocker {
                assert!(blocker.await.unwrap().is_err());
            }
            assert_eq!(plan.opens.load(Ordering::SeqCst), 2);
            assert_eq!(queries.server_epoch().unwrap(), epoch);
            assert!(
                !queries
                    .capabilities()
                    .unwrap()
                    .supports(CAPABILITY_REMOTE_NATIVE_LIFECYCLE)
            );
            assert_eq!(
                queries
                    .native_account_runtime(b"refused after capability loss")
                    .await
                    .unwrap_err()
                    .class(),
                ClientFailureClass::MissingCapability
            );
            assert_eq!(runtime_requests(&plan).len(), before);
            assert_eq!(
                queries
                    .account_runtime(b"legacy still supported")
                    .await
                    .unwrap(),
                b"legacy still supported"
            );
            assert_eq!(runtime_requests(&plan).len(), before + 1);
            runtime.shutdown().await.unwrap();
        }
    }
}

#[tokio::test(start_paused = true)]
async fn account_runtime_deadline_removes_the_request_without_retrying() {
    for major in [1, 2] {
        let (server, runtime, journal, plan) = setup(major, None).await;
        let queries = server.queries(journal).unwrap();
        // More than the four simultaneous slots proves each deadline frees its entry.
        for expected in 1..=5 {
            let mut call = Box::pin(queries.account_runtime(b"expired"));
            poll_pending(call.as_mut());
            settle().await;
            tokio::time::advance(Duration::from_secs(6)).await;
            assert_eq!(
                call.await.unwrap_err().class(),
                ClientFailureClass::Unavailable
            );
            assert_eq!(runtime_requests(&plan).len(), expected);
            assert_eq!(plan.opens.load(Ordering::SeqCst), 1);
        }
        runtime.shutdown().await.unwrap();
    }
}

fn pairing_inventory_reply(payload: &Payload, target: &ConnectionTarget) -> Option<Payload> {
    macro_rules! reply {
        ($p:ident, $variant:ident, $query:ident) => {{
            if !matches!($query.action, Some($p::pairing_request::Action::List(true)))
                || $query.operation.is_some()
            {
                return None;
            }
            Some(Payload::$variant($p::Message {
                kind: Some($p::message::Kind::PairingOutcome($p::PairingOutcome {
                    request: $query.request,
                    request_generation: $query.request_generation,
                    inventory_version: if $query.inventory_after == 0 { 7 } else { 8 },
                    inventory_total: 2,
                    inventory_next: if $query.inventory_after == 0 { 1 } else { 0 },
                    this_device: Some(spaceterm_protocol::convert::identity::encode_global(
                        target.device().bytes(),
                    )),
                    devices: vec![$p::PairedDevice {
                        device: Some(spaceterm_protocol::convert::identity::encode_global(
                            target.device().bytes(),
                        )),
                        pairing: Some(spaceterm_protocol::convert::identity::encode_local(
                            if $query.inventory_after == 0 { 1 } else { 2 },
                        )),
                        ..Default::default()
                    }],
                    ..Default::default()
                })),
            }))
        }};
    }
    match payload {
        Payload::Major1(spaceterm_protocol::wire::v1::Message {
            kind: Some(spaceterm_protocol::wire::v1::message::Kind::PairingRequest(query)),
        }) => {
            use spaceterm_protocol::wire::v1;
            reply!(v1, Major1, query)
        }
        Payload::Major2(spaceterm_protocol::wire::v2::Message {
            kind: Some(spaceterm_protocol::wire::v2::message::Kind::PairingRequest(query)),
        }) => {
            use spaceterm_protocol::wire::v2;
            reply!(v2, Major2, query)
        }
        _ => None,
    }
}

#[tokio::test(start_paused = true)]
async fn paired_device_inventory_uses_the_authenticated_worker_in_both_majors() {
    for major in [1, 2] {
        let (server, runtime, journal, plan) = setup(major, Some(pairing_inventory_reply)).await;
        let page = server
            .queries(journal)
            .unwrap()
            .paired_devices(None, None)
            .await
            .unwrap();
        assert_eq!(page.version.get(), 7);
        assert_eq!(page.this_device, binding(1).target().device());
        assert_eq!(page.total, 2);
        assert_eq!(page.devices.len(), 1);
        assert_eq!(page.devices[0].parts().server, server.identity());
        assert_eq!(page.next, Some(page.devices[0].parts().pairing));
        assert_eq!(plan.opens.load(std::sync::atomic::Ordering::SeqCst), 1);
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn paired_device_inventory_never_combines_pages_from_different_revisions() {
    for major in [1, 2] {
        let (server, runtime, journal, _) = setup(major, Some(pairing_inventory_reply)).await;
        let queries = server.queries(journal).unwrap();
        let page = queries.paired_devices(None, None).await.unwrap();
        assert_eq!(
            queries.paired_devices(page.next, Some(page.version)).await,
            Err(spaceterm_client::management_cli::ManagementFailure::UnsafeNativeState)
        );
        runtime.shutdown().await.unwrap();
    }
}
