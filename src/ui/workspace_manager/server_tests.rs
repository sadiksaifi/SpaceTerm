use super::*;
use crate::domain::server_view_testing::{A, B, Org, Replica, ready, server, tab, ws};
use crate::ui::server_client_runtime::DesktopClientProvider as _;
use crate::ui::server_desktop_contracts::DesktopServerInfo;
use crate::ui::tab_view::server_testing::{
    TestPanes, TestRuntime, frame, operation, view_revision,
};
use gpui::{TestAppContext, VisualTestContext};
use spaceterm_client::view::ReplicaObservation;
use spaceterm_core::operations::{AssessmentRef, AssessmentSummary, RequestDigest};

pub(in crate::ui::workspace_manager) fn harness(
    cx: &mut TestAppContext,
) -> (
    Entity<WorkspaceManager>,
    TestRuntime,
    &mut VisualTestContext,
) {
    cx.update(crate::ui::init).unwrap();
    let runtime = TestRuntime::new(cx);
    let source = runtime.runtime.clone();
    let (manager, cx) = cx.add_window_view(|window, cx| {
        let mut manager = WorkspaceManager::new_server(
            source,
            super::super::tests::workspace_adapters(
                super::super::tests::test_remote_backend_factory(),
            ),
            SidebarState::Expanded,
            window,
            cx,
        );
        manager.server.as_mut().unwrap().panes = Rc::new(TestPanes::default());
        manager
    });
    cx.run_until_parked();
    (manager, runtime, cx)
}

fn replica(byte: u8) -> Replica {
    let mut org = Org::new(byte);
    org.add_workspace(1, 1, 1);
    Replica::attached(org, 1)
}

pub(in crate::ui::workspace_manager) fn publish(
    replica: &Replica,
    revision: usize,
    runtime: &TestRuntime,
    cx: &mut VisualTestContext,
) {
    let mut publication = frame(replica, ReplicaStatus::Ready);
    publication.view_revision = view_revision(revision);
    let facts = ready(replica.org.server().bytes()[0]);
    publication.connection = ConnectionState::Connected {
        info: spaceterm_client::api::ConnectedInfo {
            server: replica.org.server(),
            pairing: facts.pairing,
            client: facts.client,
            epoch: publication.organization.epoch(),
            binding: facts.connection.unwrap(),
            major: 2,
            limits: spaceterm_protocol::limits::NegotiatedLimits::default(),
            capabilities: spaceterm_client::api::ConnectionCapabilities::NONE,
            organization_revision: publication.organization.state().revision,
            attempt: 1,
        },
    };
    runtime.provider.set_frame(Arc::new(publication));
    runtime.notifier.frame_changed(replica.org.server());
    cx.run_until_parked();
}

pub(in crate::ui::workspace_manager) fn servers(runtime: &TestRuntime, bytes: &[u8]) {
    runtime.provider.set_servers(
        bytes
            .iter()
            .map(|byte| DesktopServerInfo {
                server: server(*byte),
                name: DisplayName::reported(if *byte == A { "Local" } else { "Remote" }),
                local: *byte == A,
            })
            .collect(),
    );
}

#[gpui::test]
fn desktop_server_two_servers_share_one_window_without_federation(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[B, A]);
    let a = replica(A);
    let mut b = replica(B);
    b.personal
        .attach(
            ws(A, 1),
            &ReplicaObservation::new(&a.org.snapshot(), &[], ready(A)).unwrap(),
        )
        .unwrap();
    let mut a = a;
    a.personal = b.personal.clone();
    publish(&a, 1, &runtime, cx);
    publish(&b, 1, &runtime, cx);
    let attachments = manager.read_with(cx, |manager, _| {
        manager
            .server
            .as_ref()
            .unwrap()
            .collection
            .order()
            .collect::<Vec<_>>()
    });
    assert_eq!(attachments, vec![ws(B, 1), ws(A, 1)]);
    assert!(
        runtime.begun().is_empty(),
        "publication is presentation only"
    );
    assert_eq!(
        cx.update(|_, cx| runtime
            .runtime
            .read(cx)
            .servers()
            .iter()
            .map(|info| info.server)
            .collect::<Vec<_>>()),
        vec![server(A), server(B)]
    );
    assert!(manager.read_with(cx, |manager, _| manager.workspaces.len() == 0));
}

#[gpui::test]
fn desktop_server_detach_persists_and_final_detach_shows_empty_window(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut replica = replica(A);
    publish(&replica, 1, &runtime, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.handle_server_sidebar_event(ServerSidebarEvent::Detach(ws(A, 1)), window, cx)
        })
    });
    assert_eq!(
        runtime.begun(),
        vec![DesktopRequest::DetachWorkspace(ws(A, 1))]
    );
    // The provider owns marker-first durability. This frame represents that committed removal.
    let plan = replica.personal.prepare_detach(ws(A, 1)).unwrap();
    replica.personal.apply_detach(plan).unwrap();
    publish(&replica, 2, &runtime, cx);
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.active_server_workspace()),
        None
    );
    assert!(cx.debug_bounds("empty-workspace-window").is_some());
    assert_eq!(
        runtime.begun().len(),
        1,
        "no replacement creation or Session retirement"
    );
}

#[gpui::test]
fn desktop_server_coherent_frames_update_inactive_workspaces(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut r = replica(A);
    r.org.add_workspace(2, 2, 2);
    r.personal
        .attach(
            ws(A, 2),
            &ReplicaObservation::new(&r.org.snapshot(), &[], ready(A)).unwrap(),
        )
        .unwrap();
    r.personal.activate_workspace(ws(A, 1)).unwrap();
    publish(&r, 1, &runtime, cx);
    r.org.add_tab(2, 3, 3, None);
    r.reconcile_ready(&[]);
    publish(&r, 2, &runtime, cx);
    let order = manager.read_with(cx, |manager, cx| {
        manager
            .server
            .as_ref()
            .unwrap()
            .collection
            .payload(ws(A, 2))
            .unwrap()
            .read(cx)
            .server_tab_order()
    });
    assert_eq!(order, vec![tab(A, 2), tab(A, 3)]);
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.active_server_workspace()),
        Some(ws(A, 1))
    );
    assert!(runtime.begun().is_empty());
}

#[gpui::test]
fn desktop_server_personal_edit_receipt_binds_frame_revision(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut r = replica(A);
    r.org.add_tab(1, 2, 2, None);
    r.reconcile_ready(&[]);
    publish(&r, 1, &runtime, cx);
    let tabs = manager.read_with(cx, |manager, _| {
        manager
            .server
            .as_ref()
            .unwrap()
            .active_manager()
            .unwrap()
            .clone()
    });
    cx.update(|window, cx| {
        window.activate_window();
        manager.update(cx, |manager, cx| manager.focus(window, cx));
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.dispatch_action(Box::new(crate::ui::MoveTabRight), cx));
    cx.run_until_parked();
    assert_eq!(runtime.begun().len(), 1);
    runtime.provider.finish(0, Ok(None));
    r.personal
        .move_tab(ws(A, 1), tab(A, 1), 1, &r.org.snapshot())
        .unwrap();
    publish(&r, 2, &runtime, cx);
    let receipt = runtime.notifier.clone();
    let delivery = cx.foreground_executor().spawn(async move {
        receipt
            .personal_edit_applied(UiRequestId::FIRST, view_revision(2))
            .await
    });
    cx.run_until_parked();
    cx.foreground_executor().block_test(delivery).unwrap();
    cx.run_until_parked();
    assert_eq!(
        tabs.read_with(cx, |tabs, _| tabs.server_tab_order()),
        vec![tab(A, 2), tab(A, 1)]
    );
}

#[gpui::test]
fn desktop_server_kill_uses_close_workspace_command_and_exact_confirmation(
    cx: &mut TestAppContext,
) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut workspace = replica(A);
    workspace.org.add_tab(1, 2, 2, None);
    publish(&workspace, 1, &runtime, cx);
    cx.update(|window, cx| {
        window.activate_window();
        manager.update(cx, |manager, cx| manager.focus(window, cx));
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    assert_eq!(
        runtime.begun(),
        vec![DesktopRequest::Shared {
            server: server(A),
            command: Command::AssessKillWorkspace {
                workspace: ws(A, 1)
            }
        }]
    );
    let op = operation(server(A), 1);
    let assessment = CloseAssessment::new(
        AssessmentRef {
            operation: op,
            digest: RequestDigest::for_command(&Command::AssessKillWorkspace {
                workspace: ws(A, 1),
            })
            .unwrap(),
        },
        spaceterm_core::authority::ServerEpoch::new(1),
        AssessmentSummary::new(AssessmentTarget::KillWorkspace(ws(A, 1)), 1, 5, 7, 4, true)
            .unwrap(),
    )
    .unwrap();
    runtime
        .provider
        .set_outcome(op, Arc::new(Outcome::Assessment(Box::new(assessment))));
    runtime.provider.finish(0, Ok(Some(op)));
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .server
            .as_ref()
            .unwrap()
            .confirmation),
        Some(assessment)
    );
    assert_eq!(
        runtime.begun().len(),
        1,
        "assessment does not authorize Kill"
    );
    assert!(
        cx.debug_bounds("modal-action-close-confirmation-cancel")
            .is_some()
    );
    let cancel = cx
        .debug_bounds("modal-action-close-confirmation-cancel")
        .unwrap()
        .center();
    cx.simulate_click(cancel, gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(runtime.begun().len(), 1, "Cancel must not submit Kill");
    assert!(manager.read_with(cx, |manager, _| {
        manager.server.as_ref().unwrap().confirmation.is_none()
    }));
    cx.update(|window, cx| manager.update(cx, |manager, cx| manager.focus(window, cx)));
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    assert_eq!(runtime.begun()[1], runtime.begun()[0]);
    let op = operation(server(A), 2);
    let confirmed_assessment = CloseAssessment::new(
        AssessmentRef {
            operation: op,
            digest: assessment.reference().digest,
        },
        assessment.epoch(),
        assessment.summary(),
    )
    .unwrap();
    runtime.provider.set_outcome(
        op,
        Arc::new(Outcome::Assessment(Box::new(confirmed_assessment))),
    );
    runtime.provider.finish(1, Ok(Some(op)));
    cx.run_until_parked();
    let confirm = cx
        .debug_bounds("modal-action-close-confirmation-confirm")
        .unwrap()
        .center();
    cx.simulate_click(confirm, gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        runtime.begun()[2],
        DesktopRequest::Shared {
            server: server(A),
            command: Command::KillWorkspaceConfirmed {
                workspace: ws(A, 1),
                assessment: confirmed_assessment.reference()
            }
        }
    );
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.active_server_workspace()),
        Some(ws(A, 1)),
        "only replica deletion removes shared work"
    );
}

#[gpui::test]
fn desktop_server_view_save_failure_prevents_false_saved_quit(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    runtime
        .provider
        .fail_save(DesktopClientError::LocalStorageUnavailable);
    let save = manager.update(cx, |manager, cx| manager.save_server_view(true, cx));
    assert_eq!(
        cx.foreground_executor().block_test(save),
        Err(DesktopClientError::LocalStorageUnavailable)
    );
    assert!(runtime.begun().is_empty());
    assert_eq!(runtime.provider.quits(), 1);
}

#[gpui::test]
fn desktop_server_window_close_saves_without_detach_or_quit(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    cx.update(|window, cx| {
        assert!(!manager.update(cx, |manager, cx| manager.should_close_window(window, cx)))
    });
    cx.run_until_parked();
    assert_eq!(runtime.provider.saves(), 1);
    assert_eq!(runtime.provider.quits(), 0);
    assert!(runtime.begun().is_empty());
}

#[gpui::test]
fn desktop_server_older_other_server_frame_cannot_restore_a_detached_attachment(
    cx: &mut TestAppContext,
) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A, B]);
    let mut a = replica(A);
    let mut b = replica(B);
    a.personal
        .attach(
            ws(B, 1),
            &ReplicaObservation::new(&b.org.snapshot(), &[], ready(B)).unwrap(),
        )
        .unwrap();
    b.personal = a.personal.clone();
    publish(&a, 1, &runtime, cx);
    publish(&b, 1, &runtime, cx);
    a.personal
        .apply_detach(a.personal.prepare_detach(ws(B, 1)).unwrap())
        .unwrap();
    publish(&a, 2, &runtime, cx);
    publish(&b, 2, &runtime, cx);
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .server
            .as_ref()
            .unwrap()
            .collection
            .order()
            .collect::<Vec<_>>()),
        vec![ws(A, 1)]
    );
    assert!(manager.read_with(cx, |manager, _| {
        manager
            .server
            .as_ref()
            .unwrap()
            .collection
            .record(ws(B, 1))
            .is_none()
    }));
}

#[gpui::test]
fn desktop_server_quit_waits_for_completion_application_receipt_and_presented_frame(
    cx: &mut TestAppContext,
) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut r = replica(A);
    r.org.add_tab(1, 2, 2, None);
    r.reconcile_ready(&[]);
    publish(&r, 1, &runtime, cx);
    cx.update(|window, cx| {
        window.activate_window();
        manager.update(cx, |manager, cx| manager.focus(window, cx));
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.dispatch_action(Box::new(crate::ui::MoveTabRight), cx));
    cx.run_until_parked();
    assert_eq!(runtime.begun().len(), 1);
    let save = manager.update(cx, |manager, cx| manager.save_server_view(true, cx));
    cx.run_until_parked();
    assert_eq!(runtime.provider.quits(), 0);
    runtime.provider.finish(0, Ok(None));
    cx.run_until_parked();
    assert_eq!(
        runtime.provider.quits(),
        0,
        "completion is not application evidence"
    );
    r.personal
        .move_tab(ws(A, 1), tab(A, 1), 1, &r.org.snapshot())
        .unwrap();
    publish(&r, 2, &runtime, cx);
    assert_eq!(
        runtime.provider.quits(),
        0,
        "equal content is not an application receipt"
    );
    let notifier = runtime.notifier.clone();
    let receipt = cx.foreground_executor().spawn(async move {
        notifier
            .personal_edit_applied(UiRequestId::FIRST, view_revision(2))
            .await
    });
    cx.run_until_parked();
    cx.foreground_executor().block_test(receipt).unwrap();
    cx.run_until_parked();
    assert_eq!(cx.foreground_executor().block_test(save), Ok(()));
    assert_eq!(runtime.provider.quits(), 1);
    assert_eq!(
        runtime.begun().len(),
        1,
        "quit never detaches or retires server work"
    );
}

#[gpui::test]
fn desktop_server_switcher_lists_detached_only_for_current_connected_inventory(
    cx: &mut TestAppContext,
) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut r = replica(A);
    r.org.add_workspace(2, 2, 2);
    r.reconcile_ready(&[]);
    publish(&r, 1, &runtime, cx);
    assert_eq!(
        manager.read_with(cx, |manager, cx| manager
            .server_switcher_items(cx)
            .iter()
            .map(|item| *item.id())
            .collect::<Vec<_>>()),
        vec![ws(A, 1), ws(A, 2)]
    );
    let mut retained = frame(&r, ReplicaStatus::RetainedOffline);
    retained.view_revision = view_revision(2);
    runtime.provider.set_frame(Arc::new(retained));
    runtime.notifier.frame_changed(server(A));
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, cx| manager
            .server_switcher_items(cx)
            .iter()
            .map(|item| *item.id())
            .collect::<Vec<_>>()),
        vec![ws(A, 1)]
    );
    assert!(runtime.begun().is_empty());
}

#[gpui::test]
fn desktop_server_sidebar_arrow_landing_changes_only_personal_activation(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    publish(&replica(A), 1, &runtime, cx);
    cx.update(|window, cx| {
        window.activate_window();
        manager.update(cx, |manager, cx| manager.toggle_sidebar_focus(window, cx));
    });
    cx.run_until_parked();
    assert!(
        cx.update(|window, cx| manager.read(cx).sidebar.read(cx).is_focused(window)),
        "sidebar receives native focus before navigation"
    );
    cx.simulate_keystrokes("home");
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    assert_eq!(
        runtime.begun(),
        vec![DesktopRequest::PersonalEdit(ViewEdit::ActivateWorkspace(
            ws(A, 1)
        ))]
    );
    cx.simulate_keystrokes("left");
    cx.run_until_parked();
    assert_eq!(runtime.begun().len(), 1);
}

#[gpui::test]
fn desktop_server_offline_kill_has_no_provider_effect(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let r = replica(A);
    let retained = frame(&r, ReplicaStatus::RetainedOffline);
    runtime.provider.set_frame(Arc::new(retained));
    runtime.notifier.frame_changed(server(A));
    cx.run_until_parked();
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.request_server_close(AssessmentTarget::KillWorkspace(ws(A, 1)), window, cx)
        })
    });
    cx.run_until_parked();
    assert!(runtime.begun().is_empty());
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.active_server_workspace()),
        Some(ws(A, 1))
    );
}

#[gpui::test]
fn desktop_server_stale_close_assessment_requires_new_confirmation(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    publish(&replica(A), 1, &runtime, cx);
    let target = AssessmentTarget::Close(ServerCloseTarget::Tab(tab(A, 1)));
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.request_server_close(target, window, cx)
        })
    });
    let assessment = |sequence, live, required| {
        CloseAssessment::new(
            AssessmentRef {
                operation: operation(server(A), sequence),
                digest: RequestDigest::for_command(&Command::AssessClose {
                    target: ServerCloseTarget::Tab(tab(A, 1)),
                })
                .unwrap(),
            },
            spaceterm_core::authority::ServerEpoch::new(1),
            AssessmentSummary::new(target, 1, 1, 1, live, required).unwrap(),
        )
        .unwrap()
    };
    runtime.provider.set_outcome(
        operation(server(A), 1),
        Arc::new(Outcome::Assessment(Box::new(assessment(1, 1, true)))),
    );
    runtime
        .provider
        .finish(0, Ok(Some(operation(server(A), 1))));
    cx.run_until_parked();
    super::super::tests::click("modal-action-close-confirmation-confirm", cx);
    assert_eq!(runtime.begun().len(), 2);
    runtime.provider.set_outcome(
        operation(server(A), 2),
        Arc::new(Outcome::Failed(OperationFailure::StaleAssessment)),
    );
    runtime
        .provider
        .finish(1, Ok(Some(operation(server(A), 2))));
    cx.run_until_parked();
    assert_eq!(runtime.begun().len(), 3);
    runtime.provider.set_outcome(
        operation(server(A), 3),
        Arc::new(Outcome::Assessment(Box::new(assessment(3, 0, false)))),
    );
    runtime
        .provider
        .finish(2, Ok(Some(operation(server(A), 3))));
    cx.run_until_parked();
    assert_eq!(
        runtime.begun().len(),
        3,
        "a stale approval cannot authorize even an idle replacement assessment"
    );
    assert!(
        cx.debug_bounds("modal-action-close-confirmation-cancel-keyboard-focus")
            .is_some()
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        runtime.begun().len(),
        3,
        "Return does not approve changed scope"
    );
}

#[gpui::test]
fn desktop_server_activation_shortcuts_follow_server_section_order(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[B, A]);
    let mut a = replica(A);
    let b = replica(B);
    a.personal
        .attach(
            ws(B, 1),
            &ReplicaObservation::new(&b.org.snapshot(), &[], ready(B)).unwrap(),
        )
        .unwrap();
    let mut b = b;
    b.personal = a.personal.clone();
    publish(&a, 1, &runtime, cx);
    publish(&b, 1, &runtime, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.activate_workspace_at(0, window, cx)
        })
    });
    assert_eq!(
        runtime.begun(),
        vec![DesktopRequest::PersonalEdit(ViewEdit::ActivateWorkspace(
            ws(A, 1)
        ))]
    );
}

#[gpui::test]
fn desktop_server_inactive_publication_cannot_steal_workspace_focus(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut r = replica(A);
    r.org.add_workspace(2, 2, 2).add_tab(2, 3, 3, None);
    r.personal
        .attach(
            ws(A, 2),
            &ReplicaObservation::new(&r.org.snapshot(), &[], ready(A)).unwrap(),
        )
        .unwrap();
    r.personal.activate_tab(ws(A, 2), tab(A, 3)).unwrap();
    r.personal.activate_workspace(ws(A, 1)).unwrap();
    publish(&r, 1, &runtime, cx);
    let tabs = |workspace, cx: &mut VisualTestContext| {
        manager.read_with(cx, |manager, _| {
            manager
                .server
                .as_ref()
                .unwrap()
                .collection
                .payload(workspace)
                .unwrap()
                .clone()
        })
    };
    let a = tabs(ws(A, 1), cx);
    let b = tabs(ws(A, 2), cx);
    cx.update(|window, cx| {
        window.activate_window();
        manager.update(cx, |manager, cx| manager.focus(window, cx));
    });
    cx.run_until_parked();
    assert!(cx.update(|window, cx| a.read(cx).focused_terminal_is_focused(window, cx)));
    r.org.delete_tab(3);
    r.reconcile_ready(&[]);
    publish(&r, 2, &runtime, cx);
    assert!(
        cx.update(|window, cx| a.read(cx).focused_terminal_is_focused(window, cx)),
        "inactive Workspace publication cannot take native focus"
    );
    r.personal.activate_workspace(ws(A, 2)).unwrap();
    publish(&r, 3, &runtime, cx);
    assert!(
        cx.update(|window, cx| b.read(cx).focused_terminal_is_focused(window, cx)),
        "accepted personal activation restores focus without explicit use"
    );
    assert!(runtime.begun().is_empty());
}

fn workspace_activation_save_order(cx: &mut TestAppContext, order: [u8; 3], quitting: bool) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut r = replica(A);
    r.org.add_workspace(2, 2, 2);
    r.personal
        .attach(
            ws(A, 2),
            &ReplicaObservation::new(&r.org.snapshot(), &[], ready(A)).unwrap(),
        )
        .unwrap();
    r.personal.activate_workspace(ws(A, 1)).unwrap();
    publish(&r, 1, &runtime, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.select_server_workspace(ws(A, 2), window, cx)
        })
    });
    let save = manager.update(cx, |manager, cx| manager.save_server_view(quitting, cx));
    cx.run_until_parked();
    for (index, event) in order.into_iter().enumerate() {
        assert_eq!(
            runtime.provider.quits() + runtime.provider.saves(),
            0,
            "saving requires completion, receipt, and its presented frame"
        );
        match event {
            0 => runtime.provider.finish(0, Ok(None)),
            1 => {
                let notifier = runtime.notifier.clone();
                let delivery = cx.foreground_executor().spawn(async move {
                    notifier
                        .personal_edit_applied(UiRequestId::FIRST, view_revision(2))
                        .await
                });
                cx.run_until_parked();
                cx.foreground_executor().block_test(delivery).unwrap();
            }
            2 => {
                r.personal.activate_workspace(ws(A, 2)).unwrap();
                publish(&r, 2, &runtime, cx);
            }
            _ => unreachable!(),
        }
        cx.run_until_parked();
        if index < 2 {
            assert_eq!(runtime.provider.quits() + runtime.provider.saves(), 0);
        }
    }
    assert_eq!(cx.foreground_executor().block_test(save), Ok(()));
    assert_eq!(runtime.provider.quits(), usize::from(quitting));
    assert_eq!(runtime.provider.saves(), usize::from(!quitting));
    assert_eq!(runtime.begun().len(), 1, "save changes no server work");
}

#[gpui::test]
fn desktop_server_workspace_activation_completion_first_quit_waits_for_receipt(
    cx: &mut TestAppContext,
) {
    workspace_activation_save_order(cx, [0, 1, 2], true);
}

#[gpui::test]
fn desktop_server_workspace_activation_receipt_first_save_waits_for_frame(cx: &mut TestAppContext) {
    workspace_activation_save_order(cx, [1, 0, 2], false);
}

#[gpui::test]
fn desktop_server_workspace_activation_frame_first_quit_waits_for_completion(
    cx: &mut TestAppContext,
) {
    workspace_activation_save_order(cx, [2, 1, 0], true);
}

#[gpui::test]
fn desktop_server_admitted_detach_failure_is_reported(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    publish(&replica(A), 1, &runtime, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.handle_server_sidebar_event(ServerSidebarEvent::Detach(ws(A, 1)), window, cx)
        })
    });
    runtime
        .provider
        .finish(0, Err(DesktopClientError::LocalStorageUnavailable));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("modal-action-server-detach-failed-ok")
            .is_some(),
        "an asynchronous local durability failure must be visible"
    );
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.active_server_workspace()),
        Some(ws(A, 1))
    );
}

#[gpui::test]
fn desktop_server_unavailable_attachment_is_not_the_empty_window(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    let panes = Rc::new(TestPanes::default());
    panes.fail_with(Some(DesktopClientError::UnsupportedCapability));
    manager.update(cx, |manager, _| {
        manager.server.as_mut().unwrap().panes = panes
    });
    servers(&runtime, &[A]);
    publish(&replica(A), 1, &runtime, cx);
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.active_server_workspace()),
        Some(ws(A, 1))
    );
    assert!(
        cx.debug_bounds("empty-workspace-window").is_none(),
        "an attached Workspace whose Pane preparation failed is not zero attachments"
    );
    assert!(cx.debug_bounds("unavailable-workspace").is_some());
    assert!(runtime.begun().is_empty());
}

#[gpui::test]
fn desktop_server_workspace_save_does_not_wait_for_network_retry(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    publish(&replica(A), 1, &runtime, cx);
    runtime
        .runtime
        .update(cx, |runtime, cx| {
            runtime.request(DesktopRequest::RetryConnection(server(A)), cx)
        })
        .unwrap();
    let save = manager.update(cx, |manager, cx| manager.save_server_view(true, cx));
    cx.run_until_parked();
    assert_eq!(runtime.provider.quits(), 1);
    assert_eq!(cx.foreground_executor().block_test(save), Ok(()));
    assert_eq!(
        runtime.begun(),
        vec![DesktopRequest::RetryConnection(server(A))]
    );
}

fn created_workspace_attention(
    cx: &mut TestAppContext,
    publication_first: bool,
    navigate_away: bool,
) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut r = replica(A);
    publish(&r, 1, &runtime, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.create_server_workspace(Some(server(A)), String::new(), window, cx)
        })
    });
    if navigate_away {
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.select_server_workspace(ws(A, 1), window, cx)
            })
        });
    }
    r.org.add_workspace(2, 2, 2).publish();
    r.reconcile_ready(&[]);
    let op = operation(server(A), 1);
    let outcome = Arc::new(Outcome::CreatedWorkspace {
        workspace: ws(A, 2),
        tab: tab(A, 2),
        pane: crate::domain::server_view_testing::pane(A, 2),
    });
    if publication_first {
        publish(&r, 2, &runtime, cx);
    }
    runtime
        .provider
        .set_outcome_at(op, outcome, r.org.snapshot().state().revision);
    runtime.provider.finish(0, Ok(Some(op)));
    cx.run_until_parked();
    if !publication_first {
        assert!(
            !runtime
                .begun()
                .iter()
                .any(|request| matches!(request, DesktopRequest::AttachWorkspace(_))),
            "outcome alone cannot attach a not-yet-published target"
        );
        publish(&r, 2, &runtime, cx);
    }
    assert_eq!(
        runtime
            .begun()
            .iter()
            .any(|request| *request == DesktopRequest::AttachWorkspace(ws(A, 2))),
        !navigate_away,
        "only the still-current creation context attaches its published target"
    );
}

#[gpui::test]
fn desktop_server_create_outcome_first_attaches_only_after_its_publication(
    cx: &mut TestAppContext,
) {
    created_workspace_attention(cx, false, false);
}

#[gpui::test]
fn desktop_server_create_publication_first_attaches_after_its_original_outcome(
    cx: &mut TestAppContext,
) {
    created_workspace_attention(cx, true, false);
}

#[gpui::test]
fn desktop_server_create_cannot_override_later_workspace_navigation(cx: &mut TestAppContext) {
    created_workspace_attention(cx, false, true);
}

#[gpui::test]
fn desktop_server_switcher_attaches_detached_and_activates_attached(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut r = replica(A);
    r.org.add_workspace(2, 2, 2);
    r.reconcile_ready(&[]);
    publish(&r, 1, &runtime, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.select_server_workspace(ws(A, 2), window, cx)
        })
    });
    runtime.provider.finish(0, Ok(None));
    cx.run_until_parked();
    assert_eq!(
        runtime.begun(),
        vec![DesktopRequest::AttachWorkspace(ws(A, 2))]
    );
    r.personal
        .attach(
            ws(A, 2),
            &ReplicaObservation::new(&r.org.snapshot(), &[], ready(A)).unwrap(),
        )
        .unwrap();
    publish(&r, 2, &runtime, cx);
    assert_eq!(
        runtime.begun().len(),
        1,
        "attachment equality cannot replace application evidence"
    );
    let notifier = runtime.notifier.clone();
    let receipt = cx.foreground_executor().spawn(async move {
        notifier
            .personal_edit_applied(UiRequestId::FIRST, view_revision(2))
            .await
    });
    cx.run_until_parked();
    cx.foreground_executor().block_test(receipt).unwrap();
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.active_server_workspace()),
        Some(ws(A, 2))
    );
    assert_eq!(
        runtime.begun().len(),
        1,
        "the portable attach operation already activated its committed target"
    );
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.select_server_workspace(ws(A, 1), window, cx)
        })
    });
    assert_eq!(
        runtime.begun()[1],
        DesktopRequest::PersonalEdit(ViewEdit::ActivateWorkspace(ws(A, 1)))
    );
}

#[gpui::test]
fn desktop_server_attachment_operations_report_asynchronous_failures(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut r = replica(A);
    r.org.add_workspace(2, 2, 2);
    r.reconcile_ready(&[]);
    publish(&r, 1, &runtime, cx);
    for index in 0..3 {
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| match index {
                0 => manager.select_server_workspace(ws(A, 2), window, cx),
                1 => manager.select_server_workspace(ws(A, 1), window, cx),
                _ => manager.create_server_workspace(Some(server(A)), String::new(), window, cx),
            })
        });
        runtime
            .provider
            .finish(index, Err(DesktopClientError::AuthorizationUnavailable));
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("modal-action-server-request-failed-ok")
                .is_some()
        );
        super::super::tests::click("modal-action-server-request-failed-ok", cx);
    }
}

#[gpui::test]
fn desktop_server_failed_detach_stays_paused_across_publication_and_retry(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut r = replica(A);
    publish(&r, 1, &runtime, cx);
    let tabs = manager.read_with(cx, |manager, _| {
        manager
            .server
            .as_ref()
            .unwrap()
            .active_manager()
            .unwrap()
            .clone()
    });
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.handle_server_sidebar_event(ServerSidebarEvent::Detach(ws(A, 1)), window, cx)
        })
    });
    runtime
        .provider
        .finish(0, Err(DesktopClientError::LocalStorageUnavailable));
    cx.run_until_parked();
    super::super::tests::click("modal-action-server-detach-failed-retry", cx);
    assert_eq!(
        runtime.begun(),
        vec![
            DesktopRequest::DetachWorkspace(ws(A, 1)),
            DesktopRequest::RetryDetachWorkspace {
                workspace: ws(A, 1),
                original_request: UiRequestId::FIRST
            }
        ]
    );
    publish(&r, 2, &runtime, cx);
    assert!(cx.debug_bounds("unavailable-workspace").is_some());
    cx.update(|window, cx| {
        window.activate_window();
        manager.update(cx, |manager, cx| manager.focus(window, cx));
        assert!(
            !tabs.read(cx).focused_terminal_has_input_focus(window, cx),
            "a retained frame cannot restore presentation eligibility after a failed detach"
        );
    });
    runtime.provider.finish(1, Ok(None));
    r.personal
        .apply_detach(r.personal.prepare_detach(ws(A, 1)).unwrap())
        .unwrap();
    publish(&r, 3, &runtime, cx);
    assert!(cx.debug_bounds("empty-workspace-window").is_some());
}

#[gpui::test]
fn desktop_server_creation_attention_is_cancelled_by_detach(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut r = replica(A);
    publish(&r, 1, &runtime, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.create_server_workspace(Some(server(A)), String::new(), window, cx);
            manager.handle_server_sidebar_event(ServerSidebarEvent::Detach(ws(A, 1)), window, cx);
        })
    });
    r.org.add_workspace(2, 2, 2).publish();
    r.reconcile_ready(&[]);
    let op = operation(server(A), 1);
    runtime.provider.set_outcome_at(
        op,
        Arc::new(Outcome::CreatedWorkspace {
            workspace: ws(A, 2),
            tab: tab(A, 2),
            pane: crate::domain::server_view_testing::pane(A, 2),
        }),
        r.org.snapshot().state().revision,
    );
    runtime.provider.finish(0, Ok(Some(op)));
    publish(&r, 2, &runtime, cx);
    assert!(
        !runtime
            .begun()
            .iter()
            .any(|request| matches!(request, DesktopRequest::AttachWorkspace(_)))
    );
}

#[gpui::test]
fn desktop_server_old_detach_recovery_cannot_target_a_new_attachment(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut r = replica(A);
    publish(&r, 1, &runtime, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.handle_server_sidebar_event(ServerSidebarEvent::Detach(ws(A, 1)), window, cx)
        })
    });
    runtime
        .provider
        .finish(0, Err(DesktopClientError::LocalStorageUnavailable));
    cx.run_until_parked();
    r.personal
        .apply_detach(r.personal.prepare_detach(ws(A, 1)).unwrap())
        .unwrap();
    r.personal
        .attach(
            ws(A, 1),
            &ReplicaObservation::new(&r.org.snapshot(), &[], ready(A)).unwrap(),
        )
        .unwrap();
    publish(&r, 2, &runtime, cx);
    super::super::tests::click("modal-action-server-detach-failed-retry", cx);
    assert_eq!(
        runtime.begun(),
        vec![DesktopRequest::DetachWorkspace(ws(A, 1))],
        "an old recovery Alert cannot start another detach against the replacement incarnation"
    );
    assert!(cx.debug_bounds("unavailable-workspace").is_none());
}

#[gpui::test]
fn desktop_directory_repair_uses_server_picker_and_routes_only_the_confirmed_start(
    cx: &mut TestAppContext,
) {
    use crate::ui::server_desktop_contracts::{
        AttachmentGeneration, PaneAction, PaneViewToken, SelectionSource,
    };
    use spaceterm_core::{
        authority::{ContextGeneration, FieldVersion, OrganizationRevision},
        organization::{ControlRecord, OrganizationSnapshot, SessionPhase},
    };
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[7]);
    let replica = replica(7);
    let pane = replica.org.snapshot().state().panes[0].id;
    let token = PaneViewToken {
        pane,
        attachment_generation: AttachmentGeneration::FIRST,
        context_generation: ContextGeneration::new(1),
    };
    runtime.provider.set_pane_token(token);
    runtime.provider.set_directory_connection(
        server(7),
        crate::ui::directory_picker::server_tests::Peer::new(),
    );
    publish(&replica, 1, &runtime, cx);
    let mut initial = frame(&replica, ReplicaStatus::Ready);
    initial.connection = runtime
        .provider
        .frame(server(7))
        .unwrap()
        .connection
        .clone();
    initial.view_revision = view_revision(1);
    let mut facts = initial.organization.runtime().to_vec();
    facts[0].control = ControlRecord::Held {
        client: ready(7).client,
        generation: facts[0].control_generation,
    };
    facts[0].phase = SessionPhase::DirectoryUnavailable;
    initial.organization = Arc::new(
        OrganizationSnapshot::new(
            initial.organization.epoch(),
            initial.organization.state().clone(),
            facts,
        )
        .unwrap(),
    );
    runtime.provider.set_frame(Arc::new(DesktopServerFrame {
        server: initial.server,
        connection: initial.connection.clone(),
        replica_status: initial.replica_status,
        organization: initial.organization.clone(),
        personal: initial.personal.clone(),
        view_revision: initial.view_revision,
    }));
    runtime.notifier.frame_changed(server(7));
    cx.run_until_parked();
    assert!(runtime.begun().is_empty());
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.server_tab_event(
                ServerTabManagerEvent::PaneAction {
                    pane,
                    action: PaneAction::ChooseDirectory,
                },
                window,
                cx,
            )
        })
    });
    cx.run_until_parked();
    assert!(manager.read_with(cx, |manager, cx| {
        manager
            .server
            .as_ref()
            .unwrap()
            .repair
            .as_ref()
            .unwrap()
            .read(cx)
            .picker()
            .read(cx)
            .blocks_terminal_input()
    }));
    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();
    assert_eq!(runtime.begun().len(), 1);
    assert!(
        matches!(&runtime.begun()[0], DesktopRequest::Shared { command: Command::RepairPaneDirectory { pane: target, .. }, .. } if *target == pane)
    );
    let id = operation(server(7), 1);
    runtime.provider.finish(0, Ok(Some(id)));
    runtime
        .provider
        .set_outcome_at(id, Arc::new(Outcome::Applied), OrganizationRevision::new(2));
    cx.run_until_parked();
    assert_eq!(runtime.begun().len(), 1);
    let mut state = initial.organization.state().clone();
    state.revision = OrganizationRevision::new(2);
    state.panes[0].starting_directory =
        spaceterm_core::organization::ServerDirectory::new(server(7), b"/server/home".to_vec())
            .unwrap();
    state.panes[0].confirmed_restoration_directory.value = None;
    state.panes[0].confirmed_restoration_directory.version = FieldVersion::new(2);
    let mut facts = initial.organization.runtime().to_vec();
    facts[0].phase = SessionPhase::NotStarted;
    initial.organization =
        Arc::new(OrganizationSnapshot::new(initial.organization.epoch(), state, facts).unwrap());
    initial.view_revision = view_revision(2);
    runtime.provider.set_frame(Arc::new(initial));
    runtime.notifier.frame_changed(server(7));
    cx.run_until_parked();
    assert_eq!(
        runtime.begun()[1],
        DesktopRequest::StartSession {
            pane,
            source: SelectionSource::Command
        }
    );
    runtime.notifier.frame_changed(server(7));
    cx.run_until_parked();
    assert_eq!(runtime.begun().len(), 2);
}

#[gpui::test]
fn desktop_workspace_starting_directory_browses_its_server_and_submits_a_versioned_pin(
    cx: &mut TestAppContext,
) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[7]);
    let replica = replica(7);
    runtime.provider.set_directory_connection(
        server(7),
        crate::ui::directory_picker::server_tests::Peer::new(),
    );
    publish(&replica, 1, &runtime, cx);
    let version = replica.org.snapshot().state().workspaces[0]
        .pinned_directory
        .version;
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.handle_server_sidebar_event(
                ServerSidebarEvent::StartingDirectory(ws(7, 1)),
                window,
                cx,
            )
        })
    });
    cx.run_until_parked();
    assert!(manager.read_with(cx, |manager, cx| {
        manager
            .pin_picker
            .as_ref()
            .is_some_and(|picker| picker.read(cx).blocks_terminal_input())
    }));
    assert!(
        runtime.begun().is_empty(),
        "browsing cannot mutate or start a Session"
    );
    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();
    assert_eq!(
        runtime.begun(),
        vec![DesktopRequest::Shared {
            server: server(7),
            command: Command::SetPinnedDirectory {
                workspace: ws(7, 1),
                expected_version: version,
                value: Some(
                    spaceterm_core::organization::ServerDirectory::new(
                        server(7),
                        b"/server/home".to_vec(),
                    )
                    .unwrap()
                ),
            },
        }]
    );
}

#[gpui::test]
fn desktop_workspace_starting_directory_refuses_stopped_server_without_local_fallback(
    cx: &mut TestAppContext,
) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[7]);
    let replica = replica(7);
    publish(&replica, 1, &runtime, cx);
    let mut unavailable = frame(&replica, ReplicaStatus::Ready);
    unavailable.connection = ConnectionState::Stopped;
    runtime.provider.set_frame(Arc::new(unavailable));
    runtime.notifier.frame_changed(server(7));
    cx.run_until_parked();
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.handle_server_sidebar_event(
                ServerSidebarEvent::StartingDirectory(ws(7, 1)),
                window,
                cx,
            )
        })
    });
    cx.run_until_parked();
    assert!(manager.read_with(cx, |manager, _| manager.pin_picker.is_none()));
    assert!(runtime.begun().is_empty());
}

#[gpui::test]
fn desktop_workspace_starting_directory_rejects_a_changed_field_before_submission(
    cx: &mut TestAppContext,
) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[7]);
    let replica = replica(7);
    runtime.provider.set_directory_connection(
        server(7),
        crate::ui::directory_picker::server_tests::Peer::new(),
    );
    publish(&replica, 1, &runtime, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.handle_server_sidebar_event(
                ServerSidebarEvent::StartingDirectory(ws(7, 1)),
                window,
                cx,
            )
        })
    });
    cx.run_until_parked();
    let original = runtime.provider.frame(server(7)).unwrap();
    let mut changed = DesktopServerFrame {
        server: original.server,
        connection: original.connection.clone(),
        replica_status: original.replica_status,
        organization: original.organization.clone(),
        personal: original.personal.clone(),
        view_revision: view_revision(2),
    };
    let mut state = changed.organization.state().clone();
    state.revision = spaceterm_core::authority::OrganizationRevision::new(2);
    state.workspaces[0].pinned_directory.version = spaceterm_core::authority::FieldVersion::new(2);
    changed.organization = Arc::new(
        spaceterm_core::organization::OrganizationSnapshot::new(
            changed.organization.epoch(),
            state,
            changed.organization.runtime().to_vec(),
        )
        .unwrap(),
    );
    runtime.provider.set_frame(Arc::new(changed));
    runtime.notifier.frame_changed(server(7));
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();
    assert!(
        runtime.begun().is_empty(),
        "a replaced directory field needs a fresh chooser"
    );
}

#[gpui::test]
fn desktop_workspace_starting_directory_reports_a_server_field_conflict(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[7]);
    let replica = replica(7);
    runtime.provider.set_directory_connection(
        server(7),
        crate::ui::directory_picker::server_tests::Peer::new(),
    );
    publish(&replica, 1, &runtime, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.handle_server_sidebar_event(
                ServerSidebarEvent::StartingDirectory(ws(7, 1)),
                window,
                cx,
            )
        })
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();
    let operation = operation(server(7), 1);
    runtime.provider.set_outcome(
        operation,
        Arc::new(Outcome::Conflict(
            spaceterm_core::operations::FieldConflict::PinnedDirectory {
                workspace: ws(7, 1),
                current_value: None,
                current_version: spaceterm_core::authority::FieldVersion::new(2),
            },
        )),
    );
    runtime.provider.finish(0, Ok(Some(operation)));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("modal-action-server-request-failed-ok")
            .is_some()
    );
    assert_eq!(
        runtime.begun().len(),
        1,
        "a conflict never repeats the write"
    );
}

fn close_message(
    manager: &Entity<WorkspaceManager>,
    target: AssessmentTarget,
    counts: (u32, u32, u32, u32),
    cx: &mut VisualTestContext,
) -> String {
    let command = Command::AssessKillWorkspace {
        workspace: ws(A, 1),
    };
    let assessment = CloseAssessment::new(
        AssessmentRef {
            operation: operation(server(A), 9),
            digest: RequestDigest::for_command(&command).unwrap(),
        },
        spaceterm_core::authority::ServerEpoch::new(1),
        AssessmentSummary::new(target, counts.0, counts.1, counts.2, counts.3, true).unwrap(),
    )
    .unwrap();
    cx.update(|_, cx| {
        let manager = manager.read(cx);
        close_copy(assessment, manager.server.as_ref().unwrap(), cx).1
    })
}

#[gpui::test]
fn desktop_server_close_confirmations_use_singular_and_plural_counts(cx: &mut TestAppContext) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    publish(&replica(A), 1, &runtime, cx);
    let kill = AssessmentTarget::KillWorkspace(ws(A, 1));
    let close_tab = AssessmentTarget::Close(ServerCloseTarget::Tab(tab(A, 1)));
    assert_eq!(
        close_message(&manager, kill, (1, 1, 1, 1), cx),
        "work and its 1 Tab are deleted from Local for every client. 1 Terminal Session ends for every client. Any command running in it stops. This can't be undone. Repository files and Git Worktree directories stay on disk."
    );
    assert!(
        close_message(&manager, kill, (1, 2, 3, 2), cx)
            .starts_with("work and its 2 Tabs are deleted from Local for every client. 2 Terminal Sessions end for every client. Any command running in them stops.")
    );
    assert_eq!(
        close_message(&manager, close_tab, (1, 1, 1, 0), cx),
        "Its 1 Pane closes for every client."
    );
    assert_eq!(
        close_message(&manager, close_tab, (1, 1, 3, 0), cx),
        "Its 3 Panes close for every client."
    );
}

#[gpui::test]
fn desktop_directory_retry_shows_its_result_on_the_pane(cx: &mut TestAppContext) {
    use crate::ui::directory_picker::pane_repair::PaneRepairStatus;
    use crate::ui::server_desktop_contracts::{
        AttachmentGeneration, PaneAction, PaneViewToken, SelectionSource,
    };
    use spaceterm_core::{
        authority::ContextGeneration,
        organization::{ControlRecord, OrganizationSnapshot, SessionPhase},
    };
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[7]);
    let replica = replica(7);
    let pane = replica.org.snapshot().state().panes[0].id;
    runtime.provider.set_pane_token(PaneViewToken {
        pane,
        attachment_generation: AttachmentGeneration::FIRST,
        context_generation: ContextGeneration::new(1),
    });
    runtime.provider.set_directory_connection(
        server(7),
        crate::ui::directory_picker::server_tests::Peer::new(),
    );
    publish(&replica, 1, &runtime, cx);
    let published = runtime.provider.frame(server(7)).unwrap();
    let mut facts = published.organization.runtime().to_vec();
    facts[0].control = ControlRecord::Held {
        client: ready(7).client,
        generation: facts[0].control_generation,
    };
    facts[0].phase = SessionPhase::DirectoryUnavailable;
    runtime.provider.set_frame(Arc::new(DesktopServerFrame {
        server: published.server,
        connection: published.connection.clone(),
        replica_status: published.replica_status,
        organization: Arc::new(
            OrganizationSnapshot::new(
                published.organization.epoch(),
                published.organization.state().clone(),
                facts,
            )
            .unwrap(),
        ),
        personal: published.personal.clone(),
        view_revision: published.view_revision,
    }));
    runtime.notifier.frame_changed(server(7));
    cx.run_until_parked();
    let status = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            let manager = manager.read(cx);
            let server = manager.server.as_ref().unwrap();
            server
                .collection
                .payloads()
                .find_map(|tabs| tabs.read(cx).server_terminal(pane, cx))
                .expect("the repaired Pane is presented")
                .read(cx)
                .server_repair_status()
        })
    };
    assert_eq!(status(cx), None);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.server_tab_event(
                ServerTabManagerEvent::PaneAction {
                    pane,
                    action: PaneAction::Retry(SelectionSource::Pointer),
                },
                window,
                cx,
            )
        })
    });
    assert_eq!(status(cx), Some(PaneRepairStatus::Pending));
    cx.run_until_parked();
    assert!(
        matches!(
            status(cx),
            Some(PaneRepairStatus::StillUnavailable | PaneRepairStatus::Completed)
        ),
        "Retry reports its result on the Pane: {:?}",
        status(cx)
    );
}

#[gpui::test]
fn desktop_server_sidebar_keyboard_navigation_reveals_rows_in_a_tall_section(
    cx: &mut TestAppContext,
) {
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A]);
    let mut org = Org::new(A);
    for local in 1..=60 {
        org.add_workspace(local, local, local);
    }
    let mut replica = Replica::attached(org, 1);
    for local in 2..=60 {
        replica
            .personal
            .attach(
                ws(A, local),
                &ReplicaObservation::new(&replica.org.snapshot(), &[], ready(A)).unwrap(),
            )
            .unwrap();
    }
    publish(&replica, 1, &runtime, cx);
    cx.update(|window, cx| {
        window.activate_window();
        manager.update(cx, |manager, cx| manager.toggle_sidebar_focus(window, cx));
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("home");
    cx.run_until_parked();
    let offset = |cx: &mut VisualTestContext| {
        manager.read_with(cx, |manager, cx| {
            manager.sidebar.read(cx).scroll_handle().offset().y
        })
    };
    assert_eq!(offset(cx), gpui::px(0.0));
    cx.simulate_keystrokes("end");
    cx.run_until_parked();
    let row = cx
        .debug_bounds(
            format!(
                "workspace-{:032x}-60-row",
                u128::from_be_bytes(server(A).bytes())
            )
            .leak(),
        )
        .expect("the last Workspace row is painted");
    let list = cx
        .debug_bounds("workspace-list")
        .expect("the Workspace list is painted");
    assert!(
        offset(cx) < gpui::px(0.0),
        "the list scrolled toward the cursor"
    );
    assert!(
        row.top() >= list.top() && row.bottom() <= list.bottom(),
        "the cursor row {row:?} is inside the list {list:?}"
    );
}

pub(in crate::ui) fn stopped_pane_settings(
    cx: &mut TestAppContext,
) -> gpui::WindowHandle<crate::ui::settings_window::SettingsWindow> {
    use crate::ui::{
        pane_presentation::{OfflineCause, OfflinePrior, PanePresentation, PanePresentationAction},
        server_desktop_contracts::PaneAction,
    };
    let (manager, runtime, cx) = harness(cx);
    servers(&runtime, &[A, B]);
    let replica = replica(B);
    let pane = replica.org.snapshot().state().panes[0].id;
    publish(&replica, 1, &runtime, cx);
    let mut stopped = frame(&replica, ReplicaStatus::Ready);
    stopped.connection = ConnectionState::Stopped;
    runtime.provider.set_frame(Arc::new(stopped));
    runtime.notifier.frame_changed(server(B));
    cx.run_until_parked();
    let presentation = PanePresentation::Offline {
        prior: OfflinePrior::Frozen,
        cause: OfflineCause::Stopped(DisplayName::reported("Remote")),
    };
    assert!(presentation.offers_action(PanePresentationAction::OpenServerSettings, true));
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.server_tab_event(
                ServerTabManagerEvent::PaneAction {
                    pane,
                    action: PaneAction::OpenServerSettings,
                },
                window,
                cx,
            )
        })
    });
    cx.run_until_parked();
    assert!(
        runtime.begun().is_empty(),
        "opening Settings must not connect, Start, or enroll"
    );
    cx.update(|_, cx| {
        cx.windows()
            .into_iter()
            .find_map(|window| window.downcast::<crate::ui::settings_window::SettingsWindow>())
            .expect("Pane recovery must open Settings")
    })
}
