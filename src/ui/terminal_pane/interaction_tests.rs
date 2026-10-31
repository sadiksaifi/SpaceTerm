use super::*;

#[gpui::test]
fn accepted_pending_pane_starts_its_presentation_at_unchanged_bounds(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    cx.update(crate::ui::init).unwrap();
    let pending = WorkspaceTerminalSessionFactory::pending();
    let (pane, cx) = cx.add_window_view(|window, cx| {
        TerminalPane::with_adapters(
            pending,
            crate::terminal::testing::test_terminal_key_input_adapter(),
            &crate::platform::accesskit_terminal_accessibility::AccessKitTerminalAccessibilityAdapterFactory,
            crate::terminal::native_services::testing::adapters(),
            PaneLifecycleDependencies::testing(), window, cx,
        )
    });
    cx.update(|window, cx| {
        window.activate_window();
        pane.update(cx, |pane, cx| {
            pane.set_accessibility_hierarchy(true);
            pane.focus(window, cx);
            cx.notify();
        });
    });
    cx.activate_accessibility();
    cx.run_until_parked();
    let (bounds, geometry) = pane.read_with(cx, |pane, _| {
        assert!(
            pane.terminal_session.session.is_none(),
            "a provisional Pane has no Session authority"
        );
        (pane.grid_bounds.unwrap(), pane.last_geometry.unwrap())
    });
    let focus = cx.update(|window, cx| window.focused(cx));
    let records = TestTerminalSessionRecords::default();
    let accepted = WorkspaceTerminalSessionFactory::new_server(Rc::new(
        TestTerminalSessionFactory::new(records.clone()).with_reading_available(true),
    ));
    pane.update(cx, |pane, cx| {
        pane.bind_pending_server(accepted.clone(), cx)
    });
    cx.run_until_parked();
    assert_eq!(
        records.starts().len(),
        1,
        "acceptance opens the client presentation without a resize"
    );
    assert_eq!(
        pane.read_with(cx, |pane, _| (pane.grid_bounds, pane.last_geometry)),
        (Some(bounds), Some(geometry))
    );
    assert_eq!(cx.update(|window, cx| window.focused(cx)), focus);

    // Read through the Session's actual event lanes, which the old geometry cache never opened.
    records
        .last_event_sender()
        .unwrap()
        .send_blocking(TerminalSessionEvent::Screen(blinking_screen()))
        .unwrap();
    cx.run_until_parked();
    records
        .last_accessibility_sender()
        .unwrap()
        .force_send(accessibility_model(42))
        .unwrap();
    cx.run_until_parked();
    assert!(
        A11yTree::read(cx)
            .in_order()
            .iter()
            .any(|node| node["aria"]["role"] == "TextRun" && node["aria"]["value"] == "update-42x")
    );

    pane.update(cx, |pane, cx| pane.bind_pending_server(accepted, cx));
    cx.run_until_parked();
    assert_eq!(
        records.starts().len(),
        1,
        "repeated publication cannot replace the bound presentation"
    );
}

#[gpui::test]
fn server_backed_repository_status_keeps_its_command_handler(cx: &mut TestAppContext) {
    let (pane, cx, _) = server_terminal_pane(cx, true);
    open_crowded_repository_popover(&pane, cx);
    assert!(cx.debug_bounds("repository-status-popover").is_some());
    assert!(pane.read_with(cx, |pane, _| pane.repository_popover_open()));
}
