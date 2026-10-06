//! Publishes an editor's plain text to assistive technology as text runs with a selection.

use std::{cell::RefCell, ops::Range, rc::Rc};

use gpui::{
    A11ySubtreeBuilder,
    accesskit::{ActionData, Node, NodeId, Role, TextPosition, TextSelection},
};
use unicode_segmentation::UnicodeSegmentation as _;

/// The runs an editor last published, shared by its publication and its selection requests.
#[derive(Clone, Default)]
pub(crate) struct AccessibleText(Rc<RefCell<Vec<Run>>>);

struct Run {
    node: NodeId,
    /// Byte offsets at each character start and at the run's end, relative to the text.
    boundaries: Vec<usize>,
}

impl Run {
    fn range(&self) -> Range<usize> {
        self.boundaries[0]..*self.boundaries.last().unwrap_or(&self.boundaries[0])
    }
}

impl AccessibleText {
    /// Publishes one run per line as children of the editor's node, with the selection given by
    /// its anchor and active end in bytes.
    pub(crate) fn publish(
        &self,
        builder: &mut A11ySubtreeBuilder,
        text: &str,
        anchor: usize,
        focus: usize,
    ) {
        let mut runs = Vec::new();
        let mut start = 0;
        for (index, line) in text
            .split_inclusive('\n')
            .chain(
                // An empty value, or one ending in a line break, still has a final line.
                (text.is_empty() || text.ends_with('\n')).then_some(""),
            )
            .enumerate()
        {
            let node = builder.synthetic_node_id(("text-run", index));
            let mut lengths = Vec::new();
            let mut boundaries = vec![start];
            for character in line.graphemes(true) {
                // AccessKit counts characters in bytes no longer than u8::MAX.
                if character.len() <= usize::from(u8::MAX) {
                    lengths.push(character.len() as u8);
                } else {
                    lengths.extend(character.chars().map(|scalar| scalar.len_utf8() as u8));
                }
            }
            for length in &lengths {
                boundaries.push(boundaries.last().copied().unwrap_or(start) + usize::from(*length));
            }
            let word_starts = line
                .split_word_bound_indices()
                .filter(|(offset, word)| {
                    *offset > 0 && !word.chars().next().is_some_and(char::is_whitespace)
                })
                .filter_map(|(offset, _)| {
                    boundaries
                        .binary_search(&(start + offset))
                        .ok()
                        .map(|index| index as u32)
                })
                .collect::<Vec<_>>();
            let mut run = Node::new(Role::TextRun);
            run.set_value(line);
            run.set_character_lengths(lengths);
            run.set_word_starts_u32(word_starts);
            builder.push_child(node, run);
            start += line.len();
            runs.push(Run { node, boundaries });
        }
        let selection = position(&runs, anchor)
            .zip(position(&runs, focus))
            .map(|(anchor, focus)| TextSelection { anchor, focus });
        if let Some(selection) = selection {
            builder.parent_node().set_text_selection(selection);
        }
        *self.0.borrow_mut() = runs;
    }

    /// Resolves a selection request to its anchor and active end in bytes.
    pub(crate) fn requested_selection(&self, data: Option<&ActionData>) -> Option<(usize, usize)> {
        let Some(ActionData::SetTextSelection(selection)) = data else {
            return None;
        };
        let runs = self.0.borrow();
        Some((
            offset(&runs, selection.anchor)?,
            offset(&runs, selection.focus)?,
        ))
    }
}

fn position(runs: &[Run], offset: usize) -> Option<TextPosition> {
    let last = runs.len().checked_sub(1)?;
    // A run owns the offsets from its start up to its end; the final run also owns its end.
    let run = runs.iter().enumerate().find_map(|(index, run)| {
        let range = run.range();
        (range.contains(&offset) || (index == last && range.end == offset)).then_some(run)
    })?;
    Some(TextPosition {
        node: run.node,
        character_index: run.boundaries.binary_search(&offset).ok()?,
    })
}

fn offset(runs: &[Run], position: TextPosition) -> Option<usize> {
    runs.iter()
        .find(|run| run.node == position.node)?
        .boundaries
        .get(position.character_index)
        .copied()
}
