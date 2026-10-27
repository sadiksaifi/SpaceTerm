//! Deterministic delivery around the production server mutation engine and shared desktop path.
//! The double delays transport delivery; State::apply owns validation, identities and membership.
use super::*;
use crate::domain::{PaneId, TabId};
use crate::terminal::shared_workspace::SharedWorkspaceSnapshot;
use spaceterm_server::hierarchy::{RetainedOutcome, State};
use std::cell::RefCell;

const DEVICE: Id = [21; 32];
const OTHER_DEVICE: Id = [22; 32];

struct Delivery {
    server: State,
    snapshot: SharedWorkspaceSnapshot,
    original: Option<(u64, Mutation)>,
    dispatched: Vec<Mutation>,
}
/// Hints its readers after each change, as the real client does.
struct DelayedServer(RefCell<Delivery>, crate::terminal::changes::Changes);

impl DelayedServer {
    fn new() -> Rc<Self> {
        let mut server = State::empty();
        server.begin_boot(BOOT);
        server
            .ensure_first_workspace(DEVICE, std::path::Path::new("/fixture/home"))
            .unwrap();
        let inventory = live_inventory(&server);
        server.inventory.close.clone_from(&inventory.close);
        Rc::new(Self(
            RefCell::new(Delivery {
                server,
                snapshot: SharedWorkspaceSnapshot {
                    end_all_sessions: true,
                    worktree_writes: false,
                    inventory: Some(inventory),
                    connected: true,
                    pending: false,
                    pending_operation: None,
                    rejected: None,
                    outcome: None,
                    error: None,
                },
                original: None,
                dispatched: Vec::new(),
            }),
            crate::terminal::changes::Changes::default(),
        ))
    }
    fn inventory(&self) -> Inventory {
        self.0.borrow().snapshot.inventory.clone().unwrap()
    }
    fn dispatched(&self) -> Vec<Mutation> {
        self.0.borrow().dispatched.clone()
    }
    fn connected(&self, connected: bool) {
        self.0.borrow_mut().snapshot.connected = connected;
        self.1.publish();
    }

    fn finish(&self, acknowledge: bool, valid_directory: bool) -> RetainedOutcome {
        let mut delivery = self.0.borrow_mut();
        let (sequence, mutation) = delivery
            .original
            .clone()
            .expect("an Original Operation was dispatched");
        // Repeating this same identity resolves an admitted operation; it creates no replacement.
        delivery
            .server
            .apply(
                DEVICE,
                sequence,
                mutation,
                std::path::Path::new("/fixture/home"),
                |_| valid_directory,
            )
            .unwrap();
        let result = delivery.server.outcome(DEVICE, sequence).unwrap().clone();
        let inventory = live_inventory(&delivery.server);
        delivery.server.inventory.close.clone_from(&inventory.close);
        delivery.snapshot.inventory = Some(inventory);
        delivery.snapshot.outcome = Some(result.clone());
        if acknowledge {
            delivery.server.acknowledge(DEVICE, sequence).unwrap();
            delivery.snapshot.pending = false;
            delivery.snapshot.pending_operation = None;
            delivery.original = None;
        }
        self.1.publish();
        result
    }

    /// Another client's accepted close removes `tab`, and the server publishes the result.
    fn remove_tab_elsewhere(&self, tab: Id) {
        let mut delivery = self.0.borrow_mut();
        delivery.server.inventory.revision += 1;
        delivery.server.inventory.workspaces[0]
            .tabs
            .retain(|candidate| candidate.id != tab);
        let inventory = live_inventory(&delivery.server);
        delivery.server.inventory.close.clone_from(&inventory.close);
        delivery.snapshot.inventory = Some(inventory);
        self.1.publish();
    }

    fn rename_elsewhere(&self, workspace: Id, name: &str) {
        let mut delivery = self.0.borrow_mut();
        let sequence = delivery.server.next_sequence(OTHER_DEVICE);
        let version = delivery
            .server
            .inventory
            .workspaces
            .iter()
            .find(|w| w.id == workspace)
            .unwrap()
            .name_version;
        delivery
            .server
            .apply(
                OTHER_DEVICE,
                sequence,
                Mutation::RenameWorkspace {
                    workspace,
                    version,
                    name: name.into(),
                },
                std::path::Path::new("/fixture/home"),
                |_| true,
            )
            .unwrap();
        delivery.server.acknowledge(OTHER_DEVICE, sequence).unwrap();
        let inventory = live_inventory(&delivery.server);
        delivery.server.inventory.close.clone_from(&inventory.close);
        delivery.snapshot.inventory = Some(inventory);
        self.1.publish();
    }
}

fn live_inventory(server: &State) -> Inventory {
    let mut inventory = server.inventory.clone();
    let previous = inventory.close.take();
    inventory.close = Some(CloseInventory {
        controls: previous
            .as_ref()
            .map(|live| live.controls.clone())
            .unwrap_or_default(),
        boot: BOOT,
        sessions: inventory
            .workspaces
            .iter()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| &t.panes)
            .map(|pane| ConfirmedPane {
                id: pane.id,
                session: previous
                    .as_ref()
                    .and_then(|live| live.sessions.iter().find(|known| known.id == pane.id))
                    .and_then(|known| known.session),
            })
            .collect(),
    });
    inventory
}

impl SharedWorkspaceEdits for DelayedServer {
    fn snapshot(&self) -> SharedWorkspaceSnapshot {
        self.0.borrow().snapshot.clone()
    }
    fn availability(&self) -> (bool, bool) {
        let delivery = self.0.borrow();
        (delivery.snapshot.connected, delivery.snapshot.pending)
    }
    fn submit(&self, mutation: Mutation) -> bool {
        let mut delivery = self.0.borrow_mut();
        if !delivery.snapshot.connected || delivery.snapshot.pending {
            return false;
        }
        let sequence = delivery.server.next_sequence(DEVICE);
        delivery.original = Some((sequence, mutation.clone()));
        delivery.dispatched.push(mutation.clone());
        delivery.snapshot.pending = true;
        delivery.snapshot.pending_operation = Some(mutation);
        delivery.snapshot.outcome = None;
        delivery.snapshot.rejected = None;
        self.1.publish();
        true
    }
    fn subscribe(&self) -> async_channel::Receiver<()> {
        self.1.subscribe()
    }
}

fn sessions() -> (
    SharedPaneSessions,
    Rc<ScriptedPanes>,
    WorkspaceTerminalSessionFactory,
    crate::terminal::testing::TestTerminalSessionRecords,
) {
    let records = crate::terminal::testing::TestTerminalSessionRecords::default();
    let factory: Rc<dyn TerminalSessionFactory> = Rc::new(
        crate::terminal::testing::TestTerminalSessionFactory::new(records.clone()),
    );
    let factory = WorkspaceTerminalSessionFactory::new_local(
        factory,
        crate::terminal::testing::test_local_directory(PathBuf::from("/tmp/spaceterm-optimistic")),
    );
    let panes = ScriptedPanes::new(factory.clone());
    let sessions = SharedPaneSessions::with_opener(
        Rc::clone(&panes) as Rc<dyn SharedPaneOpener>,
        &PaneConstruction::testing(),
    );
    (sessions, panes, factory, records)
}

fn model(server: &Rc<DelayedServer>) -> (SharedEditSource, SharedWorkspaces) {
    let source = SharedEditSource::new(Rc::clone(server) as Rc<dyn SharedWorkspaceEdits>);
    let (sessions, _, _, _) = sessions();
    let mut model = SharedWorkspaces::new(source.clone(), sessions);
    let inventory = model.poll().inventory.unwrap();
    model.register(
        first_workspace_id(),
        SharedWorkspaceFacts::from_server(&inventory.workspaces[0]),
    );
    (source, model)
}

#[test]
fn a_queued_direct_close_keeps_its_original_control_fence_and_requires_review_after_takeover() {
    use spaceterm_server::hierarchy::{CloseTarget, ConfirmedControl};
    for close_tab in [false, true] {
        let server = DelayedServer::new();
        let (source, mut model) = model(&server);
        let inventory = server.inventory();
        let workspace = inventory.workspaces[0].id;
        let tab = inventory.workspaces[0].root_tab;
        let pane = inventory.workspaces[0].tabs[0].root_pane;
        source.submit(Mutation::CreateTab {
            workspace,
            source: None,
        });
        server.finish(true, true);
        model.poll();
        let control = ConfirmedControl {
            pane,
            session: Some([30; 32]),
            owner: DEVICE,
            epoch: 1,
        };
        {
            let mut delivery = server.0.borrow_mut();
            let live = delivery.server.inventory.close.as_mut().unwrap();
            live.sessions
                .iter_mut()
                .find(|known| known.id == pane)
                .unwrap()
                .session = control.session;
            live.controls = vec![control.clone()];
            delivery.snapshot.inventory = Some(live_inventory(&delivery.server));
        }
        model.poll();
        assert_eq!(
            source.submit(Mutation::RenameWorkspace {
                workspace,
                version: 0,
                name: "Later".into()
            }),
            SharedSubmission::Submitted
        );
        let target = if close_tab {
            CloseTarget::Tab { workspace, tab }
        } else {
            CloseTarget::Pane {
                workspace,
                tab,
                pane,
            }
        };
        let local = if close_tab {
            crate::close_confirmation::CloseTarget::Tab {
                workspace_id: first_workspace_id(),
                tab_id: TabId::new(1),
            }
        } else {
            crate::close_confirmation::CloseTarget::Pane {
                workspace_id: first_workspace_id(),
                tab_id: TabId::new(1),
                pane_id: PaneId::new(1),
            }
        };
        let approval = model
            .close_approval(target)
            .unwrap()
            .direct_close(|_, _| true)
            .unwrap();
        assert_eq!(model.close(approval, local), SharedSubmission::Submitted);
        assert_eq!(
            server.dispatched().len(),
            2,
            "the close waits behind the rename"
        );
        assert_eq!(
            model.poll().inventory.unwrap().workspaces[0].tabs.len(),
            1,
            "the queued close already has an optimistic frame"
        );
        {
            // A server publication reports Take Over without changing the Terminal Session.
            let mut delivery = server.0.borrow_mut();
            delivery.server.inventory.close.as_mut().unwrap().controls = vec![ConfirmedControl {
                owner: OTHER_DEVICE,
                epoch: 2,
                ..control.clone()
            }];
            delivery.snapshot.inventory = Some(live_inventory(&delivery.server));
        }
        model.poll();
        assert_eq!(
            server.finish(true, true).outcome,
            Outcome::Accepted { target: workspace }
        );
        model.poll();
        assert!(
            matches!(server.dispatched().last(), Some(Mutation::Close { confirmation, .. })
            if confirmation.own_controls.as_deref() == Some(&[control][..]))
        );
        assert_eq!(
            server.finish(true, true).outcome,
            Outcome::StaleConfirmation
        );
        let restored = model.poll().inventory.unwrap();
        assert_eq!(restored.workspaces[0].tabs.len(), 2);
        assert_eq!(restored.close.unwrap().sessions[0].session, Some([30; 32]));
        assert_eq!(model.take_stale_close(), Some((target, Some(local))));
        assert_eq!(
            server.dispatched().len(),
            3,
            "stale consent never submits a replacement"
        );
        let explicit = model.close_approval(target).unwrap();
        assert_eq!(model.close(explicit, local), SharedSubmission::Submitted);
        assert!(
            matches!(server.dispatched().last(), Some(Mutation::Close { confirmation, .. })
            if confirmation.own_controls.is_none())
        );
        assert!(matches!(
            server.finish(true, true).outcome,
            Outcome::Accepted { .. }
        ));
    }
}

#[test]
fn combined_pending_tab_closes_retain_the_workspace_until_the_final_close_is_reviewed() {
    use spaceterm_server::hierarchy::{CloseTarget, ConfirmedControl};
    let server = DelayedServer::new();
    let (source, mut model) = model(&server);
    let workspace = server.inventory().workspaces[0].id;
    source.submit(Mutation::CreateTab {
        workspace,
        source: None,
    });
    server.finish(true, true);
    model.poll();
    let tabs = server.inventory().workspaces[0].tabs.clone();
    {
        let mut delivery = server.0.borrow_mut();
        delivery.server.inventory.close.as_mut().unwrap().controls = tabs
            .iter()
            .map(|tab| ConfirmedControl {
                pane: tab.root_pane,
                session: None,
                owner: DEVICE,
                epoch: 0,
            })
            .collect();
        delivery.snapshot.inventory = Some(live_inventory(&delivery.server));
    }
    model.poll();
    for (index, tab) in tabs.iter().enumerate() {
        let approval = model
            .close_approval(CloseTarget::Tab {
                workspace,
                tab: tab.id,
            })
            .unwrap()
            .direct_close(|_, _| true)
            .unwrap();
        assert_eq!(
            model.close(
                approval,
                crate::close_confirmation::CloseTarget::Tab {
                    workspace_id: first_workspace_id(),
                    tab_id: TabId::new(index as u64 + 1),
                }
            ),
            SharedSubmission::Submitted
        );
    }
    assert!(model.poll().inventory.unwrap().workspaces.is_empty());
    assert_eq!(server.inventory().workspaces[0].tabs.len(), 2);
    assert!(
        model.optimistically_closing(first_workspace_id()),
        "the Workspace must keep its rollback owner while the two closes are pending"
    );
    assert_eq!(
        server.dispatched().len(),
        2,
        "the second close still waits for the first"
    );
    assert_eq!(
        server.finish(true, true).outcome,
        Outcome::Accepted { target: tabs[0].id }
    );
    model.poll();
    assert!(model.optimistically_closing(first_workspace_id()));
    assert!(
        source
            .presentation_inventory()
            .unwrap()
            .workspaces
            .is_empty()
    );
    assert!(
        matches!(server.dispatched().last(), Some(Mutation::Close { confirmation, .. })
        if !confirmation.removes_workspace)
    );
    assert_eq!(
        server.finish(true, true).outcome,
        Outcome::StaleConfirmation
    );
    let restored = model.poll().inventory.unwrap();
    assert_eq!(restored.workspaces[0].tabs[0].id, tabs[1].id);
    assert!(!model.optimistically_closing(first_workspace_id()));
    assert_eq!(
        server.dispatched().len(),
        3,
        "a final Tab is never closed with renewed consent"
    );
    let review = model
        .close_approval(CloseTarget::Tab {
            workspace,
            tab: tabs[1].id,
        })
        .unwrap();
    assert!(
        review.direct_close(|_, _| true).is_none(),
        "the surviving Tab now requires Workspace-level review"
    );
    assert!(review.title().starts_with("Kill Workspace"));
}

#[test]
fn optimistic_rapid_identical_actions_keep_distinct_server_identities_and_results() {
    let server = DelayedServer::new();
    let (source, mut model) = model(&server);
    let workspace = server.inventory().workspaces[0].id;
    let mutation = Mutation::CreateTab {
        workspace,
        source: None,
    };
    let first = source.submit_waiting(mutation.clone()).unwrap();
    let second = source.submit_waiting(mutation).unwrap();
    assert_eq!(server.dispatched().len(), 1);
    let projected = model.poll().inventory.unwrap();
    assert_eq!(
        projected.workspaces[0].tabs.len(),
        3,
        "both interactions have a frame before either answer"
    );
    assert!(first.try_recv().is_err());
    assert!(second.try_recv().is_err());

    let accepted_first = server.finish(true, true);
    model.poll();
    assert_eq!(first.try_recv().unwrap().unwrap(), accepted_first);
    assert!(
        second.try_recv().is_err(),
        "identical mutations still have separate receivers"
    );
    assert_eq!(server.dispatched().len(), 2);
    let accepted_second = server.finish(true, true);
    model.poll();
    assert_eq!(second.try_recv().unwrap().unwrap(), accepted_second);
    assert_ne!(accepted_first.outcome, accepted_second.outcome);
    assert_eq!(server.inventory().workspaces[0].tabs.len(), 3);
}

#[test]
fn optimistic_split_preserves_published_repository_and_close_control_facts() {
    use spaceterm_server::hierarchy::ConfirmedControl;
    let server = DelayedServer::new();
    let workspace = server.inventory().workspaces[0].id;
    let tab = server.inventory().workspaces[0].tabs[0].clone();
    let control = ConfirmedControl {
        pane: tab.root_pane,
        session: None,
        owner: DEVICE,
        epoch: 7,
    };
    {
        let mut delivery = server.0.borrow_mut();
        delivery.server.present_repository_directory(
            tab.root_pane,
            std::path::PathBuf::from("/server/protected-repository"),
        );
        delivery.server.inventory.close.as_mut().unwrap().controls = vec![control.clone()];
        delivery.snapshot.inventory = Some(live_inventory(&delivery.server));
    }
    let (source, mut model) = model(&server);
    assert_eq!(
        source.submit(Mutation::SplitPane {
            workspace,
            tab: tab.id,
            source: tab.root_pane,
            axis: Axis::Right,
        }),
        SharedSubmission::Submitted
    );
    for inventory in [source.presentation_inventory().unwrap(), {
        server.finish(true, true);
        model.poll().inventory.unwrap()
    }] {
        let decoded: Inventory =
            serde_json::from_slice(&serde_json::to_vec(&inventory).unwrap()).unwrap();
        let panes = &decoded.workspaces[0].tabs[0].panes;
        assert_eq!(panes.len(), 2);
        assert_eq!(
            panes[0].repository_directory(),
            std::path::Path::new("/server/protected-repository")
        );
        assert_eq!(panes[1].repository_directory, None);
        assert_eq!(decoded.close.unwrap().controls, vec![control.clone()]);
    }
}

#[test]
fn optimistic_split_of_a_pending_tab_dispatches_only_after_its_real_parent_exists() {
    let server = DelayedServer::new();
    let (source, mut model) = model(&server);
    let workspace = server.inventory().workspaces[0].id;
    assert_eq!(
        source.submit(Mutation::CreateTab {
            workspace,
            source: None
        }),
        SharedSubmission::Submitted
    );
    let projected = source.presentation_inventory().unwrap();
    let pending = projected.workspaces[0].tabs.last().unwrap();
    let provisional_tab = pending.id;
    let provisional_pane = pending.root_pane;
    assert_eq!(
        source.submit(Mutation::SplitPane {
            workspace,
            tab: provisional_tab,
            source: provisional_pane,
            axis: Axis::Down
        }),
        SharedSubmission::Submitted
    );
    assert_eq!(
        source.presentation_inventory().unwrap().workspaces[0]
            .tabs
            .last()
            .unwrap()
            .panes
            .len(),
        2
    );
    assert_eq!(
        server.dispatched().len(),
        1,
        "no provisional identity can cross dispatch"
    );

    let created = server.finish(true, true);
    model.poll();
    let Outcome::Accepted { target } = created.outcome else {
        panic!("the server must accept the Tab");
    };
    let real = server.inventory().workspaces[0]
        .tabs
        .iter()
        .find(|t| t.id == target)
        .unwrap()
        .clone();
    assert_eq!(
        server.dispatched()[1],
        Mutation::SplitPane {
            workspace,
            tab: real.id,
            source: real.root_pane,
            axis: Axis::Down
        }
    );
    assert_ne!(real.id, provisional_tab);
    server.finish(true, true);
    model.poll();
    assert_eq!(
        server.inventory().workspaces[0]
            .tabs
            .last()
            .unwrap()
            .panes
            .len(),
        2
    );
}

#[test]
fn optimistic_refusal_removes_dependents_and_preserves_unrelated_later_work() {
    let server = DelayedServer::new();
    let (source, mut model) = model(&server);
    let workspace = server.inventory().workspaces[0].id;
    source.submit(Mutation::CreateTab {
        workspace,
        source: None,
    });
    let projected = source.presentation_inventory().unwrap();
    let tab = projected.workspaces[0].tabs.last().unwrap();
    let dependent = source
        .submit_waiting(Mutation::SplitPane {
            workspace,
            tab: tab.id,
            source: tab.root_pane,
            axis: Axis::Right,
        })
        .unwrap();
    source.submit(Mutation::RenameWorkspace {
        workspace,
        version: 0,
        name: "Later work".into(),
    });
    assert_eq!(server.finish(true, false).outcome, Outcome::Invalid);
    let rollback = model.poll().inventory.unwrap();
    assert_eq!(rollback.workspaces[0].tabs.len(), 1);
    assert_eq!(rollback.workspaces[0].name, "Later work");
    assert!(
        dependent.try_recv().unwrap().is_err(),
        "the refused parent cannot silently drop its split"
    );
    assert_eq!(server.dispatched().len(), 2);
    assert!(
        matches!(&server.dispatched()[1], Mutation::RenameWorkspace { name, .. } if name == "Later work")
    );
    server.finish(true, true);
    model.poll();
    assert_eq!(server.inventory().workspaces[0].name, "Later work");
}

#[test]
fn batched_independent_conflicts_keep_both_drafts_and_their_reviewed_values() {
    let server = DelayedServer::new();
    let (source, mut model) = model(&server);
    source.submit(Mutation::CreateWorkspace {
        name: "Second".into(),
        directory: None,
    });
    server.finish(true, true);
    model.poll();
    let workspaces = server.inventory().workspaces;
    let second = crate::domain::WorkspaceId::new(2);
    model.register(second, SharedWorkspaceFacts::from_server(&workspaces[1]));
    for (workspace, draft) in [
        (&workspaces[0], "First draft"),
        (&workspaces[1], "Second draft"),
    ] {
        assert_eq!(
            source.submit(Mutation::RenameWorkspace {
                workspace: workspace.id,
                version: 0,
                name: draft.into(),
            }),
            SharedSubmission::Submitted
        );
    }
    server.rename_elsewhere(workspaces[0].id, "First server value");
    server.rename_elsewhere(workspaces[1].id, "Second server value");
    assert_eq!(server.finish(true, true).outcome, Outcome::Conflict);
    source.snapshot();
    assert_eq!(server.finish(true, true).outcome, Outcome::Conflict);
    source.snapshot();
    // Tab and split presentation can advance delivery without a Workspace Window poll.
    model.poll();
    for (workspace, draft, authoritative) in [
        (first_workspace_id(), "First draft", "First server value"),
        (second, "Second draft", "Second server value"),
    ] {
        let review = model
            .take_conflict()
            .expect("each independent draft survives the batch");
        assert_eq!(review.workspace, workspace);
        assert_eq!(review.draft, SharedWorkspaceDraft::Name(draft.into()));
        assert_eq!(review.authoritative.as_ref(), authoritative);
    }
    assert!(model.take_conflict().is_none());
}

#[test]
fn unread_conflict_reviews_keep_the_connected_queue_bounded() {
    let server = DelayedServer::new();
    let (source, mut model) = model(&server);
    let workspace = server.inventory().workspaces[0].id;
    server.rename_elsewhere(workspace, "Server value");
    let mut retained = 0;
    for index in 0..128 {
        let submission = source.submit(Mutation::RenameWorkspace {
            workspace,
            version: 0,
            name: format!("Draft {index}"),
        });
        if submission == SharedSubmission::Refused(SharedRefusalReason::Pending) {
            break;
        }
        assert_eq!(submission, SharedSubmission::Submitted);
        assert_eq!(server.finish(true, true).outcome, Outcome::Conflict);
        model.poll();
        retained += 1;
    }
    assert!(
        retained > 0 && retained < 128,
        "unread drafts must consume bounded connected admission capacity"
    );
    for index in 0..retained {
        assert_eq!(
            model.take_conflict().unwrap().draft,
            SharedWorkspaceDraft::Name(format!("Draft {index}"))
        );
    }
    assert_eq!(
        source.submit(Mutation::CreateTab {
            workspace,
            source: None
        }),
        SharedSubmission::Submitted,
        "reviewing the retained drafts releases admission capacity"
    );
}

#[test]
fn optimistic_conflict_preserves_its_snapshot_and_draft_beside_a_later_pin() {
    let server = DelayedServer::new();
    let (source, mut model) = model(&server);
    let workspace = server.inventory().workspaces[0].id;
    source.submit(Mutation::RenameWorkspace {
        workspace,
        version: 0,
        name: "My draft".into(),
    });
    source.submit(Mutation::PinWorkspace {
        workspace,
        version: 0,
        directory: Some("/fixture/later".into()),
    });
    let pending = model.poll().inventory.unwrap();
    assert_eq!(pending.workspaces[0].name, "My draft");
    assert_eq!(
        pending.workspaces[0].pinned_directory,
        Some("/fixture/later".into())
    );
    server.rename_elsewhere(workspace, "Their name");
    assert_eq!(server.finish(true, true).outcome, Outcome::Conflict);
    let restored = model.poll().inventory.unwrap();
    assert_eq!(restored.workspaces[0].name, "Their name");
    assert_eq!(
        restored.workspaces[0].pinned_directory,
        Some("/fixture/later".into())
    );
    let conflict = model.take_conflict().unwrap();
    assert_eq!(conflict.authoritative.as_ref(), "Their name");
    assert_eq!(
        conflict.draft,
        SharedWorkspaceDraft::Name("My draft".into())
    );
}

#[test]
fn optimistic_lost_ack_binds_committed_membership_and_keeps_the_original() {
    let server = DelayedServer::new();
    let (source, mut model) = model(&server);
    let workspace = server.inventory().workspaces[0].id;
    source.submit(Mutation::CreateTab {
        workspace,
        source: None,
    });
    model.poll();
    let committed = server.finish(false, true);
    let binding = model.poll();
    assert!(
        binding.accepted.is_none(),
        "an acknowledgement cannot reopen an earlier creation"
    );
    assert_eq!(
        binding.inventory.unwrap().workspaces[0].tabs.len(),
        2,
        "never render the real and provisional Tab together"
    );
    assert!(
        server.0.borrow().original.is_some(),
        "the validated outcome has not retired its Original Operation"
    );
    source.submit(Mutation::CreateTab {
        workspace,
        source: None,
    });
    assert_eq!(
        server.dispatched().len(),
        1,
        "the transport still owns the unacknowledged original"
    );
    server.connected(false);
    model.poll();
    assert!(!source.has_pending(), "unsent work is not an offline queue");
    server.connected(true);
    assert_eq!(
        server.finish(true, true),
        committed,
        "resolution uses the same operation identity"
    );
    model.poll();
    assert_eq!(server.dispatched().len(), 1);
    assert_eq!(server.inventory().workspaces[0].tabs.len(), 2);
}

#[test]
fn optimistic_queue_is_bounded_and_offline_submissions_never_enter_it() {
    let server = DelayedServer::new();
    let (source, _) = model(&server);
    let workspace = server.inventory().workspaces[0].id;
    for _ in 0..32 {
        assert_eq!(
            source.submit(Mutation::CreateTab {
                workspace,
                source: None
            }),
            SharedSubmission::Submitted
        );
    }
    assert_eq!(
        source.submit(Mutation::CreateTab {
            workspace,
            source: None
        }),
        SharedSubmission::Refused(SharedRefusalReason::Pending)
    );
    assert_eq!(server.dispatched().len(), 1);
    server.connected(false);
    assert_eq!(
        source.submit(Mutation::CreateTab {
            workspace,
            source: None
        }),
        SharedSubmission::Refused(SharedRefusalReason::Offline)
    );
    assert_eq!(
        source.presentation_inventory().unwrap().workspaces[0]
            .tabs
            .len(),
        2,
        "only the dispatched Original Operation survives loss"
    );
}

struct WindowHarness {
    manager: Entity<TabManager>,
    server: Rc<DelayedServer>,
    source: SharedEditSource,
    panes: Rc<ScriptedPanes>,
    model: RefCell<SharedWorkspaces>,
    records: crate::terminal::testing::TestTerminalSessionRecords,
}
fn window_harness(cx: &mut TestAppContext) -> (WindowHarness, &mut VisualTestContext) {
    cx.update(crate::ui::init).unwrap();
    let server = DelayedServer::new();
    let inventory = server.inventory();
    let workspace = &inventory.workspaces[0];
    let root = &workspace.tabs[0];
    let source = SharedEditSource::new(Rc::clone(&server) as Rc<dyn SharedWorkspaceEdits>);
    let (sessions, panes, factory, records) = sessions();
    let (_, state) = panes.open(root.root_pane).unwrap();
    let mut model = SharedWorkspaces::new(source.clone(), sessions.clone());
    model.poll();
    model.register(
        first_workspace_id(),
        SharedWorkspaceFacts::from_server(workspace),
    );
    let (manager, cx) = cx.add_window_view(|window, cx| TabManager::new(factory, window, cx));
    cx.update(|window, cx| {
        window.activate_window();
        manager.update(cx, |manager, cx| {
            manager.adopt_shared(
                SharedTabs::new(source.clone(), sessions, workspace.id),
                root.id,
                root.root_pane,
                state,
                window,
                cx,
            );
            manager.focus(window, cx);
        });
    });
    cx.run_until_parked();
    (
        WindowHarness {
            manager,
            server,
            source,
            panes,
            model: RefCell::new(model),
            records,
        },
        cx,
    )
}
fn project_window(harness: &WindowHarness, cx: &mut VisualTestContext) {
    harness.model.borrow_mut().poll();
    let projected = harness.source.presentation_inventory().unwrap();
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.reconcile_shared(
                &projected.workspaces[0],
                super::super::development::SharedRemoval::Published,
                window,
                cx,
            );
        });
        window.refresh();
    });
    cx.run_until_parked();
}

/// Submits this client's close of the local Tab `tab_id`, then records, as the Workspace Window
/// does, that the keyboard rested in the strip when the person asked for it.
fn close_own_tab(harness: &WindowHarness, tab_id: TabId, cx: &mut VisualTestContext) {
    let tab = harness
        .manager
        .read_with(cx, |manager, _| manager.shared_server_tab(tab_id))
        .unwrap();
    let workspace = harness.server.inventory().workspaces[0].id;
    let target = spaceterm_server::hierarchy::CloseTarget::Tab { workspace, tab };
    let approval = harness.model.borrow().close_approval(target).unwrap();
    assert_eq!(
        harness.model.borrow_mut().close(
            approval,
            crate::close_confirmation::CloseTarget::Tab {
                workspace_id: first_workspace_id(),
                tab_id,
            }
        ),
        SharedSubmission::Submitted
    );
    cx.update(|_, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.prepare_shared_close_keyboard(true, cx);
        });
    });
}

fn local_view(
    harness: &WindowHarness,
    cx: &mut VisualTestContext,
) -> (crate::domain::TabId, crate::domain::PaneId) {
    harness
        .manager
        .read_with(cx, |manager, cx| manager.active_terminal_identity(cx))
}

#[gpui::test]
fn optimistic_pending_tab_split_and_later_navigation_keep_the_frame_and_keyboard(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = window_harness(cx);
    let original = local_view(&harness, cx);
    click("create-tab-button", cx);
    let provisional = local_view(&harness, cx);
    assert_ne!(provisional.0, original.0);
    click("pane-split-down-1", cx);
    assert_eq!(
        harness.manager.read_with(cx, |manager, cx| manager
            .active_tab_view()
            .read(cx)
            .pane_count()),
        2
    );
    assert_eq!(harness.server.dispatched().len(), 1);
    assert_eq!(
        harness.panes.opened().len(),
        1,
        "provisional Panes subscribe to no server identity or input authority"
    );
    cx.dispatch_action(crate::ui::PreviousTab);
    cx.run_until_parked();
    // A later Tab command owns the selection before either server completion arrives.
    let navigated = local_view(&harness, cx);
    assert_eq!(navigated.0, original.0);
    let focus = cx.update(|window, cx| window.focused(cx));
    harness.server.finish(true, true);
    project_window(&harness, cx);
    assert_eq!(local_view(&harness, cx), navigated);
    assert_eq!(cx.update(|window, cx| window.focused(cx)), focus);
    harness.server.finish(true, true);
    project_window(&harness, cx);
    assert_eq!(local_view(&harness, cx), navigated);
    assert_eq!(cx.update(|window, cx| window.focused(cx)), focus);
    assert!(cx.debug_bounds("shared-tab-unavailable-ok").is_none());
    let inventory = harness.server.inventory();
    let created = inventory.workspaces[0].tabs.last().unwrap();
    for pane in &created.panes {
        assert_eq!(
            harness.panes.pane(pane.id).created_starts(),
            1,
            "both the new Tab and its split retain startup after navigation"
        );
    }
}

#[gpui::test]
fn optimistic_tab_created_after_commit_before_ack_keeps_its_bound_source(cx: &mut TestAppContext) {
    let (harness, cx) = window_harness(cx);
    cx.dispatch_action(crate::ui::CreateTab);
    let committed = harness.server.finish(false, true);
    assert!(matches!(committed.outcome, Outcome::Accepted { .. }));
    let bound_source = harness.server.inventory().workspaces[0].tabs[1].root_pane;
    // The worker published accepted membership, but its original is still awaiting ack. The
    // next key event observes that outcome before the regular window membership poll binds it.
    cx.dispatch_action(crate::ui::CreateTab);
    assert_eq!(
        harness.server.dispatched().len(),
        1,
        "a queued dependent waits for the original acknowledgement"
    );
    assert_eq!(
        harness.server.finish(true, true),
        committed,
        "settlement must acknowledge the original creation identity"
    );
    project_window(&harness, cx);
    let dependent = harness.server.finish(true, true);
    assert!(
        matches!(dependent.outcome, Outcome::Accepted { .. }),
        "a committed source Pane still on the server must not produce refusal: {:?}",
        dependent.outcome
    );
    assert_eq!(
        dependent.mutation,
        Mutation::CreateTab {
            workspace: harness.server.inventory().workspaces[0].id,
            source: Some(bound_source),
        },
        "binding may retire the view alias only after queued intent owns its canonical source"
    );
    project_window(&harness, cx);
    cx.dispatch_action(crate::ui::CreateTab);
    assert!(matches!(
        harness.server.finish(true, true).outcome,
        Outcome::Accepted { .. }
    ));
    project_window(&harness, cx);
    assert_eq!(harness.server.inventory().workspaces[0].tabs.len(), 4);
    assert!(harness.model.borrow_mut().take_refusal().is_none());
}

#[gpui::test]
fn optimistic_rapid_tabs_keep_each_start_intent_after_later_navigation(cx: &mut TestAppContext) {
    let (harness, cx) = window_harness(cx);
    for _ in 0..15 {
        cx.dispatch_action(crate::ui::CreateTab);
    }
    cx.dispatch_action(crate::ui::PreviousTab);
    let selected = local_view(&harness, cx);
    let focus = cx.update(|window, cx| window.focused(cx));
    for _ in 0..15 {
        harness.server.finish(true, true);
        project_window(&harness, cx);
        for pane in harness.panes.opened() {
            pane.admitted();
            assert_eq!(
                crate::ui::server_pane::ServerPaneConnection::status(pane.as_ref()),
                crate::terminal::server_session::DevelopmentPaneStatus::Connecting,
                "hidden admission must release capacity without fabricating a first screen"
            );
        }
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(16));
        cx.run_until_parked();
    }
    let inventory = harness.server.inventory();
    assert_eq!(inventory.workspaces[0].tabs.len(), 16);
    assert_eq!(
        harness.records.starts().len(),
        16,
        "all hidden accepted Tabs start their presentation worker without revisiting"
    );
    for tab in &inventory.workspaces[0].tabs[1..] {
        assert_eq!(
            harness.panes.pane(tab.root_pane).created_starts(),
            1,
            "each accepted creation retains one startup request even after focus moves"
        );
    }
    assert_eq!(local_view(&harness, cx), selected);
    assert_eq!(cx.update(|window, cx| window.focused(cx)), focus);
}

#[gpui::test]
fn readiness_reserves_foreground_start_capacity_during_a_hidden_creation_burst(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = window_harness(cx);
    for _ in 0..15 {
        cx.dispatch_action(crate::ui::CreateTab);
    }
    cx.dispatch_action(crate::ui::ActivateTab1);
    for _ in 0..15 {
        harness.server.finish(true, true);
        project_window(&harness, cx);
    }
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(16));
    cx.run_until_parked();
    assert_eq!(
        harness.records.starts().len(),
        4,
        "only three hidden startups may occupy the pipeline beside the existing Root Pane"
    );
    cx.dispatch_action(crate::ui::ActivateTab9);
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(16));
    cx.run_until_parked();
    assert_eq!(
        harness.records.starts().len(),
        5,
        "the newly selected Pane uses the reserved foreground slot"
    );
    cx.dispatch_action(crate::ui::PreviousTab);
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(16));
    cx.run_until_parked();
    assert_eq!(
        harness.records.starts().len(),
        5,
        "navigation never bypasses four already admitted startups"
    );
}

#[gpui::test]
fn readiness_hidden_split_uses_current_window_geometry_without_resize_on_first_visit(
    cx: &mut TestAppContext,
) {
    use crate::terminal::testing::RecordedCommand;
    let (harness, cx) = window_harness(cx);
    click("create-tab-button", cx);
    click("pane-split-down-1", cx);
    cx.dispatch_action(crate::ui::PreviousTab);
    cx.simulate_resize(gpui::size(gpui::px(1001.0), gpui::px(701.0)));
    cx.run_until_parked();
    for _ in 0..2 {
        harness.server.finish(true, true);
        project_window(&harness, cx);
        for pane in harness.panes.opened() {
            pane.reconnect();
        }
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(16));
        cx.run_until_parked();
    }
    assert_eq!(
        harness.records.starts().len(),
        3,
        "the new Tab and split must start while their Tab is hidden"
    );
    let starts = harness.records.starts();
    assert_eq!(
        starts[1].geometry.grid().cols,
        starts[2].geometry.grid().cols
    );
    assert!(
        starts[1]
            .geometry
            .grid()
            .rows
            .abs_diff(starts[2].geometry.grid().rows)
            <= 1
    );
    let new_sessions = [starts[1].session_id, starts[2].session_id];
    let resizes_before = harness
        .records
        .commands()
        .iter()
        .filter(|call| {
            new_sessions.contains(&call.session_id)
                && matches!(call.command, RecordedCommand::Resize(_))
        })
        .count();
    cx.dispatch_action(crate::ui::NextTab);
    cx.run_until_parked();
    let resizes_after = harness
        .records
        .commands()
        .iter()
        .filter(|call| {
            new_sessions.contains(&call.session_id)
                && matches!(call.command, RecordedCommand::Resize(_))
        })
        .count();
    assert_eq!(
        resizes_after, resizes_before,
        "first visible prepaint must reuse the hidden startup grid, without SIGWINCH"
    );
}

/// Accepts a new hidden Tab with three side-by-side Panes whose restored layout is wider than
/// the stage once the window narrows, optionally zooming its Focused Pane first.
fn accept_hidden_layout_wider_than_the_stage(
    harness: &WindowHarness,
    zoomed: bool,
    cx: &mut VisualTestContext,
) {
    cx.simulate_resize(gpui::size(gpui::px(1600.0), gpui::px(900.0)));
    cx.run_until_parked();
    click("create-tab-button", cx);
    for _ in 0..2 {
        cx.dispatch_action(crate::ui::SplitRight);
        cx.run_until_parked();
    }
    if zoomed {
        cx.dispatch_action(crate::ui::TogglePaneZoom);
        cx.run_until_parked();
    }
    cx.dispatch_action(crate::ui::PreviousTab);
    cx.simulate_resize(gpui::size(gpui::px(460.0), gpui::px(600.0)));
    cx.run_until_parked();
    for _ in 0..3 {
        harness.server.finish(true, true);
        project_window(harness, cx);
        for pane in harness.panes.opened() {
            pane.reconnect();
        }
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(16));
        cx.run_until_parked();
    }
    assert_eq!(
        harness.server.inventory().workspaces[0].tabs[1].panes.len(),
        3
    );
}

fn resizes(harness: &WindowHarness) -> usize {
    use crate::terminal::testing::RecordedCommand;
    harness
        .records
        .commands()
        .iter()
        .filter(|call| matches!(call.command, RecordedCommand::Resize(_)))
        .count()
}

#[gpui::test]
fn readiness_starts_visible_panes_of_a_restored_layout_wider_than_the_stage(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = window_harness(cx);
    accept_hidden_layout_wider_than_the_stage(&harness, false, cx);
    assert_eq!(
        harness.records.starts().len(),
        1,
        "a hidden layout the stage cannot hold has no validated geometry to start from"
    );
    cx.dispatch_action(crate::ui::NextTab);
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(16));
    cx.run_until_parked();
    assert_eq!(
        harness.records.starts().len(),
        4,
        "every Pane the first visit draws starts from its rendered allocation"
    );
    let resizes_before = resizes(&harness);
    cx.update(|window, _| window.refresh());
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(16));
    cx.run_until_parked();
    assert_eq!(
        resizes(&harness),
        resizes_before,
        "the started grid already matches the drawn allocation"
    );
}

#[gpui::test]
fn readiness_starts_a_zoomed_pane_that_fits_although_its_restored_layout_does_not(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = window_harness(cx);
    accept_hidden_layout_wider_than_the_stage(&harness, true, cx);
    assert_eq!(
        harness.records.starts().len(),
        2,
        "the hidden Zoomed Pane fills the stage, so the stage validates its geometry"
    );
    let resizes_before = resizes(&harness);
    cx.dispatch_action(crate::ui::NextTab);
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(16));
    cx.run_until_parked();
    assert_eq!(
        harness.records.starts().len(),
        2,
        "Panes beneath the Zoomed Pane are not drawn and their restored layout does not fit"
    );
    assert_eq!(
        resizes(&harness),
        resizes_before,
        "the first visit draws the Zoomed Pane at the grid it started with"
    );
    cx.dispatch_action(crate::ui::TogglePaneZoom);
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(16));
    cx.run_until_parked();
    assert_eq!(
        harness.records.starts().len(),
        4,
        "restoring the layout starts every drawn Pane from its rendered allocation"
    );
}

#[gpui::test]
fn optimistic_close_hides_before_a_doomed_pane_status_and_restores_the_same_view_on_refusal(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = window_harness(cx);
    click("pane-split-down-1", cx);
    harness.server.finish(true, true);
    project_window(&harness, cx);
    let closed_view = local_view(&harness, cx);
    let inventory = harness.server.inventory();
    let workspace = &inventory.workspaces[0];
    let tab = &workspace.tabs[0];
    let closing = tab.panes.last().unwrap().id;
    let target = spaceterm_server::hierarchy::CloseTarget::Pane {
        workspace: workspace.id,
        tab: tab.id,
        pane: closing,
    };
    let approval = harness.model.borrow().close_approval(target).unwrap();
    assert_eq!(
        harness.model.borrow_mut().close(
            approval,
            crate::close_confirmation::CloseTarget::Pane {
                workspace_id: first_workspace_id(),
                tab_id: closed_view.0,
                pane_id: closed_view.1,
            }
        ),
        SharedSubmission::Submitted
    );
    project_window(&harness, cx);
    assert_eq!(
        harness.manager.read_with(cx, |manager, cx| manager
            .active_tab_view()
            .read(cx)
            .pane_count()),
        1,
        "the first post-consent frame has already removed the Pane"
    );
    harness
        .panes
        .pane(closing)
        .set(DevelopmentPaneStatus::TakeoverReconnecting);
    cx.run_until_parked();
    assert!(cx.debug_bounds("shared-pane-takeover-2").is_none());
    assert!(cx.debug_bounds("shared-pane-notice-2").is_none());
    // A changed server confirmation definitely refuses the original close.
    harness.server.0.borrow_mut().server.inventory.close = Some(CloseInventory {
        controls: Vec::new(),
        boot: [99; 32],
        sessions: vec![],
    });
    assert_eq!(
        harness.server.finish(true, true).outcome,
        Outcome::StaleConfirmation
    );
    let focus = cx.update(|window, cx| window.focused(cx));
    project_window(&harness, cx);
    assert_eq!(
        harness.manager.read_with(cx, |manager, cx| manager
            .active_tab_view()
            .read(cx)
            .pane_count()),
        2
    );
    assert_eq!(
        cx.update(|window, cx| window.focused(cx)),
        focus,
        "rollback never steals the survivor's keyboard"
    );
    // The restored Pane retains its original local identity and layout, not a replacement Pane.
    assert!(
        cx.debug_bounds(format!("pane-surface-{}", closed_view.1.get()).leak())
            .is_some()
    );
}

#[test]
fn optimistic_workspace_close_keeps_its_host_through_a_lost_ack() {
    let server = DelayedServer::new();
    let (_, mut model) = model(&server);
    let workspace = server.inventory().workspaces[0].id;
    let target = spaceterm_server::hierarchy::CloseTarget::Workspace { workspace };
    let approval = model.close_approval(target).unwrap();
    assert_eq!(
        model.close(
            approval,
            crate::close_confirmation::CloseTarget::Workspace(first_workspace_id())
        ),
        SharedSubmission::Submitted
    );
    assert!(model.optimistically_closing(first_workspace_id()));
    assert!(model.poll().inventory.unwrap().workspaces.is_empty());
    server.finish(false, true);
    model.poll();
    assert!(model.latest_inventory().unwrap().workspaces.is_empty());
    assert!(
        model.optimistically_closing(first_workspace_id()),
        "published deletion does not retire an unacknowledged Original Operation"
    );
    server.connected(false);
    model.poll();
    assert!(model.optimistically_closing(first_workspace_id()));
    server.connected(true);
    server.finish(true, true);
    model.poll();
    assert!(!model.optimistically_closing(first_workspace_id()));
}

#[gpui::test]
fn a_committed_original_keeps_resolving_without_a_workspace_window(cx: &mut TestAppContext) {
    let server = DelayedServer::new();
    let (source, mut model) = model(&server);
    let workspace = server.inventory().workspaces[0].id;
    let approval = model
        .close_approval(spaceterm_server::hierarchy::CloseTarget::Workspace { workspace })
        .unwrap();
    model.close(
        approval,
        crate::close_confirmation::CloseTarget::Workspace(first_workspace_id()),
    );
    server.finish(false, true);
    model.poll();
    cx.update(|cx| source.retain_until_settled(cx));
    let retained = Rc::downgrade(&server);
    drop(model);
    drop(source);
    drop(server);
    cx.run_until_parked();
    let original = retained
        .upgrade()
        .expect("the unacknowledged Original Operation stays owned");
    assert_eq!(original.dispatched().len(), 1);
    original.finish(true, true);
    drop(original);
    cx.run_until_parked();
    assert!(
        retained.upgrade().is_none(),
        "settlement releases the background owner"
    );
}

#[gpui::test]
fn successive_refused_tab_closes_restore_the_original_order(cx: &mut TestAppContext) {
    let (harness, cx) = window_harness(cx);
    for _ in 0..3 {
        click("create-tab-button", cx);
        harness.server.finish(true, true);
        project_window(&harness, cx);
    }
    let original = harness
        .manager
        .read_with(cx, |manager, _| manager.tab_ids());
    let inventory = harness.server.inventory();
    let workspace = &inventory.workspaces[0];
    for index in [1, 2] {
        let approval = harness
            .model
            .borrow()
            .close_approval(spaceterm_server::hierarchy::CloseTarget::Tab {
                workspace: workspace.id,
                tab: workspace.tabs[index].id,
            })
            .unwrap();
        assert_eq!(
            harness.model.borrow_mut().close(
                approval,
                crate::close_confirmation::CloseTarget::Tab {
                    workspace_id: first_workspace_id(),
                    tab_id: original[index],
                },
            ),
            SharedSubmission::Submitted
        );
        project_window(&harness, cx);
    }
    for _ in 0..2 {
        harness.server.0.borrow_mut().server.inventory.close = Some(CloseInventory {
            controls: Vec::new(),
            boot: [99; 32],
            sessions: vec![],
        });
        assert_eq!(
            harness.server.finish(true, true).outcome,
            Outcome::StaleConfirmation
        );
        project_window(&harness, cx);
    }
    assert_eq!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.tab_ids()),
        original
    );
}

#[gpui::test]
fn optimistic_tab_close_ack_keeps_later_find_focus(cx: &mut TestAppContext) {
    let (harness, cx) = window_harness(cx);
    click("create-tab-button", cx);
    harness.server.finish(true, true);
    project_window(&harness, cx);
    let closed = local_view(&harness, cx);
    let inventory = harness.server.inventory();
    let workspace = &inventory.workspaces[0];
    let tab = workspace.tabs.last().unwrap();
    let doomed_pane = tab.root_pane;
    let target = spaceterm_server::hierarchy::CloseTarget::Tab {
        workspace: workspace.id,
        tab: tab.id,
    };
    let approval = harness.model.borrow().close_approval(target).unwrap();
    assert_eq!(
        harness.model.borrow_mut().close(
            approval,
            crate::close_confirmation::CloseTarget::Tab {
                workspace_id: first_workspace_id(),
                tab_id: closed.0,
            }
        ),
        SharedSubmission::Submitted
    );
    project_window(&harness, cx);
    assert_eq!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.tab_ids().len()),
        1
    );
    assert!(
        cx.debug_bounds(format!("tab-identity-{}", closed.0.get()).leak())
            .is_none()
    );

    cx.dispatch_action(crate::ui::OpenTerminalFind);
    cx.run_until_parked();
    assert!(cx.debug_bounds("terminal-find-input").is_some());
    let find_focus = cx.update(|window, cx| window.focused(cx));
    harness
        .panes
        .pane(doomed_pane)
        .set(DevelopmentPaneStatus::TakeoverReconnecting);
    cx.run_until_parked();
    assert!(cx.debug_bounds("shared-pane-takeover-1").is_none());
    assert!(cx.debug_bounds("shared-pane-notice-1").is_none());
    assert!(matches!(
        harness.server.finish(true, true).outcome,
        Outcome::Accepted { .. }
    ));
    project_window(&harness, cx);
    assert_eq!(cx.update(|window, cx| window.focused(cx)), find_focus);
    assert!(cx.debug_bounds("terminal-find-input").is_some());
    assert_eq!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.tab_ids().len()),
        1
    );
}

/// Closing the Active Tab onto a Pane whose shell already runs opens that Pane as a local close
/// does. Its Terminal Session stays the one it had, and the keyboard reaches its terminal.
#[gpui::test]
fn closing_the_active_tab_onto_a_running_pane_keeps_its_session_and_keyboard(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = window_harness(cx);
    let survivor_id = harness.server.inventory().workspaces[0].tabs[0].root_pane;
    let survivor = harness.panes.pane(survivor_id);
    survivor.reconnect();
    let survivor_view = local_view(&harness, cx);
    click("create-tab-button", cx);
    harness.server.finish(true, true);
    project_window(&harness, cx);
    cx.run_until_parked();
    let opened = harness.panes.opened().len();
    close_own_tab(&harness, local_view(&harness, cx).0, cx);
    project_window(&harness, cx);

    assert_eq!(
        local_view(&harness, cx),
        survivor_view,
        "the survivor becomes the Active Tab"
    );
    assert!(
        cx.update(|window, cx| harness
            .manager
            .read(cx)
            .focused_terminal_has_input_focus(window, cx)),
        "the keyboard reaches the survivor's terminal"
    );
    assert_eq!(
        harness.panes.opened().len(),
        opened,
        "no Pane subscribes again"
    );
    assert!(
        std::sync::Arc::ptr_eq(&harness.panes.pane(survivor_id), &survivor),
        "the survivor keeps the connection to its running Terminal Session"
    );
    assert_eq!(
        survivor.restored_starts(),
        0,
        "nothing starts a fresh shell"
    );
    assert_eq!(survivor.take_overs(), 0, "nothing takes the Pane over");
}

/// The server can publish its answer to this client's close before the window reconciles the
/// close. The dormant Tab that takes the closed Tab's place still opens, once.
#[gpui::test]
fn an_answer_published_before_the_close_reconciles_still_opens_the_successor_once(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = window_harness(cx);
    let successor_view = local_view(&harness, cx);
    let successor = harness
        .panes
        .pane(harness.server.inventory().workspaces[0].tabs[0].root_pane);
    successor.set(DevelopmentPaneStatus::Restored);
    click("create-tab-button", cx);
    harness.server.finish(true, true);
    project_window(&harness, cx);
    let before = successor.selections();

    close_own_tab(&harness, local_view(&harness, cx).0, cx);
    assert!(matches!(
        harness.server.finish(true, true).outcome,
        Outcome::Accepted { .. }
    ));
    project_window(&harness, cx);

    assert_eq!(local_view(&harness, cx), successor_view);
    assert_eq!(
        successor.selections(),
        before + 1,
        "the person's close opens the Tab that takes its place"
    );
    project_window(&harness, cx);
    assert_eq!(
        successor.selections(),
        before + 1,
        "a later reconciliation selects nothing more"
    );
}

/// This client closes an inactive Tab while another client removes the Active Tab. The Tab that
/// takes the Active Tab's place follows another client's change, so nothing selects its Pane.
#[gpui::test]
fn another_clients_removal_of_the_active_tab_beside_an_own_close_selects_nothing(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = window_harness(cx);
    let own_target = local_view(&harness, cx).0;
    for _ in 0..2 {
        click("create-tab-button", cx);
        harness.server.finish(true, true);
        project_window(&harness, cx);
    }
    cx.dispatch_action(crate::ui::PreviousTab);
    cx.run_until_parked();
    let removed = local_view(&harness, cx).0;
    let removed_server = harness
        .manager
        .read_with(cx, |manager, _| manager.shared_server_tab(removed))
        .unwrap();
    let successor = harness
        .panes
        .pane(harness.server.inventory().workspaces[0].tabs[2].root_pane);
    let before = successor.selections();

    close_own_tab(&harness, own_target, cx);
    harness.server.remove_tab_elsewhere(removed_server);
    project_window(&harness, cx);

    assert_ne!(local_view(&harness, cx).0, removed);
    assert_eq!(
        successor.selections(),
        before,
        "another client's removal asks the server for no Pane here"
    );
}

#[test]
fn optimistic_field_presentation_never_renews_an_unseen_conflicting_version() {
    let server = DelayedServer::new();
    let (_, mut model) = model(&server);
    let local = first_workspace_id();
    let workspace = server.inventory().workspaces[0].id;
    model.rename(local, "First draft".into(), Some(0));
    server.rename_elsewhere(workspace, "Their name");
    let projection = model.poll().inventory.unwrap();
    assert_eq!(projection.workspaces[0].name, "First draft");
    assert_eq!(
        projection.workspaces[0].name_version, 0,
        "an optimistic value still carries the version its editor actually observed"
    );
    model.register(
        local,
        SharedWorkspaceFacts::from_server(&projection.workspaces[0]),
    );
    let observed = model.facts(local).unwrap().name_version;
    model.rename(local, "Later draft".into(), Some(observed));
    assert_eq!(server.finish(true, true).outcome, Outcome::Conflict);
    model.poll();
    assert_eq!(server.finish(true, true).outcome, Outcome::Conflict);
    model.poll();
    assert_eq!(server.inventory().workspaces[0].name, "Their name");
    for draft in ["First draft", "Later draft"] {
        let conflict = model.take_conflict().unwrap();
        assert_eq!(conflict.draft, SharedWorkspaceDraft::Name(draft.into()));
        assert_eq!(conflict.authoritative.as_ref(), "Their name");
    }
    assert!(model.take_conflict().is_none());
}
