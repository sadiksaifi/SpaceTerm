use super::*;

#[gpui::test]
fn open_local_directory_alias_activates_the_workspace_and_keeps_its_spelling(
    cx: &mut TestAppContext,
) {
    let project = temporary_directory("launch-open-local-existing");
    std::fs::create_dir_all(&project).unwrap();
    let alias = project.with_extension("alias");
    std::os::unix::fs::symlink(&project, &alias).unwrap();
    let (harness, cx) = ready_harness(cx);
    cx.simulate_keystrokes("cmd-o");
    cx.run_until_parked();
    confirm_directory_picker_path(&format!("{}/", alias.to_str().unwrap()), cx);
    accept_creation(&harness, Some(alias.clone()), cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            let first = manager.shared.local().unwrap().local(WORKSPACE).unwrap();
            assert!(manager.activate_workspace(first, window, cx));
        })
    });

    cx.simulate_keystrokes("cmd-o");
    cx.run_until_parked();
    confirm_directory_picker_path(&format!("{}/", project.to_str().unwrap()), cx);

    assert_eq!(created_names(&harness).len(), 1, "nothing new is created");
    assert_eq!(active_server_workspace(&harness, cx), Some(CREATED));
    assert_eq!(
        harness.manager.read_with(cx, |manager, _| manager
            .shared
            .local()
            .unwrap()
            .latest_inventory()
            .unwrap()
            .workspaces
            .iter()
            .find(|w| w.id == CREATED)
            .unwrap()
            .pinned_directory
            .clone()),
        Some(alias.clone())
    );
    std::fs::remove_file(alias).unwrap();
    std::fs::remove_dir_all(project).unwrap();
}
