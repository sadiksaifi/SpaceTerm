use super::*;
use gpui::TestAppContext;
use spaceterm_client::{
    api::{ConnectionState, RetryState},
    failure::{ClientFailure, ClientFailureClass},
    pending::LocalUseInvalidator,
    ports::ClientFuture,
    replica::ReplicaState,
};
use spaceterm_core::{authority::GrantedControl, identity::WorkspaceId, pairing::LocalPairingKey};
use tokio::sync::watch;

use super::model::{ServerRequest, ServerStatus};
use crate::ui::pane_presentation::DisplayName;

struct Invalidator;
impl LocalUseInvalidator for Invalidator {
    fn invalidate_workspace(
        &self,
        _: &LocalPairingKey,
        _: WorkspaceId,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        Err(ClientFailure::new(ClientFailureClass::StaleAuthority))
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

struct Fixture {
    tokio: tokio::runtime::Runtime,
    server: Arc<spaceterm_client::testing::FakeServer>,
    secondary: Option<Arc<spaceterm_client::testing::FakeServer>>,
    journal: Arc<spaceterm_client::journal::JournalController>,
    tracker: OperationTracker,
    source: Entity<Servers>,
    connection: watch::Sender<ConnectionState>,
    replica: watch::Sender<ReplicaState>,
}
impl Fixture {
    fn new(cx: &mut TestAppContext) -> Self {
        Self::build(cx, false)
    }
    fn build(cx: &mut TestAppContext, lose_reply: bool) -> Self {
        Self::build_servers(cx, lose_reply, false)
    }
    fn build_servers(cx: &mut TestAppContext, lose_reply: bool, multiple: bool) -> Self {
        let tokio = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let pane = tokio.block_on(crate::ui::server_pane_session::tests::Fixture::new(false));
        let info = pane.channel.state().borrow().connection.unwrap();
        let journal = pane.peer.journal().clone();
        let tracker = OperationTracker::new(journal.clone());
        let operations: Arc<dyn OperationPeer> = if lose_reply {
            Arc::new(LostReplyPeer(
                pane.server.clone(),
                std::sync::atomic::AtomicBool::new(true),
            ))
        } else {
            pane.server.clone()
        };
        let (connection, state) = watch::channel(ConnectionState::Connected { info });
        let (replica, snapshots) = watch::channel(ReplicaState::Ready {
            snapshot: pane.server.replica().unwrap(),
        });
        let mut connections = vec![SettingsConnection {
            binding: pane.server.binding(1).unwrap(),
            name: DesktopServerInfo {
                server: info.server,
                name: DisplayName::reported("Studio Mac"),
                local: true,
            },
            state,
            replica: snapshots,
            queries: None,
            native: None,
            lifecycle: None,
            inventory: None,
        }];
        let secondary = multiple.then(|| {
            let server = spaceterm_client::testing::FakeServer::new(7, vec![2, 1]).unwrap();
            let peer = tokio
                .block_on(spaceterm_client::testing::FakePaneConnection::new(
                    server.clone(),
                    1,
                    2,
                ))
                .unwrap();
            let saved = peer.journal().snapshot().unwrap().bindings()[0].clone();
            tokio
                .block_on(journal.transact(vec![
                    spaceterm_client::journal::ClientJournalChange::SaveBinding(Box::new(saved)),
                ]))
                .unwrap();
            let (state_sender, state) = watch::channel(ConnectionState::Connected {
                info: server.connected_info(1, 1, 1, 2).unwrap(),
            });
            let (replica_sender, replica) = watch::channel(ReplicaState::Ready {
                snapshot: server.replica().unwrap(),
            });
            connections.push(SettingsConnection {
                binding: server.binding(1).unwrap(),
                name: DesktopServerInfo {
                    server: server.server(),
                    name: DisplayName::reported("Studio Mac"),
                    local: false,
                },
                state,
                replica,
                queries: None,
                native: None,
                lifecycle: None,
                inventory: None,
            });
            // The test has no changing second transport. Its retained watches still expose the exact frame.
            drop((state_sender, replica_sender));
            server
        });
        let operations: Arc<dyn OperationPeer> = if let Some(server) = &secondary {
            Arc::new(RoutingPeer(vec![pane.server.clone(), server.clone()]))
        } else {
            operations
        };
        let source = cx.update(|cx| {
            install(
                SettingsComposition {
                    connections,
                    tracker: tracker.clone(),
                    operations,
                    pending: PendingTasks::new(journal.clone(), Arc::new(Invalidator)),
                    executor: tokio.handle().clone(),
                },
                cx,
            )
        });
        cx.run_until_parked();
        Self {
            tokio,
            server: pane.server,
            secondary,
            journal,
            tracker,
            source,
            connection,
            replica,
        }
    }
    fn snapshot(&self, cx: &mut TestAppContext) -> model::ServersSnapshot {
        self.source
            .read_with(cx, |source, _| source.snapshot().clone())
    }
    fn send(&self, request: ServerRequest, cx: &mut TestAppContext) {
        let bound = BoundServerRequest::bind(request, &self.snapshot(cx)).unwrap();
        let requests = self.source.read_with(cx, |source, _| source.requests());
        cx.update(|cx| requests.send(bound, cx));
        self.drain(cx);
    }
    fn drain(&self, cx: &mut TestAppContext) {
        for _ in 0..100 {
            cx.run_until_parked();
            self.tokio.block_on(tokio::task::yield_now());
        }
        cx.run_until_parked();
    }
}

#[gpui::test]
fn servers_settings_runtime_publishes_canonical_live_settings(cx: &mut TestAppContext) {
    let fixture = Fixture::new(cx);
    let snapshot = fixture.snapshot(cx);
    assert_eq!(snapshot.servers.len(), 1);
    let server = &snapshot.servers[0];
    assert_eq!(
        server.pairing,
        fixture
            .server
            .binding(1)
            .unwrap()
            .target()
            .local_pairing()
            .unwrap()
    );
    assert_eq!(server.status, ServerStatus::Running);
    assert_eq!(
        server.worktree_location.as_ref().unwrap().revision,
        fixture
            .server
            .snapshot()
            .unwrap()
            .state()
            .settings
            .worktree_location
            .version
            .get()
    );
    fixture.connection.send_replace(ConnectionState::Offline {
        failure: ClientFailure::new(ClientFailureClass::Unavailable),
        retry: RetryState {
            failures: 1,
            delay: None,
        },
    });
    cx.run_until_parked();
    let offline = fixture.snapshot(cx);
    assert_eq!(offline.servers[0].status, ServerStatus::Offline);
    assert!(offline.servers[0].worktree_location.is_some());
    assert!(offline.servers[0].compatibility.is_none());
}

#[gpui::test]
fn servers_settings_runtime_retains_conflict_draft_and_current_version(cx: &mut TestAppContext) {
    let fixture = Fixture::new(cx);
    let server = fixture.snapshot(cx).servers.remove(0);
    fixture.send(
        ServerRequest::SetWorktreeLocation {
            server: server.key,
            template: "~/Worktrees/{repository}/{branch}".into(),
            base_revision: server.worktree_location.unwrap().revision + 1,
        },
        cx,
    );
    // The real durable operation runs on the recovered owner's executor.
    for _ in 0..100 {
        fixture.tokio.block_on(tokio::task::yield_now());
        cx.run_until_parked();
        if fixture.snapshot(cx).servers[0]
            .worktree_location
            .as_ref()
            .unwrap()
            .edit
            .as_ref()
            .is_some_and(|edit| edit.state == model::SettingEditState::Conflict)
        {
            break;
        }
    }
    let setting = fixture
        .snapshot(cx)
        .servers
        .remove(0)
        .worktree_location
        .unwrap();
    assert_eq!(
        setting.edit.unwrap().state,
        model::SettingEditState::Conflict
    );
    assert_eq!(
        setting.value.as_ref(),
        fixture
            .server
            .snapshot()
            .unwrap()
            .state()
            .settings
            .worktree_location
            .value
            .as_str()
    );
    assert_eq!(fixture.journal.snapshot().unwrap().operations().len(), 2);
}

#[gpui::test]
fn servers_settings_runtime_rejects_unassessed_end_all_and_protocol_only_host_actions(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new(cx);
    let key = fixture.snapshot(cx).servers[0].key.clone();
    fixture.send(ServerRequest::EndAllSessions(key.clone()), cx);
    assert_eq!(
        fixture.snapshot(cx).servers[0].management.failure,
        Some(ManagementFailure::ConfirmationRequired)
    );
    assert_eq!(fixture.journal.snapshot().unwrap().operations().len(), 1);
    for request in [
        ServerRequest::AssessStop(key.clone()),
        ServerRequest::AssessRestart(key),
    ] {
        fixture.send(request, cx);
        assert_eq!(
            fixture.snapshot(cx).servers[0].management.failure,
            Some(ManagementFailure::NativeUnavailable)
        );
        assert_eq!(fixture.journal.snapshot().unwrap().operations().len(), 1);
    }
}

#[gpui::test]
fn servers_settings_runtime_retains_exact_end_all_when_finalization_is_unavailable(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new(cx);
    let key = fixture.snapshot(cx).servers[0].key.clone();
    fixture.send(ServerRequest::AssessEndAllSessions(key.clone()), cx);
    let assessed = fixture.snapshot(cx).servers[0]
        .management
        .end_all
        .clone()
        .unwrap();
    assert_eq!(
        assessed.action(),
        &ManagementAction::EndAllSessions {
            server: fixture.server.server()
        }
    );
    let before = fixture.journal.snapshot().unwrap().operations().len();
    fixture.send(ServerRequest::EndAllSessions(key), cx);
    assert_eq!(
        fixture.journal.snapshot().unwrap().operations().len(),
        before + 1
    );
    let source = fixture.server.replica().unwrap();
    fixture
        .replica
        .send_replace(ReplicaState::Ready { snapshot: source });
    cx.run_until_parked();
    let snapshot = fixture.snapshot(cx);
    let operation = snapshot.servers[0].management.operation.unwrap();
    let record = fixture
        .journal
        .snapshot()
        .unwrap()
        .operation(operation)
        .unwrap()
        .clone();
    let canonical =
        CanonicalOperationRequest::decode(&record.request, fixture.journal.codec()).unwrap();
    assert_eq!(canonical.command(), &assessed.command());
    assert!(matches!(record.state, RetainedOperationState::Pending));
    let key = snapshot.servers[0].key.clone();
    fixture.send(ServerRequest::EndAllSessions(key), cx);
    assert_eq!(
        fixture.journal.snapshot().unwrap().operations().len(),
        before + 1
    );
}

#[gpui::test]
fn servers_settings_runtime_does_not_execute_an_obsolete_canonical_binding(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new(cx);
    let summary = fixture.snapshot(cx).servers.remove(0);
    let request = BoundServerRequest::bind(
        ServerRequest::SetWorktreeLocation {
            server: summary.key,
            template: "~/Worktrees/{repository}/{branch}".into(),
            base_revision: summary.worktree_location.unwrap().revision,
        },
        &fixture.snapshot(cx),
    )
    .unwrap();
    fixture.connection.send_replace(ConnectionState::Stopped);
    cx.run_until_parked();
    let requests = fixture.source.read_with(cx, |source, _| source.requests());
    cx.update(|cx| requests.send(request, cx));
    fixture.drain(cx);
    assert_eq!(fixture.journal.snapshot().unwrap().operations().len(), 1);
    assert_eq!(
        fixture.snapshot(cx).servers[0].status,
        ServerStatus::Stopped
    );
}

struct LostReplyPeer(
    Arc<spaceterm_client::testing::FakeServer>,
    std::sync::atomic::AtomicBool,
);
impl OperationPeer for LostReplyPeer {
    fn validate_submission(
        &self,
        binding: &PairedServerBinding,
        command: &Command,
        workspace: Option<WorkspaceId>,
    ) -> Result<(), ClientFailure> {
        self.0.validate_submission(binding, command, workspace)
    }
    fn submit<'a>(&'a self, request: &'a CanonicalOperationRequest) -> ClientFuture<'a, ()> {
        self.0.submit(request)
    }
    fn lookup(
        &self,
        binding: spaceterm_core::operations::OperationBinding,
    ) -> ClientFuture<'_, spaceterm_core::operations::OperationLookup> {
        Box::pin(async move {
            if self.1.swap(false, std::sync::atomic::Ordering::AcqRel) {
                Err(ClientFailure::new(ClientFailureClass::Unavailable))
            } else {
                self.0.lookup(binding).await
            }
        })
    }
    fn acknowledge(
        &self,
        stream: spaceterm_core::identity::OperationStreamId,
        through: u64,
    ) -> ClientFuture<'_, ()> {
        self.0.acknowledge(stream, through)
    }
}

#[gpui::test]
fn servers_settings_runtime_lost_reply_keeps_one_original_operation(cx: &mut TestAppContext) {
    let fixture = Fixture::build(cx, true);
    let server = fixture.snapshot(cx).servers.remove(0);
    let request = ServerRequest::SetWorktreeLocation {
        server: server.key.clone(),
        template: "~/Worktrees/{repository}/{branch}".into(),
        base_revision: server.worktree_location.unwrap().revision,
    };
    fixture.send(request.clone(), cx);
    let snapshot = fixture.snapshot(cx);
    let operation = snapshot.servers[0].management.operation.unwrap();
    assert_eq!(
        snapshot.servers[0]
            .worktree_location
            .as_ref()
            .unwrap()
            .edit
            .as_ref()
            .unwrap()
            .state,
        SettingEditState::Saving
    );
    fixture.send(request, cx);
    assert_eq!(
        fixture.snapshot(cx).servers[0].management.operation,
        Some(operation)
    );
    assert_eq!(fixture.journal.snapshot().unwrap().operations().len(), 2);
    let binding = fixture
        .journal
        .snapshot()
        .unwrap()
        .operation(operation)
        .unwrap()
        .binding;
    fixture.tokio.block_on(async {
        let lookup = fixture.server.lookup(binding).await.unwrap();
        fixture
            .tracker
            .resolve_bound(binding, lookup, fixture.server.as_ref())
            .await
            .unwrap();
    });
    let publication = fixture.server.replica().unwrap();
    let revision = publication.cursor.revision;
    fixture.replica.send_replace(ReplicaState::Ready {
        snapshot: publication,
    });
    fixture.connection.send_modify(|state| {
        if let ConnectionState::Connected { info } = state {
            info.organization_revision = revision;
        }
    });
    fixture.drain(cx);
    assert!(
        fixture.snapshot(cx).servers[0]
            .worktree_location
            .as_ref()
            .unwrap()
            .edit
            .is_none()
    );
    assert_eq!(fixture.journal.snapshot().unwrap().operations().len(), 2);
}

fn revocation(fixture: &Fixture) -> PendingRevocationTask {
    let state = fixture.journal.snapshot().unwrap();
    let saved = &state.bindings()[0];
    let binding = saved.binding().unwrap();
    let target = binding.target();
    let operation = OperationId::new(
        saved.streams().parts().self_revocation,
        OperationSequence::new(1).unwrap(),
    );
    let target_record = PairingTarget::new(spaceterm_core::pairing::PairingTargetParts {
        server: target.server(),
        pairing: target.pairing(),
        device: target.device(),
    })
    .unwrap();
    let request = CanonicalOperationRequest::new(
        operation,
        Command::Pairing(PairingCommand::SelfRevoke {
            target: target_record,
        }),
        fixture.journal.codec(),
    )
    .unwrap();
    let original = PendingSelfRevocation::new(PendingParts {
        original_server: target.server(),
        original_pairing: target.pairing(),
        device: target.device(),
        client: target.client(),
        operation,
        route: spaceterm_core::pairing::RevocationRouteReference::new([7; 16]),
        credential: target.credential(),
        notice_dismissed: false,
    })
    .unwrap();
    PendingRevocationTask::new(PendingRevocationTaskParts {
        original,
        local_pairing: target.local_pairing().unwrap(),
        pinned_server_public_key: target.pin().public_key,
        request_digest: request.binding().digest,
    })
    .unwrap()
}

#[gpui::test]
fn servers_settings_runtime_suppresses_pending_self_removal_and_dismisses_only_its_notice(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new(cx);
    let task = revocation(&fixture);
    let operation = task.parts().original.operation();
    let pending = PendingTasks::new(fixture.journal.clone(), Arc::new(Invalidator));
    fixture
        .tokio
        .block_on(pending.record_self_revocation(&fixture.server.binding(1).unwrap(), task))
        .unwrap();
    fixture.drain(cx);
    let snapshot = fixture.snapshot(cx);
    assert!(snapshot.servers.is_empty());
    assert_eq!(snapshot.pending_unpairs.len(), 1);
    assert_eq!(snapshot.pending_unpairs[0].operation, operation);
    fixture.send(
        ServerRequest::DismissUnpairNotice(snapshot.pending_unpairs[0].id.clone()),
        cx,
    );
    assert!(fixture.snapshot(cx).pending_unpairs.is_empty());
    let journal = fixture.journal.snapshot().unwrap();
    assert_eq!(journal.revocations().len(), 1);
    assert_eq!(
        journal.revocations()[0].task.parts().original.operation(),
        operation
    );
    assert!(journal.revocations()[0].notice_dismissed);
    assert!(
        !fixture
            .journal
            .ordinary_use_allowed(&fixture.server.binding(1).unwrap())
            .unwrap()
    );
}

struct RoutingPeer(Vec<Arc<spaceterm_client::testing::FakeServer>>);
impl RoutingPeer {
    fn server(
        &self,
        server: ServerId,
    ) -> Result<&spaceterm_client::testing::FakeServer, ClientFailure> {
        self.0
            .iter()
            .find(|peer| peer.server() == server)
            .map(Arc::as_ref)
            .ok_or(ClientFailure::new(ClientFailureClass::TargetNotFound))
    }
}
impl OperationPeer for RoutingPeer {
    fn validate_submission(
        &self,
        binding: &PairedServerBinding,
        command: &Command,
        workspace: Option<WorkspaceId>,
    ) -> Result<(), ClientFailure> {
        self.server(binding.target().server())?
            .validate_submission(binding, command, workspace)
    }
    fn submit<'a>(&'a self, request: &'a CanonicalOperationRequest) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            self.server(request.binding().id.stream().server())?
                .submit(request)
                .await
        })
    }
    fn lookup(
        &self,
        binding: spaceterm_core::operations::OperationBinding,
    ) -> ClientFuture<'_, spaceterm_core::operations::OperationLookup> {
        Box::pin(async move {
            self.server(binding.id.stream().server())?
                .lookup(binding)
                .await
        })
    }
    fn acknowledge(
        &self,
        stream: spaceterm_core::identity::OperationStreamId,
        through: u64,
    ) -> ClientFuture<'_, ()> {
        Box::pin(async move {
            self.server(stream.server())?
                .acknowledge(stream, through)
                .await
        })
    }
}

#[gpui::test]
fn servers_settings_runtime_two_servers_keep_the_captured_canonical_target(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::build_servers(cx, false, true);
    let mut snapshot = fixture.snapshot(cx);
    let server = snapshot
        .servers
        .iter()
        .find(|server| server.pairing.parts().server == fixture.server.server())
        .unwrap();
    let request = BoundServerRequest::bind(
        ServerRequest::SetWorktreeLocation {
            server: server.key.clone(),
            template: "~/Worktrees/{repository}/{branch}".into(),
            base_revision: server.worktree_location.as_ref().unwrap().revision,
        },
        &snapshot,
    )
    .unwrap();
    let other = fixture.secondary.as_ref().unwrap();
    let untouched = other
        .snapshot()
        .unwrap()
        .state()
        .settings
        .worktree_location
        .clone();
    snapshot.servers.reverse();
    fixture
        .source
        .update(cx, |source, cx| source.publish(snapshot, cx));
    let requests = fixture.source.read_with(cx, |source, _| source.requests());
    cx.update(|cx| requests.send(request, cx));
    fixture.drain(cx);
    assert_eq!(
        fixture
            .server
            .snapshot()
            .unwrap()
            .state()
            .settings
            .worktree_location
            .value
            .as_str(),
        "~/Worktrees/{repository}/{branch}"
    );
    assert_eq!(
        other.snapshot().unwrap().state().settings.worktree_location,
        untouched
    );
}

#[gpui::test]
fn servers_settings_runtime_invalid_template_retains_a_classified_draft_without_submission(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new(cx);
    let server = fixture.snapshot(cx).servers.remove(0);
    fixture.send(
        ServerRequest::SetWorktreeLocation {
            server: server.key,
            template: "../outside/{branch}".into(),
            base_revision: server.worktree_location.unwrap().revision,
        },
        cx,
    );
    let setting = fixture
        .snapshot(cx)
        .servers
        .remove(0)
        .worktree_location
        .unwrap();
    assert_eq!(
        setting.edit.unwrap().state,
        SettingEditState::Rejected(SettingRejection::InvalidTemplate(
            crate::worktrees::path_template::WorktreePathTemplateError::NotAbsolute
        ))
    );
    assert_eq!(fixture.journal.snapshot().unwrap().operations().len(), 1);
}

#[gpui::test]
fn servers_settings_runtime_applied_setting_keeps_its_draft_until_the_matching_publication(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new(cx);
    let server = fixture.snapshot(cx).servers.remove(0);
    fixture.send(
        ServerRequest::SetWorktreeLocation {
            server: server.key,
            template: "~/Worktrees/{repository}/{branch}".into(),
            base_revision: server.worktree_location.unwrap().revision,
        },
        cx,
    );
    let before = fixture
        .snapshot(cx)
        .servers
        .remove(0)
        .worktree_location
        .unwrap();
    assert_eq!(before.edit.unwrap().state, SettingEditState::Saving);
    let publication = fixture.server.replica().unwrap();
    let revision = publication.cursor.revision;
    fixture.replica.send_replace(ReplicaState::Ready {
        snapshot: publication,
    });
    fixture.connection.send_modify(|state| {
        if let ConnectionState::Connected { info } = state {
            info.organization_revision = revision;
        }
    });
    fixture.drain(cx);
    let applied = fixture
        .snapshot(cx)
        .servers
        .remove(0)
        .worktree_location
        .unwrap();
    assert!(applied.edit.is_none());
    assert_eq!(applied.value.as_ref(), "~/Worktrees/{repository}/{branch}");
}
