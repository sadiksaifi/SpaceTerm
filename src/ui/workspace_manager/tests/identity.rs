use super::*;
use crate::domain::{PaneId, SshDestination};
use crate::terminal::metadata::{MetadataFreshness, TerminalMetadataContext};

fn report_directory(
    records: &TestTerminalSessionRecords,
    session: usize,
    generation: u64,
    directory: &str,
    remote: bool,
    freshness: MetadataFreshness,
) {
    let mut screen = crate::terminal::ScreenSnapshot::from_test_parts_at(
        Arc::from([]),
        crate::terminal::ScrollbarSnapshot::default(),
        "unchanged title",
        generation,
    );
    let metadata = Arc::make_mut(&mut Arc::make_mut(&mut screen).metadata);
    metadata.directory.path = Arc::from(directory);
    metadata.freshness = freshness;
    if remote {
        metadata.context = TerminalMetadataContext::Remote(RemoteTerminalMetadataContext::new(
            SshDestination::new("work".into()).unwrap(),
            RemoteDirectory::new("~".into()).unwrap(),
        ));
    }
    records
        .event_sender(session)
        .unwrap()
        .try_send(SessionEvent::Screen(screen))
        .unwrap();
}

fn identity(manager: &Entity<WorkspaceManager>, cx: &VisualTestContext) -> (String, String) {
    manager.read_with(cx, |manager, _| {
        let workspace = manager.workspaces.active_workspace();
        let (_, path) = directory_labels(
            workspace.local_display_directory(),
            workspace.remote_display_directory(),
            &manager.local_home_directory_path,
        );
        (workspace.name().to_owned(), path)
    })
}

#[gpui::test]
fn workspace_identity_should_follow_primary_pane_while_new_tabs_follow_focus(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    let root = temporary_directory("workspace-identity");
    let alpha = root.join("alpha");
    let beta = root.join("beta");
    fs::create_dir_all(&alpha).unwrap();
    fs::create_dir_all(&beta).unwrap();
    report_directory(
        &records,
        1,
        1,
        alpha.to_str().unwrap(),
        false,
        MetadataFreshness::Live,
    );
    cx.run_until_parked();
    assert_eq!(
        identity(&manager, cx),
        ("alpha".into(), alpha.display().to_string())
    );
    cx.simulate_keystrokes("cmd-d");
    cx.run_until_parked();
    report_directory(
        &records,
        2,
        1,
        beta.to_str().unwrap(),
        false,
        MetadataFreshness::Live,
    );
    cx.run_until_parked();
    assert_eq!(identity(&manager, cx).0, "alpha");
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    assert_eq!(
        records
            .starts()
            .last()
            .unwrap()
            .local_working_directory()
            .unwrap()
            .path(),
        beta
    );
    assert_eq!(identity(&manager, cx).0, "alpha");
    let (_, tabs) = active_tab_manager(&manager, cx);
    cx.update(|window, cx| {
        tabs.update(cx, |tabs, cx| {
            tabs.close_pane_authorized(TabId::new(1), PaneId::new(1), window, cx)
        })
    });
    cx.run_until_parked();
    assert_eq!(identity(&manager, cx).0, "beta");
    fs::remove_dir_all(root).unwrap();
}

#[gpui::test]
fn workspace_identity_menu_should_select_the_target_pane_and_ignore_other_reports(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-d");
    cx.run_until_parked();
    report_directory(
        &records,
        1,
        1,
        "/projects/first",
        false,
        MetadataFreshness::Live,
    );
    report_directory(
        &records,
        2,
        1,
        "/projects/second",
        false,
        MetadataFreshness::Live,
    );
    cx.run_until_parked();
    report_directory(
        &records,
        2,
        2,
        "/projects/second",
        false,
        MetadataFreshness::Stale,
    );
    cx.run_until_parked();
    click("pane-menu-button-2", cx);
    click("pane-menu-row-workspace-identity", cx);
    assert_eq!(identity(&manager, cx).0, "second");
    report_directory(
        &records,
        1,
        2,
        "/projects/ignored",
        false,
        MetadataFreshness::Live,
    );
    report_directory(
        &records,
        2,
        3,
        "/projects/selected",
        false,
        MetadataFreshness::Live,
    );
    cx.run_until_parked();
    assert_eq!(
        identity(&manager, cx),
        ("selected".into(), "/projects/selected".into())
    );
    report_directory(
        &records,
        2,
        1,
        "/projects/late",
        false,
        MetadataFreshness::Live,
    );
    cx.run_until_parked();
    assert_eq!(identity(&manager, cx).0, "selected");
}

#[gpui::test]
fn remote_workspace_identity_should_track_primary_and_retain_last_live_path(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    let (completion, _, _, _) = remote_completion("work", "~", "/home/tester", true);
    let flow = open_remote_workspace_flow(&manager, cx);
    emit_remote_workspace_completion(&flow, completion, cx);
    manager.update(cx, |manager, _| {
        let id = manager.workspaces.active_workspace_id();
        manager
            .workspaces
            .rename_workspace(id, String::new())
            .unwrap();
    });
    report_directory(&records, 2, 1, "/srv/alpha", true, MetadataFreshness::Live);
    cx.run_until_parked();
    assert_eq!(
        identity(&manager, cx),
        ("alpha · work".into(), "/srv/alpha".into())
    );
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    report_directory(&records, 3, 1, "/srv/beta", true, MetadataFreshness::Live);
    cx.run_until_parked();
    report_directory(&records, 3, 2, "/srv/latest", true, MetadataFreshness::Live);
    report_directory(
        &records,
        3,
        3,
        "/srv/disconnected",
        true,
        MetadataFreshness::Stale,
    );
    cx.run_until_parked();
    assert_eq!(identity(&manager, cx).0, "alpha · work");
    report_directory(&records, 2, 2, "/srv/stale", true, MetadataFreshness::Stale);
    cx.run_until_parked();
    assert_eq!(identity(&manager, cx).1, "/srv/alpha");
    let (_, tabs) = active_tab_manager(&manager, cx);
    cx.update(|window, cx| {
        tabs.update(cx, |tabs, cx| {
            tabs.close_tab_authorized(TabId::new(1), window, cx)
        })
    });
    cx.run_until_parked();
    assert_eq!(
        identity(&manager, cx),
        ("latest · work".into(), "/srv/latest".into())
    );
    manager.read_with(cx, |manager, _| {
        assert!(
            manager
                .workspaces
                .active_workspace()
                .local_display_directory()
                .is_none()
        )
    });
}

#[gpui::test]
fn unchanged_rename_should_preserve_automatic_identity_on_enter_and_blur(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    for (generation, directory, submit) in
        [(1, "/projects/alpha", true), (2, "/projects/beta", false)]
    {
        report_directory(
            &records,
            1,
            generation,
            directory,
            false,
            MetadataFreshness::Live,
        );
        cx.run_until_parked();
        assert_eq!(identity(&manager, cx).1, directory);
        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-rename", cx);
        if submit {
            cx.simulate_keystrokes("enter");
        } else {
            let focus = manager.read_with(cx, |manager, _| manager.sidebar.focus.clone());
            cx.update(|window, _| focus.focus(window));
        }
        cx.run_until_parked();
        assert!(manager.read_with(cx, |manager, _| manager.sidebar.rename.is_none()));
    }
    report_directory(
        &records,
        1,
        3,
        "/projects/gamma",
        false,
        MetadataFreshness::Live,
    );
    cx.run_until_parked();
    assert_eq!(identity(&manager, cx).0, "gamma");
}

#[gpui::test]
fn terminal_context_identity_action_should_select_a_single_pane_tab(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    report_directory(
        &records,
        1,
        1,
        "/projects/alpha",
        false,
        MetadataFreshness::Live,
    );
    report_directory(
        &records,
        2,
        1,
        "/projects/beta",
        false,
        MetadataFreshness::Live,
    );
    cx.run_until_parked();
    assert_eq!(identity(&manager, cx).0, "alpha");
    redraw(cx);
    redraw(cx);
    right_click(
        "terminal-native-context-copy-false-open-false-file-preview-false-failure-false-last-frame-false",
        cx,
    );
    click("terminal-context-menu-row-workspace-identity-enabled", cx);
    assert_eq!(identity(&manager, cx).0, "beta");
}

fn assert_pin_and_custom_name_policy(cx: &mut TestAppContext, remote: bool) {
    let (manager, records, cx) = workspace_manager(cx);
    let session = if remote {
        let (completion, _, _, _) = remote_completion("work", "~", "/home/tester", true);
        let flow = open_remote_workspace_flow(&manager, cx);
        emit_remote_workspace_completion(&flow, completion, cx);
        manager.update(cx, |manager, _| {
            let id = manager.workspaces.active_workspace_id();
            manager
                .workspaces
                .rename_workspace(id, String::new())
                .unwrap();
        });
        2
    } else {
        1
    };
    let suffix = if remote { " · work" } else { "" };
    report_directory(
        &records,
        session,
        1,
        "/srv/first",
        remote,
        MetadataFreshness::Live,
    );
    cx.run_until_parked();
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
    let pin = if remote {
        PinnedDirectory::Remote {
            directory: RemoteDirectory::new("/srv/pinned".into()).unwrap(),
            identity: crate::domain::RemoteDirectoryIdentity::new("/srv/pinned".into()).unwrap(),
        }
    } else {
        PinnedDirectory::Local(ValidatedLocalDirectory::new(
            PathBuf::from("/srv/pinned"),
            LocalDirectoryIdentity::for_test(7),
        ))
    };
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.apply_directory_pin(workspace_id, Some(pin), window, cx)
        })
    });
    report_directory(
        &records,
        session,
        2,
        "/srv/updated",
        remote,
        MetadataFreshness::Live,
    );
    cx.run_until_parked();
    assert_eq!(
        identity(&manager, cx),
        (format!("pinned{suffix}"), "/srv/pinned".into())
    );
    manager.update(cx, |manager, _| {
        manager
            .workspaces
            .rename_workspace(workspace_id, "Custom".into())
            .unwrap()
    });
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.apply_directory_pin(workspace_id, None, window, cx)
        })
    });
    cx.run_until_parked();
    assert_eq!(
        identity(&manager, cx),
        ("Custom".into(), "/srv/updated".into())
    );
    manager.update(cx, |manager, _| {
        manager
            .workspaces
            .rename_workspace(workspace_id, String::new())
            .unwrap()
    });
    assert_eq!(identity(&manager, cx).0, format!("updated{suffix}"));
}

#[gpui::test]
fn local_identity_should_resume_primary_directory_after_unpin(cx: &mut TestAppContext) {
    assert_pin_and_custom_name_policy(cx, false);
}

#[gpui::test]
fn remote_identity_should_resume_primary_directory_after_unpin(cx: &mut TestAppContext) {
    assert_pin_and_custom_name_policy(cx, true);
}

#[gpui::test]
fn automatic_identity_changes_should_keep_collapsed_chrome_aligned(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    click("toggle-sidebar-button", cx);
    for (generation, path) in [
        (1, "/projects/a"),
        (2, "/projects/long-project-identity-for-collapsed-chrome"),
    ] {
        report_directory(
            &records,
            1,
            generation,
            path,
            false,
            MetadataFreshness::Live,
        );
        cx.run_until_parked();
        assert_eq!(identity(&manager, cx).1, path);
        let chrome = cx.debug_bounds("workspace-top-chrome").unwrap();
        let spacer = cx.debug_bounds("tab-manager-top-spacer").unwrap();
        assert_eq!(chrome.size.width, spacer.size.width);
    }
}

#[gpui::test]
fn switcher_local_creation_should_freeze_the_typed_name_as_directory_changes(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    open_workspace_switcher_for_creation(cx);
    click("workspace-switcher-create-local", cx);
    report_directory(
        &records,
        2,
        1,
        "/projects/alpha",
        false,
        MetadataFreshness::Live,
    );
    cx.run_until_parked();
    assert_eq!(
        identity(&manager, cx),
        ("fresh workspace".into(), "/projects/alpha".into())
    );
    report_directory(
        &records,
        2,
        2,
        "/projects/beta",
        false,
        MetadataFreshness::Live,
    );
    cx.run_until_parked();
    assert_eq!(
        identity(&manager, cx),
        ("fresh workspace".into(), "/projects/beta".into())
    );
}

#[gpui::test]
fn switcher_remote_creation_should_freeze_the_typed_name_as_directory_changes(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    let flow = open_remote_workspace_flow(&manager, cx);
    let (completion, _, _, _) = remote_completion("work", "~", "/home/tester", true);
    emit_remote_workspace_completion(&flow, completion, cx);
    report_directory(&records, 2, 1, "/srv/alpha", true, MetadataFreshness::Live);
    cx.run_until_parked();
    assert_eq!(
        identity(&manager, cx),
        ("fresh workspace 1".into(), "/srv/alpha".into())
    );
    report_directory(&records, 2, 2, "/srv/beta", true, MetadataFreshness::Live);
    cx.run_until_parked();
    assert_eq!(
        identity(&manager, cx),
        ("fresh workspace 1".into(), "/srv/beta".into())
    );
}

#[gpui::test]
fn command_n_should_keep_automatic_workspace_naming(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-n");
    cx.run_until_parked();
    report_directory(
        &records,
        2,
        1,
        "/projects/alpha",
        false,
        MetadataFreshness::Live,
    );
    cx.run_until_parked();
    assert_eq!(identity(&manager, cx).0, "alpha");
    report_directory(
        &records,
        2,
        2,
        "/projects/beta",
        false,
        MetadataFreshness::Live,
    );
    cx.run_until_parked();
    assert_eq!(identity(&manager, cx).0, "beta");
}

#[gpui::test]
fn empty_switcher_creation_should_keep_automatic_local_naming(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    open_workspace_switcher(cx);
    click("workspace-switcher-create-local", cx);
    report_directory(
        &records,
        2,
        1,
        "/projects/alpha",
        false,
        MetadataFreshness::Live,
    );
    cx.run_until_parked();
    assert_eq!(identity(&manager, cx).0, "alpha");
    report_directory(
        &records,
        2,
        2,
        "/projects/beta",
        false,
        MetadataFreshness::Live,
    );
    cx.run_until_parked();
    assert_eq!(identity(&manager, cx).0, "beta");
}

#[gpui::test]
fn empty_switcher_creation_should_keep_automatic_remote_naming(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    open_workspace_switcher(cx);
    click("workspace-switcher-create-remote", cx);
    let flow = manager.read_with(cx, |manager, _| {
        manager.remote_workspace_flow.clone().unwrap()
    });
    let (completion, _, _, _) = remote_completion("work", "~", "/home/tester", true);
    emit_remote_workspace_completion(&flow, completion, cx);
    report_directory(&records, 2, 1, "/srv/alpha", true, MetadataFreshness::Live);
    cx.run_until_parked();
    assert_eq!(identity(&manager, cx).0, "alpha · work");
    report_directory(&records, 2, 2, "/srv/beta", true, MetadataFreshness::Live);
    cx.run_until_parked();
    assert_eq!(identity(&manager, cx).0, "beta · work");
}

#[gpui::test]
fn sidebar_remote_creation_should_keep_automatic_naming(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    click("new-remote-workspace-button", cx);
    let flow = manager.read_with(cx, |manager, _| {
        manager.remote_workspace_flow.clone().unwrap()
    });
    let (completion, _, _, _) = remote_completion("work", "~", "/home/tester", true);
    emit_remote_workspace_completion(&flow, completion, cx);
    report_directory(&records, 2, 1, "/srv/alpha", true, MetadataFreshness::Live);
    cx.run_until_parked();
    assert_eq!(identity(&manager, cx).0, "alpha · work");
}

#[gpui::test]
fn numbered_creation_name_should_remain_frozen_after_directory_changes(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    for _ in 0..2 {
        open_workspace_switcher_for_creation(cx);
        click("workspace-switcher-create-local", cx);
    }
    report_directory(
        &records,
        3,
        1,
        "/projects/alpha",
        false,
        MetadataFreshness::Live,
    );
    cx.run_until_parked();
    assert_eq!(identity(&manager, cx).0, "fresh workspace 1");
}
