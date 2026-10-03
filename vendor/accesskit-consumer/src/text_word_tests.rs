// Licensed under the Apache License, Version 2.0 or the MIT license.

use crate::{TextPosition, tests::nid};
use accesskit::{Node, NodeId, Role, Tree, TreeId, TreeUpdate};
use alloc::{format, string::String, vec, vec::Vec};

fn tree(runs: Vec<Node>) -> crate::Tree {
    let mut window = Node::new(Role::Window);
    window.set_children([NodeId(1)]);
    let mut terminal = Node::new(Role::Terminal);
    terminal.set_children(
        (0..runs.len())
            .map(|index| NodeId(index as u64 + 2))
            .collect::<Vec<_>>(),
    );
    let mut nodes = vec![(NodeId(0), window), (NodeId(1), terminal)];
    nodes.extend(
        runs.into_iter()
            .enumerate()
            .map(|(index, run)| (NodeId(index as u64 + 2), run)),
    );
    crate::Tree::new(
        TreeUpdate {
            nodes,
            tree: Some(Tree::new(NodeId(0))),
            tree_id: TreeId::ROOT,
            focus: NodeId(1),
        },
        true,
    )
}

fn run(text: &str, starts: Option<&[u32]>) -> Node {
    let mut run = Node::new(Role::TextRun);
    run.set_value(text);
    run.set_character_lengths(
        text.chars()
            .map(|character| character.len_utf8() as u8)
            .collect::<Vec<_>>(),
    );
    // A deliberately conflicting legacy boundary proves the wide property takes precedence.
    run.set_word_starts([4]);
    if let Some(starts) = starts {
        run.set_word_starts_u32(starts.to_vec());
    }
    run
}

fn word_at(position: TextPosition<'_>) -> String {
    let start = if position.is_word_start() {
        position
    } else {
        position.backward_to_word_start()
    };
    let mut range = start.to_degenerate_range();
    range.set_end(start.forward_to_word_end());
    range.text()
}

#[test]
fn wide_word_boundaries_support_current_forward_and_reverse_navigation() {
    let value = format!("{}word tail", " ".repeat(260));
    let tree = tree(vec![run(&value, Some(&[260, 265]))]);
    let terminal = tree.state().node_by_id(nid(NodeId(1))).unwrap();
    let word = terminal.text_position_from_global_usv_index(260).unwrap();
    assert!(word.is_word_start());
    assert!(
        !terminal
            .text_position_from_global_usv_index(4)
            .unwrap()
            .is_word_start()
    );
    assert_eq!(
        terminal
            .document_start()
            .forward_to_word_start()
            .to_global_usv_index(),
        260
    );
    assert_eq!(word.forward_to_word_start().to_global_usv_index(), 265);
    assert_eq!(
        terminal
            .text_position_from_global_usv_index(264)
            .unwrap()
            .backward_to_word_start()
            .to_global_usv_index(),
        260
    );
    assert_eq!(
        terminal
            .document_end()
            .backward_to_word_start()
            .to_global_usv_index(),
        265
    );
    assert_eq!(
        word_at(terminal.text_position_from_global_usv_index(262).unwrap()),
        "word "
    );
    assert_eq!(
        word_at(terminal.text_position_from_global_usv_index(267).unwrap()),
        "tail"
    );
}

#[test]
fn wide_boundaries_index_accesskit_characters_after_an_oversized_grapheme_split() {
    let grapheme = format!("A{}", "\u{301}".repeat(200));
    let value = format!("{grapheme}{}word tail", " ".repeat(260));
    let mut run = run(&value, Some(&[0, 262, 267]));
    let mut lengths = vec![255, 146];
    lengths.extend(core::iter::repeat_n(1, 269));
    run.set_character_lengths(lengths);
    let tree = tree(vec![run]);
    let terminal = tree.state().node_by_id(nid(NodeId(1))).unwrap();
    assert_eq!(terminal.document_range().text(), value);
    let word = terminal.text_position_from_global_usv_index(461).unwrap();
    assert!(word.is_word_start());
    assert_eq!(
        terminal
            .document_start()
            .forward_to_word_start()
            .to_global_usv_index(),
        461
    );
    assert_eq!(word.forward_to_word_start().to_global_usv_index(), 466);
    assert_eq!(
        terminal
            .document_end()
            .backward_to_word_start()
            .to_global_usv_index(),
        466
    );
    assert_eq!(
        word_at(terminal.text_position_from_global_usv_index(463).unwrap()),
        "word "
    );
}

#[test]
fn wide_first_and_last_boundaries_work_across_soft_wrapped_runs() {
    let mut head = run("head", None);
    head.set_word_starts([0]);
    let middle = format!("{}word tail", " ".repeat(260));
    let tail = run(" suffix", Some(&[]));
    let tree = tree(vec![head, run(&middle, Some(&[260, 265])), tail]);
    let terminal = tree.state().node_by_id(nid(NodeId(1))).unwrap();
    assert_eq!(
        terminal
            .document_start()
            .forward_to_word_start()
            .to_global_usv_index(),
        264
    );
    assert_eq!(
        terminal
            .document_end()
            .backward_to_word_start()
            .to_global_usv_index(),
        269
    );
}

#[test]
fn unset_wide_property_preserves_legacy_boundaries_and_explicit_empty_overrides_them() {
    for (wide, expected) in [(None, 4), (Some(&[][..]), 9)] {
        let tree = tree(vec![run("head tail", wide)]);
        let terminal = tree.state().node_by_id(nid(NodeId(1))).unwrap();
        assert_eq!(
            terminal
                .document_start()
                .forward_to_word_start()
                .to_global_usv_index(),
            expected
        );
    }
}
