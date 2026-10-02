// Licensed under the Apache License, Version 2.0 or the MIT license.

use crate::tests::nid;
use accesskit::{
    Affine, Node, NodeId, Rect, Role, TextDirection, TextPosition, TextSelection, Tree, TreeId,
    TreeUpdate,
};
use alloc::vec;

const CARET: Rect = Rect {
    x0: 50.0,
    y0: 20.0,
    x1: 50.0,
    y1: 40.0,
};
const TEXT: Rect = Rect {
    x0: 10.0,
    y0: 20.0,
    x1: 20.0,
    y1: 40.0,
};

fn tree(
    caret: Option<Rect>,
    transform: Option<Affine>,
    character_geometry: bool,
    selection_anchor: Option<usize>,
) -> crate::Tree {
    let mut window = Node::new(Role::Window);
    window.set_children([NodeId(1)]);
    window.set_transform(Affine::scale(2.0));
    let mut terminal = Node::new(Role::Terminal);
    terminal.set_children([NodeId(2)]);
    if let Some(transform) = transform {
        terminal.set_transform(transform);
    }
    if let Some(caret) = caret {
        terminal.set_text_caret_bounds(caret);
    }
    if let Some(character_index) = selection_anchor {
        let focus = TextPosition {
            node: NodeId(2),
            character_index: 1,
        };
        terminal.set_text_selection(TextSelection {
            anchor: TextPosition {
                node: NodeId(2),
                character_index,
            },
            focus,
        });
    }
    let mut run = Node::new(Role::TextRun);
    run.set_value("x");
    run.set_character_lengths([1]);
    if character_geometry {
        run.set_bounds(TEXT);
        run.set_text_direction(TextDirection::LeftToRight);
        run.set_character_positions([0.0]);
        run.set_character_widths([10.0]);
    }
    crate::Tree::new(
        TreeUpdate {
            nodes: vec![(NodeId(0), window), (NodeId(1), terminal), (NodeId(2), run)],
            tree: Some(Tree::new(NodeId(0))),
            tree_id: TreeId::ROOT,
            focus: NodeId(1),
        },
        true,
    )
}

#[test]
fn explicit_caret_bounds_preserve_text_and_character_geometry() {
    let tree = tree(Some(CARET), None, true, Some(1));
    let terminal = tree.state().node_by_id(nid(NodeId(1))).unwrap();
    assert_eq!(terminal.document_range().text(), "x");
    assert_eq!(
        terminal.text_selection().unwrap().bounding_boxes(),
        vec![Affine::scale(2.0).transform_rect_bbox(CARET)]
    );
    assert_eq!(
        terminal
            .document_start()
            .to_degenerate_range()
            .bounding_boxes(),
        vec![Rect {
            x0: 20.0,
            y0: 40.0,
            x1: 20.0,
            y1: 80.0
        }]
    );
    assert_eq!(
        terminal.document_range().bounding_boxes(),
        vec![Affine::scale(2.0).transform_rect_bbox(TEXT)]
    );
}

#[test]
fn explicit_caret_bounds_apply_ancestor_and_text_owner_transforms() {
    let transform = Affine::translate((3.0, 7.0));
    let tree = tree(Some(CARET), Some(transform), true, Some(1));
    let terminal = tree.state().node_by_id(nid(NodeId(1))).unwrap();
    assert_eq!(
        terminal.text_selection().unwrap().bounding_boxes(),
        vec![(Affine::scale(2.0) * transform).transform_rect_bbox(CARET)]
    );
}

#[test]
fn unset_caret_bounds_keep_character_derived_endpoint() {
    let tree = tree(None, None, true, Some(1));
    let terminal = tree.state().node_by_id(nid(NodeId(1))).unwrap();
    assert_eq!(
        terminal.text_selection().unwrap().bounding_boxes(),
        vec![Rect {
            x0: 40.0,
            y0: 40.0,
            x1: 40.0,
            y1: 80.0
        }]
    );
}

#[test]
fn explicit_caret_bounds_work_without_character_geometry() {
    let tree = tree(Some(CARET), None, false, Some(1));
    let terminal = tree.state().node_by_id(nid(NodeId(1))).unwrap();
    assert_eq!(
        terminal.text_selection().unwrap().bounding_boxes(),
        vec![Affine::scale(2.0).transform_rect_bbox(CARET)]
    );
    assert!(
        terminal
            .document_start()
            .to_degenerate_range()
            .bounding_boxes()
            .is_empty()
    );
    assert!(terminal.document_range().bounding_boxes().is_empty());
}

#[test]
fn explicit_caret_bounds_require_a_selection_focus() {
    let tree = tree(Some(CARET), None, true, None);
    let terminal = tree.state().node_by_id(nid(NodeId(1))).unwrap();
    assert_eq!(
        terminal
            .document_end()
            .to_degenerate_range()
            .bounding_boxes(),
        vec![Rect {
            x0: 40.0,
            y0: 40.0,
            x1: 40.0,
            y1: 80.0
        }]
    );
}

#[test]
fn nondegenerate_selection_keeps_character_bounds_and_focus_geometry() {
    let tree = tree(Some(CARET), None, true, Some(0));
    let terminal = tree.state().node_by_id(nid(NodeId(1))).unwrap();
    assert_eq!(terminal.text_selection().unwrap().text(), "x");
    assert_eq!(
        terminal.text_selection().unwrap().bounding_boxes(),
        vec![Affine::scale(2.0).transform_rect_bbox(TEXT)]
    );
    assert_eq!(
        terminal
            .text_selection_focus()
            .unwrap()
            .to_degenerate_range()
            .bounding_boxes(),
        vec![Affine::scale(2.0).transform_rect_bbox(CARET)]
    );
}
