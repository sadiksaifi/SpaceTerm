// Licensed under the Apache License, Version 2.0 or the MIT license.

use crate::{Adapter, AdapterCallback, AppContext, Event, InterfaceSet, ObjectEvent, WindowBounds};
use accesskit::{
    ActionHandler, ActionRequest, Node, NodeId, Rect, Role, TextPosition, TextSelection, Tree,
    TreeId, TreeUpdate,
};
use std::sync::{Arc, Mutex};

const CARET: Rect = Rect {
    x0: 50.0,
    y0: 20.0,
    x1: 50.0,
    y1: 40.0,
};
const MOVED_CARET: Rect = Rect {
    x0: 80.0,
    y0: 20.0,
    x1: 80.0,
    y1: 40.0,
};

#[derive(Debug, PartialEq, Eq)]
enum TextEvent {
    Caret(i32),
    Selection,
    Insert,
    Remove,
}

struct Capture(Arc<Mutex<Vec<TextEvent>>>);
impl AdapterCallback for Capture {
    fn register_interfaces(&self, _: &Adapter, _: crate::NodeId, _: InterfaceSet) {}
    fn unregister_interfaces(&self, _: &Adapter, _: crate::NodeId, _: InterfaceSet) {}
    fn emit_event(&self, _: &Adapter, event: Event) {
        if let Event::Object { event, .. } = event {
            let event = match event {
                ObjectEvent::CaretMoved(offset) => TextEvent::Caret(offset),
                ObjectEvent::TextSelectionChanged => TextEvent::Selection,
                ObjectEvent::TextInserted { .. } => TextEvent::Insert,
                ObjectEvent::TextRemoved { .. } => TextEvent::Remove,
                _ => return,
            };
            self.0.lock().unwrap().push(event);
        }
    }
}

struct IgnoreActions;
impl ActionHandler for IgnoreActions {
    fn do_action(&mut self, _: ActionRequest) {}
}

fn tree(
    caret: Option<Rect>,
    selection: (usize, usize),
    focused: bool,
    hidden: bool,
    initial: bool,
) -> TreeUpdate {
    let mut window = Node::new(Role::Window);
    window.set_children([NodeId(1)]);
    let mut terminal = Node::new(Role::Terminal);
    terminal.set_children([NodeId(10)]);
    if let Some(bounds) = caret {
        terminal.set_text_caret_bounds(bounds);
    }
    terminal.set_text_selection(TextSelection {
        anchor: TextPosition {
            node: NodeId(10),
            character_index: selection.0,
        },
        focus: TextPosition {
            node: NodeId(10),
            character_index: selection.1,
        },
    });
    if hidden {
        terminal.set_hidden();
    }
    let mut run = Node::new(Role::TextRun);
    run.set_value("test");
    run.set_character_lengths([1, 1, 1, 1]);
    TreeUpdate {
        nodes: vec![
            (NodeId(0), window),
            (NodeId(1), terminal),
            (NodeId(10), run),
        ],
        tree: initial.then(|| Tree::new(NodeId(0))),
        tree_id: TreeId::ROOT,
        focus: if focused { NodeId(1) } else { NodeId(0) },
    }
}

fn events(old: TreeUpdate, new: TreeUpdate, window_focused: bool) -> Vec<TextEvent> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let app_context = AppContext::new(None);
    let mut adapter = Adapter::new(
        &app_context,
        Capture(events.clone()),
        old,
        window_focused,
        WindowBounds::default(),
        IgnoreActions,
    );
    events.lock().unwrap().clear();
    adapter.update(new);
    std::mem::take(&mut *events.lock().unwrap())
}

fn document_tree(prefix: Option<&str>, focused: bool, hidden: bool, initial: bool) -> TreeUpdate {
    let mut update = tree(Some(CARET), (4, 4), focused, hidden, initial);
    if let Some(prefix) = prefix {
        update.nodes[1].1.set_children([NodeId(11), NodeId(10)]);
        let mut run = Node::new(Role::TextRun);
        run.set_value(prefix);
        run.set_character_lengths(
            prefix
                .chars()
                .map(|c| c.len_utf8() as u8)
                .collect::<Vec<_>>(),
        );
        update.nodes.push((NodeId(11), run));
    }
    update
}

#[test]
fn trimming_preceding_row_emits_resolved_caret_offset_without_selection_event() {
    assert_eq!(
        events(
            document_tree(Some("removed\n"), true, false, true),
            document_tree(None, true, false, false),
            true
        ),
        vec![TextEvent::Remove, TextEvent::Caret(4)],
    );
}

#[test]
fn earlier_run_content_changes_emit_resolved_scalar_caret_offset() {
    // Parent data, the retained focus position and explicit bounds are identical.
    // Only the earlier TextRun is updated in the second tree.
    assert_eq!(
        events(
            document_tree(Some("old\n"), true, false, true),
            document_tree(Some("long😀prefix\n"), true, false, false),
            true
        ),
        vec![TextEvent::Remove, TextEvent::Insert, TextEvent::Caret(16)],
    );
}

#[test]
fn run_and_parent_changes_emit_only_one_caret_event() {
    let old = document_tree(Some("old\n"), true, false, true);
    let mut new = document_tree(Some("long😀prefix\n"), true, false, false);
    new.nodes[1].1.set_text_caret_bounds(MOVED_CARET);
    assert_eq!(
        events(old, new, true),
        vec![TextEvent::Remove, TextEvent::Insert, TextEvent::Caret(16)]
    );
}

#[test]
fn gaining_focus_after_geometry_update_announces_selection_and_caret_once() {
    assert_eq!(
        events(
            tree(Some(CARET), (1, 2), false, false, true),
            tree(Some(MOVED_CARET), (1, 2), true, false, false),
            true
        ),
        vec![TextEvent::Selection, TextEvent::Caret(2)],
    );
}

#[test]
fn hidden_and_unfocused_offset_changes_emit_no_caret_or_selection_events() {
    for (focused, hidden, window_focused) in [
        (true, true, true),
        (false, false, true),
        (true, false, false),
    ] {
        let result = events(
            document_tree(Some("removed\n"), focused, hidden, true),
            document_tree(None, focused, hidden, false),
            window_focused,
        );
        assert!(
            result
                .iter()
                .all(|event| matches!(event, TextEvent::Insert | TextEvent::Remove)),
            "focused={focused}, hidden={hidden}, window_focused={window_focused}: {result:?}"
        );
    }
}

#[test]
fn caret_geometry_change_at_fixed_offset_emits_only_caret_event() {
    for selection in [(2, 2), (1, 2)] {
        assert_eq!(
            events(
                tree(Some(CARET), selection, true, false, true),
                tree(Some(MOVED_CARET), selection, true, false, false),
                true
            ),
            vec![TextEvent::Caret(2)],
        );
    }
}

#[test]
fn unchanged_caret_geometry_and_selection_emit_no_text_events() {
    assert!(
        events(
            tree(Some(CARET), (2, 2), true, false, true),
            tree(Some(CARET), (2, 2), true, false, false),
            true
        )
        .is_empty()
    );
}

#[test]
fn hidden_or_unfocused_caret_geometry_changes_emit_no_text_events() {
    for (focused, hidden, window_focused) in [
        (true, true, true),
        (false, false, true),
        (true, false, false),
    ] {
        let result = events(
            tree(Some(CARET), (2, 2), focused, hidden, true),
            tree(Some(MOVED_CARET), (2, 2), focused, hidden, false),
            window_focused,
        );
        assert!(
            result.is_empty(),
            "focused={focused}, hidden={hidden}, window_focused={window_focused}: {result:?}"
        );
    }
}

#[test]
fn losing_focus_does_not_announce_geometry_at_the_same_offset() {
    assert!(
        events(
            tree(Some(CARET), (2, 2), true, false, true),
            tree(Some(MOVED_CARET), (2, 2), false, false, false),
            true
        )
        .is_empty()
    );
}

#[test]
fn raw_selection_changes_keep_existing_caret_and_selection_events() {
    assert_eq!(
        events(
            tree(Some(CARET), (2, 2), true, false, true),
            tree(Some(CARET), (3, 3), true, false, false),
            true
        ),
        vec![TextEvent::Caret(3)]
    );
    assert_eq!(
        events(
            tree(Some(CARET), (2, 2), true, false, true),
            tree(Some(CARET), (1, 3), true, false, false),
            true
        ),
        vec![TextEvent::Selection, TextEvent::Caret(3)]
    );
    assert_eq!(
        events(
            tree(Some(CARET), (2, 2), true, false, true),
            tree(Some(CARET), (1, 2), true, false, false),
            true
        ),
        vec![TextEvent::Selection]
    );
}

#[test]
fn adding_or_removing_explicit_caret_geometry_emits_caret_event() {
    for (old, new) in [(None, Some(CARET)), (Some(CARET), None)] {
        assert_eq!(
            events(
                tree(old, (2, 2), true, false, true),
                tree(new, (2, 2), true, false, false),
                true
            ),
            vec![TextEvent::Caret(2)]
        );
    }
}

#[test]
fn degenerate_atspi_ranges_keep_caret_bias_at_scalar_run_boundaries() {
    use atspi_common::CoordType;
    for (head, lengths, offset) in [
        ("head", vec![1, 1, 1, 1], 4),
        ("A😀e\u{301}", vec![1, 4, 3], 4),
    ] {
        let mut update = tree(
            Some(CARET),
            (lengths.len(), lengths.len()),
            true,
            false,
            true,
        );
        update.nodes[1].1.set_children([NodeId(10), NodeId(11)]);
        let head_run = &mut update.nodes[2].1;
        head_run.set_value(head);
        head_run.set_character_lengths(lengths.clone());
        head_run.set_bounds(Rect::new(10.0, 20.0, 40.0, 40.0));
        head_run.set_text_direction(accesskit::TextDirection::LeftToRight);
        head_run.set_character_positions(
            (0..lengths.len())
                .map(|i| i as f32 * 10.0)
                .collect::<Vec<_>>(),
        );
        head_run.set_character_widths(vec![10.0; lengths.len()]);
        let mut tail = Node::new(Role::TextRun);
        tail.set_value("tail");
        tail.set_character_lengths([1, 1, 1, 1]);
        tail.set_bounds(Rect::new(10.0, 40.0, 50.0, 60.0));
        tail.set_text_direction(accesskit::TextDirection::LeftToRight);
        tail.set_character_positions([0.0, 10.0, 20.0, 30.0]);
        tail.set_character_widths([10.0; 4]);
        update.nodes.push((NodeId(11), tail));
        for explicit in [true, false] {
            let mut update = update.clone();
            if !explicit {
                update.nodes[1].1.clear_text_caret_bounds();
            }
            let app_context = AppContext::new(None);
            let adapter = Adapter::new(
                &app_context,
                Capture(Arc::new(Mutex::new(Vec::new()))),
                update,
                true,
                WindowBounds::default(),
                IgnoreActions,
            );
            let terminal = adapter.platform_node(
                adapter
                    .platform_node(adapter.root_id())
                    .child_at_index(0)
                    .unwrap()
                    .unwrap(),
            );
            assert_eq!(terminal.caret_offset().unwrap(), offset);
            let bounds = terminal
                .range_extents(offset, offset, CoordType::Window)
                .unwrap();
            assert_eq!(
                (bounds.x, bounds.y, bounds.width, bounds.height),
                if explicit {
                    (50, 20, 0, 20)
                } else {
                    (10, 40, 0, 20)
                },
                "{head}, explicit={explicit}"
            );
            // A different scalar offset keeps ordinary geometry, including inside the
            // Unicode head whose byte length differs from its scalar count.
            let other = terminal.range_extents(1, 1, CoordType::Window).unwrap();
            assert_eq!(
                (other.x, other.y, other.width, other.height),
                (20, 20, 0, 20)
            );
            let next = terminal
                .range_extents(offset, offset + 1, CoordType::Window)
                .unwrap();
            assert_eq!((next.x, next.y, next.width, next.height), (10, 40, 10, 20));
        }
    }
}
