use super::*;
use spaceterm_server::client_view::{Layout, LayoutNode, TabView, WorkspaceView};

#[gpui::test]
fn a_returning_desktop_restores_its_arrangement_without_changing_a_new_desktops_default(
    app: &mut TestAppContext,
) {
    let published = inventory(
        2,
        vec![
            tab(ROOT_TAB, vec![pane(ROOT_PANE, None, Axis::Right)]),
            tab(
                SECOND_TAB,
                vec![
                    pane(SECOND_TAB_PANE, None, Axis::Right),
                    pane(SPLIT_PANE, Some(SECOND_TAB_PANE), Axis::Down),
                ],
            ),
        ],
    );
    let personal = WorkspaceView {
        workspace: WORKSPACE,
        tabs: vec![
            TabView {
                tab: SECOND_TAB,
                layout: Layout {
                    nodes: vec![
                        LayoutNode::Split {
                            axis: Axis::Right,
                            ratio: 0.72,
                            first: 1,
                            second: 2,
                        },
                        LayoutNode::Pane(SPLIT_PANE),
                        LayoutNode::Pane(SECOND_TAB_PANE),
                    ],
                },
            },
            TabView {
                tab: ROOT_TAB,
                layout: Layout {
                    nodes: vec![LayoutNode::Pane(ROOT_PANE)],
                },
            },
        ],
    };
    let retained = {
        let (first, cx) = shared_tab_manager(app);
        publish(&first, published.clone(), cx);
        let active = first
            .manager
            .read_with(cx, |manager, cx| manager.active_terminal_identity(cx));
        cx.update(|_, cx| {
            first.manager.update(cx, |manager, cx| {
                manager.restore_shared_arrangement(&personal, &published.workspaces[0], cx)
            })
        });
        let saved = first
            .manager
            .read_with(cx, |manager, cx| manager.shared_arrangement(cx).unwrap());
        assert_eq!(saved, personal);
        assert_eq!(
            first
                .manager
                .read_with(cx, |manager, cx| manager.active_terminal_identity(cx)),
            active
        );
        assert!(
            first.edits.submitted().is_empty(),
            "personal arrangement never mutates server organization"
        );
        serde_json::to_vec(&saved).unwrap()
    };
    let (second, cx) = shared_tab_manager(app);
    publish(&second, published.clone(), cx);
    let default = second
        .manager
        .read_with(cx, |manager, cx| manager.shared_arrangement(cx).unwrap());
    assert_eq!(
        default.tabs.iter().map(|tab| tab.tab).collect::<Vec<_>>(),
        vec![ROOT_TAB, SECOND_TAB]
    );
    assert_eq!(
        default.tabs[1].layout.nodes,
        vec![
            LayoutNode::Split {
                axis: Axis::Down,
                ratio: 0.5,
                first: 1,
                second: 2
            },
            LayoutNode::Pane(SECOND_TAB_PANE),
            LayoutNode::Pane(SPLIT_PANE)
        ]
    );
    // Recreate a presentation from retained bytes, after deletion and creation during its absence.
    let saved: WorkspaceView = serde_json::from_slice(&retained).unwrap();
    let mut current = published;
    current.revision += 1;
    current.workspaces[0].tabs[1]
        .panes
        .retain(|pane| pane.id != SPLIT_PANE);
    current.workspaces[0].tabs[1].panes.push(pane(
        REPLACEMENT_PANE,
        Some(SECOND_TAB_PANE),
        Axis::Right,
    ));
    publish(&second, current.clone(), cx);
    cx.update(|_, cx| {
        second.manager.update(cx, |manager, cx| {
            manager.restore_shared_arrangement(&saved, &current.workspaces[0], cx)
        })
    });
    let restored = second
        .manager
        .read_with(cx, |manager, cx| manager.shared_arrangement(cx).unwrap());
    assert_eq!(
        restored.tabs.iter().map(|tab| tab.tab).collect::<Vec<_>>(),
        vec![SECOND_TAB, ROOT_TAB]
    );
    assert_eq!(
        restored.tabs[0].layout.nodes,
        vec![
            LayoutNode::Split {
                axis: Axis::Right,
                ratio: 0.5,
                first: 1,
                second: 2
            },
            LayoutNode::Pane(SECOND_TAB_PANE),
            LayoutNode::Pane(REPLACEMENT_PANE)
        ]
    );
    assert!(second.edits.submitted().is_empty());
}
