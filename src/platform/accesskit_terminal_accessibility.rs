//! Terminal text publication through the Window's portable AccessKit tree.
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    ops::Range,
    rc::Rc,
    sync::Arc,
};

use gpui::{
    A11ySubtreeBuilder, Div, Pixels, Stateful, StatefulInteractiveElement, Window,
    accesskit::{
        Action, ActionData, Node, NodeId, Rect, Role, TextDirection, TextPosition, TextSelection,
    },
};

use super::terminal_accessibility::{
    TerminalAccessibilityAdapter, TerminalAccessibilityAdapterFactory, TerminalAccessibilityUpdate,
};
use crate::terminal::{
    AccessibilityDemandSender, AccessibilityGeometry, AccessibilityNotifications,
    AccessibilityRowId, AccessibilityRowView, AccessibilitySelectionSender,
    TerminalAccessibilityModel,
};

pub(crate) struct AccessKitTerminalAccessibilityAdapterFactory;

impl TerminalAccessibilityAdapterFactory for AccessKitTerminalAccessibilityAdapterFactory {
    fn create(
        &self,
        window: &Window,
        model: TerminalAccessibilityModel,
        font: &crate::appearance::ResolvedFontDescriptor,
        font_size: Pixels,
    ) -> Box<dyn TerminalAccessibilityAdapter> {
        Box::new(AccessKitTerminalAccessibility(Rc::new(RefCell::new(
            PaneTree {
                presented: false,
                visible: false,
                model,
                font_family: font.primary_family.clone(),
                font_size: f32::from(font_size),
                geometry: Geometry {
                    scale: window.scale_factor(),
                    ..Geometry::default()
                },
                selection_sender: None,
                demand_sender: None,
                runs: HashMap::new(),
                published: None,
            },
        ))))
    }
}

struct AccessKitTerminalAccessibility(Rc<RefCell<PaneTree>>);

impl TerminalAccessibilityAdapter for AccessKitTerminalAccessibility {
    fn set_hierarchy(&mut self, presented: bool, _: usize) {
        let mut tree = self.0.borrow_mut();
        tree.presented = presented;
        tree.visible &= presented;
        if !presented {
            tree.selection_sender = None;
            tree.demand_sender = None;
            tree.published = None;
            tree.runs.clear();
        }
    }

    fn update(&mut self, update: TerminalAccessibilityUpdate<'_>) -> AccessibilityNotifications {
        let mut tree = self.0.borrow_mut();
        tree.model = update.model.clone();
        tree.visible = tree.presented && update.bounds.is_some();
        tree.font_family.clone_from(&update.font.primary_family);
        tree.font_size = f32::from(update.font_size);
        tree.geometry = Geometry {
            origin: update.bounds.map_or((0.0, 0.0), |bounds| {
                (f32::from(bounds.origin.x), f32::from(bounds.origin.y))
            }),
            cell_width: f32::from(update.cell_width),
            line_height: f32::from(update.line_height),
            scale: update.window.scale_factor(),
        };
        tree.selection_sender = update.selection_sender.filter(|_| tree.visible);
        tree.demand_sender = update.demand_sender.filter(|_| tree.visible);
        // AccessKit derives value, selection and focus events from the published tree.
        // Repeated focus notifications without a focus change need no native event.
        if tree.visible {
            AccessibilityNotifications::default()
        } else {
            update.notifications
        }
    }

    fn decorate(&self, pane: Stateful<Div>) -> Stateful<Div> {
        if !self.0.borrow().presented {
            return pane;
        }
        let publication = Rc::clone(&self.0);
        let selection = Rc::clone(&self.0);
        pane.role(Role::Terminal)
            .aria_label("Terminal Pane")
            .a11y_synthetic_children(move |builder| publication.borrow_mut().publish(builder))
            .on_a11y_action(Action::SetTextSelection, move |data, _, _| {
                selection.borrow().select(data)
            })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Geometry {
    origin: (f32, f32),
    cell_width: f32,
    line_height: f32,
    scale: f32,
}

struct CachedRun {
    revision: u64,
    line_break: bool,
    metrics: (f32, f32),
    starts_word: bool,
    node: Arc<Node>,
    /// UTF-16 offsets at every AccessKit character boundary, relative to this row.
    offsets: Arc<[usize]>,
}

struct PublishedRun {
    node: NodeId,
    range: Range<usize>,
    offsets: Arc<[usize]>,
}

struct PublishedDocument {
    model: TerminalAccessibilityModel,
    runs: Vec<PublishedRun>,
}

impl PublishedDocument {
    fn position(&self, utf16: usize) -> Option<TextPosition> {
        if utf16 > self.model.len_utf16() {
            return None;
        }
        // At a hard line's end, use its newline character. At a soft wrap boundary,
        // use the following row. The final document boundary belongs to the last run.
        let run = self
            .runs
            .iter()
            .find(|run| utf16 < run.range.end)
            .or_else(|| self.runs.last())?;
        let local = utf16.checked_sub(run.range.start)?;
        let character_index = run
            .offsets
            .partition_point(|offset| *offset <= local)
            .saturating_sub(1);
        Some(TextPosition {
            node: run.node,
            character_index,
        })
    }

    fn utf16(&self, position: TextPosition) -> Option<usize> {
        let run = self.runs.iter().find(|run| run.node == position.node)?;
        Some(run.range.start + *run.offsets.get(position.character_index)?)
    }
}

struct PaneTree {
    presented: bool,
    visible: bool,
    model: TerminalAccessibilityModel,
    font_family: String,
    font_size: f32,
    geometry: Geometry,
    selection_sender: Option<AccessibilitySelectionSender>,
    demand_sender: Option<AccessibilityDemandSender>,
    runs: HashMap<AccessibilityRowId, CachedRun>,
    published: Option<PublishedDocument>,
}

impl PaneTree {
    fn caret_bounds(&self) -> Option<Rect> {
        let geometry = AccessibilityGeometry::new(
            self.geometry.origin.0,
            self.geometry.origin.1,
            self.geometry.cell_width,
            self.geometry.line_height,
        )?;
        let focus = self.model.selected_or_cursor_range().end;
        let (x, y, width, height) = self.model.bounds_for_range(focus..focus, geometry)?;
        let scale = f64::from(self.geometry.scale);
        Some(Rect {
            x0: f64::from(x) * scale,
            y0: f64::from(y) * scale,
            x1: f64::from(x + width) * scale,
            y1: f64::from(y + height) * scale,
        })
    }

    fn project(&mut self, id: impl Fn(AccessibilityRowId) -> NodeId) -> Vec<(NodeId, Node)> {
        let mut nodes = Vec::new();
        let mut runs = Vec::new();
        let mut retained = HashSet::new();
        let metrics = (
            self.geometry.cell_width * self.geometry.scale,
            self.geometry.line_height * self.geometry.scale,
        );
        let visible_start = self.model.visible_lines().start;
        let mut starts_word = true;
        for (line, row) in self.model.rows().enumerate() {
            retained.insert(row.id);
            let line_break = !row.soft_wrapped && row.range.end - row.range.start > row.len_utf16;
            let cached = self
                .runs
                .entry(row.id)
                .or_insert_with(|| build_run(&row, line_break, metrics, starts_word));
            if cached.revision != row.revision
                || cached.line_break != line_break
                || cached.metrics != metrics
                || cached.starts_word != starts_word
            {
                *cached = build_run(&row, line_break, metrics, starts_word);
            }
            starts_word =
                line_break || row.text.chars().next_back().is_none_or(char::is_whitespace);
            let node_id = id(row.id);
            let mut node = cached.node.as_ref().clone();
            if line_break
                && !row.text.is_empty()
                && self.model.cursor_range().start == row.range.start + row.len_utf16
                && let Some((cursor_line, column)) = self.model.cursor_cell()
                && cursor_line == line
            {
                let mut positions = node.character_positions().unwrap_or_default().to_vec();
                if let Some(newline) = positions.last_mut() {
                    *newline = f32::from(column) * metrics.0;
                    node.set_character_positions(positions);
                }
            }
            let empty_cursor_column = self
                .model
                .cursor_cell()
                .filter(|(cursor_line, _)| *cursor_line == line && row.text.is_empty())
                .map_or(0, |(_, column)| column);
            let x = f64::from(
                self.geometry.origin.0 * self.geometry.scale
                    + f32::from(empty_cursor_column) * metrics.0,
            );
            let y = f64::from(self.geometry.origin.1 * self.geometry.scale)
                + (line as f64 - visible_start as f64) * f64::from(metrics.1);
            let text_width = node
                .character_positions()
                .unwrap_or_default()
                .iter()
                .zip(node.character_widths().unwrap_or_default())
                .map(|(position, width)| position + width)
                .fold(0.0_f32, f32::max);
            node.set_bounds(Rect {
                x0: x,
                y0: y,
                x1: x + f64::from(text_width),
                y1: y + f64::from(metrics.1),
            });
            nodes.push((node_id, node));
            runs.push(PublishedRun {
                node: node_id,
                range: row.range,
                offsets: Arc::clone(&cached.offsets),
            });
        }
        self.runs.retain(|row_id, _| retained.contains(row_id));
        self.published = Some(PublishedDocument {
            model: self.model.clone(),
            runs,
        });
        nodes
    }

    fn publish(&mut self, builder: &mut A11ySubtreeBuilder<'_>) {
        if self.visible
            && let Some(demand) = &self.demand_sender
        {
            demand.request();
        }
        let nodes = self.project(|row| builder.synthetic_node_id(row));
        let caret_bounds = self.caret_bounds();
        let parent = builder.parent_node();
        parent.set_clips_children();
        parent.set_font_family(self.font_family.clone());
        parent.set_font_size(self.font_size);
        if let Some(bounds) = caret_bounds {
            parent.set_text_caret_bounds(bounds);
        }
        if !self.visible {
            parent.set_hidden();
        }
        if let Some(published) = &self.published {
            let range = self.model.selected_or_cursor_range();
            if let (Some(anchor), Some(focus)) = (
                published.position(range.start),
                published.position(range.end),
            ) {
                parent.set_text_selection(TextSelection { anchor, focus });
            }
        }
        for (id, node) in nodes {
            builder.push_child(id, node);
        }
    }

    fn select(&self, data: Option<&ActionData>) {
        if !self.visible {
            return;
        }
        let (Some(ActionData::SetTextSelection(selection)), Some(published), Some(sender)) =
            (data, &self.published, &self.selection_sender)
        else {
            return;
        };
        // A new model may arrive before the next frame. Selection belongs to exactly
        // the document the assistive client observed, never to newer row revisions.
        if !self.model.shares_snapshot(&published.model) {
            return;
        }
        let (Some(anchor), Some(focus)) = (
            published.utf16(selection.anchor),
            published.utf16(selection.focus),
        ) else {
            return;
        };
        if let Some(request) = published
            .model
            .selection_request(anchor.min(focus)..anchor.max(focus))
        {
            sender.request(request);
        }
    }
}

fn build_run(
    row: &AccessibilityRowView<'_>,
    line_break: bool,
    metrics: (f32, f32),
    starts_word: bool,
) -> CachedRun {
    let mut node = Node::new(Role::TextRun);
    let mut value = row.text.to_owned();
    let mut lengths = Vec::new();
    let mut positions = Vec::new();
    let mut widths = Vec::new();
    let mut offsets = vec![0];
    let mut words = Vec::new();
    let mut whitespace = starts_word;
    let mut last_column = 0;
    for cell in row.cells() {
        last_column = cell.columns.end;
        let is_whitespace = cell.text.chars().all(char::is_whitespace);
        if whitespace && !is_whitespace {
            words.push(
                u32::try_from(lengths.len())
                    .expect("AccessKit supports up to u32::MAX characters in one TextRun"),
            );
        }
        whitespace = is_whitespace;
        let mut start = 0;
        let mut utf16 = cell.utf16.start;
        // AccessKit stores byte lengths in u8. Preserve scalar boundaries when a
        // terminal grapheme exceeds that limit; selection still normalizes to cells.
        while start < cell.text.len() {
            let mut end = (start + usize::from(u8::MAX)).min(cell.text.len());
            while !cell.text.is_char_boundary(end) {
                end -= 1;
            }
            utf16 += cell.text[start..end].encode_utf16().count();
            lengths.push((end - start) as u8);
            positions.push(
                f32::from(if start == 0 {
                    cell.columns.start
                } else {
                    cell.columns.end
                }) * metrics.0,
            );
            widths.push(if start == 0 {
                f32::from(cell.columns.end - cell.columns.start) * metrics.0
            } else {
                0.0
            });
            offsets.push(utf16);
            start = end;
        }
    }
    if line_break {
        value.push('\n');
        lengths.push(1);
        let end = f32::from(last_column) * metrics.0;
        positions.push(end);
        widths.push(0.0);
        offsets.push(offsets.last().copied().unwrap_or(0) + 1);
    }
    node.set_value(value);
    node.set_text_direction(TextDirection::LeftToRight);
    node.set_character_lengths(lengths);
    node.set_character_positions(positions);
    node.set_character_widths(widths);
    node.set_word_starts(
        words
            .iter()
            .copied()
            .filter_map(|index| u8::try_from(index).ok())
            .collect::<Vec<_>>(),
    );
    node.set_word_starts_u32(words);
    CachedRun {
        revision: row.revision,
        line_break,
        metrics,
        starts_word,
        node: Arc::new(node),
        offsets: Arc::from(offsets),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{AccessibilityCell, AccessibilityLine};
    use gpui::accesskit::{Tree, TreeId, TreeUpdate};
    use std::hash::{Hash, Hasher};

    fn row_id(id: AccessibilityRowId) -> NodeId {
        let mut hasher = std::hash::DefaultHasher::new();
        id.hash(&mut hasher);
        NodeId(hasher.finish())
    }

    fn pane_tree(model: TerminalAccessibilityModel) -> PaneTree {
        PaneTree {
            presented: true,
            visible: true,
            model,
            font_family: "SpaceTerm Default".into(),
            font_size: 14.0,
            geometry: Geometry {
                origin: (10.0, 20.0),
                cell_width: 8.0,
                line_height: 18.0,
                scale: 2.0,
            },
            selection_sender: None,
            demand_sender: None,
            runs: HashMap::new(),
            published: None,
        }
    }

    fn update(tree: &mut PaneTree) -> TreeUpdate {
        let runs = tree.project(row_id);
        let mut root = Node::new(Role::Window);
        root.set_children([NodeId(1)]);
        let mut terminal = Node::new(Role::Terminal);
        terminal.set_label("Terminal Pane");
        terminal.set_children(runs.iter().map(|(id, _)| *id).collect::<Vec<_>>());
        terminal.add_action(Action::SetTextSelection);
        if let Some(bounds) = tree.caret_bounds() {
            terminal.set_text_caret_bounds(bounds);
        }
        let range = tree.model.selected_or_cursor_range();
        let published = tree.published.as_ref().unwrap();
        terminal.set_text_selection(TextSelection {
            anchor: published.position(range.start).unwrap(),
            focus: published.position(range.end).unwrap(),
        });
        let mut nodes = vec![(NodeId(0), root), (NodeId(1), terminal)];
        nodes.extend(runs);
        TreeUpdate {
            nodes,
            tree: Some(Tree::new(NodeId(0))),
            tree_id: TreeId::ROOT,
            focus: NodeId(1),
        }
    }

    #[test]
    fn accesskit_projection_preserves_graphemes_soft_wraps_utf16_and_selection() {
        let long_grapheme = format!("A{}", "\u{301}".repeat(200));
        let model = TerminalAccessibilityModel::new(
            vec![
                AccessibilityLine::new(
                    vec![
                        AccessibilityCell::new("界", 2, true),
                        AccessibilityCell::new("A\u{301}", 1, true),
                        AccessibilityCell::new("😀", 2, false),
                    ],
                    true,
                ),
                AccessibilityLine::new(vec![AccessibilityCell::new("x", 1, false)], false),
                AccessibilityLine::new(
                    vec![AccessibilityCell::new(long_grapheme, 1, false)],
                    false,
                ),
            ],
            1..3,
            Some((1, 1)),
        );
        let mut projected = pane_tree(model.clone());
        let update = update(&mut projected);
        let consumer = accesskit_consumer::Tree::new(update.clone(), true);
        let terminal = consumer.state().root().children().next().unwrap();
        assert!(terminal.supports_text_ranges());
        assert_eq!(terminal.document_range().text(), model.text());
        assert_eq!(terminal.text_selection().unwrap().text(), "界A\u{301}");
        for (_, node) in update.nodes.iter().skip(2) {
            assert_eq!(
                node.character_lengths()
                    .iter()
                    .map(|length| usize::from(*length))
                    .sum::<usize>(),
                node.value().unwrap().len()
            );
            assert!(node.character_lengths().iter().all(|length| *length > 0));
            assert_eq!(
                node.character_positions().unwrap().len(),
                node.character_lengths().len()
            );
            assert_eq!(
                node.character_widths().unwrap().len(),
                node.character_lengths().len()
            );
        }
        let document = projected.published.as_ref().unwrap();
        for row in model.rows() {
            for cell in row.cells() {
                for boundary in [
                    row.range.start + cell.utf16.start,
                    row.range.start + cell.utf16.end,
                ] {
                    assert_eq!(
                        document.utf16(document.position(boundary).unwrap()),
                        Some(boundary)
                    );
                }
            }
        }
        let newline = document.position(6).unwrap();
        assert_eq!(newline.node, update.nodes[3].0);
        assert_eq!(newline.character_index, 1);
        let wide = &update.nodes[2].1;
        assert_eq!(wide.character_positions().unwrap(), &[0.0, 32.0, 48.0]);
        assert_eq!(wide.character_widths().unwrap(), &[32.0, 16.0, 32.0]);
        assert_eq!(wide.bounds().unwrap().y0, 4.0);
    }

    #[test]
    fn accesskit_caret_geometry_preserves_blank_cursor_columns_and_large_grapheme_ends() {
        let long = format!("A{}", "\u{301}".repeat(200));
        for (lines, cursor, x) in [
            (vec![AccessibilityLine::new(vec![], false)], (0, 5), 100.0),
            (
                vec![AccessibilityLine::new(
                    vec![AccessibilityCell::new("x", 1, false)],
                    false,
                )],
                (0, 5),
                100.0,
            ),
            (
                vec![AccessibilityLine::new(
                    vec![AccessibilityCell::new(long.clone(), 1, false)],
                    false,
                )],
                (0, 1),
                36.0,
            ),
            (
                vec![
                    AccessibilityLine::new(
                        vec![AccessibilityCell::new(long.clone(), 1, false)],
                        false,
                    ),
                    AccessibilityLine::new(vec![AccessibilityCell::new("x", 1, false)], false),
                ],
                (0, 1),
                36.0,
            ),
            (
                vec![
                    AccessibilityLine::new(vec![AccessibilityCell::new("x", 1, false)], false),
                    AccessibilityLine::new(vec![AccessibilityCell::new("y", 1, false)], false),
                ],
                (0, 5),
                100.0,
            ),
        ] {
            let count = lines.len();
            let mut tree = pane_tree(TerminalAccessibilityModel::new(
                lines,
                0..count,
                Some(cursor),
            ));
            let consumer = accesskit_consumer::Tree::new(update(&mut tree), true);
            let terminal = consumer.state().root().children().next().unwrap();
            assert_eq!(terminal.document_range().text(), tree.model.text());
            let boxes = terminal.text_selection().unwrap().bounding_boxes();
            assert_eq!(
                boxes,
                vec![Rect {
                    x0: x,
                    x1: x,
                    y0: 40.0,
                    y1: 76.0
                }]
            );
            if tree.model.text() == "x" {
                assert_eq!(
                    terminal.document_range().bounding_boxes(),
                    vec![Rect {
                        x0: 20.0,
                        x1: 36.0,
                        y0: 40.0,
                        y1: 76.0
                    }]
                );
            }
        }
    }

    #[test]
    fn accesskit_blank_hard_break_character_keeps_zero_width_at_cursor_column() {
        let mut tree = pane_tree(TerminalAccessibilityModel::new(
            vec![
                AccessibilityLine::new(vec![], false),
                AccessibilityLine::new(vec![AccessibilityCell::new("x", 1, false)], false),
            ],
            0..2,
            Some((0, 5)),
        ));
        let consumer = accesskit_consumer::Tree::new(update(&mut tree), true);
        let terminal = consumer.state().root().children().next().unwrap();
        let mut newline = terminal.document_range();
        newline.set_end(newline.start().forward_to_character_end());
        assert_eq!(newline.text(), "\n");
        assert_eq!(
            newline.bounding_boxes(),
            vec![Rect {
                x0: 100.0,
                x1: 100.0,
                y0: 40.0,
                y1: 76.0,
            }]
        );
    }

    #[test]
    fn accesskit_word_boundaries_continue_across_soft_wrapped_rows() {
        let line = |text: &str, wrapped| {
            AccessibilityLine::new(
                text.chars()
                    .map(|ch| AccessibilityCell::new(ch.to_string(), 1, false))
                    .collect(),
                wrapped,
            )
        };
        let mut tree = pane_tree(TerminalAccessibilityModel::new(
            vec![line("hel", true), line("lo world", false)],
            0..2,
            None,
        ));
        let update = update(&mut tree);
        let consumer = accesskit_consumer::Tree::new(update.clone(), true);
        assert_eq!(
            consumer
                .state()
                .root()
                .children()
                .next()
                .unwrap()
                .document_range()
                .text(),
            "hello world"
        );
        assert_eq!(update.nodes[2].1.word_starts(), &[0]);
        assert_eq!(update.nodes[3].1.word_starts(), &[3]);
    }

    #[test]
    fn accesskit_wide_rows_keep_word_boundaries_after_large_grapheme_chunks() {
        let long = format!("A{}", "\u{301}".repeat(200));
        let mut cells = vec![AccessibilityCell::new(long, 1, false)];
        cells.extend((0..260).map(|_| AccessibilityCell::new(" ", 1, false)));
        cells.extend(
            "hello world"
                .chars()
                .map(|ch| AccessibilityCell::new(ch.to_string(), 1, false)),
        );
        let mut tree = pane_tree(TerminalAccessibilityModel::new(
            vec![AccessibilityLine::new(cells, false)],
            0..1,
            None,
        ));
        let update = update(&mut tree);
        assert_eq!(
            update.nodes[2].1.word_starts_u32(),
            Some(&[0, 262, 268][..])
        );
        let consumer = accesskit_consumer::Tree::new(update, true);
        let terminal = consumer.state().root().children().next().unwrap();
        let inside_hello = terminal.text_position_from_global_usv_index(463).unwrap();
        assert_eq!(
            inside_hello.backward_to_word_start().to_global_usv_index(),
            461
        );
        assert_eq!(
            inside_hello.forward_to_word_start().to_global_usv_index(),
            467
        );
    }

    #[test]
    fn accesskit_selection_actions_are_document_bound_and_reject_stale_or_hidden_targets() {
        let model = TerminalAccessibilityModel::new(
            vec![
                AccessibilityLine::new(
                    vec![
                        AccessibilityCell::new("A", 1, false),
                        AccessibilityCell::new("😀", 2, false),
                    ],
                    false,
                ),
                AccessibilityLine::new(vec![AccessibilityCell::new("B", 1, false)], false),
            ],
            0..2,
            Some((0, 0)),
        );
        let mut tree = pane_tree(model.clone());
        let (sender, receiver) = AccessibilitySelectionSender::recording_channel();
        tree.selection_sender = Some(sender);
        update(&mut tree);
        let published = tree.published.as_ref().unwrap();
        let selection = ActionData::SetTextSelection(TextSelection {
            anchor: published.position(1).unwrap(),
            focus: published.position(5).unwrap(),
        });
        tree.select(Some(&selection));
        assert_eq!(
            receiver.drain(),
            vec![model.selection_request(1..5).unwrap()]
        );
        tree.select(Some(&ActionData::SetTextSelection(TextSelection {
            anchor: TextPosition {
                node: NodeId(999),
                character_index: 0,
            },
            focus: published.position(5).unwrap(),
        })));
        assert!(receiver.drain().is_empty());
        tree.model = TerminalAccessibilityModel::new(
            vec![AccessibilityLine::new(
                vec![AccessibilityCell::new("new", 3, false)],
                false,
            )],
            0..1,
            None,
        );
        tree.select(Some(&selection));
        assert!(receiver.drain().is_empty());
        tree.model = model;
        let mut adapter = AccessKitTerminalAccessibility(Rc::new(RefCell::new(tree)));
        adapter.set_hierarchy(false, 0);
        adapter.0.borrow().select(Some(&selection));
        assert!(receiver.drain().is_empty());
        assert!(adapter.0.borrow().selection_sender.is_none());
        assert!(adapter.0.borrow().published.is_none());
    }

    #[test]
    fn accesskit_native_terminal_output_refreshes_cached_rows() {
        use crate::terminal::geometry::{
            BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry,
        };
        let mut emulator =
            crate::terminal::testing::TerminalEmulator::new(TerminalGeometry::from_grid(
                CellGridSize::new(80, 4),
                LogicalCellSize::new(8.0, 18.0),
                BackingScale::ONE,
            ))
            .unwrap();
        emulator.feed(b"prompt$ ");
        emulator.snapshot().unwrap().unwrap();
        let initial = loop {
            let (model, more) = emulator
                .accessibility_snapshot_for_current_presentation()
                .unwrap();
            if !more {
                break model.unwrap();
            }
        };
        let mut tree = pane_tree(initial.as_ref().clone());
        let _ = update(&mut tree);

        emulator.feed(b"printf fixture\r\nspaceterm-a11y\r\nprompt$ ");
        emulator.snapshot().unwrap().unwrap();
        let current = loop {
            let (model, more) = emulator
                .accessibility_snapshot_for_current_presentation()
                .unwrap();
            if !more {
                break model.unwrap();
            }
        };
        tree.model = current.as_ref().clone();
        assert!(tree.model.text().contains("spaceterm-a11y"));
        let consumer = accesskit_consumer::Tree::new(update(&mut tree), true);
        let terminal = consumer.state().root().children().next().unwrap();
        assert_eq!(terminal.document_range().text(), tree.model.text());
    }

    #[test]
    fn accesskit_projection_reuses_stable_rows_and_keeps_only_current_nodes() {
        use crate::terminal::{
            AccessibilityRowUpdate, AccessibilityScreen, AccessibilityUpdate,
            TerminalAccessibilityState,
        };
        let first = AccessibilityRowId {
            screen: AccessibilityScreen::Primary,
            screen_generation: 1,
            node_serial: 42,
            page_row: 0,
        };
        let second = AccessibilityRowId {
            page_row: 1,
            ..first
        };
        let mut state = TerminalAccessibilityState::default();
        let make_update = |revision, topology, changed_rows| AccessibilityUpdate {
            revision,
            screen: AccessibilityScreen::Primary,
            screen_generation: 1,
            complete: true,
            more: false,
            topology,
            visible_lines: 0..2,
            cursor: None,
            selection: None,
            changed_rows,
        };
        let row = |id, revision, text| AccessibilityRowUpdate {
            id,
            revision,
            soft_wrapped: false,
            cells: vec![AccessibilityCell::new(text, 1, false)],
        };
        let model = state
            .apply(
                make_update(
                    1,
                    Some(vec![first, second]),
                    vec![row(first, 1, "A"), row(second, 1, "B")],
                ),
                crate::terminal::PresentationGeneration::test(1),
            )
            .unwrap();
        let mut tree = pane_tree(model.as_ref().clone());
        let before = tree.project(row_id);
        let retained = Arc::clone(&tree.runs[&first].node);
        let changed = Arc::clone(&tree.runs[&second].node);
        assert_eq!(before, tree.project(row_id));
        tree.model = state
            .apply(
                make_update(2, None, vec![row(second, 2, "C")]),
                crate::terminal::PresentationGeneration::test(2),
            )
            .unwrap()
            .as_ref()
            .clone();
        let after = tree.project(row_id);
        assert_eq!(before[0], after[0]);
        assert_eq!(before[1].0, after[1].0);
        assert!(Arc::ptr_eq(&retained, &tree.runs[&first].node));
        assert!(!Arc::ptr_eq(&changed, &tree.runs[&second].node));
        tree.model = state
            .apply(
                make_update(3, Some(vec![second]), vec![]),
                crate::terminal::PresentationGeneration::test(3),
            )
            .unwrap()
            .as_ref()
            .clone();
        assert_eq!(tree.project(row_id).len(), 1);
        assert_eq!(tree.runs.len(), 1);
    }

    #[gpui::test]
    fn accesskit_publication_retains_notifications_until_visible_and_retires_authority(
        cx: &mut gpui::TestAppContext,
    ) {
        let cx = cx.add_empty_window();
        let model = TerminalAccessibilityModel::new(
            vec![AccessibilityLine::new(vec![], false)],
            0..1,
            None,
        );
        let font = crate::appearance::ResolvedFontDescriptor {
            primary_family: "SpaceTerm Default".into(),
            fallback_families: vec![],
            size: 14.0,
            line_height: 18.0,
            weight: 400,
            style: crate::appearance::FontStyle::Normal,
            features: vec![],
            resolution_identity: "test".into(),
        };
        let mut adapter =
            AccessKitTerminalAccessibility(Rc::new(RefCell::new(pane_tree(model.clone()))));
        let mut notifications = AccessibilityNotifications::default();
        notifications.extend([
            crate::terminal::AccessibilityNotification::Value,
            crate::terminal::AccessibilityNotification::Selection,
            crate::terminal::AccessibilityNotification::Focus,
        ]);
        let (sender, _) = AccessibilitySelectionSender::recording_channel();
        let publish = |adapter: &mut AccessKitTerminalAccessibility, bounds, window: &Window| {
            adapter.update(TerminalAccessibilityUpdate {
                window,
                model: &model,
                bounds,
                cell_width: gpui::px(8.0),
                line_height: gpui::px(18.0),
                font: &font,
                font_size: gpui::px(14.0),
                focused: true,
                notifications,
                selection_sender: Some(sender.clone()),
                demand_sender: None,
            })
        };
        cx.update(|window, _| {
            assert_eq!(publish(&mut adapter, None, window), notifications);
            assert!(adapter.0.borrow().selection_sender.is_none());
            let bounds = Some(gpui::Bounds::new(
                gpui::point(gpui::px(10.0), gpui::px(20.0)),
                gpui::size(gpui::px(200.0), gpui::px(100.0)),
            ));
            assert!(publish(&mut adapter, bounds, window).is_empty());
            assert!(adapter.0.borrow().selection_sender.is_some());
            adapter.set_hierarchy(false, usize::MAX);
            assert_eq!(publish(&mut adapter, bounds, window), notifications);
            assert!(adapter.0.borrow().selection_sender.is_none());
            assert!(adapter.0.borrow().demand_sender.is_none());
        });
    }
}
