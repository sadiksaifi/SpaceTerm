// Licensed under the Apache License, Version 2.0 or the MIT license.

use crate::ObjectEvent;
use accesskit::Role;
use accesskit_consumer::{FilterResult, Node, NodeId};
use std::collections::HashMap;

struct Run<'a> {
    id: NodeId,
    value: &'a str,
    byte_start: usize,
    usv_start: usize,
}

#[derive(Clone, Copy, Default, Eq, Ord, PartialEq, PartialOrd)]
struct AnchorChain {
    retained_usv: usize,
    anchor_count: usize,
    last_match: Option<usize>,
}

fn document<'a>(node: &Node<'a>) -> (String, Vec<Run<'a>>) {
    let mut text = String::new();
    let mut runs = Vec::new();
    let mut usv_start = 0;
    // Match accesskit_consumer's text_node_filter: descend through non-text
    // nodes, including nested containers, and stop at each TextRun.
    for child in node.filtered_children(|child| {
        if child.role() == Role::TextRun {
            FilterResult::Include
        } else {
            FilterResult::ExcludeNode
        }
    }) {
        let value = child.data().value().unwrap();
        runs.push(Run {
            id: child.id(),
            value,
            byte_start: text.len(),
            usv_start,
        });
        text.push_str(value);
        usv_start += value.chars().count();
    }
    (text, runs)
}

fn unchanged_anchors(old: &[Run<'_>], new: &[Run<'_>]) -> Vec<(usize, usize)> {
    let old_indices: HashMap<_, _> = old
        .iter()
        .enumerate()
        .map(|(index, run)| (run.id, index))
        .collect();
    let mut old_values = HashMap::new();
    for (index, run) in old.iter().enumerate() {
        old_values
            .entry(run.value)
            .and_modify(|unique_index| *unique_index = None)
            .or_insert(Some(index));
    }
    let mut new_value_counts = HashMap::new();
    for run in new {
        *new_value_counts.entry(run.value).or_insert(0usize) += 1;
    }
    let matches: Vec<_> = new
        .iter()
        .enumerate()
        .filter_map(|(new_index, run)| {
            let old_index = old_indices
                .get(&run.id)
                .copied()
                .filter(|&index| old[index].value == run.value)
                .or_else(|| {
                    // Some terminals scroll by copying text through stationary
                    // row nodes. A value unique in both documents provides an
                    // unambiguous anchor even when its node identity changes.
                    // Repeated values and blanks still use identity or gap diffs.
                    if run.value.is_empty() || new_value_counts[run.value] != 1 {
                        return None;
                    }
                    old_values.get(run.value).copied().flatten()
                })?;
            Some((old_index, new_index))
        })
        .collect();

    // A reordered run cannot anchor text on both sides of a run that crossed
    // it. Maximize retained Unicode scalars, not the number of runs: many
    // stationary blank rows must not outweigh meaningful surviving content.
    // A Fenwick tree of prefix maxima finds the weighted monotonic chain in
    // O(runs * log(runs)) time and linear memory. Anchor count breaks ties,
    // including empty runs, followed by a deterministic final match index.
    let mut prefix_best = vec![AnchorChain::default(); old.len() + 1];
    let mut best = AnchorChain::default();
    let mut predecessors = vec![None; matches.len()];
    for (index, &(old_index, _)) in matches.iter().enumerate() {
        let mut preceding = AnchorChain::default();
        // Query strictly preceding old runs so anchors cannot reuse or cross
        // an old run. Matches are already visited in new-document order.
        let mut prefix = old_index;
        while prefix > 0 {
            preceding = preceding.max(prefix_best[prefix]);
            prefix &= prefix - 1;
        }
        predecessors[index] = preceding.last_match;
        let chain = AnchorChain {
            retained_usv: preceding.retained_usv + old[old_index].value.chars().count(),
            anchor_count: preceding.anchor_count + 1,
            last_match: Some(index),
        };
        let mut position = old_index + 1;
        while position < prefix_best.len() {
            prefix_best[position] = prefix_best[position].max(chain);
            position += position & position.wrapping_neg();
        }
        best = best.max(chain);
    }
    let mut anchors = Vec::with_capacity(best.anchor_count);
    let mut next = best.last_match;
    while let Some(index) = next {
        anchors.push(matches[index]);
        next = predecessors[index];
    }
    anchors.reverse();
    anchors
}

fn emit_gap(old: &str, new: &str, start_usv: usize, emit: &mut impl FnMut(ObjectEvent)) {
    let (prefix_bytes, prefix_usv) = old
        .chars()
        .zip(new.chars())
        .take_while(|(a, b)| a == b)
        .fold((0, 0), |(bytes, usv), (c, _)| {
            (bytes + c.len_utf8(), usv + 1)
        });
    let suffix_bytes = old[prefix_bytes..]
        .chars()
        .rev()
        .zip(new[prefix_bytes..].chars().rev())
        .take_while(|(a, b)| a == b)
        .fold(0, |bytes, (c, _)| bytes + c.len_utf8());
    let old = &old[prefix_bytes..old.len() - suffix_bytes];
    let new = &new[prefix_bytes..new.len() - suffix_bytes];
    let Ok(start_index) = i32::try_from(start_usv + prefix_usv) else {
        return;
    };
    if !old.is_empty() {
        if let Ok(length) = old.chars().count().try_into() {
            emit(ObjectEvent::TextRemoved {
                start_index,
                length,
                content: old.to_string(),
            });
        }
    }
    if !new.is_empty() {
        if let Ok(length) = new.chars().count().try_into() {
            emit(ObjectEvent::TextInserted {
                start_index,
                length,
                content: new.to_string(),
            });
        }
    }
}

pub(crate) fn emit_text_changes(
    old_node: &Node<'_>,
    new_node: &Node<'_>,
    mut emit: impl FnMut(ObjectEvent),
) {
    if let Some(changes) = new_node.direct_text_run_changes(old_node) {
        let mut group = Vec::new();
        let mut start = 0;
        let mut last_position = None;
        let emit_group = |group: &[(NodeId, &str, &str)], start, emit: &mut _| {
            let make_runs = |old: bool| {
                let mut text = String::new();
                let mut runs = Vec::new();
                let mut usv_start = 0;
                for (id, old_value, new_value) in group {
                    let value = if old { *old_value } else { *new_value };
                    runs.push(Run {
                        id: *id,
                        value,
                        byte_start: text.len(),
                        usv_start,
                    });
                    text.push_str(value);
                    usv_start += value.chars().count();
                }
                (text, runs)
            };
            let (old_text, old_runs) = make_runs(true);
            let (new_text, new_runs) = make_runs(false);
            emit_run_changes(&old_text, &old_runs, &new_text, &new_runs, start, emit);
        };
        // Content copied across stationary rows can cross unchanged blanks.
        // Include the intervening runs so the weighted anchor chain preserves
        // moved text instead of announcing it again in two independent gaps.
        let old_values: HashMap<_, _> = changes
            .iter()
            .filter_map(|(id, _)| {
                let value = old_node.tree_state.node_by_id(*id)?.data().value()?;
                (!value.chars().all(char::is_whitespace)).then_some((value, *id))
            })
            .collect();
        let relocated = changes.iter().any(|(id, _)| {
            new_node
                .tree_state
                .node_by_id(*id)
                .and_then(|node| node.data().value())
                .and_then(|value| old_values.get(value))
                .is_some_and(|old_id| old_id != id)
        });
        if relocated {
            let (first, offset) = changes.first().copied().unwrap();
            let last = changes.last().unwrap().0;
            let first = new_node
                .tree_state
                .node_by_id(first)
                .unwrap()
                .parent_and_index()
                .unwrap()
                .1;
            let last = new_node
                .tree_state
                .node_by_id(last)
                .unwrap()
                .parent_and_index()
                .unwrap()
                .1;
            for id in &new_node.data().children()[first..=last] {
                let old = old_node
                    .tree_state
                    .node_by_tree_local_id(
                        *id,
                        old_node.tree_state.locate_node(old_node.id()).unwrap().1,
                    )
                    .unwrap();
                let new = new_node
                    .tree_state
                    .node_by_tree_local_id(
                        *id,
                        new_node.tree_state.locate_node(new_node.id()).unwrap().1,
                    )
                    .unwrap();
                group.push((
                    new.id(),
                    old.data().value().unwrap_or(""),
                    new.data().value().unwrap_or(""),
                ));
            }
            emit_group(&group, offset, &mut emit);
            return;
        }
        for (id, offset) in changes {
            let old = old_node.tree_state.node_by_id(id).unwrap();
            let new = new_node.tree_state.node_by_id(id).unwrap();
            let position = new.parent_and_index().unwrap().1;
            if last_position.is_none_or(|last| position != last + 1) {
                emit_group(&group, start, &mut emit);
                group.clear();
                start = offset;
            }
            group.push((
                id,
                old.data().value().unwrap_or(""),
                new.data().value().unwrap_or(""),
            ));
            last_position = Some(position);
        }
        emit_group(&group, start, &mut emit);
        return;
    }
    let (old_text, old_runs) = document(old_node);
    let (new_text, new_runs) = document(new_node);
    emit_run_changes(&old_text, &old_runs, &new_text, &new_runs, 0, &mut emit);
}

fn emit_run_changes(
    old_text: &str,
    old_runs: &[Run<'_>],
    new_text: &str,
    new_runs: &[Run<'_>],
    start: usize,
    emit: &mut impl FnMut(ObjectEvent),
) {
    if old_text == new_text {
        return;
    }
    let mut old_start = 0;
    let mut new_start = 0;
    let mut new_usv_start = start;
    for (old_index, new_index) in unchanged_anchors(old_runs, new_runs) {
        let old_run = &old_runs[old_index];
        let new_run = &new_runs[new_index];
        emit_gap(
            &old_text[old_start..old_run.byte_start],
            &new_text[new_start..new_run.byte_start],
            new_usv_start,
            emit,
        );
        // Earlier events have already transformed the preceding text. Later
        // offsets therefore refer to this new-document position, not the old
        // index that may have shifted when history was removed.
        old_start = old_run.byte_start + old_run.value.len();
        new_start = new_run.byte_start + new_run.value.len();
        new_usv_start = start + new_run.usv_start + new_run.value.chars().count();
    }
    emit_gap(
        &old_text[old_start..],
        &new_text[new_start..],
        new_usv_start,
        emit,
    );
}
