#[path = "operation_recovery/support.rs"]
mod durable;
mod support;

use spaceterm_client::{
    api::ConnectionTarget,
    directories::DirectoryClient,
    journal::{ClientJournalChange, JournalController, JournalLimits},
    registry::ClientRegistry,
};
use spaceterm_core::authority::ServerEpoch;
use spaceterm_core::server_directories::{DirectoryQueryAction, DirectoryQueryResult};
use spaceterm_protocol::{codec::Payload, convert::directories};
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
    plan.capabilities.extend([
        spaceterm_protocol::negotiation::CAPABILITY_SERVER_DIRECTORIES,
        spaceterm_protocol::negotiation::CAPABILITY_SERVER_GIT,
    ]);
    plan.capabilities.sort_unstable();
    plan.query_handler = reply;
    let plan = Arc::new(plan);
    let mut services = services(
        vec![binding.clone()],
        Arc::new(Peers::one(&binding, plan.clone())),
    );
    services.journal = journal.clone();
    services.additional_capabilities = vec![
        spaceterm_protocol::negotiation::CAPABILITY_SERVER_DIRECTORIES,
        spaceterm_protocol::negotiation::CAPABILITY_SERVER_GIT,
    ];
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
                })),
            },
            target.server(),
        )
        .unwrap(),
    )
}
fn status(server: spaceterm_core::identity::ServerId, revision: u64) -> ServerStatus {
    ServerStatus::new(ServerStatusParts {
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
    assert!(server.queries(journal).is_ok());
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
                request: ManagementRequestReference::Operation(operation),
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
