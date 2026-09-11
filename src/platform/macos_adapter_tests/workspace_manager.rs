use super::*;
use std::os::unix::fs::symlink;
fn workspace_manager_with_picker(
    selections: impl IntoIterator<
        Item = Result<Option<PathBuf>, crate::directory_selection::DirectoryChooserError>,
    >,
    cx: &mut TestAppContext,
) -> (
    Entity<WorkspaceManager>,
    TestTerminalSessionRecords,
    &mut VisualTestContext,
) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(records.clone()).with_fallback_title("zsh"));
    let directory_selection_fallback: Rc<dyn SystemDirectorySelection> =
        Rc::new(ScriptedDirectorySelection::new(selections));
    let (manager, cx) = cx.add_window_view(|window, cx| {
        WorkspaceManager::new_with_adapters(
            session_factory,
            std::env::temp_dir(),
            WorkspaceManagerAdapters {
                local_filesystem: crate::platform::macos_adapter_tests::local_filesystem(),
                key_input: Rc::new(GpuiTerminalKeyInputAdapterFactory::default()),
                accessibility: Rc::new(crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default()),
                native_services: crate::terminal::native_services::testing::adapters(),
                lifecycle: PaneLifecycleDependencies::testing(), directory_selection: directory_selection_fallback,
                permission_recovery: None,
                window_drag: Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
                remote_workspace: test_remote_backend_factory(),
            },
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| manager.focus(window, cx));
    });
    cx.run_until_parked();
    (manager, records, cx)
}

#[gpui::test]
fn changing_pin_to_equivalent_directory_should_preserve_selected_spelling(cx: &mut TestAppContext) {
    let root = temporary_directory("project");
    let project = root.join("selected-project");
    let equivalent = root.join("equivalent-project");
    fs::create_dir_all(&project).unwrap();
    symlink(&project, &equivalent).unwrap();
    let selections = [Ok(Some(project.clone())), Ok(Some(equivalent.clone()))];
    let (manager, records, cx) = workspace_manager_with_picker(selections, cx);

    choose_with_directory_selection_fallback(&manager, cx);
    choose_with_directory_selection_fallback(&manager, cx);
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-d");
    cx.run_until_parked();

    let state = manager.read_with(cx, |manager, _| {
        let workspace = manager.workspaces.active_workspace();
        (
            manager.workspaces.len(),
            workspace.local_display_directory().unwrap().to_path_buf(),
            workspace.location().clone(),
        )
    });
    assert_eq!((state.0, state.1), (1, equivalent.clone()));
    assert!(matches!(state.2, WorkspaceLocation::Local));
    assert_eq!(records.starts().len(), 3);
    assert!(records.starts()[1..].iter().all(|start| {
        start
            .local_working_directory()
            .is_some_and(|directory| directory.path() == equivalent)
    }));
    fs::remove_dir_all(root).unwrap();
}

#[gpui::test]
fn replaced_pinned_directory_is_rejected_between_picker_validation_and_activation(
    cx: &mut TestAppContext,
) {
    let root = temporary_directory("activation-replacement");
    let project = root.join("project");
    let parked = root.join("parked");
    fs::create_dir_all(&project).unwrap();
    let (manager, records, cx) = workspace_manager_with_picker([], cx);
    let directory = manager.read_with(cx, |manager, _| {
        manager
            .local_filesystem
            .validate_directory(&project)
            .unwrap()
    });
    let original_drops = records.dropped_session_ids();
    fs::rename(&project, &parked).unwrap();
    fs::create_dir(&project).unwrap();
    let activate = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.apply_validated_local_pin(
                    WorkspaceId::new(1),
                    directory.clone(),
                    window,
                    cx,
                )
            })
        })
    };
    assert!(!activate(cx));
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        1
    );
    assert_eq!(records.starts().len(), 1);
    assert_eq!(records.dropped_session_ids(), original_drops);
    fs::remove_dir(&project).unwrap();
    fs::rename(&parked, &project).unwrap();
    assert!(activate(cx));
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        1
    );
    assert_eq!(records.starts().len(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[gpui::test]
fn replaced_pinned_directory_blocks_both_child_actions_without_closing_sessions(
    cx: &mut TestAppContext,
) {
    let root = temporary_directory("child-replacement");
    let project = root.join("project");
    let parked = root.join("parked");
    fs::create_dir_all(&project).unwrap();
    let (manager, records, cx) = workspace_manager_with_picker([Ok(Some(project.clone()))], cx);
    choose_with_directory_selection_fallback(&manager, cx);
    let original_counts = manager.read_with(cx, |manager, cx| {
        manager
            .workspaces
            .active_workspace()
            .payload()
            .read(cx)
            .aggregate_counts(cx)
    });
    let original_drops = records.dropped_session_ids();
    fs::rename(&project, &parked).unwrap();
    fs::create_dir(&project).unwrap();

    for action in ["cmd-t", "cmd-d"] {
        cx.simulate_keystrokes(action);
        cx.run_until_parked();
        assert_eq!(records.starts().len(), 1);
        assert_eq!(records.dropped_session_ids(), original_drops);
        assert_eq!(
            manager.read_with(cx, |manager, cx| {
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .aggregate_counts(cx)
            }),
            original_counts
        );
        assert!(manager.read_with(cx, |manager, _| {
            manager
                .workspaces
                .active_workspace()
                .pinned_directory()
                .is_some()
        }));
        let modal_action = if action == "cmd-t" {
            "modal-action-tab-start-error-ok"
        } else {
            "modal-action-pane-start-error-ok"
        };
        click(modal_action, cx);
        cx.run_until_parked();
    }
    fs::remove_dir(&project).unwrap();
    fs::rename(&parked, &project).unwrap();
    for action in ["cmd-t", "cmd-d"] {
        cx.simulate_keystrokes(action);
        cx.run_until_parked();
    }
    assert_eq!(records.starts().len(), 3);
    assert_eq!(records.dropped_session_ids(), original_drops);
    fs::remove_dir_all(root).unwrap();
}
