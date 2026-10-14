use super::*;
use gpui::TestAppContext;

#[path = "../server_client_runtime/composed/pending_pairing_tests.rs"]
mod pending_pairing;

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
        Self::build_sources(cx, lose_reply, multiple, false)
    }
    fn build_machine_servers(cx: &mut TestAppContext, multiple: bool) -> Self {
        Self::build_sources(cx, false, multiple, true)
    }
    fn build_ipc_machine_server(cx: &mut TestAppContext) -> Self {
        use spaceterm_protocol::negotiation::CAPABILITY_SERVER_MACHINE_FACTS;
        let fixture = Self::build_servers(cx, false, false);
        let ConnectionState::Connected { info } = fixture.connection.borrow().clone() else {
            panic!("connected fixture")
        };
        let negotiated = fixture
            .server
            .connected_info_with_capabilities(
                1,
                info.binding.generation.get(),
                info.attempt,
                info.major,
                &[CAPABILITY_SERVER_MACHINE_FACTS],
            )
            .unwrap();
        assert_eq!(negotiated.binding, info.binding);
        assert!(
            negotiated
                .capabilities
                .supports(CAPABILITY_SERVER_MACHINE_FACTS)
        );
        fixture
            .connection
            .send_replace(ConnectionState::Connected { info: negotiated });
        let runtime = cx.update(|cx| cx.global::<InstalledSettingsRuntime>().0.clone());
        {
            let mut sources = runtime.connections.borrow_mut();
            let source = &mut sources[0];
            assert!(matches!(
                source.binding.target().route().parts(),
                RouteParts::Ipc { .. }
            ));
            source.machine = Some(SettingsMachineFacts::from_server(&offered_machine(
                spaceterm_core::management::ServerPlatform::Linux,
                spaceterm_core::management::ServerPermissionState::NotNeeded,
                60606,
            )));
            assert!(source.machine.as_ref().unwrap().permissions.is_some());
            assert_eq!(source.machine.as_ref().unwrap().scopes.len(), 2);
        }
        fixture.drain(cx);
        fixture
    }
    fn build_sources(
        cx: &mut TestAppContext,
        lose_reply: bool,
        multiple: bool,
        remote_routes: bool,
    ) -> Self {
        let tokio = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let pane = tokio.block_on(crate::ui::server_pane_session::tests::Fixture::new(false));
        let info = pane.channel.state().borrow().connection.unwrap();
        let journal = if remote_routes {
            use spaceterm_client::journal::{
                ClientJournalChange, JournalController, JournalLimits,
            };
            let original = pane.peer.journal().snapshot().unwrap();
            let journal = Arc::new(
                tokio
                    .block_on(JournalController::recover(
                        Arc::new(spaceterm_client::testing::MemoryJournal::default()),
                        Arc::new(spaceterm_core::operation_ledger::canonical::CanonicalCodec),
                        Arc::new(FactsReceiptVerifier),
                        JournalLimits::default(),
                    ))
                    .unwrap(),
            );
            // Routes are chosen before initial storage. Saved bindings are immutable.
            let saved = saved_remote_route(
                &original.bindings()[0],
                RouteParts::Ssh {
                    destination: "studio".into(),
                    remote_socket: "/fixture/server.sock".into(),
                },
            );
            let mut changes = vec![ClientJournalChange::SaveBinding(Box::new(saved))];
            for operation in original.operations() {
                let mut pending = operation.clone();
                pending.state = RetainedOperationState::Pending;
                changes.push(ClientJournalChange::Prepare(pending));
                if let RetainedOperationState::Completed {
                    outcome,
                    publication_revision,
                    consumed,
                } = &operation.state
                {
                    changes.push(ClientJournalChange::Complete {
                        binding: operation.binding,
                        outcome: outcome.clone(),
                        publication_revision: *publication_revision,
                    });
                    if *consumed {
                        changes.push(ClientJournalChange::ConsumeResult(operation.binding.id));
                    }
                } else {
                    assert!(matches!(operation.state, RetainedOperationState::Pending));
                }
            }
            tokio.block_on(journal.transact(changes)).unwrap();
            journal
        } else {
            pane.peer.journal().clone()
        };
        let primary_binding = journal.snapshot().unwrap().bindings()[0].binding().unwrap();
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
            binding: primary_binding,
            name: DesktopServerInfo {
                server: info.server,
                name: DisplayName::reported("Studio Mac"),
                local: !remote_routes,
            },
            state,
            replica: snapshots,
            queries: None,
            native: None,
            lifecycle: None,
            inventory: None,
            machine: None,
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
            let saved = if remote_routes {
                saved_remote_route(
                    &saved,
                    RouteParts::Wss {
                        address: "127.0.0.1:443".parse().unwrap(),
                        server_name: "paired-server".into(),
                    },
                )
            } else {
                saved
            };
            let binding = saved.binding().unwrap();
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
                binding,
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
                machine: None,
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

#[gpui::test]
fn servers_settings_remote_conflict_publishes_the_exact_draft_and_current_field_version(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new(cx);
    let summary = fixture.snapshot(cx).servers.remove(0);
    let old = summary
        .remote_access
        .as_ref()
        .expect("authenticated retained policy")
        .version;
    let server = fixture.server.server();
    let policy = spaceterm_core::remote_access::RemoteAccessPolicy::enabled(
        server,
        vec!["127.0.0.1:4444".parse().unwrap()],
    )
    .unwrap();
    let binding = fixture.server.binding(1).unwrap();
    fixture
        .tokio
        .block_on(fixture.tracker.submit(
            &binding,
            Command::SetRemoteAccess {
                server,
                expected_version: old,
                value: policy.clone(),
            },
            None,
            fixture.server.as_ref(),
        ))
        .unwrap();
    // The other edit committed before this connection received its publication.
    fixture.send(ServerRequest::DisableRemoteAccess(summary.key), cx);
    let remote = fixture
        .snapshot(cx)
        .servers
        .remove(0)
        .remote_access
        .unwrap();
    assert_eq!(remote.version.get(), old.get() + 1);
    assert_eq!(remote.endpoints, policy.endpoints());
    let edit = remote.edit.unwrap();
    assert_eq!(
        edit.draft,
        spaceterm_core::remote_access::RemoteAccessPolicy::disabled(server)
    );
    assert_eq!(edit.state, SettingEditState::Conflict);
    fixture.connection.send_replace(ConnectionState::Offline {
        failure: ClientFailure::new(ClientFailureClass::Unavailable),
        retry: RetryState {
            failures: 1,
            delay: None,
        },
    });
    fixture.drain(cx);
    assert_eq!(
        fixture.snapshot(cx).servers[0]
            .remote_access
            .as_ref()
            .unwrap()
            .edit
            .as_ref()
            .unwrap()
            .draft,
        edit.draft
    );
}

#[gpui::test]
fn servers_settings_end_all_ui_uses_the_application_assessment_publisher(cx: &mut TestAppContext) {
    let fixture = Fixture::new(cx);
    let before = fixture.journal.snapshot().unwrap().operations().len();
    let mut settings =
        crate::ui::settings_window::servers::tests::open_from_publisher(fixture.source.clone(), cx);
    settings.click("settings-server-end-all-sessions");
    assert!(!settings.modal_open());
    for _ in 0..100 {
        fixture.tokio.block_on(tokio::task::yield_now());
        settings.cx.run_until_parked();
    }
    assert!(settings.modal_open());
    assert_eq!(
        fixture.journal.snapshot().unwrap().operations().len(),
        before + 1
    );
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(settings.cx);
    assert_eq!(tree.focused().unwrap()["aria"]["label"], "Cancel");
    settings.click("modal-action-settings-server-end-all-sessions-confirm");
    for _ in 0..100 {
        fixture.tokio.block_on(tokio::task::yield_now());
        settings.cx.run_until_parked();
    }
    assert_eq!(
        fixture.journal.snapshot().unwrap().operations().len(),
        before + 2
    );
}

#[gpui::test]
fn servers_settings_remote_noop_does_not_wait_for_a_nonexistent_field_revision(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new(cx);
    let summary = fixture.snapshot(cx).servers.remove(0);
    let version = summary.remote_access.as_ref().unwrap().version;
    fixture.send(ServerRequest::DisableRemoteAccess(summary.key), cx);
    let current = fixture
        .snapshot(cx)
        .servers
        .remove(0)
        .remote_access
        .unwrap();
    assert_eq!(current.version, version);
    assert!(current.edit.is_none());
    assert!(!current.pending);
}

#[gpui::test]
fn servers_settings_client_import_export_and_reset_leave_the_live_journal_and_server_unchanged(
    cx: &mut TestAppContext,
) {
    use crate::ui::settings_window::catalog::SettingsSectionId;
    use crate::ui::sidebar_window::SidebarOwner as _;
    let fixture = Fixture::new(cx);
    let pending = PendingTasks::new(fixture.journal.clone(), Arc::new(Invalidator));
    fixture
        .tokio
        .block_on(
            pending
                .record_self_revocation(&fixture.server.binding(1).unwrap(), revocation(&fixture)),
        )
        .unwrap();
    fixture.connection.send_replace(ConnectionState::Stopped);
    fixture.drain(cx);
    let journal = fixture.journal.snapshot().unwrap();
    let organization = fixture.server.snapshot().unwrap();
    let published = fixture.snapshot(cx);
    assert_eq!(published.pending_unpairs.len(), 1);
    let mut settings =
        crate::ui::settings_window::servers::tests::open_from_publisher(fixture.source.clone(), cx);
    let mut imported = crate::settings::SettingsDocument::default();
    imported.git.worktree_path_template = "~/client-only/{branch}".into();
    let bytes = crate::settings::export_settings(&imported)
        .unwrap()
        .into_bytes();
    let window = settings.window.clone();
    settings.cx.update(|native, cx| {
        window.update(cx, |settings, cx| {
            settings.select_section(SettingsSectionId::Advanced, cx);
            settings.finish_settings_import(Ok(bytes), native, cx);
        })
    });
    settings.cx.run_until_parked();
    settings.click("modal-action-settings-import-confirm");
    assert_eq!(
        settings.document().git.worktree_path_template,
        imported.git.worktree_path_template
    );
    let exported = crate::settings::export_settings(&settings.document()).unwrap();
    let roundtrip: crate::settings::SettingsDocument = serde_json::from_str(&exported).unwrap();
    assert_eq!(
        roundtrip.git.worktree_path_template,
        imported.git.worktree_path_template
    );
    settings.click("settings-reset-all");
    settings.click("modal-action-settings-reset-all-confirm");
    assert_eq!(fixture.journal.snapshot().unwrap(), journal);
    assert_eq!(fixture.server.snapshot().unwrap(), organization);
    assert_eq!(
        fixture
            .source
            .read_with(settings.cx, |source, _| source.snapshot().clone()),
        published
    );
    assert!(matches!(
        *fixture.connection.borrow(),
        ConnectionState::Stopped
    ));
}

#[gpui::test]
fn servers_settings_remote_completion_does_not_restore_an_old_draft_after_a_later_policy_change(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new(cx);
    let server = fixture.server.server();
    let binding = fixture.server.binding(1).unwrap();
    let enabled = spaceterm_core::remote_access::RemoteAccessPolicy::enabled(
        server,
        vec!["127.0.0.1:4444".parse().unwrap()],
    )
    .unwrap();
    let publish_latest = |cx: &mut TestAppContext| {
        let snapshot = fixture.server.replica().unwrap();
        let ConnectionState::Connected { mut info } = fixture.connection.borrow().clone() else {
            panic!("live fixture")
        };
        info.organization_revision = snapshot.cursor.revision;
        fixture
            .connection
            .send_replace(ConnectionState::Connected { info });
        fixture
            .replica
            .send_replace(ReplicaState::Ready { snapshot });
        fixture.drain(cx);
    };
    fixture
        .tokio
        .block_on(fixture.tracker.submit(
            &binding,
            Command::SetRemoteAccess {
                server,
                expected_version: FieldVersion::new(1),
                value: enabled.clone(),
            },
            None,
            fixture.server.as_ref(),
        ))
        .unwrap();
    publish_latest(cx);
    let selected = fixture.snapshot(cx).servers.remove(0);
    fixture.send(ServerRequest::DisableRemoteAccess(selected.key), cx);
    assert!(
        fixture.snapshot(cx).servers[0]
            .remote_access
            .as_ref()
            .unwrap()
            .edit
            .is_some()
    );
    publish_latest(cx);
    assert!(
        fixture.snapshot(cx).servers[0]
            .remote_access
            .as_ref()
            .unwrap()
            .edit
            .is_none()
    );
    fixture
        .tokio
        .block_on(fixture.tracker.submit(
            &binding,
            Command::SetRemoteAccess {
                server,
                expected_version: FieldVersion::new(3),
                value: enabled,
            },
            None,
            fixture.server.as_ref(),
        ))
        .unwrap();
    publish_latest(cx);
    let final_policy = fixture
        .snapshot(cx)
        .servers
        .remove(0)
        .remote_access
        .unwrap();
    assert!(final_policy.enabled);
    assert_eq!(final_policy.version.get(), 4);
    assert!(final_policy.edit.is_none());
}

#[gpui::test]
fn enrolled_settings_connection_replaces_only_an_inactive_original_source(cx: &mut TestAppContext) {
    let h = Fixture::new(cx);
    let runtime = cx.update(|cx| cx.global::<InstalledSettingsRuntime>().0.clone());
    let active = runtime.connections.borrow()[0].clone();
    let mut replacement = active.clone();
    replacement.name.name = DisplayName::reported("New enrollment");
    cx.update(|cx| add_connection(replacement.clone(), cx))
        .unwrap();
    assert_eq!(runtime.connections.borrow().len(), 1);
    assert_eq!(runtime.connections.borrow()[0].name.name, active.name.name);

    // An obsolete source cannot shadow the exact binding that the one journal admits.
    runtime.connections.borrow_mut()[0].binding = h.server.binding(2).unwrap();
    cx.update(|cx| add_connection(replacement, cx)).unwrap();
    assert_eq!(runtime.connections.borrow().len(), 1);
    assert_eq!(runtime.connections.borrow()[0].binding, active.binding);
    assert_eq!(
        runtime.connections.borrow()[0].name.name,
        DisplayName::reported("New enrollment")
    );
}

#[gpui::test]
fn servers_settings_native_lost_reply_completion_admits_next_action(cx: &mut TestAppContext) {
    use spaceterm_host::{
        account_service::{ServiceStatus, SupervisorState},
        native_deployment::contracts::NativeRequestId,
        startup::{AccountBuildKey, BuildIdentity, DesiredServerState},
    };
    let fixture = Fixture::new(cx);
    let runtime = cx.update(|cx| cx.global::<InstalledSettingsRuntime>().0.clone());
    let request = NativeRequestId::new([19; 16]).unwrap();
    {
        let mut states = runtime.state.borrow_mut();
        let state = states.entry(fixture.server.server()).or_default();
        state.native_pending = Some(request);
        state.management.native_request = Some(request);
        state.management.failure = Some(ManagementFailure::Indeterminate);
        apply_native_read(
            state,
            NativeRead {
                desired: DesiredServerState::Running,
                generation: std::num::NonZeroU64::new(1).unwrap(),
                status: ServiceStatus {
                    key: AccountBuildKey {
                        account_uid: 501,
                        build_identity: BuildIdentity::Development,
                    },
                    server: Some(fixture.server.server()),
                    desired: Some(DesiredServerState::Running),
                    supervisor: SupervisorState::Running,
                    linger: None,
                    approved: None,
                    staged: None,
                    pending: None,
                },
                recovered: Some(ResultPublication::Native {
                    request,
                    unresolved: false,
                    failure: None,
                }),
            },
        );
    }
    cx.update(|cx| runtime.publish(cx));
    assert!(!fixture.snapshot(cx).servers[0].management.pending);
    assert_eq!(
        fixture.snapshot(cx).servers[0].management.native_request,
        Some(request)
    );
    assert_eq!(fixture.snapshot(cx).servers[0].management.failure, None);
    let server = fixture.snapshot(cx).servers[0].clone();
    fixture.send(
        ServerRequest::SetWorktreeLocation {
            server: server.key,
            template: "~/recovered/{repository}/{branch}".into(),
            base_revision: server.worktree_location.unwrap().revision,
        },
        cx,
    );
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
        "~/recovered/{repository}/{branch}"
    );
}

#[gpui::test]
fn servers_settings_native_recovery_does_not_clear_protocol_pending(cx: &mut TestAppContext) {
    use spaceterm_host::{
        account_service::{ServiceStatus, SupervisorState},
        native_deployment::contracts::NativeRequestId,
        startup::{AccountBuildKey, BuildIdentity, DesiredServerState},
    };
    let fixture = Fixture::new(cx);
    let runtime = cx.update(|cx| cx.global::<InstalledSettingsRuntime>().0.clone());
    let request = NativeRequestId::new([20; 16]).unwrap();
    {
        let mut states = runtime.state.borrow_mut();
        let state = states.entry(fixture.server.server()).or_default();
        state.busy = true;
        state.native_pending = Some(request);
        apply_native_read(
            state,
            NativeRead {
                desired: DesiredServerState::Running,
                generation: std::num::NonZeroU64::new(1).unwrap(),
                status: ServiceStatus {
                    key: AccountBuildKey {
                        account_uid: 501,
                        build_identity: BuildIdentity::Development,
                    },
                    server: Some(fixture.server.server()),
                    desired: Some(DesiredServerState::Running),
                    supervisor: SupervisorState::Running,
                    linger: None,
                    approved: None,
                    staged: None,
                    pending: None,
                },
                recovered: Some(ResultPublication::Native {
                    request,
                    unresolved: false,
                    failure: None,
                }),
            },
        );
        assert!(state.native_pending.is_none());
    }
    cx.update(|cx| runtime.publish(cx));
    assert!(
        fixture.snapshot(cx).servers[0].management.pending,
        "a completed native request cannot complete unrelated protocol work"
    );
}

struct FactsReceiptVerifier;
impl spaceterm_client::journal::ReceiptVerifier for FactsReceiptVerifier {
    fn verify(
        &self,
        pin: spaceterm_protocol::authentication::ServerPin,
        receipt: &spaceterm_core::pairing::SignedPairingReceipt,
    ) -> Result<(), ClientFailure> {
        spaceterm_protocol::pairing_adapter::ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(Into::into)
    }
}

fn saved_remote_route(
    saved: &spaceterm_client::journal::SavedBinding,
    route: RouteParts,
) -> spaceterm_client::journal::SavedBinding {
    let original = saved.binding().unwrap();
    let target = original.target();
    let binding = PairedServerBinding::new(
        spaceterm_client::api::ConnectionTarget::new(
            target.pin(),
            target.device(),
            target.pairing(),
            target.client(),
            spaceterm_client::api::RouteDescription::new(route).unwrap(),
            target.credential(),
            target.incarnation(),
        )
        .unwrap(),
    );
    spaceterm_client::journal::SavedBinding::verified(
        binding,
        saved.streams().clone(),
        saved.proof().clone(),
        &FactsReceiptVerifier,
    )
    .unwrap()
}

impl Fixture {
    fn machine_status(
        &self,
        server: &spaceterm_client::testing::FakeServer,
        facts: Option<spaceterm_core::management::ServerMachineFacts>,
        cx: &mut TestAppContext,
    ) -> spaceterm_core::management::ServerStatus {
        use spaceterm_core::management::{
            NativeVerification, ServerStatus as Status, ServerStatusParts,
        };
        let runtime = cx.update(|cx| cx.global::<InstalledSettingsRuntime>().0.clone());
        let ConnectionState::Connected { info } = runtime
            .connections
            .borrow()
            .iter()
            .find(|source| source.name.server == server.server())
            .unwrap()
            .state
            .borrow()
            .clone()
        else {
            panic!("connected fixture")
        };
        let snapshot = server.snapshot().unwrap();
        let status = Status::new(ServerStatusParts {
            runtime: Default::default(),
            server: info.server,
            epoch: info.epoch,
            revision: std::num::NonZeroU64::MIN,
            running: None,
            approved: None,
            staged: None,
            native_verification: NativeVerification::Unverified,
            capabilities: Vec::new(),
            remote_access: snapshot
                .state()
                .retained
                .as_ref()
                .unwrap()
                .remote_access()
                .clone(),
            listeners: Vec::new(),
            progress: Vec::new(),
        })
        .unwrap();
        let status = facts.map_or_else(
            || status.clone(),
            |facts| status.clone().with_machine_facts(facts).unwrap(),
        );
        {
            let mut states = runtime.state.borrow_mut();
            let state = states.entry(server.server()).or_default();
            state.status = Some(status.clone());
            state.status_connection = Some(info.binding);
        }
        cx.update(|cx| runtime.publish(cx));
        status
    }
}

fn offered_machine(
    platform: spaceterm_core::management::ServerPlatform,
    state: spaceterm_core::management::ServerPermissionState,
    port: u16,
) -> spaceterm_core::management::ServerMachineFacts {
    use spaceterm_core::management::*;
    ServerMachineFacts {
        platform,
        permissions: ServerPermissionFacts {
            microphone: state,
            screen_recording: state,
            accessibility: state,
            accessibility_naming: match platform {
                ServerPlatform::MacOs => ServerAccessibilityNaming::DeviceControl,
                _ => ServerAccessibilityNaming::Accessibility,
            },
        },
        bind_scope_availability: MachineFactsAvailability::Available,
        bind_scopes: vec![
            ServerBindScopeOffer {
                scope: ServerBindScope::Loopback,
                endpoints: vec![
                    std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                    std::net::SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port)),
                ],
            },
            ServerBindScopeOffer {
                scope: ServerBindScope::AllInterfaces,
                endpoints: vec![
                    std::net::SocketAddr::from(([0, 0, 0, 0], port)),
                    std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port)),
                ],
            },
        ],
    }
}

#[gpui::test]
fn servers_settings_authenticated_machine_facts_isolate_ssh_and_wss_without_native_authority(
    cx: &mut TestAppContext,
) {
    use spaceterm_core::management::{ServerPermissionState as PermissionState, ServerPlatform};
    let fixture = Fixture::build_machine_servers(cx, true);
    let other = fixture.secondary.as_ref().unwrap();
    let before = fixture.journal.snapshot().unwrap().operations().len();
    let mac = offered_machine(ServerPlatform::MacOs, PermissionState::Unavailable, 48484);
    let linux = offered_machine(ServerPlatform::Linux, PermissionState::NotNeeded, 59595);
    fixture.machine_status(&fixture.server, Some(mac.clone()), cx);
    fixture.machine_status(other, Some(linux.clone()), cx);
    let snapshot = fixture.snapshot(cx);
    for (id, facts, expected) in [
        (
            fixture.server.server(),
            mac,
            ServerPermissionState::Unavailable,
        ),
        (other.server(), linux, ServerPermissionState::NotNeeded),
    ] {
        let server = snapshot
            .servers
            .iter()
            .find(|server| server.pairing.parts().server == id)
            .unwrap();
        assert!(!server.route.is_local());
        assert!(server.service_lifecycle.is_none());
        assert_eq!(server.permissions.as_ref().unwrap().entries.len(), 3);
        assert!(
            server
                .permissions
                .as_ref()
                .unwrap()
                .entries
                .iter()
                .all(|entry| entry.state == expected)
        );
        let access = server.remote_access.as_ref().unwrap();
        assert!(!access.enabled, "offers must not enable network policy");
        assert!(access.endpoints.is_empty());
        assert_eq!(access.scopes.len(), 2);
        assert_eq!(access.scopes[0].endpoints, facts.bind_scopes[0].endpoints);
        let bound = BoundServerRequest::bind(
            ServerRequest::EnableRemoteAccess {
                server: server.key.clone(),
                scope: access.scopes[0].id.clone(),
            },
            &snapshot,
        )
        .unwrap();
        let model::ServerRequestTarget::Server {
            pairing,
            remote_policy: Some((version, policy)),
            ..
        } = bound.target
        else {
            panic!("offered scope binds explicit mutation")
        };
        assert_eq!(pairing.parts().server, id);
        assert_eq!(version, access.version);
        assert_eq!(policy.endpoints(), facts.bind_scopes[0].endpoints);
    }
    let runtime = cx.update(|cx| cx.global::<InstalledSettingsRuntime>().0.clone());
    assert!(
        runtime
            .connections
            .borrow()
            .iter()
            .all(|source| source.native.is_none()
                && source.lifecycle.is_none()
                && source.machine.is_none())
    );
    assert_eq!(
        fixture.journal.snapshot().unwrap().operations().len(),
        before
    );
    let other_key = server_key(other.server());
    let mut settings =
        crate::ui::settings_window::servers::tests::open_from_publisher(fixture.source.clone(), cx);
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(settings.cx);
    assert!(tree.find_text("Unavailable").is_some());
    assert!(tree.find_text("Not Needed").is_none());
    settings.click(&format!("settings-server-option-{}", other_key.0));
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(settings.cx);
    assert!(tree.find_text("Not Needed").is_some());
    assert!(tree.find_text("Unavailable").is_none());
}

#[gpui::test]
fn servers_settings_authenticated_machine_facts_require_current_server_epoch_connection_and_replica(
    cx: &mut TestAppContext,
) {
    use spaceterm_core::{
        authority::{ConnectionBinding, ConnectionGeneration, ServerEpoch},
        management::{
            ServerPermissionState as PermissionState, ServerPlatform, ServerStatus as Status,
        },
    };
    let fixture = Fixture::build_ipc_machine_server(cx);
    let missing = fixture.snapshot(cx);
    assert!(missing.servers[0].permissions.is_none());
    assert!(
        missing.servers[0]
            .remote_access
            .as_ref()
            .unwrap()
            .scopes
            .is_empty()
    );
    fixture.machine_status(&fixture.server, None, cx);
    let unattached = fixture.snapshot(cx);
    assert!(unattached.servers[0].permissions.is_none());
    assert!(
        unattached.servers[0]
            .remote_access
            .as_ref()
            .unwrap()
            .scopes
            .is_empty()
    );
    let status = fixture.machine_status(
        &fixture.server,
        Some(offered_machine(
            ServerPlatform::MacOs,
            PermissionState::Granted,
            48484,
        )),
        cx,
    );
    let runtime = cx.update(|cx| cx.global::<InstalledSettingsRuntime>().0.clone());
    let assert_absent = |snapshot: model::ServersSnapshot| {
        assert!(snapshot.servers[0].permissions.is_none());
        assert!(
            snapshot.servers[0]
                .remote_access
                .as_ref()
                .unwrap()
                .scopes
                .is_empty()
        );
    };
    assert!(
        fixture.snapshot(cx).servers[0]
            .permissions
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .all(|entry| entry.state == ServerPermissionState::Allowed)
    );
    for foreign_server in [false, true] {
        let mut parts = status.parts().clone();
        if foreign_server {
            parts.server = ServerId::new([42; 16]);
            parts.remote_access.value =
                spaceterm_core::remote_access::RemoteAccessPolicy::disabled(parts.server);
        } else {
            parts.epoch = ServerEpoch::new(parts.epoch.get() + 1);
        }
        runtime
            .state
            .borrow_mut()
            .get_mut(&fixture.server.server())
            .unwrap()
            .status = Some(
            Status::new(parts)
                .unwrap()
                .with_machine_facts(status.machine_facts().unwrap().clone())
                .unwrap(),
        );
        cx.update(|cx| runtime.publish(cx));
        assert_absent(fixture.snapshot(cx));
    }
    fixture.machine_status(
        &fixture.server,
        Some(status.machine_facts().unwrap().clone()),
        cx,
    );
    runtime
        .state
        .borrow_mut()
        .get_mut(&fixture.server.server())
        .unwrap()
        .status_connection = Some(ConnectionBinding::new(
        fixture.server.binding(1).unwrap().target().client(),
        ConnectionGeneration::new(99),
    ));
    cx.update(|cx| runtime.publish(cx));
    assert_absent(fixture.snapshot(cx));
    fixture.machine_status(
        &fixture.server,
        Some(status.machine_facts().unwrap().clone()),
        cx,
    );
    fixture.replica.send_replace(ReplicaState::Resynchronizing {
        retained: Some(fixture.server.replica().unwrap()),
    });
    fixture.drain(cx);
    assert_absent(fixture.snapshot(cx));
    fixture.replica.send_replace(ReplicaState::Ready {
        snapshot: fixture.server.replica().unwrap(),
    });
    fixture.connection.send_replace(ConnectionState::Offline {
        failure: ClientFailure::new(ClientFailureClass::Unavailable),
        retry: RetryState {
            failures: 1,
            delay: None,
        },
    });
    fixture.drain(cx);
    assert_absent(fixture.snapshot(cx));
}

#[gpui::test]
fn servers_settings_unknown_unavailable_and_legacy_machine_facts_keep_disable_independent_of_offers(
    cx: &mut TestAppContext,
) {
    use spaceterm_core::management::{
        MachineFactsAvailability, ServerMachineFacts, ServerPermissionFacts,
        ServerPermissionState as PermissionState,
    };
    let fixture = Fixture::build_machine_servers(cx, false);
    fixture.machine_status(&fixture.server, None, cx);
    let legacy = fixture.snapshot(cx);
    assert!(legacy.servers[0].permissions.is_none());
    assert!(
        legacy.servers[0]
            .remote_access
            .as_ref()
            .unwrap()
            .scopes
            .is_empty()
    );
    for (availability, state, expected) in [
        (
            MachineFactsAvailability::Unknown,
            PermissionState::Unknown,
            ServerPermissionState::Unknown,
        ),
        (
            MachineFactsAvailability::Unavailable,
            PermissionState::Unavailable,
            ServerPermissionState::Unavailable,
        ),
    ] {
        fixture.machine_status(
            &fixture.server,
            Some(ServerMachineFacts {
                bind_scope_availability: availability,
                permissions: ServerPermissionFacts {
                    microphone: state,
                    screen_recording: state,
                    accessibility: state,
                    ..Default::default()
                },
                ..Default::default()
            }),
            cx,
        );
        let snapshot = fixture.snapshot(cx);
        assert!(
            snapshot.servers[0]
                .permissions
                .as_ref()
                .unwrap()
                .entries
                .iter()
                .all(|entry| entry.state == expected)
        );
        assert!(
            snapshot.servers[0]
                .remote_access
                .as_ref()
                .unwrap()
                .scopes
                .is_empty()
        );
        assert!(!snapshot.servers[0].remote_access.as_ref().unwrap().enabled);
    }
    let status = fixture.machine_status(&fixture.server, Some(ServerMachineFacts::default()), cx);
    // Status can report a committed policy before its organization publication arrives.
    let mut parts = status.parts().clone();
    parts.revision = std::num::NonZeroU64::new(2).unwrap();
    parts.remote_access = spaceterm_core::organization::Versioned {
        value: spaceterm_core::remote_access::RemoteAccessPolicy::enabled(
            fixture.server.server(),
            vec!["127.0.0.1:48484".parse().unwrap()],
        )
        .unwrap(),
        version: FieldVersion::new(2),
    };
    let enabled = spaceterm_core::management::ServerStatus::new(parts)
        .unwrap()
        .with_machine_facts(ServerMachineFacts::default())
        .unwrap();
    let runtime = cx.update(|cx| cx.global::<InstalledSettingsRuntime>().0.clone());
    runtime
        .state
        .borrow_mut()
        .get_mut(&fixture.server.server())
        .unwrap()
        .status = Some(enabled);
    cx.update(|cx| runtime.publish(cx));
    let snapshot = fixture.snapshot(cx);
    let server = &snapshot.servers[0];
    assert!(server.remote_access.as_ref().unwrap().enabled);
    assert!(server.remote_access.as_ref().unwrap().scopes.is_empty());
    let bound = BoundServerRequest::bind(
        ServerRequest::DisableRemoteAccess(server.key.clone()),
        &snapshot,
    )
    .unwrap();
    let model::ServerRequestTarget::Server {
        remote_policy: Some((version, policy)),
        ..
    } = bound.target
    else {
        panic!("disable must not require scope offers")
    };
    assert_eq!(version, server.remote_access.as_ref().unwrap().version);
    assert!(!policy.is_enabled());
    assert!(policy.endpoints().is_empty());
}

#[gpui::test]
fn servers_settings_authenticated_machine_facts_disappear_during_status_resync_until_replacement(
    cx: &mut TestAppContext,
) {
    verify_ipc_machine_facts_resync(false, cx);
}

#[gpui::test]
fn servers_settings_authenticated_ipc_machine_facts_disappear_during_status_base_gap_until_replacement(
    cx: &mut TestAppContext,
) {
    verify_ipc_machine_facts_resync(true, cx);
}

fn verify_ipc_machine_facts_resync(delta_gap: bool, cx: &mut TestAppContext) {
    use spaceterm_client::management_cli::status::{
        StatusResync, StatusSubscription, StatusUpdate,
    };
    use spaceterm_core::management::{
        ServerPermissionState as PermissionState, ServerPlatform, ServerStatus as Status,
        StatusPublication, StatusQuery,
    };
    struct Resync;
    impl StatusResync for Resync {
        fn request_resync(&self) {}
    }
    let fixture = Fixture::build_ipc_machine_server(cx);
    let runtime = cx.update(|cx| cx.global::<InstalledSettingsRuntime>().0.clone());
    let ConnectionState::Connected { info } = fixture.connection.borrow().clone() else {
        panic!("connected fixture")
    };
    let query = StatusQuery {
        request: std::num::NonZeroU64::MIN,
        generation: 1,
        connection: info.binding,
        known_epoch: Some(info.epoch),
        known_revision: None,
        subscribe: true,
    };
    let original = fixture.machine_status(
        &fixture.server,
        Some(offered_machine(
            ServerPlatform::MacOs,
            PermissionState::Granted,
            48484,
        )),
        cx,
    );
    let observe = |subscription: &StatusSubscription, cx: &mut TestAppContext| {
        runtime
            .state
            .borrow_mut()
            .entry(fixture.server.server())
            .or_default()
            .observe_status(subscription);
        cx.update(|cx| runtime.publish(cx));
    };
    let subscription =
        StatusSubscription::new(fixture.server.server(), query, Arc::new(Resync)).unwrap();
    assert_eq!(
        subscription
            .apply(StatusPublication::Snapshot {
                request: query.request,
                generation: query.generation,
                status: original.clone()
            })
            .unwrap(),
        StatusUpdate::Replaced
    );
    observe(&subscription, cx);
    assert!(
        fixture.snapshot(cx).servers[0]
            .permissions
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .all(|entry| entry.state == ServerPermissionState::Allowed)
    );
    let mut parts = original.parts().clone();
    parts.revision = std::num::NonZeroU64::new(3).unwrap();
    let replacement = Status::new(parts)
        .unwrap()
        .with_machine_facts(offered_machine(
            ServerPlatform::MacOs,
            PermissionState::Unavailable,
            59595,
        ))
        .unwrap();
    let gap = if delta_gap {
        StatusPublication::Changed {
            request: query.request,
            generation: query.generation,
            base: std::num::NonZeroU64::new(2).unwrap(),
            status: replacement.clone(),
        }
    } else {
        StatusPublication::ResyncRequired {
            request: query.request,
            generation: query.generation,
        }
    };
    assert_eq!(
        subscription.apply(gap).unwrap(),
        StatusUpdate::ResyncRequired
    );
    assert_eq!(
        subscription.latest().unwrap(),
        Some(original.clone()),
        "the subscription deliberately retains historical evidence"
    );
    assert!(subscription.current().unwrap().is_none());
    observe(&subscription, cx);
    assert!(matches!(
        *fixture.connection.borrow(),
        ConnectionState::Connected { .. }
    ));
    assert!(fixture.replica.borrow().is_ready());
    let stale = fixture.snapshot(cx);
    assert!(stale.servers[0].permissions.is_none());
    assert!(
        stale.servers[0]
            .remote_access
            .as_ref()
            .unwrap()
            .scopes
            .is_empty()
    );
    assert_eq!(
        subscription
            .apply(StatusPublication::Snapshot {
                request: query.request,
                generation: query.generation,
                status: replacement
            })
            .unwrap(),
        StatusUpdate::Replaced
    );
    observe(&subscription, cx);
    let current = fixture.snapshot(cx);
    assert!(
        current.servers[0]
            .permissions
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .all(|entry| entry.state == ServerPermissionState::Unavailable)
    );
    assert!(
        current.servers[0].remote_access.as_ref().unwrap().scopes[0]
            .endpoints
            .iter()
            .all(|endpoint| endpoint.port() == 59595)
    );
}
