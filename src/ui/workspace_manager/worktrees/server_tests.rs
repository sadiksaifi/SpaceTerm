use super::*;
use crate::domain::server_view_testing::{A, Org, Replica, pane, server, tab, worktree, ws};
use crate::ui::workspace_manager::server::tests::{harness, publish, servers};
use gpui::{AppContext as _, TestAppContext};
use spaceterm_client::git::rpc::*;
use spaceterm_core::{
    authority::{ConnectionGeneration, FieldVersion, ServerEpoch},
    identity::{ClientId, PairingId, RepositoryId},
    operations::RepositoryScope,
    organization::{BranchName, ServerDirectory},
    server_directories::{DirectoryContext, DirectoryContextParts},
    worktrees::{
        GitAssessmentKind, GitAssessmentToken, GitAssessmentTokenParts, GitPageCursor,
        GitPageCursorParts, GitQuery, queries::*,
    },
};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct Peer {
    state: GitConnectionState,
    queries: Mutex<Vec<GitQuery>>,
    submissions: Mutex<Vec<GitCommand>>,
    lookups: Mutex<Vec<OperationId>>,
    sequence: AtomicUsize,
    operation: Mutex<Option<Arc<Operation>>>,
}
struct Operation {
    id: OperationId,
    command: GitCommand,
    state: Mutex<GitOperationState>,
}
impl GitOperationPort for Operation {
    fn id(&self) -> OperationId {
        self.id
    }
    fn command(&self) -> &GitCommand {
        &self.command
    }
    fn state(&self) -> GitClientFuture<'_, Result<GitOperationState, ClientGitFailure>> {
        let state = self.state.lock().unwrap().clone();
        Box::pin(async move { Ok(state) })
    }
}
struct PendingSubscription;
impl GitSubscriptionPort for PendingSubscription {
    fn next(
        &mut self,
    ) -> GitClientFuture<'_, Option<Result<GitSubscriptionEvent, ClientGitFailure>>> {
        Box::pin(std::future::pending())
    }
}
impl Peer {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: GitConnectionState {
                context: DirectoryContext::new(DirectoryContextParts {
                    server: server(A),
                    epoch: ServerEpoch::new(1),
                    pairing: PairingId::new(server(A), 1).unwrap(),
                    client: ClientId::new(server(A), 1).unwrap(),
                })
                .unwrap(),
                generation: ConnectionGeneration::new(1),
                readiness: GitReadiness::Ready,
            },
            queries: Mutex::default(),
            submissions: Mutex::default(),
            lookups: Mutex::default(),
            sequence: AtomicUsize::new(0),
            operation: Mutex::default(),
        })
    }
    fn page(&self, token: GitAssessmentToken, second: bool) -> GitAssessmentPage {
        GitAssessmentPage {
            token,
            tabs: vec![GitAffectedTab {
                workspace: ws(A, if second { 2 } else { 1 }),
                tab: tab(A, if second { 2 } else { 1 }),
                worktree: Some(worktree(A, 2)),
                membership_version: FieldVersion::new(1),
            }],
            panes: vec![GitAffectedPane {
                pane: pane(A, if second { 2 } else { 1 }),
                tab: tab(A, if second { 2 } else { 1 }),
                session_generation: spaceterm_core::authority::SessionGeneration::new(3),
                control_generation: spaceterm_core::authority::ControlGeneration::new(4),
                work_generation: spaceterm_core::authority::CloseWorkGeneration::new(5),
                starting: second,
            }],
            fallbacks: vec![GitFallback {
                workspace: ws(A, if second { 2 } else { 1 }),
                worktree: Some(worktree(A, 1)),
                directory: ServerDirectory::new(server(A), b"/server-only/main".to_vec()).unwrap(),
            }],
            next: if second {
                None
            } else {
                Some(
                    GitPageCursor::new(GitPageCursorParts {
                        scope: self.scope(),
                        epoch: ServerEpoch::new(1),
                        observation: 1,
                        random: [9; 32],
                    })
                    .unwrap(),
                )
            },
            requires_confirmation: true,
            creation_preview: None,
            removal: Some(GitRemovalSummary {
                worktree: worktree(A, 2),
                directory: ServerDirectory::new(server(A), b"/server-only/feature".to_vec())
                    .unwrap(),
                branch: Some(BranchName::new("feature".into()).unwrap()),
                risk: ApprovedRemoval::DiscardChangesAndDeleteSubmoduleCheckouts,
            }),
        }
    }
    fn scope(&self) -> RepositoryScope {
        RepositoryScope {
            server: server(A),
            repository: RepositoryId::new(server(A), 1).unwrap(),
        }
    }
}
impl GitConnectionPort for Peer {
    fn state(&self) -> Result<GitConnectionState, ClientGitFailure> {
        Ok(self.state)
    }
    fn query(
        &self,
        query: GitQuery,
    ) -> GitClientFuture<'_, Result<GitReadResult, ClientGitFailure>> {
        self.queries.lock().unwrap().push(query.clone());
        let result = match query {
            GitQuery::Inventory { .. } => GitQueryResult::Inventory(GitInventoryPage {
                records: [1, 2]
                    .into_iter()
                    .map(|id| GitInventoryRecord {
                        worktree: worktree(A, id),
                        directory: ServerDirectory::new(
                            server(A),
                            format!("/server-only/{id}").into_bytes(),
                        )
                        .unwrap(),
                        main: id == 1,
                        missing: false,
                        listed: true,
                        version: FieldVersion::new(1),
                    })
                    .collect(),
                next: None,
                version: OrganizationRevision::new(1),
            }),
            GitQuery::AssessRemove { .. } => {
                let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
                let context = self.state.context.parts();
                GitQueryResult::Assessment(Box::new(
                    self.page(
                        GitAssessmentToken::new(GitAssessmentTokenParts {
                            server: context.server,
                            epoch: context.epoch,
                            pairing: context.pairing,
                            client: context.client,
                            kind: GitAssessmentKind::Remove,
                            random: [sequence as u8; 32],
                        })
                        .unwrap(),
                        false,
                    ),
                ))
            }
            GitQuery::AssessmentPage { assessment, .. } => {
                GitQueryResult::Assessment(Box::new(self.page(assessment, true)))
            }
            GitQuery::Branches { .. } => GitQueryResult::Branches(GitBranchPage {
                records: vec![GitBranchRecord {
                    name: BranchName::new("main".into()).unwrap(),
                    remote: false,
                    checked_out: true,
                }],
                next: None,
            }),
            _ => return Box::pin(async { Err(ClientGitFailure::Unavailable) }),
        };
        let read = GitReadResult {
            epoch: ServerEpoch::new(1),
            scope: Some(self.scope()),
            result,
        };
        Box::pin(async move { Ok(read) })
    }
    fn subscribe(
        &self,
        _: GitStatusTarget,
    ) -> Result<Box<dyn GitSubscriptionPort>, ClientGitFailure> {
        Ok(Box::new(PendingSubscription))
    }
    fn submit(
        &self,
        command: Command,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>> {
        let Command::Worktree(command) = command else {
            panic!()
        };
        self.submissions.lock().unwrap().push(command.clone());
        let operation = Arc::new(Operation {
            id: crate::ui::tab_view::server_testing::operation(server(A), 8),
            command,
            state: Mutex::new(GitOperationState::Unresolved),
        });
        *self.operation.lock().unwrap() = Some(operation.clone());
        Box::pin(async move { Ok(operation as Arc<dyn GitOperationPort>) })
    }
    fn lookup(
        &self,
        id: OperationId,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>> {
        self.lookups.lock().unwrap().push(id);
        let operation = self.operation.lock().unwrap().clone().unwrap();
        Box::pin(async move { Ok(operation as Arc<dyn GitOperationPort>) })
    }
}
fn install(cx: &mut TestAppContext) -> (Entity<ServerGitStore>, Arc<Peer>) {
    let peer = Peer::new();
    let store = cx.new(ServerGitStore::new);
    store.update(cx, |store, cx| {
        store
            .connect(
                GitClient::new(server(A), peer.clone()).unwrap(),
                ServerEpoch::new(1),
                cx,
            )
            .unwrap()
    });
    cx.update(|cx| cx.set_global(crate::ui::server_git::InstalledServerGit(store.clone())));
    (store, peer)
}
fn organization() -> Replica {
    let mut org = Org::new(A);
    org.add_workspace(1, 1, 1)
        .add_tab(1, 3, 3, Some(1))
        .add_workspace(2, 2, 2);
    Replica::attached(org, 1)
}

#[gpui::test]
fn server_worktree_inventory_follows_unattached_workspaces(cx: &mut TestAppContext) {
    let (_, peer) = install(cx);
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let replica = organization();
    publish(&replica, 1, &runtime, cx);
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .sidebar_worktrees
            .server
            .inventories
            .keys()
            .copied()
            .collect::<Vec<_>>()),
        vec![ws(A, 1), ws(A, 2)]
    );
    assert!(peer.queries.lock().unwrap().iter().any(
        |query| matches!(query, GitQuery::Inventory { workspace, .. } if *workspace == ws(A, 2))
    ));
}

#[gpui::test]
fn server_worktree_removal_displays_every_page_without_local_holder_reduction(
    cx: &mut TestAppContext,
) {
    let (store, peer) = install(cx);
    let requests = store.read_with(cx, |store, _| store.requests(server(A)).unwrap());
    let assessment = cx
        .foreground_executor()
        .block_test(complete_assessment(&requests, worktree(A, 2)))
        .unwrap();
    assert_eq!(
        assessment
            .tabs
            .iter()
            .map(|tab| tab.workspace)
            .collect::<Vec<_>>(),
        vec![ws(A, 1), ws(A, 2)]
    );
    let message = removal_message(&assessment);
    assert!(message.contains("2 Tabs in 2 Workspaces"));
    assert!(message.contains("Workspace 2, Tab 2"));
    assert!(message.contains("uncommitted changes"));
    assert!(message.contains("submodule checkouts"));
    assert!(message.contains("for every client"));
    assert_eq!(peer.submissions.lock().unwrap().len(), 0);
}

#[gpui::test]
fn desktop_worktree_creation_waits_for_membership_and_preserves_focus_after_detach(
    cx: &mut TestAppContext,
) {
    let (_, _) = install(cx);
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut replica = organization();
    publish(&replica, 1, &runtime, cx);
    cx.update(|window, cx| {
        assert!(
            manager.update(cx, |manager, cx| manager.new_server_worktree(
                ws(A, 1),
                window,
                cx
            ))
        )
    });
    let form = manager.read_with(cx, |manager, _| {
        manager
            .sidebar_worktrees
            .server
            .form
            .as_ref()
            .unwrap()
            .1
            .clone()
    });
    form.update(cx, |_, cx| {
        cx.emit(WorktreeFormEvent::ServerCreated {
            workspace: ws(A, 1),
            repository: RepositoryScope {
                server: server(A),
                repository: RepositoryId::new(server(A), 1).unwrap(),
            },
            operation: crate::ui::tab_view::server_testing::operation(server(A), 9),
            worktree: worktree(A, 2),
            revision: OrganizationRevision::new(2),
        })
    });
    cx.run_until_parked();
    assert!(
        runtime.begun().is_empty(),
        "a completed write is not yet replica membership"
    );
    let detach = replica.personal.prepare_detach(ws(A, 1)).unwrap();
    replica.personal.apply_detach(detach).unwrap();
    replica
        .org
        .add_tab(1, 4, 4, Some(2))
        .delete_tab(4)
        .publish();
    publish(&replica, 2, &runtime, cx);
    assert!(
        runtime.begun().is_empty(),
        "late creation cannot attach or start a Tab after detach"
    );
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.active_server_workspace()),
        None
    );
}

#[gpui::test]
fn server_worktree_creation_opens_only_after_its_committed_revision(cx: &mut TestAppContext) {
    let (_, _) = install(cx);
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut replica = organization();
    publish(&replica, 1, &runtime, cx);
    cx.update(|window, cx| {
        assert!(
            manager.update(cx, |manager, cx| manager.new_server_worktree(
                ws(A, 1),
                window,
                cx
            ))
        )
    });
    let form = manager.read_with(cx, |manager, _| {
        manager
            .sidebar_worktrees
            .server
            .form
            .as_ref()
            .unwrap()
            .1
            .clone()
    });
    form.update(cx, |_, cx| {
        cx.emit(WorktreeFormEvent::ServerCreated {
            workspace: ws(A, 1),
            repository: RepositoryScope {
                server: server(A),
                repository: RepositoryId::new(server(A), 1).unwrap(),
            },
            operation: crate::ui::tab_view::server_testing::operation(server(A), 9),
            worktree: worktree(A, 2),
            revision: OrganizationRevision::new(2),
        })
    });
    cx.run_until_parked();
    assert!(runtime.begun().is_empty());
    replica
        .org
        .add_tab(1, 4, 4, Some(2))
        .delete_tab(4)
        .publish();
    publish(&replica, 2, &runtime, cx);
    assert_eq!(
        runtime.begun(),
        vec![DesktopRequest::Shared {
            server: server(A),
            command: Command::CreateTab {
                workspace: ws(A, 1),
                source: TabCreationSource::Worktree(worktree(A, 2))
            }
        }]
    );
}

#[gpui::test]
fn server_worktree_removal_uses_exact_token_and_retains_unresolved_operation(
    cx: &mut TestAppContext,
) {
    let (_, peer) = install(cx);
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let replica = organization();
    publish(&replica, 1, &runtime, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.remove_server_worktree(worktree(A, 2), window, cx)
        })
    });
    cx.run_until_parked();
    let token = manager.read_with(cx, |manager, _| {
        manager
            .sidebar_worktrees
            .server
            .removal
            .as_ref()
            .unwrap()
            .assessment
            .as_ref()
            .unwrap()
            .token
            .clone()
    });
    let bounds = cx
        .debug_bounds("modal-action-server-worktree-removal-confirm")
        .unwrap();
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        *peer.submissions.lock().unwrap(),
        vec![GitCommand::RemoveConfirmed { assessment: token }]
    );
    assert!(
        runtime.begun().is_empty(),
        "removal never directly closes local Tabs"
    );
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.remove_server_worktree(worktree(A, 2), window, cx)
        })
    });
    cx.run_until_parked();
    assert_eq!(peer.submissions.lock().unwrap().len(), 1);
    assert_eq!(peer.lookups.lock().unwrap().len(), 1);
}

#[gpui::test]
fn server_worktree_membership_change_requires_new_confirmation(cx: &mut TestAppContext) {
    let (_, peer) = install(cx);
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut replica = organization();
    publish(&replica, 1, &runtime, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.remove_server_worktree(worktree(A, 2), window, cx)
        })
    });
    cx.run_until_parked();
    replica.org.add_tab(2, 5, 5, Some(2)).publish();
    publish(&replica, 2, &runtime, cx);
    let bounds = cx
        .debug_bounds("modal-action-server-worktree-removal-confirm")
        .unwrap();
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(peer.submissions.lock().unwrap().is_empty());
    assert_eq!(peer.sequence.load(Ordering::SeqCst), 2);
}

#[gpui::test]
fn server_worktree_removal_keyboard_cancellation_preserves_focus(cx: &mut TestAppContext) {
    let (_, peer) = install(cx);
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let replica = organization();
    publish(&replica, 1, &runtime, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.focus(window, cx);
            manager.remove_server_worktree(worktree(A, 2), window, cx);
        })
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("modal-action-server-worktree-removal-cancel-keyboard-focus")
            .is_some()
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(peer.submissions.lock().unwrap().is_empty());
    assert!(manager.read_with(cx, |manager, _| {
        manager.sidebar_worktrees.server.removal.is_none()
    }));
}

#[gpui::test]
fn server_worktree_offline_actions_never_start_git_or_tabs(cx: &mut TestAppContext) {
    let (_, peer) = install(cx);
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let replica = organization();
    publish(&replica, 1, &runtime, cx);
    let mut frame = crate::ui::tab_view::server_testing::frame(
        &replica,
        crate::ui::server_desktop_contracts::ReplicaStatus::RetainedOffline,
    );
    frame.view_revision = crate::ui::tab_view::server_testing::view_revision(2);
    frame.connection = spaceterm_client::api::ConnectionState::Offline {
        retry: spaceterm_client::api::RetryState {
            failures: 1,
            delay: None,
        },
        failure: spaceterm_client::failure::ClientFailure::new(
            spaceterm_client::failure::ClientFailureClass::Unavailable,
        ),
    };
    runtime.provider.set_frame(Arc::new(frame));
    runtime.notifier.frame_changed(server(A));
    cx.run_until_parked();
    let before = peer.queries.lock().unwrap().len();
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            assert!(!manager.new_server_worktree(ws(A, 1), window, cx));
            manager.remove_server_worktree(worktree(A, 2), window, cx);
        })
    });
    assert_eq!(peer.queries.lock().unwrap().len(), before);
    assert!(peer.submissions.lock().unwrap().is_empty());
    assert!(runtime.begun().is_empty());
}

#[gpui::test]
fn server_worktree_selection_waits_for_the_recipient_workspace(cx: &mut TestAppContext) {
    use crate::domain::server_view_testing::ready;
    use spaceterm_client::view::ReplicaObservation;
    let (_, _) = install(cx);
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut replica = organization();
    replica.org.add_tab(2, 6, 6, Some(2)).delete_tab(6);
    replica
        .personal
        .attach(
            ws(A, 2),
            &ReplicaObservation::new(&replica.org.snapshot(), &[], ready(A)).unwrap(),
        )
        .unwrap();
    replica.personal.activate_workspace(ws(A, 1)).unwrap();
    publish(&replica, 1, &runtime, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.open_server_worktree(ws(A, 2), worktree(A, 2), window, cx)
        })
    });
    cx.run_until_parked();
    assert_eq!(
        runtime.begun(),
        vec![DesktopRequest::PersonalEdit(ViewEdit::ActivateWorkspace(
            ws(A, 2)
        ))]
    );
    replica.personal.activate_workspace(ws(A, 2)).unwrap();
    publish(&replica, 2, &runtime, cx);
    assert_eq!(
        runtime.begun().last(),
        Some(&DesktopRequest::Shared {
            server: server(A),
            command: Command::CreateTab {
                workspace: ws(A, 2),
                source: TabCreationSource::Worktree(worktree(A, 2))
            }
        })
    );
}

#[gpui::test]
fn server_worktree_session_change_requires_new_confirmation(cx: &mut TestAppContext) {
    use spaceterm_core::authority::SessionGeneration;
    use spaceterm_core::organization::{OrganizationSnapshot, SessionPhase};
    use spaceterm_core::organization_retained::{
        OrganizationRetainedFacts, PaneGenerationHighWater,
    };
    let (_, peer) = install(cx);
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let replica = organization();
    publish(&replica, 1, &runtime, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.remove_server_worktree(worktree(A, 2), window, cx)
        })
    });
    cx.run_until_parked();
    let snapshot = replica.org.snapshot();
    let mut state = snapshot.state().clone();
    let mut records = snapshot.runtime().to_vec();
    records[0].session_generation = SessionGeneration::new(1);
    records[0].phase = SessionPhase::Starting {
        generation: SessionGeneration::new(1),
    };
    let mut retained = state.retained.as_ref().unwrap().parts().clone();
    retained.pane_generations[0] = PaneGenerationHighWater::new(
        records[0].pane,
        records[0].session_generation,
        records[0].control_generation,
    );
    state.retained = Some(OrganizationRetainedFacts::new(retained).unwrap());
    let changed = OrganizationSnapshot::new(snapshot.epoch(), state, records).unwrap();
    let mut frame = crate::ui::tab_view::server_testing::frame(
        &replica,
        crate::ui::server_desktop_contracts::ReplicaStatus::Ready,
    );
    frame.connection = cx.update(|_, cx| {
        runtime
            .runtime
            .read(cx)
            .frame(server(A))
            .unwrap()
            .connection
            .clone()
    });
    frame.organization = Arc::new(changed);
    frame.view_revision = crate::ui::tab_view::server_testing::view_revision(2);
    runtime.provider.set_frame(Arc::new(frame));
    runtime.notifier.frame_changed(server(A));
    cx.run_until_parked();
    let bounds = cx
        .debug_bounds("modal-action-server-worktree-removal-confirm")
        .unwrap();
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(peer.submissions.lock().unwrap().is_empty());
    assert_eq!(peer.sequence.load(Ordering::SeqCst), 2);
}

#[gpui::test]
fn server_created_tab_waits_for_operation_membership_before_selection(cx: &mut TestAppContext) {
    use crate::ui::server_desktop_contracts::Outcome;
    let (_, _) = install(cx);
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut replica = organization();
    replica.org.add_tab(1, 8, 8, Some(2)).delete_tab(8);
    publish(&replica, 1, &runtime, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.open_server_worktree(ws(A, 1), worktree(A, 2), window, cx)
        })
    });
    cx.run_until_parked();
    let operation = crate::ui::tab_view::server_testing::operation(server(A), 9);
    runtime.provider.set_outcome_at(
        operation,
        Arc::new(Outcome::CreatedTab {
            tab: tab(A, 8),
            pane: pane(A, 8),
        }),
        OrganizationRevision::new(2),
    );
    runtime.provider.finish(0, Ok(Some(operation)));
    cx.run_until_parked();
    assert_eq!(
        runtime.begun().len(),
        1,
        "the outcome cannot select a Tab absent from the replica"
    );
    replica.org.add_tab(1, 8, 8, Some(2)).publish();
    publish(&replica, 2, &runtime, cx);
    assert_eq!(
        runtime.begun().last(),
        Some(&DesktopRequest::PersonalEdit(ViewEdit::ActivateTab {
            workspace: ws(A, 1),
            tab: tab(A, 8)
        }))
    );
}

#[gpui::test]
fn server_worktree_detach_does_not_lose_unattached_inventory_interest(cx: &mut TestAppContext) {
    let (store, peer) = install(cx);
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let replica = organization();
    publish(&replica, 1, &runtime, cx);
    let before = peer.queries.lock().unwrap().len();
    store.update(cx, |store, cx| store.detach(ws(A, 1), cx));
    cx.run_until_parked();
    let id = manager.read_with(cx, |manager, _| {
        manager.sidebar_worktrees.server.inventories[&ws(A, 1)]
    });
    assert!(store.read_with(cx, |store, _| store.inventory(id).is_some()));
    assert!(peer.queries.lock().unwrap().len() > before);
}
