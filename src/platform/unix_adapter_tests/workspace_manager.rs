use super::*;
use std::os::unix::fs::symlink;
fn workspace_manager_with_directory_selection(
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
    let directory_selection: Rc<dyn SystemDirectorySelection> =
        Rc::new(ScriptedDirectorySelection::new(selections));
    let (manager, cx) = cx.add_window_view(|window, cx| {
        new_desktop_manager(
            session_factory,
            std::env::temp_dir(),
            WorkspaceManagerAdapters {
                local_filesystem: crate::platform::unix_adapter_tests::local_filesystem(),
                key_input: Rc::new(GpuiTerminalKeyInputAdapterFactory::default()),
                accessibility: Rc::new(crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default()),
                native_services: crate::terminal::native_services::testing::adapters(),
                lifecycle: PaneLifecycleDependencies::testing(),
                directory_selection,
                window_drag: Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
                local_server: crate::ui::launch_testing::unanswered(),
            },
            SidebarState::Expanded,
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
    let (manager, _records, cx) = workspace_manager_with_directory_selection(selections, cx);

    choose_pin_directory(&manager, cx);
    choose_pin_directory(&manager, cx);

    let state = manager.read_with(cx, |manager, _| {
        let workspace = manager.workspaces.active();
        (
            manager.workspaces.len(),
            workspace.local_display_directory().unwrap().to_path_buf(),
        )
    });
    assert_eq!(state, (1, equivalent.clone()));
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
    let (manager, records, cx) = workspace_manager_with_directory_selection([], cx);
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
