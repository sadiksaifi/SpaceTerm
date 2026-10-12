use spaceterm_core::authority::{
    ControlGeneration, CreationOrdinal, FieldVersion, OrganizationRevision, ServerEpoch,
    SessionGeneration,
};
use spaceterm_core::identity::{PaneId, ServerId, TabId, WorkspaceId, WorktreeId};
use spaceterm_core::organization::{
    AcceptedPaneCreation, ControlRecord, NameMode, NameText, OrganizationDelta,
    OrganizationSnapshot, OrganizationState, OrganizationWrite, PaneCreation, PaneRecord,
    PaneRuntimeRecord, ServerDirectory, ServerSettingsRecord, SessionPhase, SplitAxis, SplitEdge,
    TabRecord, Versioned, WorkspaceName, WorkspaceRecord, WorktreeLocationTemplate,
};
use spaceterm_core::organization_retained::TabCreationDefault;

use crate::view::{
    ArrangementMetrics, ArrangementNode, ArrangementRecord, CreationLayout, CreationNode,
    FocusDirection, LayoutRevision, MAX_TAB_PANES, MAX_VIEW_TABS, PaneArrangement, PaneEdge,
    PaneNodeRef, PanePlacement, PaneSize, PaneTreeRef, SplitId, TabOrder, TabOrderChange,
    TabOrderRecord, TabStep, ViewCounter, ViewError, ViewResource, ZoomState,
};

fn server() -> ServerId {
    ServerId::new([7; 16])
}

fn foreign_server() -> ServerId {
    ServerId::new([8; 16])
}

fn workspace() -> WorkspaceId {
    WorkspaceId::new(server(), 1).unwrap()
}

fn tab(local: u64) -> TabId {
    TabId::new(server(), local).unwrap()
}

fn pane(local: u64) -> PaneId {
    PaneId::new(server(), local).unwrap()
}

fn panes<const N: usize>(locals: [u64; N]) -> [PaneId; N] {
    locals.map(pane)
}

fn tabs<const N: usize>(locals: [u64; N]) -> [TabId; N] {
    locals.map(tab)
}

fn split(local: u64) -> SplitId {
    SplitId::new(local).unwrap()
}

fn size(width: f32, height: f32) -> PaneSize {
    PaneSize::new(width, height).unwrap()
}

fn roomy() -> PaneSize {
    size(1000.0, 1000.0)
}

fn metrics() -> ArrangementMetrics {
    ArrangementMetrics {
        minimum_leaf: size(10.0, 10.0),
        divider: 1.0,
    }
}

fn by_local(pane: PaneId) -> Option<CreationOrdinal> {
    Some(CreationOrdinal::new(pane.local()))
}

/// Parses `H(1,V(2,3))`: H places its children side by side and V stacks them.
fn creation(spec: &str) -> CreationLayout {
    fn parse(spec: &str, at: &mut usize, nodes: &mut Vec<CreationNode>) -> u32 {
        let index = u32::try_from(nodes.len()).unwrap();
        let bytes = spec.as_bytes();
        match bytes[*at] {
            kind @ (b'H' | b'V') => {
                nodes.push(CreationNode::Pane(pane(1)));
                *at += 2;
                let first = parse(spec, at, nodes);
                *at += 1;
                let second = parse(spec, at, nodes);
                *at += 1;
                let axis = if kind == b'H' {
                    SplitAxis::Horizontal
                } else {
                    SplitAxis::Vertical
                };
                nodes[index as usize] = CreationNode::Split {
                    axis,
                    first,
                    second,
                };
            }
            _ => {
                let start = *at;
                while *at < bytes.len() && bytes[*at].is_ascii_digit() {
                    *at += 1;
                }
                nodes.push(CreationNode::Pane(pane(spec[start..*at].parse().unwrap())));
            }
        }
        index
    }

    let mut nodes = Vec::new();
    let root = parse(spec, &mut 0, &mut nodes);
    CreationLayout { root, nodes }
}

/// A right-leaning chain `H(1,H(2,...H(count-1,count)))`.
fn chain(count: u64) -> CreationLayout {
    chain_on(SplitAxis::Horizontal, count)
}

/// A chain whose every Split puts one Pane first and the rest second along `axis`.
fn chain_on(axis: SplitAxis, count: u64) -> CreationLayout {
    let mut nodes = Vec::new();
    for local in 1..count {
        let index = u32::try_from(nodes.len()).unwrap();
        nodes.push(CreationNode::Split {
            axis,
            first: index + 1,
            second: index + 2,
        });
        nodes.push(CreationNode::Pane(pane(local)));
    }
    nodes.push(CreationNode::Pane(pane(count)));
    CreationLayout { root: 0, nodes }
}

fn arrangement(spec: &str, focus: u64) -> PaneArrangement {
    PaneArrangement::from_creation(tab(1), &creation(spec), pane(focus)).unwrap()
}

fn shape(arrangement: &PaneArrangement) -> String {
    fn render(tree: PaneTreeRef<'_>, out: &mut String) {
        match tree.node() {
            PaneNodeRef::Pane(pane) => out.push_str(&pane.local().to_string()),
            PaneNodeRef::Split {
                axis,
                first,
                second,
                ..
            } => {
                out.push(if axis == SplitAxis::Horizontal {
                    'H'
                } else {
                    'V'
                });
                out.push('(');
                render(first, out);
                out.push(',');
                render(second, out);
                out.push(')');
            }
        }
    }

    let mut out = String::new();
    render(arrangement.tree(), &mut out);
    out
}

/// Split identities and ratios in visual pre-order.
fn splits(arrangement: &PaneArrangement) -> Vec<(u64, f32)> {
    arrangement
        .export()
        .nodes
        .iter()
        .filter_map(|node| match *node {
            ArrangementNode::Split { id, ratio, .. } => Some((id.get(), ratio)),
            ArrangementNode::Pane(_) => None,
        })
        .collect()
}

fn edit_split(
    record: &mut ArrangementRecord,
    index: usize,
    edit: impl FnOnce(&mut SplitId, &mut f32, &mut u32, &mut u32),
) {
    let ArrangementNode::Split {
        id,
        ratio,
        first,
        second,
        ..
    } = &mut record.nodes[index]
    else {
        panic!("node {index} is not a Split");
    };
    edit(id, ratio, first, second);
}

fn directory() -> ServerDirectory {
    ServerDirectory::new(server(), b"/work".to_vec()).unwrap()
}

fn record(local: u64, ordinal: u64, creation: PaneCreation) -> PaneRecord {
    PaneRecord {
        id: pane(local),
        tab: tab(1),
        creation_ordinal: CreationOrdinal::new(ordinal),
        creation,
        starting_directory: directory(),
        confirmed_restoration_directory: Versioned {
            value: None,
            version: FieldVersion::new(1),
        },
    }
}

fn split_of(source: u64, axis: SplitAxis, edge: SplitEdge) -> PaneCreation {
    PaneCreation::SplitOf {
        source: pane(source),
        axis,
        edge,
    }
}

fn default_layout(order: &[u64], panes: &[PaneRecord]) -> Result<CreationLayout, ViewError> {
    let default =
        TabCreationDefault::new(tab(1), order.iter().copied().map(pane).collect()).unwrap();
    CreationLayout::from_default(&default, panes)
}

fn accepted_split(
    created: u64,
    source: u64,
    axis: SplitAxis,
    edge: SplitEdge,
    ordinal: u64,
) -> AcceptedPaneCreation {
    let creation = split_of(source, axis, edge);
    let delta = OrganizationDelta::new(
        server(),
        ServerEpoch::new(1),
        OrganizationRevision::new(1),
        OrganizationRevision::new(2),
        vec![OrganizationWrite::PutPane(record(
            created, ordinal, creation,
        ))],
        vec![],
    )
    .unwrap();
    AcceptedPaneCreation::new(
        &delta,
        tab(1),
        pane(created),
        CreationOrdinal::new(ordinal),
        creation,
    )
    .unwrap()
}

fn snapshot(panes: &[PaneRecord]) -> OrganizationSnapshot {
    let state = OrganizationState {
        server: server(),
        revision: OrganizationRevision::new(1),
        settings: ServerSettingsRecord {
            worktree_location: Versioned {
                value: WorktreeLocationTemplate::new("../{repo}/{branch}".to_owned()).unwrap(),
                version: FieldVersion::new(1),
            },
        },
        workspaces: vec![WorkspaceRecord {
            id: workspace(),
            name: Versioned {
                value: WorkspaceName {
                    mode: NameMode::Automatic,
                    display: NameText::new("work".to_owned()).unwrap(),
                },
                version: FieldVersion::new(1),
            },
            pinned_directory: Versioned {
                value: None,
                version: FieldVersion::new(1),
            },
            root_tab: tab(1),
            creation_ordinal: CreationOrdinal::new(1),
        }],
        tabs: vec![TabRecord {
            id: tab(1),
            workspace: workspace(),
            worktree: None,
            root_pane: pane(1),
            creation_ordinal: CreationOrdinal::new(2),
        }],
        panes: panes.to_vec(),
        repositories: vec![],
        worktrees: vec![],
        retained: None,
    };
    let runtime = panes
        .iter()
        .map(|record| PaneRuntimeRecord {
            pane: record.id,
            session_generation: SessionGeneration::new(0),
            phase: SessionPhase::NotStarted,
            control_generation: ControlGeneration::new(0),
            control: ControlRecord::Unclaimed,
        })
        .collect();
    OrganizationSnapshot::new(ServerEpoch::new(1), state, runtime).unwrap()
}

fn tab_record(local: u64, worktree: Option<u64>) -> TabRecord {
    TabRecord {
        id: tab(local),
        workspace: workspace(),
        worktree: worktree.map(|local| WorktreeId::new(server(), local).unwrap()),
        root_pane: pane(local * 100),
        creation_ordinal: CreationOrdinal::new(local),
    }
}

fn without(records: &[TabRecord], removed: &[u64]) -> Vec<TabRecord> {
    records
        .iter()
        .filter(|record| !removed.contains(&record.id.local()))
        .cloned()
        .collect()
}

/// Tabs 1, 3 and 5 belong to the Workspace's home scope; 2 and 4 to Worktree 1.
fn interleaved_tabs() -> Vec<TabRecord> {
    vec![
        tab_record(1, None),
        tab_record(2, Some(1)),
        tab_record(3, None),
        tab_record(4, Some(1)),
        tab_record(5, None),
    ]
}

#[test]
fn creation_layout_reproduces_normalized_surviving_topology() {
    let records = [
        record(1, 10, PaneCreation::Initial),
        record(2, 11, split_of(1, SplitAxis::Horizontal, SplitEdge::After)),
        record(3, 12, split_of(1, SplitAxis::Vertical, SplitEdge::After)),
        record(4, 13, split_of(2, SplitAxis::Vertical, SplitEdge::After)),
        record(5, 14, split_of(4, SplitAxis::Horizontal, SplitEdge::Before)),
    ];
    let layout = default_layout(&[1, 2, 3, 4, 5], &records).unwrap();

    let arrangement = PaneArrangement::from_creation(tab(1), &layout, pane(1)).unwrap();

    assert_eq!(shape(&arrangement), "H(V(1,3),V(2,H(5,4)))");
    assert_eq!(
        splits(&arrangement),
        [(1, 0.5), (2, 0.5), (3, 0.5), (4, 0.5)]
    );
    assert_eq!(arrangement.split_high_water(), 4);
    assert_eq!(arrangement.pane_ids(), panes([1, 3, 2, 5, 4]));
    assert_eq!(arrangement.focused_pane(), Some(pane(1)));
    assert_eq!(arrangement.focus_history(), [pane(1)]);
    assert_eq!(arrangement.zoom(), ZoomState::Restored);
    assert_eq!(arrangement.layout_revision(), LayoutRevision::INITIAL);

    // The replay order must name each source before it is split and cover exact membership.
    assert_eq!(
        default_layout(&[1, 4, 2, 3, 5], &records),
        Err(ViewError::InvalidCreationTopology)
    );
    assert_eq!(
        default_layout(&[1, 2, 3, 4], &records),
        Err(ViewError::InvalidCreationTopology)
    );
    assert_eq!(
        PaneArrangement::from_creation(tab(1), &creation("H(1,1)"), pane(1)),
        Err(ViewError::InvalidCreationTopology)
    );
    assert_eq!(
        PaneArrangement::from_creation(tab(1), &layout, pane(9)),
        Err(ViewError::TargetMissing)
    );
}

#[test]
fn creation_layout_after_deleted_source_does_not_replay_by_creation_ordinal() {
    // The Server built H(V(1,3),V(2,4)), then deleted Pane 2. Its default normalized
    // Pane 4 to split Pane 1 and moved it after Pane 3 in the replay order.
    let records = [
        record(1, 10, PaneCreation::Initial),
        record(3, 12, split_of(1, SplitAxis::Vertical, SplitEdge::After)),
        record(4, 13, split_of(1, SplitAxis::Horizontal, SplitEdge::After)),
    ];

    let normalized = default_layout(&[1, 4, 3], &records).unwrap();
    let arrangement = PaneArrangement::from_creation(tab(1), &normalized, pane(1)).unwrap();

    assert_eq!(shape(&arrangement), "H(V(1,3),4)");
    // The same links replayed by creation ordinal produce a different, wrong layout.
    let by_ordinal = default_layout(&[1, 3, 4], &records).unwrap();
    assert_eq!(
        shape(&PaneArrangement::from_creation(tab(1), &by_ordinal, pane(1)).unwrap()),
        "V(H(1,4),3)"
    );
}

#[test]
fn remote_split_uses_source_personal_placement_and_preserves_focus_zoom() {
    let mut arrangement = arrangement("H(1,2)", 1);
    assert_eq!(
        arrangement.move_pane(pane(1), pane(2), PaneEdge::Bottom, roomy(), metrics()),
        Ok(true)
    );
    assert_eq!(shape(&arrangement), "V(2,1)");
    assert!(arrangement.toggle_zoom());
    let revision = arrangement.layout_revision();

    let placement = PanePlacement::from_accepted(accepted_split(
        3,
        2,
        SplitAxis::Horizontal,
        SplitEdge::After,
        7,
    ));
    assert_eq!(
        placement,
        PanePlacement {
            pane: pane(3),
            source: pane(2),
            edge: PaneEdge::Right,
            ordinal: CreationOrdinal::new(7),
        }
    );
    // The Server's default differs from the personal arrangement; only the source matters.
    let change = arrangement
        .reconcile(&creation("H(1,H(2,3))"), &[placement], by_local)
        .unwrap();

    assert_eq!(shape(&arrangement), "V(H(2,3),1)");
    assert_eq!(change.inserted, [pane(3)]);
    assert!(change.removed.is_empty());
    assert!(!change.focus_changed);
    assert!(!change.zoom_cleared);
    assert_eq!(arrangement.focused_pane(), Some(pane(1)));
    assert_eq!(arrangement.zoom(), ZoomState::Zoomed(pane(1)));
    assert!(arrangement.layout_revision() > revision);
}

#[test]
fn accepted_placements_apply_in_creation_order_when_chained() {
    let mut arrangement = arrangement("H(1,2)", 2);
    let placements = [
        PanePlacement {
            pane: pane(4),
            source: pane(3),
            edge: PaneEdge::Top,
            ordinal: CreationOrdinal::new(4),
        },
        PanePlacement {
            pane: pane(3),
            source: pane(1),
            edge: PaneEdge::Bottom,
            ordinal: CreationOrdinal::new(3),
        },
    ];

    let change = arrangement
        .reconcile(&creation("H(V(1,V(4,3)),2)"), &placements, by_local)
        .unwrap();

    assert_eq!(shape(&arrangement), "H(V(1,V(4,3)),2)");
    assert_eq!(change.inserted, panes([3, 4]));
    assert_eq!(arrangement.focused_pane(), Some(pane(2)));
}

#[test]
fn personal_move_and_resize_never_change_published_roots() {
    let published = snapshot(&[
        record(1, 3, PaneCreation::Initial),
        record(2, 4, split_of(1, SplitAxis::Horizontal, SplitEdge::After)),
        record(3, 5, split_of(2, SplitAxis::Vertical, SplitEdge::After)),
    ]);
    let before = published.clone();
    let default = TabCreationDefault::new(tab(1), panes([1, 2, 3]).to_vec()).unwrap();
    let layout = CreationLayout::from_default(&default, &published.state().panes).unwrap();
    let root_pane = published.state().tabs[0].root_pane;
    let mut arrangement = PaneArrangement::from_creation(tab(1), &layout, root_pane).unwrap();
    assert_eq!(shape(&arrangement), "H(1,V(2,3))");

    assert_eq!(
        arrangement.move_pane(pane(1), pane(3), PaneEdge::Right, roomy(), metrics()),
        Ok(true)
    );
    assert_eq!(shape(&arrangement), "V(2,H(3,1))");
    let root = split(2);
    assert_eq!(
        arrangement.resize_split(root, roomy(), metrics(), 0.25),
        Ok(0.25)
    );
    assert_eq!(
        arrangement.set_split_axis(root, SplitAxis::Horizontal, roomy(), metrics()),
        Ok(true)
    );
    assert_eq!(arrangement.focus_pane(pane(2)), Ok(true));
    assert!(arrangement.toggle_zoom());

    // The personal arrangement no longer leads with the Root Pane, but the
    // published organization and its creation default are untouched.
    assert_eq!(arrangement.pane_ids().first(), Some(&pane(2)));
    assert_eq!(published, before);
    assert_eq!(published.state().workspaces[0].root_tab, tab(1));
    assert_eq!(published.state().tabs[0].root_pane, pane(1));
    assert_eq!(
        CreationLayout::from_default(&default, &published.state().panes),
        Ok(layout)
    );
}

#[test]
fn unchanged_move_preserves_split_ids_and_ratios() {
    let mut arrangement = arrangement("H(V(1,2),V(3,4))", 1);
    assert_eq!(
        arrangement.resize_split(split(2), roomy(), metrics(), 0.3),
        Ok(0.3)
    );
    assert!(arrangement.toggle_zoom());
    let before = arrangement.clone();

    // Moving Pane 2 back below Pane 1 rebuilds the same arrangement.
    assert_eq!(
        arrangement.move_pane(pane(2), pane(1), PaneEdge::Bottom, roomy(), metrics()),
        Ok(false)
    );
    assert_eq!(
        arrangement.move_pane(pane(1), pane(2), PaneEdge::Top, roomy(), metrics()),
        Ok(false)
    );
    assert_eq!(
        arrangement.move_pane(pane(1), pane(1), PaneEdge::Left, roomy(), metrics()),
        Ok(false)
    );
    assert_eq!(arrangement, before);

    // A changed move keeps every untouched Split and allocates one new Split.
    assert_eq!(
        arrangement.resize_split(split(3), roomy(), metrics(), 0.7),
        Ok(0.7)
    );
    assert_eq!(
        arrangement.move_pane(pane(1), pane(2), PaneEdge::Right, roomy(), metrics()),
        Ok(true)
    );
    assert_eq!(shape(&arrangement), "H(H(2,1),V(3,4))");
    assert_eq!(splits(&arrangement), [(1, 0.5), (4, 0.5), (3, 0.7)]);
    assert_eq!(arrangement.split_high_water(), 4);
    assert_eq!(arrangement.focused_pane(), Some(pane(1)));
    assert_eq!(arrangement.zoom(), ZoomState::Restored);
}

#[test]
fn removed_leaf_collapses_only_its_parent_and_preserves_other_ratios() {
    let mut arrangement = arrangement("H(V(1,2),V(3,4))", 1);
    for (id, ratio) in [(1, 0.4), (2, 0.3), (3, 0.7)] {
        assert_eq!(
            arrangement.resize_split(split(id), roomy(), metrics(), ratio),
            Ok(ratio)
        );
    }

    let change = arrangement
        .reconcile(&creation("H(1,V(3,4))"), &[], by_local)
        .unwrap();

    assert_eq!(change.removed, [pane(2)]);
    assert!(change.inserted.is_empty());
    assert!(!change.focus_changed);
    assert_eq!(shape(&arrangement), "H(1,V(3,4))");
    assert_eq!(splits(&arrangement), [(1, 0.4), (3, 0.7)]);

    // Reconciling the same membership again is structurally idempotent.
    let settled = arrangement.clone();
    let change = arrangement
        .reconcile(&creation("V(1,H(3,4))"), &[], by_local)
        .unwrap();
    assert!(change.is_empty());
    assert_eq!(arrangement, settled);
}

#[test]
fn focused_pane_removal_uses_personal_sibling_fallback() {
    // The nearest surviving leaf of the sibling subtree wins over focus history.
    let mut arrangement = arrangement("H(V(1,2),3)", 1);
    arrangement.focus_pane(pane(3)).unwrap();
    arrangement.focus_pane(pane(1)).unwrap();
    let change = arrangement
        .reconcile(&creation("H(2,3)"), &[], by_local)
        .unwrap();
    assert!(change.focus_changed);
    assert_eq!(arrangement.focused_pane(), Some(pane(2)));
    assert_eq!(arrangement.focus_history(), panes([3, 2]));

    // A second child falls back to the nearest leaf before it.
    let mut arrangement = self::arrangement("H(V(1,2),3)", 3);
    arrangement
        .reconcile(&creation("V(1,2)"), &[], by_local)
        .unwrap();
    assert_eq!(arrangement.focused_pane(), Some(pane(2)));

    // Remaining focus history follows when the whole sibling subtree is gone.
    let mut arrangement = self::arrangement("H(V(1,2),V(3,4))", 4);
    arrangement.focus_pane(pane(1)).unwrap();
    arrangement
        .reconcile(&creation("V(3,4)"), &[], by_local)
        .unwrap();
    assert_eq!(arrangement.focused_pane(), Some(pane(4)));
    assert_eq!(arrangement.focus_history(), [pane(4)]);

    // Without a surviving Pane focused here, the first surviving visual leaf
    // takes focus.
    let mut arrangement = self::arrangement("H(V(1,2),V(3,4))", 1);
    arrangement
        .reconcile(&creation("V(3,4)"), &[], by_local)
        .unwrap();
    assert_eq!(arrangement.focused_pane(), Some(pane(3)));
    assert_eq!(arrangement.focus_history(), [pane(3)]);
}

#[test]
fn focus_fallback_ignores_panes_this_client_never_focused() {
    // Only Pane 1 was focused here. A remote split puts Pane 3 left of Pane 4.
    let mut arrangement = arrangement("H(V(1,2),V(4,5))", 1);
    let placement = PanePlacement {
        pane: pane(3),
        source: pane(4),
        edge: PaneEdge::Left,
        ordinal: CreationOrdinal::new(6),
    };
    arrangement
        .reconcile(&creation("H(V(1,2),V(H(3,4),5))"), &[placement], by_local)
        .unwrap();
    assert_eq!(arrangement.pane_ids(), panes([1, 2, 3, 4, 5]));

    // Removing Panes 1 and 2 leaves no sibling survivor and no focused
    // survivor, so the first surviving visual leaf takes focus.
    let change = arrangement
        .reconcile(&creation("V(H(3,4),5)"), &[], by_local)
        .unwrap();
    assert!(change.focus_changed);
    assert_eq!(shape(&arrangement), "V(H(3,4),5)");
    assert_eq!(arrangement.focused_pane(), Some(pane(3)));
}

#[test]
fn directional_focus_and_history_match_existing_behavior() {
    let mut arrangement = arrangement("H(V(1,3),V(2,4))", 1);

    let focused = [
        FocusDirection::Right,
        FocusDirection::Down,
        FocusDirection::Left,
        FocusDirection::Up,
    ]
    .map(|direction| arrangement.focus_in_direction(direction).unwrap());
    assert_eq!(focused, [2, 4, 3, 1].map(|local| Some(pane(local))));
    // Directional focus does not wrap at a layout edge.
    assert_eq!(
        arrangement.focus_in_direction(FocusDirection::Left),
        Ok(None)
    );
    assert_eq!(arrangement.focused_pane(), Some(pane(1)));

    // Explicit focus orders a unique history; cycling walks it without reordering.
    let mut arrangement = self::arrangement("H(V(1,3),V(2,4))", 1);
    for local in [2, 3, 4, 2, 1] {
        arrangement.focus_pane(pane(local)).unwrap();
    }
    assert_eq!(arrangement.focus_history(), panes([3, 4, 2, 1]));
    let previous: Vec<_> = (0..4)
        .map(|_| arrangement.focus_previous().unwrap())
        .collect();
    let next: Vec<_> = (0..4).map(|_| arrangement.focus_next().unwrap()).collect();
    assert_eq!(previous, panes([2, 4, 3, 1]));
    assert_eq!(next, panes([3, 4, 2, 1]));
    assert_eq!(arrangement.focus_history(), panes([3, 4, 2, 1]));

    // A one-Pane history has nothing to cycle.
    let mut single = self::arrangement("1", 1);
    assert_eq!(single.focus_next(), None);
    assert_eq!(single.focus_pane(pane(1)), Ok(false));
}

#[test]
fn zoom_tracks_explicit_focus_and_clears_when_its_pane_is_removed() {
    let mut arrangement = arrangement("H(1,V(2,3))", 1);
    assert!(arrangement.toggle_zoom());
    assert_eq!(arrangement.zoom(), ZoomState::Zoomed(pane(1)));
    arrangement.focus_pane(pane(2)).unwrap();
    assert_eq!(arrangement.zoom(), ZoomState::Zoomed(pane(2)));
    assert_eq!(
        arrangement.focus_in_direction(FocusDirection::Left),
        Ok(Some(pane(1)))
    );
    assert_eq!(arrangement.zoom(), ZoomState::Zoomed(pane(1)));
    assert_eq!(arrangement.focus_previous(), Some(pane(2)));
    assert_eq!(arrangement.zoom(), ZoomState::Zoomed(pane(2)));

    let change = arrangement
        .reconcile(&creation("H(1,3)"), &[], by_local)
        .unwrap();
    assert!(change.zoom_cleared);
    assert_eq!(arrangement.zoom(), ZoomState::Restored);
    assert_eq!(arrangement.focused_pane(), Some(pane(3)));

    // A one-Pane arrangement is never zoomed.
    let mut single = self::arrangement("1", 1);
    assert!(!single.toggle_zoom());
    let mut pair = self::arrangement("H(1,2)", 1);
    assert!(pair.toggle_zoom());
    let change = pair.reconcile(&creation("1"), &[], by_local).unwrap();
    assert!(change.zoom_cleared);
    assert_eq!(pair.zoom(), ZoomState::Restored);
}

#[test]
fn batched_removal_and_insertion_preserve_surviving_zoom() {
    // One resynchronized snapshot removes Pane 2 and adds Pane 3. The zoomed
    // Pane survives in a two-Pane result, so it stays zoomed.
    let mut arrangement = arrangement("H(1,2)", 1);
    assert!(arrangement.toggle_zoom());

    let change = arrangement
        .reconcile(&creation("H(1,3)"), &[], by_local)
        .unwrap();

    assert_eq!(shape(&arrangement), "H(1,3)");
    assert_eq!(change.removed, [pane(2)]);
    assert_eq!(change.inserted, [pane(3)]);
    assert!(!change.focus_changed);
    assert!(!change.zoom_cleared);
    assert_eq!(arrangement.focused_pane(), Some(pane(1)));
    assert_eq!(arrangement.zoom(), ZoomState::Zoomed(pane(1)));

    // A completed one-Pane result is still never zoomed.
    let mut pair = self::arrangement("H(1,2)", 1);
    assert!(pair.toggle_zoom());
    let change = pair.prune(|pane| pane.local() == 1).unwrap().unwrap();
    assert!(change.zoom_cleared);
    assert_eq!(pair.zoom(), ZoomState::Restored);
}

#[test]
fn pane_cycle_reaches_panes_this_client_never_focused() {
    // Unfocused Panes count as older than every focused Pane; the earliest
    // visual Pane among them is the most recent.
    let mut arrangement = arrangement("H(1,V(2,3))", 1);
    assert_eq!(arrangement.focus_history(), [pane(1)]);
    let next: Vec<_> = (0..3).map(|_| arrangement.focus_next().unwrap()).collect();
    assert_eq!(next, panes([3, 2, 1]));
    let previous: Vec<_> = (0..3)
        .map(|_| arrangement.focus_previous().unwrap())
        .collect();
    assert_eq!(previous, panes([2, 3, 1]));
    // Cycling records nothing.
    assert_eq!(arrangement.focus_history(), [pane(1)]);

    // A remote insertion joins the unfocused Panes at its visual position and
    // keeps focus and zoom.
    assert!(arrangement.toggle_zoom());
    let placement = PanePlacement {
        pane: pane(4),
        source: pane(2),
        edge: PaneEdge::Right,
        ordinal: CreationOrdinal::new(4),
    };
    arrangement
        .reconcile(&creation("H(1,V(H(2,4),3))"), &[placement], by_local)
        .unwrap();
    assert_eq!(arrangement.focused_pane(), Some(pane(1)));
    assert_eq!(arrangement.zoom(), ZoomState::Zoomed(pane(1)));
    assert_eq!(arrangement.focus_history(), [pane(1)]);
    let previous: Vec<_> = (0..4)
        .map(|_| arrangement.focus_previous().unwrap())
        .collect();
    assert_eq!(previous, panes([2, 4, 3, 1]));
    assert_eq!(arrangement.focus_next(), Some(pane(3)));
    assert_eq!(arrangement.zoom(), ZoomState::Zoomed(pane(3)));

    // A Focused Pane reached by cycling is absent from the retained history.
    let record = arrangement.export();
    assert_eq!(record.focused, pane(3));
    assert_eq!(record.focus_history, [pane(1)]);
    assert_eq!(
        PaneArrangement::import(tab(1), &record).map(|restored| restored.export()),
        Ok(record)
    );
}

#[test]
fn directional_focus_walks_deep_chains_without_returning_its_source() {
    // From Pane 20 on, each Pane is narrower than the edge tolerance, so its
    // own bounds and its neighbors on both sides pass the edge test.
    let count = 24;
    for (axis, forward, backward) in [
        (
            SplitAxis::Horizontal,
            FocusDirection::Right,
            FocusDirection::Left,
        ),
        (
            SplitAxis::Vertical,
            FocusDirection::Down,
            FocusDirection::Up,
        ),
    ] {
        let layout = chain_on(axis, count);
        let mut arrangement = PaneArrangement::from_creation(tab(1), &layout, pane(1)).unwrap();
        for local in 2..=count {
            assert_eq!(
                arrangement.focus_in_direction(forward),
                Ok(Some(pane(local))),
                "{forward:?} to Pane {local}"
            );
        }
        assert_eq!(arrangement.focus_in_direction(forward), Ok(None));
        for local in (1..count).rev() {
            assert_eq!(
                arrangement.focus_in_direction(backward),
                Ok(Some(pane(local))),
                "{backward:?} to Pane {local}"
            );
        }
        assert_eq!(arrangement.focus_in_direction(backward), Ok(None));
    }
}

#[test]
fn directional_focus_keeps_adjacent_panes_whose_centers_round_together() {
    // With u = 2^-24, Pane 4 spans [1 - 3u, 1 - 2u] and Pane 2 spans
    // [1 - 2u, 1 - u]. Both f32 centers round to 1 - 2u.
    for (axis, forward, backward) in [
        (
            SplitAxis::Horizontal,
            FocusDirection::Right,
            FocusDirection::Left,
        ),
        (
            SplitAxis::Vertical,
            FocusDirection::Down,
            FocusDirection::Up,
        ),
    ] {
        let node = |id, ratio, first, second| ArrangementNode::Split {
            id: split(id),
            axis,
            ratio,
            first,
            second,
        };
        let record = ArrangementRecord {
            root: 0,
            nodes: vec![
                node(1, f32::from_bits(0x3f7f_fffd), 1, 2),
                ArrangementNode::Pane(pane(1)),
                node(2, 1.0 / 3.0, 3, 4),
                ArrangementNode::Pane(pane(4)),
                node(3, 0.5, 5, 6),
                ArrangementNode::Pane(pane(2)),
                ArrangementNode::Pane(pane(3)),
            ],
            split_high_water: 3,
            focused: pane(4),
            focus_history: panes([1, 2, 3, 4]).to_vec(),
            zoomed: None,
        };
        let mut arrangement = PaneArrangement::import(tab(1), &record).unwrap();

        assert_eq!(
            arrangement.focus_in_direction(forward),
            Ok(Some(pane(2))),
            "{forward:?}"
        );
        assert_eq!(
            arrangement.focus_in_direction(backward),
            Ok(Some(pane(4))),
            "{backward:?}"
        );
    }
}

#[test]
fn exactly_fitting_layout_resizes_at_its_minimum_size() {
    // At width 43 the root Split's independently rounded f32 bounds,
    // 10/42 and 1 - 32/42, cross by one unit in the last place.
    let mut arrangement = PaneArrangement::from_creation(tab(1), &chain(4), pane(1)).unwrap();
    assert_eq!(arrangement.minimum_size(metrics()), Ok(size(43.0, 10.0)));

    // Each Split's own region when the whole arrangement sits at its minimum.
    for (id, width) in [(1, 43.0), (2, 32.0), (3, 21.0)] {
        let region = size(width, 10.0);
        let range = arrangement
            .split_ratio_range(split(id), region, metrics())
            .unwrap();
        assert_eq!(range.start(), range.end(), "Split {id}");
        assert_eq!(
            arrangement.resize_split(split(id), region, metrics(), 0.9),
            Ok(*range.start())
        );
        // The first side keeps its minimum, and the second side the rest.
        let first = *range.start() * (width - 1.0);
        assert!((first - 10.0).abs() <= 1e-5, "Split {id}: {first}");
    }
}

#[test]
fn sizes_holding_the_minimum_always_have_a_valid_ratio_range() {
    // Each f32 minimum rounds its children's sum down, so exact arithmetic at
    // that size finds the children overlapping.
    for (spec, leaf, divider) in [
        // The overlap is about 1.99e-7 in ratio terms, beyond one f32 step.
        ("H(1,2)", 1.2, 10.0),
        // The divider absorbs both leaves, leaving no content at all.
        ("H(1,2)", 0.01, 1.0e6),
        // The first side's exact share underflows f32 to zero.
        ("H(1,H(2,3))", f32::from_bits(1), 100.0),
    ] {
        let metrics = ArrangementMetrics {
            minimum_leaf: size(leaf, leaf),
            divider,
        };
        let mut arrangement = arrangement(spec, 1);
        let minimum = arrangement.minimum_size(metrics).unwrap();
        let range = arrangement
            .split_ratio_range(split(1), minimum, metrics)
            .unwrap_or_else(|error| panic!("{spec} with {leaf} and {divider}: {error:?}"));
        assert!(
            0.0 < *range.start() && range.start() <= range.end() && *range.end() < 1.0,
            "{spec} with {leaf} and {divider}: {range:?}"
        );
        let applied = arrangement
            .resize_split(split(1), minimum, metrics, 0.9)
            .unwrap();
        assert!(range.contains(&applied), "{applied} outside {range:?}");
    }
}

#[test]
fn resize_clamps_against_recursive_child_minima() {
    let mut arrangement = arrangement("H(V(1,2),H(3,4))", 1);
    let available = size(100.0, 50.0);

    assert_eq!(arrangement.minimum_size(metrics()), Ok(size(32.0, 21.0)));
    let range = arrangement
        .split_ratio_range(split(1), available, metrics())
        .unwrap();
    // Each bound is the exact ratio rounded once: 10/99 and (99 - 21)/99.
    assert_eq!((*range.start(), *range.end()), (10.0 / 99.0, 78.0 / 99.0));
    assert_eq!(
        arrangement.resize_split(split(1), available, metrics(), 0.0),
        Ok(10.0 / 99.0)
    );
    assert_eq!(
        arrangement.resize_split(split(1), available, metrics(), 1.0),
        Ok(78.0 / 99.0)
    );
    assert_eq!(
        arrangement.resize_split(split(1), available, metrics(), 0.5),
        Ok(0.5)
    );
    assert_eq!(
        arrangement.resize_split(split(1), size(31.0, 50.0), metrics(), 0.5),
        Err(ViewError::InsufficientSpace)
    );
    assert_eq!(
        arrangement.resize_split(split(9), available, metrics(), 0.5),
        Err(ViewError::TargetMissing)
    );
}

#[test]
fn set_split_axis_clamps_the_ratio_into_the_new_axis_range() {
    let mut arrangement = arrangement("V(1,H(2,3))", 1);
    let available = size(100.0, 100.0);
    assert_eq!(
        arrangement.resize_split(split(1), available, metrics(), 0.85),
        Ok(0.85)
    );

    assert_eq!(
        arrangement.set_split_axis(split(1), SplitAxis::Horizontal, available, metrics()),
        Ok(true)
    );
    assert_eq!(shape(&arrangement), "H(1,H(2,3))");
    assert_eq!(splits(&arrangement), [(1, 78.0 / 99.0), (2, 0.5)]);
    assert_eq!(
        arrangement.set_split_axis(split(1), SplitAxis::Horizontal, available, metrics()),
        Ok(false)
    );
    assert_eq!(
        arrangement.set_split_axis(split(9), SplitAxis::Vertical, available, metrics()),
        Err(ViewError::TargetMissing)
    );
}

#[test]
fn nonfinite_ratio_and_insufficient_space_leave_arrangement_unchanged() {
    let mut arrangement = arrangement("H(1,V(2,3))", 1);
    let before = arrangement.clone();

    for ratio in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(
            arrangement.resize_split(split(1), roomy(), metrics(), ratio),
            Err(ViewError::InvalidRatio)
        );
    }
    assert_eq!(
        arrangement.resize_split(split(1), size(20.0, 20.0), metrics(), 0.5),
        Err(ViewError::InsufficientSpace)
    );
    assert_eq!(
        arrangement.move_pane(
            pane(1),
            pane(2),
            PaneEdge::Bottom,
            size(10.0, 20.0),
            metrics()
        ),
        Err(ViewError::InsufficientSpace)
    );
    assert!(!arrangement.can_receive_pane(pane(2), PaneEdge::Bottom, size(10.0, 20.0), metrics()));
    assert!(arrangement.can_receive_pane(pane(2), PaneEdge::Bottom, size(10.0, 21.0), metrics()));
    assert_eq!(
        arrangement.set_split_axis(split(2), SplitAxis::Horizontal, size(20.0, 50.0), metrics()),
        Err(ViewError::InsufficientSpace)
    );
    let invalid = ArrangementMetrics {
        minimum_leaf: size(10.0, 10.0),
        divider: f32::NAN,
    };
    assert_eq!(
        arrangement.resize_split(split(1), roomy(), invalid, 0.5),
        Err(ViewError::InvalidGeometry)
    );
    assert_eq!(
        arrangement.minimum_size(invalid),
        Err(ViewError::InvalidGeometry)
    );
    assert_eq!(arrangement, before);

    for (width, height) in [
        (0.0, 1.0),
        (1.0, -1.0),
        (f32::NAN, 1.0),
        (1.0, f32::INFINITY),
    ] {
        assert_eq!(
            PaneSize::new(width, height),
            Err(ViewError::InvalidGeometry)
        );
    }
}

#[test]
fn cross_tab_and_cross_server_moves_are_rejected() {
    let mut arrangement = arrangement("H(1,2)", 1);
    let before = arrangement.clone();
    let other_tab_pane = pane(9);
    let foreign = PaneId::new(foreign_server(), 1).unwrap();

    for (moved, target, error) in [
        (other_tab_pane, pane(1), ViewError::TargetMissing),
        (pane(1), other_tab_pane, ViewError::TargetMissing),
        (foreign, pane(1), ViewError::WrongServer),
        (pane(1), foreign, ViewError::WrongServer),
    ] {
        assert_eq!(
            arrangement.move_pane(moved, target, PaneEdge::Left, roomy(), metrics()),
            Err(error)
        );
    }
    assert_eq!(arrangement.focus_pane(foreign), Err(ViewError::WrongServer));
    assert_eq!(
        arrangement.focus_pane(other_tab_pane),
        Err(ViewError::TargetMissing)
    );
    assert_eq!(arrangement, before);

    let foreign_layout = CreationLayout {
        root: 0,
        nodes: vec![
            CreationNode::Split {
                axis: SplitAxis::Horizontal,
                first: 1,
                second: 2,
            },
            CreationNode::Pane(pane(1)),
            CreationNode::Pane(foreign),
        ],
    };
    assert_eq!(
        PaneArrangement::from_creation(tab(1), &foreign_layout, pane(1)),
        Err(ViewError::WrongServer)
    );
    assert_eq!(
        arrangement.reconcile(&foreign_layout, &[], by_local),
        Err(ViewError::WrongServer)
    );
    assert_eq!(
        PaneArrangement::import(TabId::new(foreign_server(), 1).unwrap(), &before.export()),
        Err(ViewError::InvalidPersistedState)
    );
    assert_eq!(arrangement, before);
}

#[test]
fn split_id_exhaustion_rejects_before_mutation() {
    let mut record = arrangement("H(1,2)", 1).export();
    record.split_high_water = u64::MAX;
    edit_split(&mut record, 0, |id, _, _, _| *id = split(u64::MAX));
    let mut arrangement = PaneArrangement::import(tab(1), &record).unwrap();
    let before = arrangement.clone();
    let exhausted = ViewError::IdentityExhausted {
        counter: ViewCounter::SplitId,
    };

    assert_eq!(
        arrangement.move_pane(pane(1), pane(2), PaneEdge::Bottom, roomy(), metrics()),
        Err(exhausted)
    );
    let placement = PanePlacement {
        pane: pane(3),
        source: pane(2),
        edge: PaneEdge::Right,
        ordinal: CreationOrdinal::new(3),
    };
    assert_eq!(
        arrangement.reconcile(&creation("H(1,H(2,3))"), &[placement], by_local),
        Err(exhausted)
    );
    assert_eq!(arrangement, before);

    // Personal changes that allocate no Split remain available.
    assert_eq!(
        arrangement.resize_split(split(u64::MAX), roomy(), metrics(), 0.4),
        Ok(0.4)
    );
    assert!(arrangement.toggle_zoom());
}

#[test]
fn layout_revision_exhaustion_rejects_geometry_changes_before_mutation() {
    let mut arrangement = arrangement("H(1,V(2,3))", 1);
    assert!(arrangement.toggle_zoom());
    arrangement.focus_pane(pane(2)).unwrap();
    arrangement.focus_pane(pane(1)).unwrap();
    arrangement.set_layout_revision_for_test(LayoutRevision::new(u64::MAX));
    let before = arrangement.clone();
    let exhausted = ViewError::IdentityExhausted {
        counter: ViewCounter::LayoutRevision,
    };

    assert!(!arrangement.toggle_zoom());
    assert_eq!(arrangement.focus_pane(pane(2)), Err(exhausted));
    assert_eq!(arrangement.focus_next(), None);
    assert_eq!(
        arrangement.resize_split(split(1), roomy(), metrics(), 0.3),
        Err(exhausted)
    );
    assert_eq!(
        arrangement.move_pane(pane(1), pane(3), PaneEdge::Right, roomy(), metrics()),
        Err(exhausted)
    );
    assert_eq!(
        arrangement.reconcile(&creation("H(1,2)"), &[], by_local),
        Err(exhausted)
    );
    assert_eq!(arrangement, before);
}

#[test]
fn layout_revision_tracks_geometry_changes_only() {
    let mut arrangement = arrangement("H(1,V(2,3))", 1);
    let initial = arrangement.layout_revision();

    assert_eq!(arrangement.focus_pane(pane(2)), Ok(true));
    assert_eq!(
        arrangement.resize_split(split(1), roomy(), metrics(), 0.5),
        Ok(0.5)
    );
    assert_eq!(
        arrangement.set_split_axis(split(2), SplitAxis::Vertical, roomy(), metrics()),
        Ok(false)
    );
    assert_eq!(arrangement.layout_revision(), initial);

    let mut expected = initial;
    let mut advanced = |arrangement: &PaneArrangement| {
        expected = expected.checked_successor().unwrap();
        assert_eq!(arrangement.layout_revision(), expected);
    };
    arrangement
        .resize_split(split(1), roomy(), metrics(), 0.4)
        .unwrap();
    advanced(&arrangement);
    assert!(arrangement.toggle_zoom());
    advanced(&arrangement);
    arrangement.focus_pane(pane(3)).unwrap();
    advanced(&arrangement);
    arrangement
        .set_split_axis(split(2), SplitAxis::Horizontal, roomy(), metrics())
        .unwrap();
    advanced(&arrangement);
}

#[test]
fn deeply_nested_maximum_layout_uses_bounded_iterative_traversal() {
    let maximum = u64::try_from(MAX_TAB_PANES).unwrap();
    // A recursive walk of this chain would need several times this stack.
    let worker = std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(move || {
            let layout = chain(maximum);
            let mut arrangement = PaneArrangement::from_creation(tab(1), &layout, pane(1)).unwrap();
            assert_eq!(arrangement.pane_count(), MAX_TAB_PANES);
            assert_eq!(arrangement.pane_ids().len(), MAX_TAB_PANES);
            let tiny = ArrangementMetrics {
                minimum_leaf: size(0.001, 0.001),
                divider: 0.0,
            };
            assert!(arrangement.minimum_size(tiny).is_ok());
            assert_eq!(
                arrangement.focus_in_direction(FocusDirection::Right),
                Ok(Some(pane(2)))
            );
            assert_eq!(
                arrangement.move_pane(pane(maximum), pane(1), PaneEdge::Left, roomy(), tiny),
                Ok(true)
            );
            let record = arrangement.export();
            let restored = PaneArrangement::import(tab(1), &record).unwrap();
            assert_eq!(restored.export(), record);

            let change = arrangement
                .prune(|pane| pane.local() % 2 == 0)
                .unwrap()
                .unwrap();
            assert_eq!(change.removed.len(), MAX_TAB_PANES / 2);
            assert_eq!(arrangement.pane_count(), MAX_TAB_PANES / 2);
            let change = arrangement.reconcile(&layout, &[], by_local).unwrap();
            assert_eq!(change.inserted.len(), MAX_TAB_PANES / 2);
            assert_eq!(arrangement.pane_count(), MAX_TAB_PANES);
            drop(arrangement);
            drop(restored);

            // Membership past the client limit is a typed resource limit, not truncation.
            let limit = ViewError::ResourceLimit {
                resource: ViewResource::TabPanes,
            };
            assert_eq!(
                PaneArrangement::from_creation(tab(1), &chain(maximum + 1), pane(1)),
                Err(limit)
            );
            let mut small = self::arrangement("H(1,2)", 1);
            let before = small.clone();
            assert_eq!(
                small.reconcile(&chain(maximum + 1), &[], by_local),
                Err(limit)
            );
            assert_eq!(small, before);
        })
        .unwrap();
    worker.join().unwrap();
}

#[test]
fn resync_places_missing_subtree_beside_nearest_surviving_creation_relative() {
    // The personal arrangement swapped the Server's H(1,2). While this client was
    // away, Panes 3 and 4 were created below them.
    let mut arrangement = arrangement("H(2,1)", 1);
    let resynchronized = creation("V(H(1,2),H(3,4))");

    let change = arrangement
        .reconcile(&resynchronized, &[], by_local)
        .unwrap();

    assert_eq!(shape(&arrangement), "H(2,V(1,H(3,4)))");
    assert_eq!(change.inserted, panes([3, 4]));
    assert_eq!(splits(&arrangement), [(1, 0.5), (2, 0.5), (3, 0.5)]);
    assert_eq!(arrangement.focused_pane(), Some(pane(1)));

    // A placement whose source no longer exists uses the same fallback.
    let mut stale = self::arrangement("H(2,1)", 1);
    let orphan = PanePlacement {
        pane: pane(3),
        source: pane(9),
        edge: PaneEdge::Right,
        ordinal: CreationOrdinal::new(3),
    };
    stale
        .reconcile(&resynchronized, &[orphan], by_local)
        .unwrap();
    assert_eq!(stale, arrangement);
}

#[test]
fn reconcile_without_survivors_rebuilds_from_the_default_with_fresh_split_ids() {
    let mut arrangement = arrangement("H(1,2)", 1);
    assert!(arrangement.toggle_zoom());

    let change = arrangement
        .reconcile(&creation("V(3,4)"), &[], by_local)
        .unwrap();

    assert_eq!(shape(&arrangement), "V(3,4)");
    assert_eq!(splits(&arrangement), [(2, 0.5)]);
    assert_eq!(change.removed, panes([1, 2]));
    assert_eq!(change.inserted, panes([3, 4]));
    assert!(change.focus_changed);
    assert!(change.zoom_cleared);
    assert_eq!(arrangement.focused_pane(), Some(pane(3)));
    assert_eq!(arrangement.focus_history(), [pane(3)]);
    assert_eq!(arrangement.zoom(), ZoomState::Restored);
}

#[test]
fn reconcile_rejects_invalid_membership_without_mutation() {
    let mut arrangement = arrangement("H(1,2)", 1);
    let before = arrangement.clone();

    assert_eq!(
        arrangement.reconcile(&creation("H(1,1)"), &[], by_local),
        Err(ViewError::InvalidCreationTopology)
    );
    let empty = CreationLayout {
        root: 0,
        nodes: vec![],
    };
    assert_eq!(
        arrangement.reconcile(&empty, &[], by_local),
        Err(ViewError::InvalidCreationTopology)
    );
    // Pruning every leaf reports that the caller must drop the arrangement.
    assert_eq!(arrangement.prune(|_| false), Ok(None));
    assert_eq!(arrangement, before);
}

#[test]
fn arrangement_export_import_round_trips_and_rejects_malformed_records() {
    let mut arrangement = arrangement("H(V(1,2),3)", 1);
    arrangement.focus_pane(pane(3)).unwrap();
    assert!(arrangement.toggle_zoom());
    arrangement
        .resize_split(split(2), roomy(), metrics(), 0.25)
        .unwrap();
    let record = arrangement.export();

    let restored = PaneArrangement::import(tab(1), &record).unwrap();
    assert_eq!(restored.export(), record);
    assert_eq!(restored.layout_revision(), LayoutRevision::INITIAL);
    assert_eq!(restored.zoom(), ZoomState::Zoomed(pane(3)));

    // A valid table in another order is normalized to visual pre-order.
    let reordered = ArrangementRecord {
        root: 2,
        nodes: vec![
            ArrangementNode::Pane(pane(2)),
            ArrangementNode::Pane(pane(1)),
            ArrangementNode::Split {
                id: split(5),
                axis: SplitAxis::Vertical,
                ratio: 0.5,
                first: 1,
                second: 0,
            },
        ],
        split_high_water: 5,
        focused: pane(2),
        focus_history: vec![pane(1), pane(2)],
        zoomed: None,
    };
    let normalized = PaneArrangement::import(tab(1), &reordered).unwrap();
    assert_eq!(shape(&normalized), "V(1,2)");
    assert_eq!(normalized.export().root, 0);

    // A history may omit Panes, including the Focused Pane.
    let mut partial = record.clone();
    partial.focus_history = vec![pane(1)];
    assert!(PaneArrangement::import(tab(1), &partial).is_ok());

    let malformed: [fn(&mut ArrangementRecord); 19] = [
        |record| record.root = 9,
        |record| record.nodes.clear(),
        |record| edit_split(record, 1, |_, _, first, _| *first = 0),
        |record| edit_split(record, 0, |_, _, _, second| *second = 1),
        |record| edit_split(record, 1, |_, _, _, second| *second = 2),
        |record| edit_split(record, 1, |_, _, _, second| *second = 9),
        |record| record.nodes.push(ArrangementNode::Pane(pane(7))),
        |record| record.nodes[3] = ArrangementNode::Pane(pane(1)),
        |record| {
            record.nodes[3] = ArrangementNode::Pane(PaneId::new(foreign_server(), 2).unwrap());
        },
        |record| edit_split(record, 1, |_, ratio, _, _| *ratio = f32::NAN),
        |record| edit_split(record, 1, |_, ratio, _, _| *ratio = f32::INFINITY),
        |record| edit_split(record, 1, |_, ratio, _, _| *ratio = 0.0),
        |record| edit_split(record, 1, |_, ratio, _, _| *ratio = 1.0),
        |record| edit_split(record, 1, |id, _, _, _| *id = split(1)),
        |record| record.split_high_water = 1,
        |record| record.focused = pane(9),
        |record| record.focus_history = vec![pane(1), pane(3), pane(1)],
        |record| record.focus_history = vec![pane(9), pane(3)],
        |record| record.zoomed = Some(pane(1)),
    ];
    for (case, mutate) in malformed.into_iter().enumerate() {
        let mut candidate = record.clone();
        mutate(&mut candidate);
        assert_eq!(
            PaneArrangement::import(tab(1), &candidate),
            Err(ViewError::InvalidPersistedState),
            "case {case}"
        );
    }

    let mut single = self::arrangement("1", 1).export();
    single.zoomed = Some(pane(1));
    assert_eq!(
        PaneArrangement::import(tab(1), &single),
        Err(ViewError::InvalidPersistedState)
    );
}

#[test]
fn tab_order_moves_only_within_its_worktree_slots() {
    let records = interleaved_tabs();
    let mut order = TabOrder::from_creation(workspace(), &records, tab(1)).unwrap();
    assert_eq!(order.order(), tabs([1, 2, 3, 4, 5]));
    assert_eq!(order.tabs_in(&records, None), tabs([1, 3, 5]));

    assert_eq!(order.move_tab(tab(5), 0, &records), Ok(true));
    assert_eq!(order.order(), tabs([5, 2, 1, 4, 3]));
    assert_eq!(
        order.step_tab(tab(4), TabStep::Previous, &records),
        Ok(true)
    );
    assert_eq!(order.order(), tabs([5, 4, 1, 2, 3]));
    // Steps stop at either end of the Worktree's sequence instead of wrapping.
    assert_eq!(
        order.step_tab(tab(4), TabStep::Previous, &records),
        Ok(false)
    );
    assert_eq!(order.step_tab(tab(3), TabStep::Next, &records), Ok(false));
    assert_eq!(order.move_tab(tab(1), 1, &records), Ok(false));

    let before = order.clone();
    assert_eq!(
        order.move_tab(tab(2), 2, &records),
        Err(ViewError::TargetMissing)
    );
    assert_eq!(
        order.move_tab(tab(9), 0, &records),
        Err(ViewError::TargetMissing)
    );
    assert_eq!(
        order.move_tab(TabId::new(foreign_server(), 1).unwrap(), 0, &records),
        Err(ViewError::WrongServer)
    );
    assert_eq!(order, before);
    // Personal reordering never changes activation.
    assert_eq!(order.active(), Some(tab(1)));
    assert_eq!(order.activation_history(), [tab(1)]);
}

#[test]
fn tab_order_from_creation_orders_one_workspace_and_rejects_bad_membership() {
    let mut records = interleaved_tabs();
    let mut other = tab_record(6, None);
    other.workspace = WorkspaceId::new(server(), 2).unwrap();
    other.creation_ordinal = CreationOrdinal::new(0);
    records.push(other);
    records.swap(0, 4);

    let order = TabOrder::from_creation(workspace(), &records, tab(3)).unwrap();
    assert_eq!(order.order(), tabs([1, 2, 3, 4, 5]));
    assert_eq!(order.active(), Some(tab(3)));
    assert_eq!(order.activation_history(), [tab(3)]);

    // A duplicate identity is rejected even when its ordinals are not adjacent.
    let mut duplicated = interleaved_tabs();
    let mut repeated = tab_record(1, None);
    repeated.creation_ordinal = CreationOrdinal::new(9);
    duplicated.push(repeated);
    assert_eq!(
        TabOrder::from_creation(workspace(), &duplicated, tab(1)),
        Err(ViewError::WrongMembership)
    );
    assert_eq!(
        TabOrder::from_creation(workspace(), &records, tab(6)),
        Err(ViewError::TargetMissing)
    );
    assert_eq!(
        TabOrder::from_creation(
            workspace(),
            &records,
            TabId::new(foreign_server(), 1).unwrap()
        ),
        Err(ViewError::WrongServer)
    );
}

#[test]
fn tab_order_appends_without_activation_and_neighbors_wrap_in_active_worktree() {
    let records = interleaved_tabs();
    let mut order = TabOrder::from_creation(workspace(), &records[..4], tab(1)).unwrap();

    assert_eq!(order.append(tab(5)), Ok(true));
    assert_eq!(order.append(tab(5)), Ok(false));
    assert_eq!(order.active(), Some(tab(1)));
    assert_eq!(order.activation_history(), [tab(1)]);

    assert_eq!(
        order.neighbor_of_active(TabStep::Previous, &records),
        Some(tab(5))
    );
    assert_eq!(
        order.neighbor_of_active(TabStep::Next, &records),
        Some(tab(3))
    );
    assert_eq!(order.activate(tab(4)), Ok(true));
    assert_eq!(order.activate(tab(4)), Ok(false));
    assert_eq!(
        order.neighbor_of_active(TabStep::Next, &records),
        Some(tab(2))
    );
    assert_eq!(order.most_recent_in(&records, None), Some(tab(1)));
    assert_eq!(order.activate(tab(9)), Err(ViewError::TargetMissing));

    // An empty order activates its first Tab as restoration, not selection.
    let mut empty = TabOrder::new(workspace());
    assert_eq!(empty.active(), None);
    assert_eq!(empty.append(tab(2)), Ok(true));
    assert_eq!(empty.active(), Some(tab(2)));
}

#[test]
fn removing_active_tab_prefers_its_old_worktree_sequence_then_history() {
    let records = interleaved_tabs();
    let mut order = TabOrder::from_creation(workspace(), &records, tab(1)).unwrap();
    order.activate(tab(5)).unwrap();
    order.activate(tab(3)).unwrap();

    // The next surviving Tab of the old Worktree comes first.
    let survivors = without(&records, &[3]);
    assert_eq!(
        order.retain(&survivors, Some(None)),
        TabOrderChange {
            removed: vec![tab(3)],
            active_changed: true,
        }
    );
    assert_eq!(order.active(), Some(tab(5)));

    // Then the preceding one.
    let survivors = without(&survivors, &[5]);
    order.retain(&survivors, Some(None));
    assert_eq!(order.active(), Some(tab(1)));
    assert_eq!(order.order(), tabs([1, 2, 4]));

    // Then surviving activation history.
    order.activate(tab(4)).unwrap();
    order.activate(tab(2)).unwrap();
    let worktree = WorktreeId::new(server(), 1).unwrap();
    let survivors = without(&survivors, &[2, 4]);
    order.retain(&survivors, Some(Some(worktree)));
    assert_eq!(order.active(), Some(tab(1)));
    assert_eq!(order.activation_history(), [tab(1)]);

    // Without the old Worktree or another Tab activated here, the first
    // remaining Tab in personal order takes over.
    let mut order = TabOrder::from_creation(workspace(), &records, tab(3)).unwrap();
    order.retain(&without(&records, &[3]), None);
    assert_eq!(order.active(), Some(tab(1)));
    assert_eq!(order.activation_history(), [tab(1)]);

    // Removing an inactive Tab changes no activation.
    let mut order = TabOrder::from_creation(workspace(), &records, tab(3)).unwrap();
    assert_eq!(
        order.retain(&without(&records, &[4]), None),
        TabOrderChange {
            removed: vec![tab(4)],
            active_changed: false,
        }
    );
    assert_eq!(order.active(), Some(tab(3)));
}

#[test]
fn active_tab_fallback_ignores_tabs_this_client_never_activated() {
    let records = interleaved_tabs();
    let mut order = TabOrder::from_creation(workspace(), &records, tab(1)).unwrap();
    assert_eq!(order.move_tab(tab(4), 0, &records), Ok(true));
    assert_eq!(order.order(), tabs([1, 4, 3, 2, 5]));

    // No Tab of the old home sequence survives, and Tab 1 was the only Tab
    // activated here, so the first remaining Tab in personal order wins.
    let change = order.retain(&without(&records, &[1, 3, 5]), Some(None));
    assert!(change.active_changed);
    assert_eq!(order.order(), tabs([4, 2]));
    assert_eq!(order.active(), Some(tab(4)));
}

#[test]
fn every_nonempty_worktree_has_a_most_recent_tab() {
    let worktree = |local| WorktreeId::new(server(), local).unwrap();
    let mut records = interleaved_tabs();
    let mut order = TabOrder::from_creation(workspace(), &records, tab(1)).unwrap();

    // A Worktree with no Tab activated here offers its first Tab in personal
    // order.
    assert_eq!(order.activation_history(), [tab(1)]);
    assert_eq!(
        order.most_recent_in(&records, Some(worktree(1))),
        Some(tab(2))
    );
    assert_eq!(order.move_tab(tab(4), 0, &records), Ok(true));
    assert_eq!(
        order.most_recent_in(&records, Some(worktree(1))),
        Some(tab(4))
    );
    // A Tab activated here outranks personal order.
    order.activate(tab(2)).unwrap();
    order.activate(tab(1)).unwrap();
    assert_eq!(
        order.most_recent_in(&records, Some(worktree(1))),
        Some(tab(2))
    );

    // A Tab observed later joins without activation.
    records.push(tab_record(6, Some(2)));
    assert_eq!(order.append(tab(6)), Ok(true));
    assert_eq!(order.active(), Some(tab(1)));
    assert_eq!(order.activation_history(), tabs([2, 1]));
    assert_eq!(
        order.most_recent_in(&records, Some(worktree(2))),
        Some(tab(6))
    );
    assert_eq!(order.most_recent_in(&records, Some(worktree(3))), None);
}

#[test]
fn tab_order_import_rejects_inconsistent_records() {
    let records = interleaved_tabs();
    let mut order = TabOrder::from_creation(workspace(), &records, tab(1)).unwrap();
    order.activate(tab(4)).unwrap();
    let record = order.export();
    assert_eq!(TabOrder::import(workspace(), &record), Ok(order));
    assert_eq!(
        TabOrder::import(workspace(), &TabOrder::new(workspace()).export()),
        Ok(TabOrder::new(workspace()))
    );
    // A history may omit Tabs never activated here.
    let mut partial = record.clone();
    partial.activation_history = vec![tab(4)];
    assert!(TabOrder::import(workspace(), &partial).is_ok());

    let malformed: [fn(&mut TabOrderRecord); 8] = [
        |record| record.order.push(tab(1)),
        |record| record.order.push(TabId::new(foreign_server(), 9).unwrap()),
        |record| record.activation_history.push(tab(9)),
        |record| record.activation_history = vec![tab(4), tab(1), tab(4)],
        |record| record.activation_history = vec![tab(4), tab(1)],
        |record| record.activation_history.clear(),
        |record| record.active = None,
        |record| record.active = Some(tab(9)),
    ];
    for (case, mutate) in malformed.into_iter().enumerate() {
        let mut candidate = record.clone();
        mutate(&mut candidate);
        assert_eq!(
            TabOrder::import(workspace(), &candidate),
            Err(ViewError::InvalidPersistedState),
            "case {case}"
        );
    }

    let oversized = TabOrderRecord {
        order: (1..=u64::try_from(MAX_VIEW_TABS).unwrap() + 1)
            .map(tab)
            .collect(),
        active: Some(tab(1)),
        activation_history: vec![tab(1)],
    };
    assert_eq!(
        TabOrder::import(workspace(), &oversized),
        Err(ViewError::ResourceLimit {
            resource: ViewResource::Tabs,
        })
    );
}

#[test]
fn local_counters_reject_zero_and_exhaustion() {
    assert_eq!(SplitId::new(0), None);
    assert_eq!(SplitId::FIRST.get(), 1);
    assert_eq!(split(u64::MAX).checked_successor(), None);
    assert_eq!(LayoutRevision::new(u64::MAX).checked_successor(), None);
    assert_eq!(
        LayoutRevision::INITIAL.checked_successor(),
        Some(LayoutRevision::new(1))
    );
}
