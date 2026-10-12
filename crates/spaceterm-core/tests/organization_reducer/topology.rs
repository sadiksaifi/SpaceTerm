use super::support::*;

#[derive(Debug, Eq, PartialEq)]
enum Layout {
    Pane(PaneId),
    Split(SplitAxis, Box<Layout>, Box<Layout>),
}
fn layout(retained: &RetainedOrganization, tab: TabId) -> Layout {
    fn split(
        node: &mut Layout,
        source: PaneId,
        pane: PaneId,
        axis: SplitAxis,
        edge: SplitEdge,
    ) -> bool {
        match node {
            Layout::Pane(id) if *id == source => {
                let old = Box::new(Layout::Pane(source));
                let new = Box::new(Layout::Pane(pane));
                *node = if edge == SplitEdge::Before {
                    Layout::Split(axis, new, old)
                } else {
                    Layout::Split(axis, old, new)
                };
                true
            }
            Layout::Pane(_) => false,
            Layout::Split(_, first, second) => {
                split(first, source, pane, axis, edge) || split(second, source, pane, axis, edge)
            }
        }
    }
    let org = retained.organization();
    let default = org
        .retained
        .as_ref()
        .unwrap()
        .creation_defaults()
        .iter()
        .find(|v| v.tab() == tab)
        .unwrap();
    let mut node = Layout::Pane(default.replay_order()[0]);
    for id in &default.replay_order()[1..] {
        let record = org.panes.iter().find(|v| v.id == *id).unwrap();
        let PaneCreation::SplitOf { source, axis, edge } = record.creation else {
            panic!("invalid reconstruction")
        };
        assert!(split(&mut node, source, *id, axis, edge));
    }
    node
}
fn split(
    reducer: &mut OrganizationReducer,
    tab: TabId,
    source: PaneId,
    axis: SplitAxis,
    edge: SplitEdge,
    sequence: u64,
) -> PaneId {
    let result = apply(
        reducer,
        Command::CreatePane {
            tab,
            source,
            axis,
            edge,
        },
        sequence,
    );
    match result.resolution {
        Resolution::Completed(Outcome::CreatedWithControl(v)) => v.entities().pane(),
        _ => panic!("split rejected"),
    }
}
#[test]
fn deleted_split_source_preserves_surviving_creation_topology() {
    let mut reducer = reducer();
    let (_, tab, a) = create(&mut reducer, 1);
    let b = split(
        &mut reducer,
        tab,
        a,
        SplitAxis::Horizontal,
        SplitEdge::After,
        2,
    );
    let c = split(
        &mut reducer,
        tab,
        b,
        SplitAxis::Vertical,
        SplitEdge::After,
        3,
    );
    let d = split(
        &mut reducer,
        tab,
        a,
        SplitAxis::Vertical,
        SplitEdge::After,
        4,
    );
    let retained = reducer
        .retained()
        .preview_removal(&[a], OrganizationLimits::default())
        .unwrap();
    assert_eq!(
        layout(&retained, tab),
        Layout::Split(
            SplitAxis::Horizontal,
            Box::new(Layout::Pane(d)),
            Box::new(Layout::Split(
                SplitAxis::Vertical,
                Box::new(Layout::Pane(b)),
                Box::new(Layout::Pane(c))
            ))
        )
    );
    assert_eq!(retained.organization().tabs[0].root_pane, b);
    retained.validate(OrganizationLimits::default()).unwrap();
    let again = retained
        .preview_removal(&[], OrganizationLimits::default())
        .unwrap();
    assert_eq!(again.organization(), retained.organization());
}
#[test]
fn root_pane_succession_uses_creation_order_not_default_leaf_order() {
    let mut reducer = reducer();
    let (_, tab, a) = create(&mut reducer, 1);
    let b = split(
        &mut reducer,
        tab,
        a,
        SplitAxis::Horizontal,
        SplitEdge::After,
        2,
    );
    split(
        &mut reducer,
        tab,
        a,
        SplitAxis::Horizontal,
        SplitEdge::Before,
        3,
    );
    let retained = reducer
        .retained()
        .preview_removal(&[a], OrganizationLimits::default())
        .unwrap();
    assert_eq!(retained.organization().tabs[0].root_pane, b);
    assert_ne!(
        retained
            .organization()
            .retained
            .as_ref()
            .unwrap()
            .creation_defaults()[0]
            .replay_order()[0],
        b
    );
}
#[test]
fn final_pane_deletes_tab_and_final_tab_deletes_workspace_without_replacement() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let retained = reducer
        .retained()
        .preview_removal(&[pane], OrganizationLimits::default())
        .unwrap();
    assert!(
        retained.organization().workspaces.is_empty()
            && retained.organization().tabs.is_empty()
            && retained.organization().panes.is_empty()
    );
    assert_eq!(retained.high_water(), reducer.retained().high_water());
    retained.validate(OrganizationLimits::default()).unwrap();
}
#[test]
fn repeated_split_delete_has_no_deleted_ancestry_growth() {
    let mut reducer = reducer();
    let (_, tab, mut source) = create(&mut reducer, 1);
    for index in 0..40 {
        let next = split(
            &mut reducer,
            tab,
            source,
            SplitAxis::Vertical,
            SplitEdge::Before,
            index + 2,
        );
        let retained = reducer
            .retained()
            .preview_removal(&[source], OrganizationLimits::default())
            .unwrap();
        assert_eq!(retained.organization().panes.len(), 1);
        assert_eq!(
            retained.organization().panes[0].creation,
            PaneCreation::Initial
        );
        assert_eq!(
            retained
                .organization()
                .retained
                .as_ref()
                .unwrap()
                .creation_defaults()[0]
                .replay_order()
                .len(),
            1
        );
        reducer = OrganizationReducer::from_recovered(
            retained,
            StoreRevision::new(index + 4),
            ServerEpoch::new(index + 10),
            OrganizationLimits::default(),
        )
        .unwrap();
        source = next;
    }
}
#[test]
fn recovery_rejects_deleted_or_cyclic_default_sources() {
    let mut reducer = reducer();
    let (_, tab, a) = create(&mut reducer, 1);
    let b = split(
        &mut reducer,
        tab,
        a,
        SplitAxis::Horizontal,
        SplitEdge::After,
        2,
    );
    let retained = reducer.retained();
    let mut org = retained.organization().clone();
    org.panes[0].creation = PaneCreation::SplitOf {
        source: b,
        axis: SplitAxis::Vertical,
        edge: SplitEdge::After,
    };
    let invalid =
        RetainedOrganization::new(org, retained.high_water(), retained.server_home().clone());
    assert!(matches!(
        invalid.validate(OrganizationLimits::default()),
        Err(ReducerFailure::InvalidDurableState(_))
    ));
}

#[test]
fn root_tab_succession_prefers_same_worktree_then_creation_order() {
    let mut base = reducer();
    let (workspace, root, first) = create(&mut base, 1);
    let mut reducer = OrganizationReducer::from_recovered(
        with_worktrees(base.retained()),
        StoreRevision::new(1),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    let worktree = WorktreeId::new(SERVER, 2).unwrap();
    apply(
        &mut reducer,
        Command::CreateTab {
            workspace,
            source: TabCreationSource::Worktree(worktree),
        },
        2,
    );
    apply(
        &mut reducer,
        Command::CreateTab {
            workspace,
            source: TabCreationSource::Pane(first),
        },
        3,
    );
    let same_class = reducer.snapshot().state().tabs[2].id;
    let retained = reducer
        .retained()
        .preview_removal(&[first], OrganizationLimits::default())
        .unwrap();
    assert_eq!(retained.organization().workspaces[0].root_tab, same_class);
    assert!(!retained.organization().tabs.iter().any(|v| v.id == root));
}
