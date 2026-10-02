// Licensed under the Apache License, Version 2.0 or the MIT license.

use crate::{Adapter, AdapterCallback, AppContext, Event, InterfaceSet, ObjectEvent, WindowBounds};
use accesskit::{ActionHandler, ActionRequest, Node, NodeId, Role, Tree, TreeId, TreeUpdate};
use std::sync::{Arc, Mutex};

#[derive(Debug, PartialEq, Eq)]
enum Edit {
    Remove(usize, String),
    Insert(usize, String),
}

struct Capture(Arc<Mutex<Vec<Edit>>>);

impl AdapterCallback for Capture {
    fn register_interfaces(&self, _: &Adapter, _: crate::NodeId, _: InterfaceSet) {}
    fn unregister_interfaces(&self, _: &Adapter, _: crate::NodeId, _: InterfaceSet) {}
    fn emit_event(&self, _: &Adapter, event: Event) {
        if let Event::Object { event, .. } = event {
            let edit = match event {
                ObjectEvent::TextRemoved {
                    start_index,
                    length,
                    content,
                } => {
                    assert_eq!(length as usize, content.chars().count());
                    Edit::Remove(usize::try_from(start_index).unwrap(), content)
                }
                ObjectEvent::TextInserted {
                    start_index,
                    length,
                    content,
                } => {
                    assert_eq!(length as usize, content.chars().count());
                    Edit::Insert(usize::try_from(start_index).unwrap(), content)
                }
                _ => return,
            };
            self.0.lock().unwrap().push(edit);
        }
    }
}

struct IgnoreActions;
impl ActionHandler for IgnoreActions {
    fn do_action(&mut self, _: ActionRequest) {}
}

fn tree(rows: &[(u64, &str)], initial: bool, nested: bool) -> TreeUpdate {
    let mut window = Node::new(Role::Window);
    window.set_children(vec![NodeId(1)]);
    let mut terminal = Node::new(Role::Terminal);
    terminal.set_children(if nested {
        vec![NodeId(2)]
    } else {
        rows.iter().map(|(id, _)| NodeId(*id)).collect()
    });
    let mut nodes = vec![(NodeId(0), window), (NodeId(1), terminal)];
    if nested {
        let mut container = Node::new(Role::GenericContainer);
        container.set_children(rows.iter().map(|(id, _)| NodeId(*id)).collect::<Vec<_>>());
        nodes.push((NodeId(2), container));
    }
    nodes.extend(rows.iter().map(|(id, text)| {
        let mut run = Node::new(Role::TextRun);
        run.set_value(*text);
        run.set_character_lengths(text.chars().map(|c| c.len_utf8() as u8).collect::<Vec<_>>());
        (NodeId(*id), run)
    }));
    TreeUpdate {
        nodes,
        tree: initial.then(|| Tree::new(NodeId(0))),
        tree_id: TreeId::ROOT,
        focus: NodeId(1),
    }
}

fn edits(old: &[(u64, &str)], new: &[(u64, &str)], nested: bool) -> Vec<Edit> {
    let capture = Arc::new(Mutex::new(Vec::new()));
    let app_context = AppContext::new(None);
    let mut adapter = Adapter::new(
        &app_context,
        Capture(capture.clone()),
        tree(old, true, nested),
        true,
        WindowBounds::default(),
        IgnoreActions,
    );
    capture.lock().unwrap().clear();
    adapter.update(tree(new, false, nested));
    let result = std::mem::take(&mut *capture.lock().unwrap());
    let mut reconstructed: Vec<char> = old.iter().flat_map(|(_, text)| text.chars()).collect();
    for edit in &result {
        match edit {
            Edit::Remove(offset, text) => {
                let end = offset + text.chars().count();
                assert_eq!(
                    reconstructed[*offset..end].iter().collect::<String>(),
                    *text
                );
                reconstructed.drain(*offset..end);
            }
            Edit::Insert(offset, text) => {
                reconstructed.splice(*offset..*offset, text.chars());
            }
        }
    }
    assert_eq!(
        reconstructed.iter().collect::<String>(),
        new.iter().map(|(_, text)| *text).collect::<String>()
    );
    // The platform Text interface continues exposing the complete new document.
    let terminal_id = adapter
        .platform_node(adapter.root_id())
        .child_at_index(0)
        .unwrap()
        .unwrap();
    let node = adapter.platform_node(terminal_id);
    assert_eq!(
        node.text(0, -1).unwrap(),
        new.iter().map(|(_, text)| *text).collect::<String>()
    );
    result
}

#[test]
fn scrollback_trimming_preserves_surviving_rows() {
    assert_eq!(
        edits(
            &[(10, "first\n"), (11, "second\n"), (12, "third\n")],
            &[(11, "second\n"), (12, "third\n"), (13, "fourth\n")],
            false
        ),
        vec![
            Edit::Remove(0, "first\n".into()),
            Edit::Insert(13, "fourth\n".into())
        ]
    );
}

#[test]
fn alternate_screen_scroll_uses_scalar_offsets_through_nested_runs() {
    assert_eq!(
        edits(
            &[(10, "😀\n"), (11, "A\u{301}\n"), (12, "中\n")],
            &[(11, "A\u{301}\n"), (12, "中\n"), (13, "🦀\n")],
            true
        ),
        vec![
            Edit::Remove(0, "😀\n".into()),
            Edit::Insert(5, "🦀\n".into())
        ]
    );
}

#[test]
fn simultaneous_trim_and_row_edit_preserve_unmodified_middle() {
    assert_eq!(
        edits(
            &[(10, "old\n"), (11, "stable\n"), (12, "prompt> ")],
            &[(11, "stable\n"), (12, "prompt> echo")],
            false
        ),
        vec![
            Edit::Remove(0, "old\n".into()),
            Edit::Insert(15, "echo".into())
        ]
    );
}

#[test]
fn separate_row_edits_have_progressive_offsets() {
    assert_eq!(
        edits(
            &[(10, "one\n"), (11, "middle\n"), (12, "three")],
            &[(10, "ONE-LONG\n"), (11, "middle\n"), (12, "THREE")],
            false
        ),
        vec![
            Edit::Remove(0, "one".into()),
            Edit::Insert(0, "ONE-LONG".into()),
            Edit::Remove(16, "three".into()),
            Edit::Insert(16, "THREE".into())
        ]
    );
}

#[test]
fn reverse_scroll_inserts_above_surviving_rows_and_removes_tail() {
    assert_eq!(
        edits(
            &[(11, "B\n"), (12, "C\n"), (13, "D\n")],
            &[(10, "A\n"), (11, "B\n"), (12, "C\n")],
            false
        ),
        vec![Edit::Insert(0, "A\n".into()), Edit::Remove(6, "D\n".into())]
    );
}

#[test]
fn unchanged_text_with_replaced_node_ids_emits_nothing() {
    assert!(
        edits(
            &[(10, "same\n"), (11, "text")],
            &[(20, "same\n"), (21, "text")],
            false
        )
        .is_empty()
    );
}

#[test]
fn append_and_unicode_edit_keep_existing_precision() {
    assert_eq!(
        edits(&[(10, "😀abc")], &[(10, "😀axc")], false),
        vec![Edit::Remove(2, "b".into()), Edit::Insert(2, "x".into())]
    );
    assert_eq!(
        edits(&[(10, "line\n")], &[(10, "line\n"), (11, "next")], false),
        vec![Edit::Insert(5, "next".into())]
    );
}

#[test]
fn repeated_text_and_reordered_nodes_reconstruct_document() {
    edits(
        &[(10, "same\n"), (11, "same\n"), (12, "other\n")],
        &[(11, "same\n"), (12, "other\n"), (13, "new\n")],
        false,
    );
    edits(
        &[(10, "aaa\n"), (11, "bbb\n"), (12, "ccc\n")],
        &[(12, "ccc\n"), (10, "aaa\n"), (11, "bbb\n")],
        false,
    );
}

#[test]
fn many_trimmed_rows_do_not_reannounce_retained_history() {
    let values: Vec<_> = (0..1000).map(|i| format!("row {i:04}\n")).collect();
    let old: Vec<_> = values[..999]
        .iter()
        .enumerate()
        .map(|(i, value)| (i as u64 + 10, value.as_str()))
        .collect();
    let new: Vec<_> = values[5..]
        .iter()
        .enumerate()
        .map(|(i, value)| (i as u64 + 15, value.as_str()))
        .collect();
    let result = edits(&old, &new, false);
    let changed: usize = result
        .iter()
        .map(|edit| match edit {
            Edit::Remove(_, content) | Edit::Insert(_, content) => content.chars().count(),
        })
        .sum();
    assert_eq!(changed, 54);
    assert_eq!(result.len(), 2);
}

#[test]
fn reordered_and_modified_runs_always_reconstruct_the_complete_text() {
    let orders = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    let values = ["😀\n", "A\u{301}\n", "中\n"];
    for old_order in orders {
        let old = old_order.map(|i| (i as u64 + 10, values[i]));
        for new_order in orders {
            for changed in 0..=3 {
                let new = new_order.map(|i| {
                    (
                        i as u64 + 10,
                        if i == changed {
                            "replacement\n"
                        } else {
                            values[i]
                        },
                    )
                });
                edits(&old, &new, false);
            }
        }
    }
}

#[test]
fn empty_runs_do_not_hide_trim_or_insertion() {
    edits(
        &[(10, ""), (11, "removed\n"), (12, ""), (13, "kept\n")],
        &[(12, ""), (13, "kept\n"), (14, ""), (15, "new\n")],
        false,
    );
}

#[test]
fn scrolling_content_through_stationary_row_ids_preserves_the_middle() {
    assert_eq!(
        edits(
            &[(10, "A\n"), (11, "B\n"), (12, "C\n")],
            &[(10, "B\n"), (11, "C\n"), (12, "D\n")],
            false,
        ),
        vec![Edit::Remove(0, "A\n".into()), Edit::Insert(4, "D\n".into())]
    );
}

#[test]
fn numbered_alternate_scroll_with_stationary_ids_has_bounded_churn() {
    let values: Vec<_> = (0..240).map(|i| format!("row {i:04}\n")).collect();
    let old: Vec<_> = values[..239]
        .iter()
        .enumerate()
        .map(|(i, value)| (i as u64 + 10, value.as_str()))
        .collect();
    let new: Vec<_> = values[3..]
        .iter()
        .enumerate()
        .map(|(i, value)| (i as u64 + 10, value.as_str()))
        .collect();
    let result = edits(&old, &new, false);
    let changed: usize = result
        .iter()
        .map(|edit| match edit {
            Edit::Remove(_, content) | Edit::Insert(_, content) => content.chars().count(),
        })
        .sum();
    assert_eq!(changed, 36);
    assert_eq!(result.len(), 2);
}

#[test]
fn repeated_blanks_remain_exact_with_stationary_ids() {
    edits(
        &[(10, "\n"), (11, "\n"), (12, "B\n"), (13, "\n"), (14, "\n")],
        &[(10, "\n"), (11, "B\n"), (12, "\n"), (13, "\n"), (14, "D\n")],
        false,
    );
    edits(
        &[(10, ""), (11, ""), (12, "")],
        &[(10, ""), (11, ""), (12, "D")],
        false,
    );
}

#[test]
fn blank_heavy_stationary_scroll_does_not_reannounce_surviving_content() {
    let content = [
        "first meaningful row with A\u{301} and 😀 survives this scroll\n",
        "second meaningful row with 中 and 🦀 survives this scroll\n",
        "third meaningful row with more terminal output survives this scroll\n",
    ];
    let mut old: Vec<_> = (0..10).map(|i| (i + 10, "\n")).collect();
    old.extend(
        content
            .iter()
            .enumerate()
            .map(|(i, value)| (i as u64 + 20, *value)),
    );
    old.push((23, "\n"));
    let mut new: Vec<_> = content
        .iter()
        .enumerate()
        .map(|(i, value)| (i as u64 + 10, *value))
        .collect();
    new.extend((3..14).map(|i| (i + 10, "\n")));
    let content_length = content.iter().map(|value| value.chars().count()).sum();
    assert_eq!(
        edits(&old, &new, false),
        vec![
            Edit::Remove(0, "\n".repeat(10)),
            Edit::Insert(content_length, "\n".repeat(10))
        ],
    );
}

#[test]
fn retained_text_weight_counts_unicode_scalars_instead_of_bytes() {
    let ascii = format!("{}\n", "a".repeat(20));
    let emoji = format!("{}\n", "😀".repeat(8));
    assert_eq!(
        edits(
            &[(10, &ascii), (11, &emoji)],
            &[(11, &emoji), (10, &ascii)],
            false
        ),
        vec![Edit::Insert(0, emoji.clone()), Edit::Remove(30, emoji)],
    );
}
