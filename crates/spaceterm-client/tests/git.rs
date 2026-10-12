use spaceterm_client::git::{replica::*, rpc::*};
use spaceterm_core::{
    authority::{ConnectionGeneration, FieldVersion, OrganizationRevision, ServerEpoch},
    identity::*,
    operations::{Command, OperationId},
    organization::ServerDirectory,
    worktrees::{queries::*, *},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn server() -> ServerId {
    ServerId::new([1; 16])
}
fn binding() -> GitConnectionState {
    GitConnectionState {
        context: spaceterm_core::server_directories::DirectoryContext::new(
            spaceterm_core::server_directories::DirectoryContextParts {
                server: server(),
                epoch: ServerEpoch::new(7),
                pairing: PairingId::new(server(), 3).unwrap(),
                client: ClientId::new(server(), 4).unwrap(),
            },
        )
        .unwrap(),
        generation: ConnectionGeneration::new(1),
        readiness: GitReadiness::Ready,
    }
}
struct Connection {
    state: GitConnectionState,
    submissions: AtomicUsize,
}
impl GitConnectionPort for Connection {
    fn state(&self) -> Result<GitConnectionState, ClientGitFailure> {
        Ok(self.state)
    }
    fn query(&self, _: GitQuery) -> GitClientFuture<'_, Result<GitReadResult, ClientGitFailure>> {
        Box::pin(async { Err(ClientGitFailure::Remote(GitFailure::ToolMissing)) })
    }
    fn subscribe(
        &self,
        _: GitStatusTarget,
    ) -> Result<Box<dyn GitSubscriptionPort>, ClientGitFailure> {
        Err(ClientGitFailure::Unavailable)
    }
    fn submit(
        &self,
        _: Command,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>> {
        self.submissions.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(ClientGitFailure::Unavailable) })
    }
    fn lookup(
        &self,
        _: OperationId,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>> {
        Box::pin(async { Err(ClientGitFailure::Unavailable) })
    }
}
fn command() -> GitCommand {
    GitCommand::RemoveConfirmed {
        assessment: GitAssessmentToken::new(GitAssessmentTokenParts {
            server: server(),
            epoch: ServerEpoch::new(7),
            pairing: PairingId::new(server(), 3).unwrap(),
            client: ClientId::new(server(), 4).unwrap(),
            kind: GitAssessmentKind::Remove,
            random: [9; 32],
        })
        .unwrap(),
    }
}

#[tokio::test]
async fn offline_new_write_is_rejected_without_operation_allocation() {
    let mut state = binding();
    state.readiness = GitReadiness::Offline;
    let connection = Arc::new(Connection {
        state,
        submissions: AtomicUsize::new(0),
    });
    let client = GitClient::new(server(), connection.clone()).unwrap();
    assert!(matches!(
        client.submit(command()).await,
        Err(ClientGitFailure::Offline)
    ));
    assert_eq!(connection.submissions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn assessment_from_another_client_never_enters_tracker() {
    let connection = Arc::new(Connection {
        state: binding(),
        submissions: AtomicUsize::new(0),
    });
    let client = GitClient::new(server(), connection.clone()).unwrap();
    let mut parts = command().assessment().parts().clone();
    parts.client = ClientId::new(server(), 99).unwrap();
    let wrong = GitCommand::RemoveConfirmed {
        assessment: GitAssessmentToken::new(parts).unwrap(),
    };
    assert!(matches!(
        client.submit(wrong).await,
        Err(ClientGitFailure::Unauthorized)
    ));
    assert_eq!(connection.submissions.load(Ordering::SeqCst), 0);
}

fn read(version: u64, next: Option<GitPageCursor>, id: u64) -> GitReadResult {
    let scope = spaceterm_core::operations::RepositoryScope {
        server: server(),
        repository: RepositoryId::new(server(), 1).unwrap(),
    };
    GitReadResult {
        epoch: ServerEpoch::new(7),
        scope: Some(scope),
        result: GitQueryResult::Inventory(GitInventoryPage {
            records: vec![GitInventoryRecord {
                worktree: WorktreeId::new(server(), id).unwrap(),
                directory: ServerDirectory::new(server(), format!("/server/{id}").into_bytes())
                    .unwrap(),
                main: false,
                missing: false,
                listed: true,
                version: FieldVersion::new(1),
            }],
            next,
            version: OrganizationRevision::new(version),
        }),
    }
}

#[test]
fn paged_inventory_never_mixes_revisions_or_exposes_partial_replacement() {
    let workspace = WorkspaceId::new(server(), 1).unwrap();
    let mut replica = GitInventoryReplica::new(workspace);
    let ticket = replica.begin(ServerEpoch::new(7)).unwrap();
    replica
        .accept(
            ticket,
            &GitQuery::Inventory {
                workspace,
                cursor: None,
            },
            read(2, None, 1),
        )
        .unwrap();
    let scope = read(2, None, 1).scope.unwrap();
    let cursor = GitPageCursor::new(GitPageCursorParts {
        scope,
        epoch: ServerEpoch::new(7),
        observation: 3,
        random: [5; 32],
    })
    .unwrap();
    let ticket = replica.begin(ServerEpoch::new(7)).unwrap();
    assert_eq!(
        replica.accept(
            ticket,
            &GitQuery::Inventory {
                workspace,
                cursor: None
            },
            read(3, Some(cursor.clone()), 2)
        ),
        Ok(InventoryProgress::More(cursor.clone()))
    );
    assert_eq!(replica.records()[0].worktree.local(), 1);
    assert_eq!(
        replica.accept(
            ticket,
            &GitQuery::Inventory {
                workspace,
                cursor: Some(cursor)
            },
            read(4, None, 3)
        ),
        Err(ClientGitFailure::ResyncRequired)
    );
    assert_eq!(replica.records()[0].worktree.local(), 1);
    assert!(replica.needs_resync());
}

#[tokio::test]
async fn old_epoch_cursor_is_rejected_before_connection_read() {
    let connection = Arc::new(Connection {
        state: binding(),
        submissions: AtomicUsize::new(0),
    });
    let client = GitClient::new(server(), connection).unwrap();
    let scope = read(2, None, 1).scope.unwrap();
    let cursor = GitPageCursor::new(GitPageCursorParts {
        scope,
        epoch: ServerEpoch::new(6),
        observation: 2,
        random: [5; 32],
    })
    .unwrap();
    assert_eq!(
        client
            .query(GitQuery::Branches {
                repository: scope,
                cursor: Some(cursor)
            })
            .await,
        Err(ClientGitFailure::Remote(GitFailure::StaleAssessment))
    );
}

struct Reader {
    state: std::sync::Mutex<GitConnectionState>,
    result: GitReadResult,
    reconnect: bool,
}
impl GitConnectionPort for Reader {
    fn state(&self) -> Result<GitConnectionState, ClientGitFailure> {
        Ok(*self.state.lock().unwrap())
    }
    fn query(&self, _: GitQuery) -> GitClientFuture<'_, Result<GitReadResult, ClientGitFailure>> {
        if self.reconnect {
            self.state.lock().unwrap().generation = ConnectionGeneration::new(2);
        }
        Box::pin(async { Ok(self.result.clone()) })
    }
    fn subscribe(
        &self,
        _: GitStatusTarget,
    ) -> Result<Box<dyn GitSubscriptionPort>, ClientGitFailure> {
        Err(ClientGitFailure::Unavailable)
    }
    fn submit(
        &self,
        _: Command,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>> {
        Box::pin(async { Err(ClientGitFailure::Unavailable) })
    }
    fn lookup(
        &self,
        _: OperationId,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>> {
        Box::pin(async { Err(ClientGitFailure::Unavailable) })
    }
}
#[tokio::test]
async fn final_inventory_page_cannot_redirect_original_cursor_scope() {
    let scope = read(2, None, 1).scope.unwrap();
    let cursor = GitPageCursor::new(GitPageCursorParts {
        scope,
        epoch: ServerEpoch::new(7),
        observation: 2,
        random: [5; 32],
    })
    .unwrap();
    let mut result = read(2, None, 1);
    result.scope.as_mut().unwrap().repository = RepositoryId::new(server(), 9).unwrap();
    let connection = Arc::new(Reader {
        state: std::sync::Mutex::new(binding()),
        result,
        reconnect: false,
    });
    let client = GitClient::new(server(), connection).unwrap();
    assert_eq!(
        client
            .query(GitQuery::Inventory {
                workspace: WorkspaceId::new(server(), 1).unwrap(),
                cursor: Some(cursor)
            })
            .await,
        Err(ClientGitFailure::ResyncRequired)
    );
}
#[tokio::test]
async fn late_read_from_previous_connection_generation_is_rejected() {
    let connection = Arc::new(Reader {
        state: std::sync::Mutex::new(binding()),
        result: read(2, None, 1),
        reconnect: true,
    });
    let client = GitClient::new(server(), connection).unwrap();
    assert_eq!(
        client
            .query(GitQuery::Inventory {
                workspace: WorkspaceId::new(server(), 1).unwrap(),
                cursor: None
            })
            .await,
        Err(ClientGitFailure::StaleResponse)
    );
}

fn operation() -> OperationId {
    OperationId::new(
        OperationStreamId::new(
            OperationStreamKey::new(server(), 1).unwrap(),
            PairingId::new(server(), 3).unwrap(),
            ClientId::new(server(), 4).unwrap(),
        )
        .unwrap(),
        spaceterm_core::operations::OperationSequence::new(1).unwrap(),
    )
}
struct Tracker {
    id: OperationId,
    command: GitCommand,
    result: std::sync::Mutex<GitOperationState>,
}
impl GitOperationPort for Tracker {
    fn id(&self) -> OperationId {
        self.id
    }
    fn command(&self) -> &GitCommand {
        &self.command
    }
    fn state(&self) -> GitClientFuture<'_, Result<GitOperationState, ClientGitFailure>> {
        Box::pin(async { Ok(self.result.lock().unwrap().clone()) })
    }
}
struct TrackingConnection {
    state: std::sync::Mutex<GitConnectionState>,
    tracker: Arc<Tracker>,
    retained_outcome: Option<GitMutationOutcome>,
    submits: AtomicUsize,
    lookups: AtomicUsize,
    disconnect: bool,
}
impl GitConnectionPort for TrackingConnection {
    fn state(&self) -> Result<GitConnectionState, ClientGitFailure> {
        Ok(*self.state.lock().unwrap())
    }
    fn query(&self, _: GitQuery) -> GitClientFuture<'_, Result<GitReadResult, ClientGitFailure>> {
        Box::pin(async { Err(ClientGitFailure::Unavailable) })
    }
    fn subscribe(
        &self,
        _: GitStatusTarget,
    ) -> Result<Box<dyn GitSubscriptionPort>, ClientGitFailure> {
        Err(ClientGitFailure::Unavailable)
    }
    fn submit(
        &self,
        command: Command,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>> {
        assert_eq!(command, Command::Worktree(self.tracker.command.clone()));
        self.submits.fetch_add(1, Ordering::SeqCst);
        if self.disconnect {
            self.state.lock().unwrap().readiness = GitReadiness::Offline;
        }
        Box::pin(async { Ok(self.tracker.clone() as Arc<dyn GitOperationPort>) })
    }
    fn lookup(
        &self,
        id: OperationId,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>> {
        assert_eq!(id, self.tracker.id);
        self.lookups.fetch_add(1, Ordering::SeqCst);
        // The provider retained completion while its acknowledgement was lost.
        // Lookup updates the original tracker instead of allocating or submitting.
        if let Some(outcome) = &self.retained_outcome {
            *self.tracker.result.lock().unwrap() = GitOperationState::Completed(outcome.clone());
        }
        Box::pin(async { Ok(self.tracker.clone() as Arc<dyn GitOperationPort>) })
    }
}
// Facade coverage at the frozen connection port. Authenticated worker/journal
// lost-acknowledgement coverage remains a separate real-composition gate.
#[tokio::test]
async fn reconnect_facade_observes_retained_completion_without_second_submission() {
    let outcome = GitMutationOutcome::new(GitMutationOutcomeParts::Removed {
        worktree: WorktreeId::new(server(), 9).unwrap(),
        closed_tabs: 1,
        ended_sessions: 2,
        revision: OrganizationRevision::new(42),
    })
    .unwrap();
    let connection = Arc::new(TrackingConnection {
        state: std::sync::Mutex::new(binding()),
        tracker: Arc::new(Tracker {
            id: operation(),
            command: command(),
            result: std::sync::Mutex::new(GitOperationState::Unresolved),
        }),
        retained_outcome: Some(outcome.clone()),
        submits: AtomicUsize::new(0),
        lookups: AtomicUsize::new(0),
        disconnect: true,
    });
    let client = GitClient::new(server(), connection.clone()).unwrap();
    let tracked = client.submit(command()).await.unwrap();
    assert_eq!(tracked.id(), operation());
    assert_eq!(tracked.state().await, Ok(GitOperationState::Unresolved));
    let mut reconnected = binding();
    reconnected.generation = ConnectionGeneration::new(2);
    *connection.state.lock().unwrap() = reconnected;
    let resolved = client.resume(tracked.id()).await.unwrap();
    assert_eq!(resolved.id(), operation());
    assert_eq!(
        resolved.state().await,
        Ok(GitOperationState::Completed(outcome.clone()))
    );
    assert_eq!(
        tracked.state().await,
        Ok(GitOperationState::Completed(outcome))
    );
    assert_eq!(connection.submits.load(Ordering::SeqCst), 1);
    assert_eq!(connection.lookups.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn retained_outcome_from_previous_epoch_is_lookup_not_a_new_confirmation() {
    let mut parts = command().assessment().parts().clone();
    parts.epoch = ServerEpoch::new(6);
    let historical = GitCommand::RemoveConfirmed {
        assessment: GitAssessmentToken::new(parts).unwrap(),
    };
    let outcome = GitMutationOutcome::new(GitMutationOutcomeParts::Removed {
        worktree: WorktreeId::new(server(), 9).unwrap(),
        closed_tabs: 1,
        ended_sessions: 2,
        revision: OrganizationRevision::new(42),
    })
    .unwrap();
    let connection = Arc::new(TrackingConnection {
        state: std::sync::Mutex::new(binding()),
        tracker: Arc::new(Tracker {
            id: operation(),
            command: historical,
            result: std::sync::Mutex::new(GitOperationState::Completed(outcome.clone())),
        }),
        retained_outcome: None,
        submits: AtomicUsize::new(0),
        lookups: AtomicUsize::new(0),
        disconnect: false,
    });
    let client = GitClient::new(server(), connection.clone()).unwrap();
    let tracked = client.resume(operation()).await.unwrap();
    assert_eq!(
        tracked.state().await,
        Ok(GitOperationState::Completed(outcome))
    );
    assert_eq!(connection.submits.load(Ordering::SeqCst), 0);
}
#[test]
fn status_failure_keeps_last_successful_facts_and_stale_revision_cannot_replace_them() {
    let target = GitStatusTarget::Pane(PaneId::new(server(), 1).unwrap());
    let mut replica = GitStatusReplica::new(target, ServerEpoch::new(7));
    let result = GitReadResult {
        epoch: ServerEpoch::new(7),
        scope: read(1, None, 1).scope,
        result: GitQueryResult::Status(GitStatusFacts {
            revision: std::num::NonZeroU64::new(3).unwrap(),
            branch: Some(spaceterm_core::organization::BranchName::new("topic".into()).unwrap()),
            changes: vec![],
            dirty: false,
            unchecked: false,
            pull_request: None,
            upstream: None,
            operation: None,
            counts: None,
        }),
    };
    replica.accept(result.clone()).unwrap();
    replica.mark_last_known();
    assert_eq!(replica.freshness(), GitFreshness::LastKnown);
    assert_eq!(
        replica.facts().unwrap().branch.as_ref().unwrap().as_str(),
        "topic"
    );
    assert_eq!(replica.accept(result), Err(ClientGitFailure::StaleResponse));
    assert_eq!(replica.freshness(), GitFreshness::LastKnown);
}
#[test]
fn superseded_inventory_ticket_cannot_cancel_current_refresh() {
    let workspace = WorkspaceId::new(server(), 1).unwrap();
    let mut replica = GitInventoryReplica::new(workspace);
    let old = replica.begin(ServerEpoch::new(7)).unwrap();
    let current = replica.begin(ServerEpoch::new(7)).unwrap();
    let query = GitQuery::Inventory {
        workspace,
        cursor: None,
    };
    assert_eq!(
        replica.accept(old, &query, read(2, None, 1)),
        Err(ClientGitFailure::StaleResponse)
    );
    assert_eq!(
        replica.accept(current, &query, read(3, None, 2)),
        Ok(InventoryProgress::Complete)
    );
    assert_eq!(replica.records()[0].worktree.local(), 2);
}
#[test]
fn inventory_refresh_cannot_clear_resync_below_observed_organization_revision() {
    let workspace = WorkspaceId::new(server(), 1).unwrap();
    let mut replica = GitInventoryReplica::new(workspace);
    let query = GitQuery::Inventory {
        workspace,
        cursor: None,
    };
    let ticket = replica.begin(ServerEpoch::new(7)).unwrap();
    replica.accept(ticket, &query, read(2, None, 1)).unwrap();
    replica.organization_changed(
        ServerEpoch::new(7),
        OrganizationRevision::new(2),
        OrganizationRevision::new(3),
    );
    let ticket = replica.begin(ServerEpoch::new(7)).unwrap();
    assert_eq!(
        replica.accept(ticket, &query, read(2, None, 1)),
        Err(ClientGitFailure::ResyncRequired)
    );
    assert!(replica.needs_resync());
    assert_eq!(replica.version(), Some(OrganizationRevision::new(2)));
    assert_eq!(replica.records()[0].worktree.local(), 1);

    let ticket = replica.begin(ServerEpoch::new(7)).unwrap();
    assert_eq!(
        replica.accept(ticket, &query, read(3, None, 2)),
        Ok(InventoryProgress::Complete)
    );
    assert!(!replica.needs_resync());
    assert_eq!(replica.version(), Some(OrganizationRevision::new(3)));
    assert_eq!(replica.records()[0].worktree.local(), 2);
}

#[test]
fn late_organization_revision_cannot_lower_inventory_refresh_fence() {
    let workspace = WorkspaceId::new(server(), 1).unwrap();
    let mut replica = GitInventoryReplica::new(workspace);
    let query = GitQuery::Inventory {
        workspace,
        cursor: None,
    };
    replica.organization_changed(
        ServerEpoch::new(7),
        OrganizationRevision::new(4),
        OrganizationRevision::new(5),
    );
    replica.organization_changed(
        ServerEpoch::new(7),
        OrganizationRevision::new(2),
        OrganizationRevision::new(3),
    );
    let ticket = replica.begin(ServerEpoch::new(7)).unwrap();
    assert_eq!(
        replica.accept(ticket, &query, read(4, None, 1)),
        Err(ClientGitFailure::ResyncRequired)
    );
    assert!(replica.records().is_empty());
    assert!(replica.needs_resync());

    let ticket = replica.begin(ServerEpoch::new(7)).unwrap();
    assert_eq!(
        replica.accept(ticket, &query, read(6, None, 2)),
        Ok(InventoryProgress::Complete)
    );
    assert!(!replica.needs_resync());
}

#[test]
fn new_epoch_inventory_fence_rejects_old_epoch_without_reusing_revision_floor() {
    let workspace = WorkspaceId::new(server(), 1).unwrap();
    let mut replica = GitInventoryReplica::new(workspace);
    let query = GitQuery::Inventory {
        workspace,
        cursor: None,
    };
    let ticket = replica.begin(ServerEpoch::new(7)).unwrap();
    replica.accept(ticket, &query, read(40, None, 1)).unwrap();
    replica.organization_changed(
        ServerEpoch::new(8),
        OrganizationRevision::new(0),
        OrganizationRevision::new(1),
    );
    assert_eq!(
        replica.begin(ServerEpoch::new(7)),
        Err(ClientGitFailure::StaleResponse)
    );
    assert!(replica.needs_resync());
    assert_eq!(replica.records()[0].worktree.local(), 1);

    let mut current = read(1, None, 2);
    current.epoch = ServerEpoch::new(8);
    let ticket = replica.begin(ServerEpoch::new(8)).unwrap();
    assert_eq!(
        replica.accept(ticket, &query, current),
        Ok(InventoryProgress::Complete)
    );
    replica.organization_changed(
        ServerEpoch::new(7),
        OrganizationRevision::new(99),
        OrganizationRevision::new(100),
    );
    assert!(!replica.needs_resync());
    assert_eq!(replica.version(), Some(OrganizationRevision::new(1)));
}

#[test]
fn inventory_gap_requests_authoritative_resync_without_mutating_saved_membership() {
    let workspace = WorkspaceId::new(server(), 1).unwrap();
    let mut replica = GitInventoryReplica::new(workspace);
    let ticket = replica.begin(ServerEpoch::new(7)).unwrap();
    replica
        .accept(
            ticket,
            &GitQuery::Inventory {
                workspace,
                cursor: None,
            },
            read(2, None, 1),
        )
        .unwrap();
    replica.organization_changed(
        ServerEpoch::new(7),
        OrganizationRevision::new(5),
        OrganizationRevision::new(6),
    );
    assert!(replica.needs_resync());
    assert_eq!(replica.records()[0].worktree.local(), 1);
}

struct Subscription {
    events: std::collections::VecDeque<GitSubscriptionEvent>,
}
impl GitSubscriptionPort for Subscription {
    fn next(
        &mut self,
    ) -> GitClientFuture<'_, Option<Result<GitSubscriptionEvent, ClientGitFailure>>> {
        let event = self.events.pop_front();
        Box::pin(async move { event.map(Ok) })
    }
}
struct Subscriber {
    state: std::sync::Mutex<GitConnectionState>,
}
impl GitConnectionPort for Subscriber {
    fn state(&self) -> Result<GitConnectionState, ClientGitFailure> {
        Ok(*self.state.lock().unwrap())
    }
    fn query(&self, _: GitQuery) -> GitClientFuture<'_, Result<GitReadResult, ClientGitFailure>> {
        Box::pin(async { Err(ClientGitFailure::Unavailable) })
    }
    fn subscribe(
        &self,
        _: GitStatusTarget,
    ) -> Result<Box<dyn GitSubscriptionPort>, ClientGitFailure> {
        Ok(Box::new(Subscription {
            events: std::collections::VecDeque::from([
                GitSubscriptionEvent::Status(Box::new(GitReadResult {
                    epoch: ServerEpoch::new(7),
                    scope: None,
                    result: GitQueryResult::Status(GitStatusFacts {
                        revision: std::num::NonZeroU64::new(1).unwrap(),
                        branch: None,
                        changes: vec![],
                        dirty: false,
                        unchecked: false,
                        pull_request: None,
                        upstream: None,
                        operation: None,
                        counts: None,
                    }),
                })),
                GitSubscriptionEvent::LastKnown(GitFailure::ToolMissing),
                GitSubscriptionEvent::ResyncRequired,
            ]),
        }))
    }
    fn submit(
        &self,
        _: Command,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>> {
        Box::pin(async { Err(ClientGitFailure::Unavailable) })
    }
    fn lookup(
        &self,
        _: OperationId,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>> {
        Box::pin(async { Err(ClientGitFailure::Unavailable) })
    }
}
#[tokio::test]
async fn subscription_delivers_status_and_last_known_then_rejects_replaced_connection() {
    let connection = Arc::new(Subscriber {
        state: std::sync::Mutex::new(binding()),
    });
    let client = GitClient::new(server(), connection.clone()).unwrap();
    let mut subscription = client
        .subscribe(GitStatusTarget::Pane(PaneId::new(server(), 1).unwrap()))
        .unwrap();
    assert!(matches!(
        subscription.next().await,
        Some(Ok(GitSubscriptionEvent::Status(_)))
    ));
    assert_eq!(
        subscription.next().await,
        Some(Ok(GitSubscriptionEvent::LastKnown(GitFailure::ToolMissing)))
    );
    connection.state.lock().unwrap().generation = ConnectionGeneration::new(2);
    assert_eq!(
        subscription.next().await,
        Some(Err(ClientGitFailure::StaleResponse))
    );
    assert_eq!(subscription.next().await, None);
}

#[test]
fn inventory_scope_count_plus_one_rejects_entire_refresh() {
    let workspace = WorkspaceId::new(server(), 1).unwrap();
    let mut replica = GitInventoryReplica::new(workspace);
    let ticket = replica.begin(ServerEpoch::new(7)).unwrap();
    let mut cursor = None;
    for index in 0..33 {
        let count = if index == 32 { 1 } else { 128 };
        let mut result = read(2, None, 1);
        let next = GitPageCursor::new(GitPageCursorParts {
            scope: result.scope.unwrap(),
            epoch: ServerEpoch::new(7),
            observation: 2,
            random: [index + 1; 32],
        })
        .unwrap();
        let GitQueryResult::Inventory(page) = &mut result.result else {
            unreachable!()
        };
        page.records = (0..count)
            .map(|i| GitInventoryRecord {
                worktree: WorktreeId::new(server(), index as u64 * 128 + i + 1).unwrap(),
                directory: ServerDirectory::new(server(), b"/server/root".to_vec()).unwrap(),
                main: false,
                missing: false,
                listed: true,
                version: FieldVersion::new(1),
            })
            .collect();
        page.next = Some(next.clone());
        let accepted = replica.accept(
            ticket,
            &GitQuery::Inventory {
                workspace,
                cursor: cursor.clone(),
            },
            result,
        );
        if index == 32 {
            assert_eq!(accepted, Err(ClientGitFailure::Capacity));
        } else {
            assert_eq!(accepted, Ok(InventoryProgress::More(next.clone())));
        }
        cursor = Some(next);
    }
    assert!(replica.records().is_empty());
    assert!(replica.needs_resync());
}

#[tokio::test]
async fn removal_read_requires_summary_for_the_requested_target() {
    let requested = WorktreeId::new(server(), 8).unwrap();
    for (target, expected) in [
        (None, Err(ClientGitFailure::InvalidResponse)),
        (
            Some(WorktreeId::new(server(), 9).unwrap()),
            Err(ClientGitFailure::InvalidResponse),
        ),
        (Some(requested), Ok(())),
    ] {
        let result = GitReadResult {
            epoch: ServerEpoch::new(7),
            scope: read(1, None, 1).scope,
            result: GitQueryResult::Assessment(Box::new(GitAssessmentPage {
                token: command().assessment().clone(),
                tabs: vec![],
                panes: vec![],
                fallbacks: vec![],
                next: None,
                requires_confirmation: true,
                creation_preview: None,
                removal: target.map(|worktree| GitRemovalSummary {
                    worktree,
                    directory: ServerDirectory::new(server(), b"/linked".to_vec()).unwrap(),
                    branch: None,
                    risk: ApprovedRemoval::DiscardChanges,
                }),
            })),
        };
        let client = GitClient::new(
            server(),
            Arc::new(Reader {
                state: std::sync::Mutex::new(binding()),
                result,
                reconnect: false,
            }),
        )
        .unwrap();
        assert_eq!(
            client
                .query(GitQuery::AssessRemove {
                    worktree: requested
                })
                .await
                .map(|_| ()),
            expected
        );
    }
}
