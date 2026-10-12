use super::support::*;

#[test]
fn same_version_renames_first_wins_second_conflicts() {
    let mut reducer = reducer();
    let (workspace, _, _) = create(&mut reducer, 1);
    let command = |text: &str| Command::RenameWorkspace {
        workspace,
        expected_version: FieldVersion::new(1),
        draft: NameDraft::Custom(NameText::new(text.into()).unwrap()),
    };
    let first = apply(&mut reducer, command("first"), 2);
    assert!(matches!(
        first.resolution,
        Resolution::Completed(Outcome::Applied)
    ));
    let second = apply(&mut reducer, command("second"), 3);
    assert!(
        matches!(second.resolution, Resolution::Completed(Outcome::Conflict(FieldConflict::WorkspaceName { current_version, .. })) if current_version == FieldVersion::new(2))
    );
    assert!(second.publication.is_none());
}
#[test]
fn worktree_location_has_its_own_conflict_version() {
    let mut reducer = reducer();
    create(&mut reducer, 1);
    let command = Command::SetWorktreeLocation {
        server: SERVER,
        expected_version: FieldVersion::new(1),
        template: WorktreeLocationTemplate::new("~/trees/{branch}".into()).unwrap(),
    };
    assert!(matches!(
        apply(&mut reducer, command.clone(), 2).resolution,
        Resolution::Completed(Outcome::Applied)
    ));
    assert!(matches!(
        apply(&mut reducer, command, 3).resolution,
        Resolution::Completed(Outcome::Conflict(FieldConflict::WorktreeLocation { .. }))
    ));
}

#[test]
fn unrelated_tab_create_does_not_invalidate_name_version() {
    let mut reducer = reducer();
    let (workspace, _, source) = create(&mut reducer, 1);
    apply(
        &mut reducer,
        Command::CreateTab {
            workspace,
            source: TabCreationSource::Pane(source),
        },
        2,
    );
    let result = apply(
        &mut reducer,
        Command::RenameWorkspace {
            workspace,
            expected_version: FieldVersion::new(1),
            draft: NameDraft::Custom(NameText::new("kept".into()).unwrap()),
        },
        3,
    );
    assert!(matches!(
        result.resolution,
        Resolution::Completed(Outcome::Applied)
    ));
}
#[test]
fn root_directory_repair_does_not_change_name_after_unrelated_creation() {
    let mut reducer = reducer();
    let (workspace, _, source) = create(&mut reducer, 1);
    let before = reducer.snapshot().state().workspaces[0].name.clone();
    let basis = reducer
        .retained()
        .organization()
        .retained
        .as_ref()
        .unwrap()
        .workspace_names()[0]
        .clone();
    apply(
        &mut reducer,
        Command::RepairPaneDirectory {
            pane: source,
            expected_version: FieldVersion::new(1),
            value: directory(b"/repaired"),
        },
        2,
    );
    apply(
        &mut reducer,
        Command::CreateTab {
            workspace,
            source: TabCreationSource::Pane(source),
        },
        3,
    );
    assert_eq!(reducer.snapshot().state().workspaces[0].name, before);
    assert_eq!(
        reducer
            .retained()
            .organization()
            .retained
            .as_ref()
            .unwrap()
            .workspace_names()[0],
        basis
    );
    assert!(matches!(
        apply(
            &mut reducer,
            Command::RenameWorkspace {
                workspace,
                expected_version: FieldVersion::new(1),
                draft: NameDraft::Custom(NameText::new("kept".into()).unwrap()),
            },
            4,
        )
        .resolution,
        Resolution::Completed(Outcome::Applied)
    ));
}

#[test]
fn root_directory_repair_does_not_change_name_after_unrelated_deletion() {
    let mut reducer = reducer();
    let (workspace, _, pane) = create(&mut reducer, 1);
    let (_, _, other) = create(&mut reducer, 2);
    let before = reducer.snapshot().state().workspaces[0].name.clone();
    apply(
        &mut reducer,
        Command::RepairPaneDirectory {
            pane,
            expected_version: FieldVersion::new(1),
            value: directory(b"/repaired"),
        },
        3,
    );
    let removed = reducer
        .retained()
        .preview_removal(&[other], OrganizationLimits::default())
        .unwrap();
    let record = removed
        .organization()
        .workspaces
        .iter()
        .find(|v| v.id == workspace)
        .unwrap();
    assert_eq!(record.name, before);
}

#[test]
fn rename_to_automatic_value_changes_name_mode() {
    let mut reducer = reducer();
    let (workspace, _, _) = create(&mut reducer, 1);
    let current = reducer.snapshot().state().workspaces[0]
        .name
        .value
        .display
        .clone();
    let result = apply(
        &mut reducer,
        Command::RenameWorkspace {
            workspace,
            expected_version: FieldVersion::new(1),
            draft: NameDraft::Custom(current),
        },
        2,
    );
    assert!(result.publication.is_some());
    assert_eq!(
        reducer.snapshot().state().workspaces[0].name.value.mode,
        NameMode::Custom
    );
}
#[test]
fn identical_custom_rename_and_blank_rename_are_noops() {
    let mut reducer = reducer();
    let (workspace, _, _) = create(&mut reducer, 1);
    let rename = |version, draft| Command::RenameWorkspace {
        workspace,
        expected_version: FieldVersion::new(version),
        draft,
    };
    let name = NameText::new("custom".into()).unwrap();
    apply(&mut reducer, rename(1, NameDraft::Custom(name.clone())), 2);
    let before = reducer.snapshot();
    let same = apply(&mut reducer, rename(2, NameDraft::Custom(name)), 3);
    let blank = apply(&mut reducer, rename(2, NameDraft::Blank), 4);
    assert!(same.publication.is_none() && blank.publication.is_none());
    assert_eq!(reducer.snapshot(), before);
}
#[test]
fn automatic_names_preserve_survivors_and_avoid_custom_suffix_collisions() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let (second, _, _) = create(&mut reducer, 2);
    let third = apply(
        &mut reducer,
        Command::CreateWorkspace {
            server: SERVER,
            source: WorkspaceCreationSource::ServerHome { server: SERVER },
            name: NameDraft::Custom(NameText::new("Default (2)".into()).unwrap()),
        },
        3,
    );
    assert!(
        third
            .publication
            .as_ref()
            .unwrap()
            .writes()
            .iter()
            .any(|v| matches!(v, OrganizationWrite::PutWorkspace(w) if w.id == second))
    );
    assert!(
        reducer
            .snapshot()
            .state()
            .workspaces
            .iter()
            .find(|v| v.id == second)
            .unwrap()
            .name
            .value
            .display
            .as_str()
            == "Default (3)"
    );
    let removed = reducer
        .retained()
        .preview_removal(&[pane], OrganizationLimits::default())
        .unwrap();
    assert!(
        removed
            .organization()
            .workspaces
            .iter()
            .find(|v| v.id == second)
            .unwrap()
            .name
            .value
            .display
            .as_str()
            == "Default (3)"
    );
}
#[test]
fn custom_name_pin_change_updates_dormant_automatic_base_without_renaming() {
    for custom in ["custom", "project"] {
        let mut reducer = reducer();
        let (workspace, _, _) = create(&mut reducer, 1);
        apply(
            &mut reducer,
            Command::RenameWorkspace {
                workspace,
                expected_version: FieldVersion::new(1),
                draft: NameDraft::Custom(NameText::new(custom.into()).unwrap()),
            },
            2,
        );
        let before = reducer.snapshot().state().workspaces[0].name.clone();
        apply(
            &mut reducer,
            Command::SetPinnedDirectory {
                workspace,
                expected_version: FieldVersion::new(1),
                value: Some(directory(b"/project")),
            },
            3,
        );
        assert_eq!(reducer.snapshot().state().workspaces[0].name, before);
        let automatic = NameText::new("project".into()).unwrap();
        let retained = reducer.retained();
        let basis = &retained
            .organization()
            .retained
            .as_ref()
            .unwrap()
            .workspace_names()[0];
        assert_eq!(basis.automatic_base(), &automatic);
        assert_eq!(basis.assigned_automatic(), &automatic);
        apply(
            &mut reducer,
            Command::RenameWorkspace {
                workspace,
                expected_version: FieldVersion::new(2),
                draft: NameDraft::Automatic,
            },
            4,
        );
        assert_eq!(
            reducer.snapshot().state().workspaces[0].name,
            Versioned {
                value: WorkspaceName {
                    mode: NameMode::Automatic,
                    display: automatic
                },
                version: FieldVersion::new(3),
            }
        );
    }
}

#[test]
fn custom_name_pin_change_resolves_current_collisions_when_automatic_is_selected() {
    let mut reducer = reducer();
    let (workspace, _, _) = create(&mut reducer, 1);
    apply(
        &mut reducer,
        Command::RenameWorkspace {
            workspace,
            expected_version: FieldVersion::new(1),
            draft: NameDraft::Custom(NameText::new("custom".into()).unwrap()),
        },
        2,
    );
    apply(
        &mut reducer,
        Command::CreateWorkspace {
            server: SERVER,
            source: WorkspaceCreationSource::Directory(directory(b"/other/project")),
            name: NameDraft::Automatic,
        },
        3,
    );
    apply(
        &mut reducer,
        Command::CreateWorkspace {
            server: SERVER,
            source: WorkspaceCreationSource::ServerHome { server: SERVER },
            name: NameDraft::Custom(NameText::new("project (2)".into()).unwrap()),
        },
        4,
    );
    let others = reducer.snapshot().state().workspaces[1..].to_vec();
    apply(
        &mut reducer,
        Command::SetPinnedDirectory {
            workspace,
            expected_version: FieldVersion::new(1),
            value: Some(directory(b"/project")),
        },
        5,
    );
    assert_eq!(
        reducer.snapshot().state().workspaces[0].name.version,
        FieldVersion::new(2)
    );
    apply(
        &mut reducer,
        Command::RenameWorkspace {
            workspace,
            expected_version: FieldVersion::new(2),
            draft: NameDraft::Automatic,
        },
        6,
    );
    assert_eq!(
        reducer.snapshot().state().workspaces[0].name.value.display,
        NameText::new("project (3)".into()).unwrap()
    );
    assert_eq!(reducer.snapshot().state().workspaces[1..], others);
}

#[test]
fn pin_changes_only_future_creation_defaults() {
    let mut reducer = reducer();
    let (workspace, _, source) = create(&mut reducer, 1);
    let starting = reducer.snapshot().state().panes[0]
        .starting_directory
        .clone();
    apply(
        &mut reducer,
        Command::SetPinnedDirectory {
            workspace,
            expected_version: FieldVersion::new(1),
            value: Some(directory(b"/pinned")),
        },
        2,
    );
    apply(
        &mut reducer,
        Command::CreateTab {
            workspace,
            source: TabCreationSource::Pane(source),
        },
        3,
    );
    let snapshot = reducer.snapshot();
    assert!(snapshot.state().panes[0].starting_directory == starting);
    assert!(snapshot.state().panes[1].starting_directory == directory(b"/pinned"));
    assert_eq!(
        snapshot.state().workspaces[0].name.version,
        FieldVersion::new(2)
    );
}
#[test]
fn directory_repair_changes_only_the_nonrunning_pane() {
    let mut reducer = reducer();
    let (workspace, tab, pane) = create(&mut reducer, 1);
    let before = reducer.snapshot();
    let result = apply(
        &mut reducer,
        Command::RepairPaneDirectory {
            pane,
            expected_version: FieldVersion::new(1),
            value: directory(b"/repaired"),
        },
        2,
    );
    assert!(result.effects.is_empty());
    let after = reducer.snapshot();
    assert_eq!(after.state().workspaces, before.state().workspaces);
    assert_eq!(after.state().tabs, before.state().tabs);
    assert_eq!(
        (after.state().workspaces[0].id, after.state().tabs[0].id),
        (workspace, tab)
    );
    assert_eq!(
        after.state().panes[0]
            .confirmed_restoration_directory
            .version,
        FieldVersion::new(2)
    );
}
#[test]
fn invalid_worktree_location_is_rejected_without_field_change() {
    let mut reducer = reducer();
    for (index, text) in [
        "relative/{branch}",
        "~/../{branch}",
        "~owner/{branch}",
        "~/trees/{unknown}",
        "~/trees/{repository}",
        "~/trees/{branch",
    ]
    .into_iter()
    .enumerate()
    {
        let before = reducer.snapshot();
        let result = apply(
            &mut reducer,
            Command::SetWorktreeLocation {
                server: SERVER,
                expected_version: FieldVersion::new(1),
                template: WorktreeLocationTemplate::new(text.into()).unwrap(),
            },
            index as u64 + 1,
        );
        assert!(matches!(
            result.resolution,
            Resolution::Completed(Outcome::Failed(OperationFailure::InvalidValue))
        ));
        assert_eq!(reducer.snapshot(), before);
    }
}
#[test]
fn recovery_rejects_invalid_worktree_location_grammar() {
    for text in [
        "relative/{branch}",
        "~/trees/../{branch}",
        "~/trees/{unknown}/{branch}",
        "~/trees/{repository}",
    ] {
        let retained = empty();
        let mut org = retained.organization().clone();
        org.settings.worktree_location.value = WorktreeLocationTemplate::new(text.into()).unwrap();
        let corrupt =
            RetainedOrganization::new(org, retained.high_water(), retained.server_home().clone());
        assert!(matches!(
            corrupt.validate(OrganizationLimits::default()),
            Err(ReducerFailure::InvalidDurableState(
                DurableInvariant::WorktreeLocation
            ))
        ));
        assert!(matches!(
            OrganizationReducer::from_recovered(
                corrupt,
                StoreRevision::new(0),
                ServerEpoch::new(9),
                OrganizationLimits::default(),
            ),
            Err(ReducerFailure::InvalidDurableState(
                DurableInvariant::WorktreeLocation
            ))
        ));
    }
}

#[test]
fn remote_access_has_an_independent_version_and_no_listener_effects() {
    let mut reducer = reducer();
    let value = RemoteAccessPolicy::enabled(
        SERVER,
        vec![
            "127.0.0.1:443".parse().unwrap(),
            "[::1]:443".parse().unwrap(),
        ],
    )
    .unwrap();
    let command = Command::SetRemoteAccess {
        server: SERVER,
        expected_version: FieldVersion::new(1),
        value,
    };
    let accepted = apply(&mut reducer, command.clone(), 1);
    assert!(accepted.effects.is_empty());
    assert!(accepted.publication.is_some());
    let conflict = apply(&mut reducer, command, 2);
    assert!(matches!(
        conflict.resolution,
        Resolution::Completed(Outcome::Conflict(FieldConflict::RemoteAccess { .. }))
    ));
    assert_eq!(
        reducer
            .snapshot()
            .state()
            .settings
            .worktree_location
            .version,
        FieldVersion::new(1)
    );
}

#[test]
fn root_replacement_refreshes_only_its_workspaces_directory_name() {
    let mut reducer = reducer();
    let (workspace, tab, root) = create(&mut reducer, 1);
    let split = apply(
        &mut reducer,
        Command::CreatePane {
            tab,
            source: root,
            axis: SplitAxis::Horizontal,
            edge: SplitEdge::After,
        },
        2,
    );
    let successor = match split.resolution {
        Resolution::Completed(Outcome::CreatedWithControl(created)) => match created.entities() {
            CreatedEntities::Pane(pane) => pane,
            _ => panic!("unexpected creation kind"),
        },
        _ => panic!("creation did not complete"),
    };
    let (unaffected, _, other) = create(&mut reducer, 3);
    let unchanged = reducer.snapshot().state().workspaces[1].name.clone();
    for (index, (pane, value)) in [(successor, b"/project".as_slice()), (other, b"/unrelated")]
        .into_iter()
        .enumerate()
    {
        apply(
            &mut reducer,
            Command::RepairPaneDirectory {
                pane,
                expected_version: FieldVersion::new(1),
                value: directory(value),
            },
            index as u64 + 4,
        );
    }
    let removed = reducer
        .retained()
        .preview_removal(&[root], OrganizationLimits::default())
        .unwrap();
    let changed = removed
        .organization()
        .workspaces
        .iter()
        .find(|v| v.id == workspace)
        .unwrap();
    assert_eq!(
        changed.name.value.display,
        NameText::new("project".into()).unwrap()
    );
    assert_eq!(changed.name.version, FieldVersion::new(2));
    let other = removed
        .organization()
        .workspaces
        .iter()
        .find(|v| v.id == unaffected)
        .unwrap();
    assert_eq!(other.name, unchanged);
}
