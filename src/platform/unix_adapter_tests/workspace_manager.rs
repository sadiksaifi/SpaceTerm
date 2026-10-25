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
                server_directories: crate::ui::directory_picker::server_directory_testing::older(),
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

/// The paired server compares a selected symbolic link with its own pins before the client
/// creates another Workspace.
#[gpui::test]
fn open_paired_server_directory_reuses_the_workspace_pinned_through_a_symlink(
    cx: &mut TestAppContext,
) {
    use crate::ui::server_worktrees::testing::{ScriptedReads, present_direct};
    use crate::ui::shared_workspaces::testing::{ScriptedEdits, ScriptedPanes};
    use crate::ui::shared_workspaces::{DirectServer, SharedEditSource, SharedPaneSessions};

    cx.executor().allow_parking();
    let root = std::env::temp_dir().join(format!(
        "spaceterm-paired-directory-alias-{}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("project")).unwrap();
    std::fs::create_dir(root.join("elsewhere")).unwrap();
    symlink(root.join("project"), root.join("alias")).unwrap();
    let adapters = WorkspaceManagerAdapters {
        server_directories: crate::ui::directory_picker::server_directory_testing::in_process(),
        ..workspace_adapters()
    };
    let (harness, cx) = MachineGroupsHarness::using(adapters, cx);
    let route = [9; 32];
    let workspaces = [("Other", 6), ("Far", 7)];
    let mut inventory = machine_inventory(harness.next_revision(), &workspaces);
    inventory.workspaces[0].pinned_directory = Some(root.join("elsewhere"));
    inventory.workspaces[1].pinned_directory = Some(root.join("project"));
    let edits = Rc::new(ScriptedEdits::new(Some(inventory)));
    let key = cx.update(|window, cx| {
        present_direct(
            route,
            Rc::clone(&edits),
            ScriptedReads::new("~/worktrees/{branch}"),
            cx,
        );
        harness.manager.update(cx, |manager, cx| {
            let key = manager.present_server_workspaces(
                ServerOrigin::Direct(DirectServer {
                    route,
                    name: "Penguin".into(),
                }),
                SharedEditSource::new(Rc::clone(&edits) as Rc<_>),
                SharedPaneSessions::with_opener(
                    ScriptedPanes::new(MachineGroupsHarness::session_factory()) as Rc<_>,
                    &PaneConstruction::testing(),
                ),
                window,
                cx,
            );
            manager.sync_shared_workspaces(key, window, cx);
            key
        })
    });
    harness.attach(key, &workspaces, cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.open_server_directory(key, String::new(), root.join("alias"), window, cx);
        })
    });
    cx.run_until_parked();
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while harness
        .manager
        .read_with(cx, |manager, _| manager.pending_server_directory.is_some())
    {
        assert!(
            Instant::now() < deadline,
            "the owning server's directory comparison finishes"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
        cx.run_until_parked();
    }
    let opened = harness.manager.read_with(cx, |manager, _| {
        let active = manager.workspaces.active_workspace_id()?;
        (manager.shared.owner(active) == Some(key))
            .then(|| manager.workspaces.active().name().to_owned())
    });
    assert_eq!(opened.as_deref(), Some("Far"));
    assert!(
        edits.submitted().is_empty(),
        "a matching pin must not create another Workspace"
    );
    assert!(
        harness.local.submitted().is_empty(),
        "the local server receives no mutation"
    );
    std::fs::remove_dir_all(root).unwrap();
}
