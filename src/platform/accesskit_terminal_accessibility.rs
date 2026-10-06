//! Terminal text publication through the Window's portable AccessKit tree.
use std::{
    cell::RefCell,
    collections::{BTreeSet, HashMap, HashSet},
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
                publication_has_same_topology: false,
            },
        ))))
    }
}

struct AccessKitTerminalAccessibility(Rc<RefCell<PaneTree>>);

impl TerminalAccessibilityAdapter for AccessKitTerminalAccessibility {
    fn set_hierarchy(&mut self, presented: bool) {
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
    text_width: f32,
    newline_x: f32,
    projection: Option<RunProjection>,
}

#[derive(Clone, Copy, PartialEq)]
struct RunProjection {
    node: NodeId,
    bounds: Rect,
    newline_x: Option<f32>,
}

struct PublishedRun {
    row: AccessibilityRowId,
    node: NodeId,
    offsets: Arc<[usize]>,
}

struct PublishedDocument {
    namespace: NodeId,
    geometry: Geometry,
    model: TerminalAccessibilityModel,
    runs: Vec<PublishedRun>,
    children: Arc<Vec<NodeId>>,
    lines_by_node: HashMap<NodeId, usize>,
}

impl PublishedDocument {
    fn position(&self, utf16: usize) -> Option<TextPosition> {
        if utf16 > self.model.len_utf16() {
            return None;
        }
        // At a hard line's end, use its newline character. At a soft wrap boundary,
        // use the following row. The final document boundary belongs to the last run.
        let line = self.model.line_for_index(utf16)?;
        let run = self.runs.get(line)?;
        let local = utf16.checked_sub(self.model.range_for_line(line)?.start)?;
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
        let line = *self.lines_by_node.get(&position.node)?;
        let run = self.runs.get(line)?;
        Some(self.model.range_for_line(line)?.start + *run.offsets.get(position.character_index)?)
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
    publication_has_same_topology: bool,
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

    fn project(
        &mut self,
        namespace: NodeId,
        id: impl Fn(AccessibilityRowId) -> NodeId,
    ) -> Vec<(NodeId, Node)> {
        let mut published = self.published.take();
        let changes = published.as_ref().and_then(|old| {
            (old.namespace == namespace)
                .then(|| self.model.changed_rows_since(&old.model))
                .flatten()
        });
        let topology_changed = changes.is_none();
        self.publication_has_same_topology = !topology_changed;
        let all_geometry_changed = published.as_ref().is_none_or(|old| {
            old.geometry != self.geometry
                || old.model.visible_lines().start != self.model.visible_lines().start
        });
        let mut lines = BTreeSet::new();
        if topology_changed || all_geometry_changed {
            lines.extend(0..self.model.rows().count());
        } else {
            for line in changes.unwrap_or_default() {
                lines.insert(line);
                if self.model.row(line + 1).is_some() {
                    lines.insert(line + 1);
                }
            }
            if let Some(old) = &published
                && old.model.cursor_cell() != self.model.cursor_cell()
            {
                lines.extend(old.model.cursor_cell().map(|(line, _)| line));
                lines.extend(self.model.cursor_cell().map(|(line, _)| line));
            }
        }
        let mut nodes = Vec::new();
        if topology_changed {
            published = Some(PublishedDocument {
                namespace,
                geometry: self.geometry,
                model: self.model.clone(),
                runs: Vec::new(),
                children: Arc::new(Vec::new()),
                lines_by_node: HashMap::new(),
            });
        }
        let document = published.as_mut().expect("a publication owns its document");
        let mut retained = HashSet::new();
        let metrics = (
            self.geometry.cell_width * self.geometry.scale,
            self.geometry.line_height * self.geometry.scale,
        );
        let visible_start = self.model.visible_lines().start;
        let cursor_cell = self.model.cursor_cell();
        let cursor = self.model.cursor_range().start;
        for line in lines {
            let Some(row) = self.model.row(line) else {
                continue;
            };
            if topology_changed {
                retained.insert(row.id);
            }
            let starts_word = line == 0
                || self.model.row(line - 1).is_none_or(|previous| {
                    !previous.soft_wrapped
                        || previous
                            .text
                            .chars()
                            .next_back()
                            .is_none_or(char::is_whitespace)
                });
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
            let node_id = id(row.id);
            let newline_x = (line_break && !row.text.is_empty()).then(|| {
                cursor_cell
                    .filter(|(cursor_line, _)| {
                        *cursor_line == line && cursor == row.range.start + row.len_utf16
                    })
                    .map_or(cached.newline_x, |(_, column)| {
                        f32::from(column) * metrics.0
                    })
            });
            let empty_cursor_column = cursor_cell
                .filter(|(cursor_line, _)| *cursor_line == line && row.text.is_empty())
                .map_or(0, |(_, column)| column);
            let x = f64::from(
                self.geometry.origin.0 * self.geometry.scale
                    + f32::from(empty_cursor_column) * metrics.0,
            );
            let y = f64::from(self.geometry.origin.1 * self.geometry.scale)
                + (line as f64 - visible_start as f64) * f64::from(metrics.1);
            let projection = RunProjection {
                node: node_id,
                bounds: Rect {
                    x0: x,
                    y0: y,
                    x1: x + f64::from(cached.text_width.max(newline_x.unwrap_or(0.0))),
                    y1: y + f64::from(metrics.1),
                },
                newline_x,
            };
            if cached.projection != Some(projection) {
                let node = Arc::make_mut(&mut cached.node);
                if let Some(newline_x) = newline_x {
                    let mut positions = node.character_positions().unwrap_or_default().to_vec();
                    if let Some(newline) = positions.last_mut() {
                        *newline = newline_x;
                        node.set_character_positions(positions);
                    }
                }
                node.set_bounds(projection.bounds);
                cached.projection = Some(projection);
                nodes.push((node_id, node.clone()));
            }
            let run = PublishedRun {
                row: row.id,
                node: node_id,
                offsets: Arc::clone(&cached.offsets),
            };
            if topology_changed {
                document.lines_by_node.insert(node_id, line);
                Arc::make_mut(&mut document.children).push(node_id);
                document.runs.push(run);
            } else {
                document.runs[line] = run;
            }
        }
        if topology_changed {
            self.runs.retain(|row_id, _| retained.contains(row_id));
        }
        document.geometry = self.geometry;
        document.model = self.model.clone();
        self.published = published;
        nodes
    }

    fn publish(&mut self, builder: &mut A11ySubtreeBuilder<'_>) {
        if self.visible
            && let Some(demand) = &self.demand_sender
        {
            demand.request();
        }
        let namespace = builder.synthetic_node_id(());
        let nodes = self.project(namespace, |row| builder.synthetic_node_id(row));
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
        if let Some(published) = &self.published {
            if self.publication_has_same_topology && builder.retain_children() {
                for (id, node) in nodes {
                    assert!(builder.update_child(id, node));
                }
            } else {
                for run in &published.runs {
                    builder.push_child(run.node, self.runs[&run.row].node.as_ref().clone());
                }
            }
            builder
                .parent_node()
                .set_shared_children(Arc::clone(&published.children));
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
    let text_width = positions
        .iter()
        .zip(&widths)
        .map(|(position, width)| position + width)
        .fold(0.0_f32, f32::max);
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
        text_width,
        newline_x: f32::from(last_column) * metrics.0,
        projection: None,
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
            publication_has_same_topology: false,
        }
    }

    fn full_project(
        tree: &mut PaneTree,
        namespace: NodeId,
        id: impl Fn(AccessibilityRowId) -> NodeId,
    ) -> Vec<(NodeId, Node)> {
        let mut nodes = Vec::new();
        let mut runs = Vec::new();
        let mut retained = HashSet::new();
        let metrics = (
            tree.geometry.cell_width * tree.geometry.scale,
            tree.geometry.line_height * tree.geometry.scale,
        );
        let visible_start = tree.model.visible_lines().start;
        let mut starts_word = true;
        for (line, row) in tree.model.rows().enumerate() {
            retained.insert(row.id);
            let line_break = !row.soft_wrapped && row.range.end - row.range.start > row.len_utf16;
            let cached = tree
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
                && tree.model.cursor_range().start == row.range.start + row.len_utf16
                && let Some((cursor_line, column)) = tree.model.cursor_cell()
                && cursor_line == line
            {
                let mut positions = node.character_positions().unwrap_or_default().to_vec();
                if let Some(newline) = positions.last_mut() {
                    *newline = f32::from(column) * metrics.0;
                    node.set_character_positions(positions);
                }
            }
            let empty_cursor_column = tree
                .model
                .cursor_cell()
                .filter(|(cursor_line, _)| *cursor_line == line && row.text.is_empty())
                .map_or(0, |(_, column)| column);
            let x = f64::from(
                tree.geometry.origin.0 * tree.geometry.scale
                    + f32::from(empty_cursor_column) * metrics.0,
            );
            let y = f64::from(tree.geometry.origin.1 * tree.geometry.scale)
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
                row: row.id,
                offsets: Arc::clone(&cached.offsets),
            });
        }
        tree.runs.retain(|row_id, _| retained.contains(row_id));
        let children = Arc::new(runs.iter().map(|run| run.node).collect());
        let lines_by_node = runs
            .iter()
            .enumerate()
            .map(|(line, run)| (run.node, line))
            .collect();
        tree.published = Some(PublishedDocument {
            namespace,
            geometry: tree.geometry,
            children,
            lines_by_node,
            model: tree.model.clone(),
            runs,
        });
        nodes
    }

    fn update(tree: &mut PaneTree) -> TreeUpdate {
        update_with(tree, false)
    }

    fn update_with(tree: &mut PaneTree, full: bool) -> TreeUpdate {
        let runs = if full {
            full_project(tree, NodeId(1), row_id)
        } else {
            tree.project(NodeId(1), row_id)
        };
        let mut root = Node::new(Role::Window);
        root.set_children([NodeId(1)]);
        let mut terminal = Node::new(Role::Terminal);
        terminal.set_label("Terminal Pane");
        terminal.set_shared_children(Arc::clone(&tree.published.as_ref().unwrap().children));
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

    struct IgnoreChanges;

    impl accesskit_consumer::TreeChangeHandler for IgnoreChanges {
        fn node_added(&mut self, _: &accesskit_consumer::Node<'_>) {}
        fn node_updated(
            &mut self,
            _: &accesskit_consumer::Node<'_>,
            _: &accesskit_consumer::Node<'_>,
        ) {
        }
        fn focus_moved(
            &mut self,
            _: Option<&accesskit_consumer::Node<'_>>,
            _: Option<&accesskit_consumer::Node<'_>>,
        ) {
        }
        fn node_removed(&mut self, _: &accesskit_consumer::Node<'_>) {}
    }

    struct RetainedFixture {
        state: crate::terminal::TerminalAccessibilityState,
        rows: Vec<crate::terminal::AccessibilityRowUpdate>,
        visible: std::ops::Range<usize>,
        cursor: Option<(usize, u16)>,
        selection: Option<(usize, usize)>,
        revision: u64,
    }

    impl RetainedFixture {
        fn new(count: usize) -> Self {
            use crate::terminal::{AccessibilityRowUpdate, AccessibilityScreen};
            Self {
                state: Default::default(),
                rows: (0..count)
                    .map(|line| AccessibilityRowUpdate {
                        id: AccessibilityRowId {
                            screen: AccessibilityScreen::Primary,
                            screen_generation: 1,
                            node_serial: 1 + line as u64 / 64,
                            page_row: (line % 64) as u16,
                        },
                        revision: 1,
                        soft_wrapped: false,
                        cells: (0..80)
                            .map(|_| AccessibilityCell::new("x", 1, false))
                            .collect(),
                    })
                    .collect(),
                visible: count.saturating_sub(20)..count,
                cursor: Some((count - 1, 80)),
                selection: None,
                revision: 0,
            }
        }

        fn snapshot(&mut self, topology: bool, changed: Vec<usize>) -> TerminalAccessibilityModel {
            use crate::terminal::{
                AccessibilityCellRef, AccessibilitySelectionRefs, AccessibilityUpdate,
                PresentationGeneration,
            };
            self.revision += 1;
            let reference = |line: usize, column| AccessibilityCellRef {
                row: self.rows[line].id,
                row_revision: self.rows[line].revision,
                column,
            };
            self.state
                .apply(
                    AccessibilityUpdate {
                        revision: self.revision,
                        screen: self.rows[0].id.screen,
                        screen_generation: self.rows[0].id.screen_generation,
                        complete: true,
                        more: false,
                        topology: topology.then(|| self.rows.iter().map(|row| row.id).collect()),
                        visible_lines: self.visible.clone(),
                        cursor: self.cursor.map(|(line, column)| reference(line, column)),
                        selection: self
                            .selection
                            .map(|(start, end)| AccessibilitySelectionRefs {
                                start: reference(start, 0),
                                end: reference(end, 1),
                                rectangle: false,
                            }),
                        changed_rows: changed
                            .into_iter()
                            .map(|line| self.rows[line].clone())
                            .collect(),
                    },
                    PresentationGeneration::test(self.revision),
                )
                .unwrap()
                .as_ref()
                .clone()
        }
    }

    fn assert_equivalent(incremental: &accesskit_consumer::Tree, full: &accesskit_consumer::Tree) {
        let incremental = incremental.state().root().children().next().unwrap();
        let full = full.state().root().children().next().unwrap();
        assert_eq!(incremental.data(), full.data());
        let left = incremental.children().collect::<Vec<_>>();
        let right = full.children().collect::<Vec<_>>();
        assert_eq!(left.len(), right.len());
        for (left, right) in left.iter().zip(&right) {
            assert_eq!(left.id(), right.id());
            assert_eq!(left.data(), right.data());
        }
        assert_eq!(
            incremental.document_range().text(),
            full.document_range().text()
        );
        assert_eq!(
            incremental.document_range().bounding_boxes(),
            full.document_range().bounding_boxes()
        );
        assert_eq!(
            incremental
                .text_selection()
                .map(|range| (range.text(), range.bounding_boxes())),
            full.text_selection()
                .map(|range| (range.text(), range.bounding_boxes()))
        );
    }

    #[test]
    fn accesskit_incremental_publication_matches_full_projection_random_mutations() {
        use crate::terminal::{AccessibilityRowUpdate, AccessibilityScreen};
        for seed in 1..=32_u64 {
            let mut random = seed;
            let mut next = || {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                random as usize
            };
            let mut fixture = RetainedFixture::new(48);
            let model = fixture.snapshot(true, (0..48).collect());
            let mut incremental = pane_tree(model.clone());
            let mut full = pane_tree(model);
            let mut incremental_consumer =
                accesskit_consumer::Tree::new(update(&mut incremental), true);
            let mut full_consumer =
                accesskit_consumer::Tree::new(update_with(&mut full, true), true);
            for _ in 0..128 {
                // Coalesced snapshots must diff against the last published document.
                for _ in 0..1 + next() % 3 {
                    let line = next() % fixture.rows.len();
                    let mut topology = false;
                    let mut changed = Vec::new();
                    match next() % 10 {
                        0 | 1 => {
                            let row = &mut fixture.rows[line];
                            row.revision += 1;
                            row.soft_wrapped = next() % 2 == 0;
                            row.cells = match next() % 5 {
                                0 => vec![],
                                1 => vec![
                                    AccessibilityCell::new("界", 2, false),
                                    AccessibilityCell::new("A\u{301}", 1, false),
                                ],
                                2 => vec![AccessibilityCell::new(
                                    format!("A{}", "\u{301}".repeat(200)),
                                    1,
                                    false,
                                )],
                                3 => vec![
                                    AccessibilityCell::new(" ", 1, false),
                                    AccessibilityCell::new("word", 4, false),
                                ],
                                _ => vec![AccessibilityCell::new("😀", 2, false)],
                            };
                            changed.push(line);
                        }
                        2 => fixture.cursor = Some((line, (next() % 100) as u16)),
                        3 => {
                            fixture.selection =
                                Some((line.min(fixture.rows.len() - 1), fixture.rows.len() - 1));
                        }
                        4 => {
                            fixture.selection = None;
                            fixture.cursor = None;
                        }
                        5 => {
                            fixture.visible = line..fixture.rows.len();
                        }
                        6 => {
                            incremental.geometry.cell_width = (1 + next() % 16) as f32;
                            incremental.geometry.scale = (1 + next() % 3) as f32;
                        }
                        7 => {
                            // Append output and trim the oldest row at the retention limit.
                            let mut row = fixture.rows.last().unwrap().clone();
                            row.id.node_serial += 1000;
                            row.revision += 1;
                            fixture.rows.push(row);
                            if fixture.rows.len() > 64 {
                                fixture.rows.remove(0);
                            }
                            topology = true;
                            changed.extend(0..fixture.rows.len());
                        }
                        8 => {
                            // Clear/reflow into a new screen generation, with primary/alternate transitions.
                            let screen = if next() % 2 == 0 {
                                AccessibilityScreen::Primary
                            } else {
                                AccessibilityScreen::Alternate
                            };
                            let generation = fixture.revision as usize + 2;
                            fixture.rows = (0..1 + next() % 32)
                                .map(|line| AccessibilityRowUpdate {
                                    id: AccessibilityRowId {
                                        screen,
                                        screen_generation: generation,
                                        node_serial: generation as u64,
                                        page_row: line as u16,
                                    },
                                    revision: 1,
                                    soft_wrapped: false,
                                    cells: vec![AccessibilityCell::new("reset", 5, false)],
                                })
                                .collect();
                            topology = true;
                            changed.extend(0..fixture.rows.len());
                        }
                        _ => {
                            incremental.geometry.origin.0 += 1.0;
                            incremental.geometry.line_height = (1 + next() % 20) as f32;
                        }
                    }
                    fixture.cursor = fixture
                        .cursor
                        .map(|(line, column)| (line.min(fixture.rows.len() - 1), column));
                    fixture.selection = fixture.selection.map(|(start, end)| {
                        (
                            start.min(fixture.rows.len() - 1),
                            end.min(fixture.rows.len() - 1),
                        )
                    });
                    fixture.visible =
                        fixture.visible.start.min(fixture.rows.len() - 1)..fixture.rows.len();
                    let model = fixture.snapshot(topology, changed);
                    incremental.model = model.clone();
                    full.model = model;
                    full.geometry = incremental.geometry;
                }
                let mut delta = update(&mut incremental);
                let mut complete = update_with(&mut full, true);
                if next() % 2 == 0 {
                    delta.focus = NodeId(0);
                    complete.focus = NodeId(0);
                }
                incremental_consumer.update_and_process_changes(delta, &mut IgnoreChanges);
                full_consumer.update_and_process_changes(complete, &mut IgnoreChanges);
                assert_equivalent(&incremental_consumer, &full_consumer);
                assert_eq!(
                    incremental_consumer.state().focus_id(),
                    full_consumer.state().focus_id()
                );
            }
        }
    }

    #[test]
    fn accesskit_large_history_frame_work_is_bounded_by_changed_rows() {
        for count in [100, 10_000] {
            let mut fixture = RetainedFixture::new(count);
            let mut tree = pane_tree(fixture.snapshot(true, (0..count).collect()));
            tree.project(NodeId(1), row_id);
            let project = |tree: &mut PaneTree, bound| {
                let visited = std::cell::Cell::new(0);
                let nodes = tree.project(NodeId(1), |id| {
                    visited.set(visited.get() + 1);
                    row_id(id)
                });
                assert!(
                    visited.get() <= bound,
                    "visited {} rows, bound {bound}",
                    visited.get()
                );
                assert!(nodes.len() <= bound);
            };
            project(&mut tree, 0);
            fixture.cursor = Some((count - 2, 99));
            tree.model = fixture.snapshot(false, vec![]);
            project(&mut tree, 2);
            fixture.rows[count / 2].revision += 1;
            fixture.rows[count / 2].cells = vec![AccessibilityCell::new("changed", 7, false)];
            tree.model = fixture.snapshot(false, vec![count / 2]);
            project(&mut tree, 2);
            fixture.selection = Some((0, count - 1));
            tree.model = fixture.snapshot(false, vec![]);
            project(&mut tree, 0);
        }
    }

    #[gpui::test]
    fn accesskit_retained_groups_rehydrate_after_retirement_and_parent_changes(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::{InteractiveElement, IntoElement, ParentElement, Styled};
        struct View {
            tree: Rc<RefCell<PaneTree>>,
            enabled: bool,
            identity: usize,
        }
        impl gpui::Render for View {
            fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
                let tree = Rc::clone(&self.tree);
                let root = gpui::div().size_full();
                if self.enabled {
                    root.child(
                        gpui::div()
                            .id(self.identity)
                            .role(Role::Terminal)
                            .size_full()
                            .a11y_synthetic_children(move |builder| {
                                tree.borrow_mut().publish(builder)
                            }),
                    )
                } else {
                    root
                }
            }
        }
        let mut fixture = RetainedFixture::new(32);
        let tree = Rc::new(RefCell::new(pane_tree(
            fixture.snapshot(true, (0..32).collect()),
        )));
        let (view, cx) = cx.add_window_view(|_, _| View {
            tree: Rc::clone(&tree),
            enabled: true,
            identity: 1,
        });
        cx.activate_accessibility();
        cx.run_until_parked();
        let values = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, _| {
                let dump: serde_json::Value =
                    serde_json::from_str(&window.debug_a11y_tree_json().unwrap()).unwrap();
                dump["nodes"]
                    .as_object()
                    .unwrap()
                    .values()
                    .filter(|node| node["aria"]["role"] == "TextRun")
                    .map(|node| node["aria"]["value"].as_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(values(cx).len(), 32);
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert_eq!(values(cx).len(), 32);
        fixture.rows[17].revision += 1;
        fixture.rows[17].cells = vec![AccessibilityCell::new("changed", 7, false)];
        tree.borrow_mut().model = fixture.snapshot(false, vec![17]);
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert!(values(cx).iter().any(|value| value == "changed\n"));
        cx.deactivate_accessibility();
        cx.run_until_parked();
        cx.activate_accessibility();
        cx.run_until_parked();
        assert_eq!(values(cx).len(), 32);
        view.update(cx, |view, cx| {
            view.enabled = false;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(values(cx).is_empty());
        view.update(cx, |view, cx| {
            view.enabled = true;
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(values(cx).len(), 32);
        view.update(cx, |view, cx| {
            view.identity = 2;
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(values(cx).len(), 32);
        assert!(values(cx).iter().any(|value| value == "changed\n"));
    }

    #[test]
    #[ignore = "manual measurement of full and incremental large-history publication"]
    fn accesskit_large_history_publication_measurement() {
        use std::{hint::black_box, time::Instant};
        let mut fixture = RetainedFixture::new(10_000);
        let initial = fixture.snapshot(true, (0..10_000).collect());
        let frames = 100;
        let measure = |name, models: [TerminalAccessibilityModel; 2]| {
            let mut full = pane_tree(models[0].clone());
            let mut incremental = pane_tree(models[0].clone());
            update_with(&mut full, true);
            update(&mut incremental);
            let run = |tree: &mut PaneTree, full: bool| {
                let start = Instant::now();
                for frame in 0..frames {
                    tree.model = models[frame % 2].clone();
                    black_box(update_with(tree, full));
                }
                start.elapsed().as_secs_f64() * 1_000_000.0 / frames as f64
            };
            let before = run(&mut full, true);
            let after = run(&mut incremental, false);
            eprintln!(
                "accesskit publication {name}, 10,000 rows x 80 cells, {frames} frames: full {before:.2} us/frame, incremental {after:.2} us/frame"
            );
        };
        measure("unchanged", [initial.clone(), initial.clone()]);
        fixture.cursor = Some((9_998, 79));
        let cursor = fixture.snapshot(false, vec![]);
        measure("cursor", [initial.clone(), cursor.clone()]);
        fixture.rows[5_000].revision += 1;
        fixture.rows[5_000].cells[0] = AccessibilityCell::new("y", 1, false);
        let changed = fixture.snapshot(false, vec![5_000]);
        measure("single row", [cursor, changed]);
    }

    #[test]
    fn accesskit_unchanged_large_history_publishes_no_rows() {
        let model = TerminalAccessibilityModel::new(
            (0..10_000)
                .map(|_| {
                    AccessibilityLine::new(
                        vec![AccessibilityCell::new("x".repeat(200), 200, false)],
                        false,
                    )
                })
                .collect(),
            9_980..10_000,
            Some((9_999, 200)),
        );
        let mut tree = pane_tree(model);
        let first = update(&mut tree);
        assert_eq!(first.nodes.len(), 10_002);
        let mut consumer = accesskit_consumer::Tree::new(first, true);
        let repeated = update(&mut tree);
        assert_eq!(repeated.nodes.len(), 2);
        assert!(
            tree.project(NodeId(1), |_| panic!(
                "unchanged rows must not be projected"
            ))
            .is_empty()
        );
        consumer.update_and_process_changes(repeated, &mut IgnoreChanges);
        let terminal = consumer.state().root().children().next().unwrap();
        assert_eq!(terminal.document_range().text(), tree.model.text());
        assert_eq!(terminal.children().count(), 10_000);
    }

    #[test]
    fn accesskit_incremental_rows_refresh_geometry_cursor_and_topology() {
        use crate::terminal::AccessibilityCellRef;
        use crate::terminal::{
            AccessibilityRowUpdate, AccessibilityScreen, AccessibilityUpdate,
            PresentationGeneration, TerminalAccessibilityState,
        };
        let first = AccessibilityRowId {
            screen: AccessibilityScreen::Primary,
            screen_generation: 1,
            node_serial: 123,
            page_row: 0,
        };
        let second = AccessibilityRowId {
            page_row: 1,
            ..first
        };
        let cursor = |row, column| {
            Some(AccessibilityCellRef {
                row,
                row_revision: 1,
                column,
            })
        };
        let generation = std::cell::Cell::new(0);
        let apply = |state: &mut TerminalAccessibilityState,
                     topology,
                     changed_rows,
                     visible_lines,
                     cursor| {
            generation.set(generation.get() + 1);
            state
                .apply(
                    AccessibilityUpdate {
                        revision: 1,
                        screen: AccessibilityScreen::Primary,
                        screen_generation: 1,
                        complete: true,
                        more: false,
                        topology,
                        visible_lines,
                        cursor,
                        selection: None,
                        changed_rows,
                    },
                    PresentationGeneration::test(generation.get()),
                )
                .unwrap()
                .as_ref()
                .clone()
        };
        let mut state = TerminalAccessibilityState::default();
        let model = apply(
            &mut state,
            Some(vec![first, second]),
            vec![
                AccessibilityRowUpdate {
                    id: first,
                    revision: 1,
                    soft_wrapped: false,
                    cells: vec![AccessibilityCell::new("A", 1, false)],
                },
                AccessibilityRowUpdate {
                    id: second,
                    revision: 1,
                    soft_wrapped: false,
                    cells: vec![],
                },
            ],
            0..2,
            cursor(first, 5),
        );
        let mut tree = pane_tree(model);
        let mut consumer = accesskit_consumer::Tree::new(update(&mut tree), true);
        let model = apply(&mut state, None, vec![], 0..2, cursor(second, 5));
        assert!(tree.model.shares_document(&model));
        tree.model = model;
        let moved = update(&mut tree);
        assert_eq!(moved.nodes.len(), 4);
        // Moving off a hard break restores its natural position; an empty row
        // follows the cursor's actual column even when it lies beyond its text.
        assert_eq!(
            moved.nodes[2].1.character_positions().unwrap(),
            &[0.0, 16.0]
        );
        assert_eq!(moved.nodes[3].1.bounds().unwrap().x0, 100.0);
        consumer.update_and_process_changes(moved, &mut IgnoreChanges);
        assert_eq!(
            consumer
                .state()
                .root()
                .children()
                .next()
                .unwrap()
                .text_selection()
                .unwrap()
                .bounding_boxes()[0]
                .x0,
            100.0
        );
        // A new presentation snapshot keeps unchanged row nodes and advances
        // the document authority used for incoming selection actions.
        tree.model = apply(&mut state, None, vec![], 0..2, cursor(second, 5));
        let unchanged = update(&mut tree);
        assert_eq!(unchanged.nodes.len(), 2);
        assert!(
            tree.model
                .shares_snapshot(&tree.published.as_ref().unwrap().model)
        );
        consumer.update_and_process_changes(unchanged, &mut IgnoreChanges);
        tree.model = apply(&mut state, None, vec![], 1..2, cursor(second, 5));
        let scrolled = update(&mut tree);
        assert_eq!(scrolled.nodes.len(), 4);
        assert_eq!(scrolled.nodes[3].1.bounds().unwrap().y0, 40.0);
        consumer.update_and_process_changes(scrolled, &mut IgnoreChanges);
        tree.geometry.origin.0 += 3.0;
        let relocated = update(&mut tree);
        assert_eq!(relocated.nodes.len(), 4);
        consumer.update_and_process_changes(relocated, &mut IgnoreChanges);
        tree.geometry.cell_width = 10.0;
        let resized = update(&mut tree);
        assert_eq!(resized.nodes.len(), 4);
        assert_eq!(resized.nodes[2].1.character_widths().unwrap(), &[20.0, 0.0]);
        consumer.update_and_process_changes(resized, &mut IgnoreChanges);
        tree.model = apply(
            &mut state,
            Some(vec![first]),
            vec![],
            0..1,
            cursor(first, 1),
        );
        let trimmed = update(&mut tree);
        assert_eq!(trimmed.nodes.len(), 3);
        consumer.update_and_process_changes(trimmed, &mut IgnoreChanges);
        let terminal = consumer.state().root().children().next().unwrap();
        assert_eq!(terminal.children().count(), 1);
        assert_eq!(terminal.document_range().text(), "A");
        assert_eq!(tree.runs.len(), 1);
        // A different synthetic parent owns different NodeIds, even when the
        // terminal model and its row identities are unchanged.
        let remapped = tree.project(NodeId(2), |row| NodeId(row_id(row).0 ^ 0x8000_0000));
        assert_eq!(remapped.len(), 1);
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
            anchor: published.position(5).unwrap(),
            focus: published.position(1).unwrap(),
        })));
        assert_eq!(
            receiver.drain(),
            vec![model.selection_request(1..5).unwrap()]
        );
        for (anchor, focus) in [
            (
                TextPosition {
                    character_index: usize::MAX,
                    ..published.position(1).unwrap()
                },
                published.position(5).unwrap(),
            ),
            (
                published.position(1).unwrap(),
                TextPosition {
                    character_index: usize::MAX,
                    ..published.position(5).unwrap()
                },
            ),
        ] {
            tree.select(Some(&ActionData::SetTextSelection(TextSelection {
                anchor,
                focus,
            })));
            assert!(receiver.drain().is_empty());
        }
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
        adapter.set_hierarchy(false);
        adapter.0.borrow().select(Some(&selection));
        assert!(receiver.drain().is_empty());
        assert!(adapter.0.borrow().selection_sender.is_none());
        assert!(adapter.0.borrow().published.is_none());
    }

    #[test]
    fn accesskit_native_mutations_match_full_projection() {
        use crate::terminal::geometry::{
            BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry,
        };
        let geometry = |columns, rows| {
            TerminalGeometry::from_grid(
                CellGridSize::new(columns, rows),
                LogicalCellSize::new(8.0, 18.0),
                BackingScale::ONE,
            )
        };
        let mut emulator =
            crate::terminal::testing::TerminalEmulator::new(geometry(20, 4)).unwrap();
        let capture = |emulator: &mut crate::terminal::testing::TerminalEmulator,
                       previous: Option<TerminalAccessibilityModel>| {
            emulator.snapshot().unwrap();
            let mut latest = previous;
            loop {
                let (model, more) = emulator
                    .accessibility_snapshot_for_current_presentation()
                    .unwrap();
                if let Some(model) = model {
                    latest = Some(model.as_ref().clone());
                }
                if !more {
                    return latest.unwrap();
                }
            }
        };
        emulator.feed(b"first\r\nsecond\r\nthird");
        let model = capture(&mut emulator, None);
        let mut incremental = pane_tree(model.clone());
        let mut full = pane_tree(model);
        let mut incremental_consumer =
            accesskit_consumer::Tree::new(update(&mut incremental), true);
        let mut full_consumer = accesskit_consumer::Tree::new(update_with(&mut full, true), true);
        let mut random = 0x1234_5678_u64;
        for step in 0..96 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            match random % 8 {
                0 => emulator.feed("界A\u{301}😀\r\n".as_bytes()),
                1 => emulator.feed(b"\x1b[2;3Hcursor\x1b[K"),
                2 => {
                    emulator.scroll_to(random % 20);
                }
                3 => {
                    emulator
                        .resize(geometry(10 + (random % 20) as u16, 3 + (random % 4) as u16))
                        .unwrap();
                }
                4 => {
                    emulator.clear_screen_and_scrollback();
                }
                5 => emulator.feed(b"\x1b[?1049hALT\x1b[?1049l"),
                6 => {
                    emulator.feed(b"\x1b[?1049h");
                    emulator.feed("alternate界".as_bytes());
                }
                _ => emulator.feed(b"\x1b[?1049l\r\noutput\r\n"),
            }
            if step == 48 {
                emulator.feed(b"\x1b[?1049l");
                emulator.feed("trim-row\r\n".repeat(10_100).as_bytes());
            }
            let model = capture(&mut emulator, Some(incremental.model.clone()));
            incremental.model = model.clone();
            full.model = model;
            incremental_consumer
                .update_and_process_changes(update(&mut incremental), &mut IgnoreChanges);
            full_consumer
                .update_and_process_changes(update_with(&mut full, true), &mut IgnoreChanges);
            assert_equivalent(&incremental_consumer, &full_consumer);
            if let Some(request) = incremental
                .model
                .range_for_index(0)
                .and_then(|range| incremental.model.selection_request(range))
            {
                emulator.set_accessibility_selection(request).unwrap();
                let model = capture(&mut emulator, Some(incremental.model.clone()));
                incremental.model = model.clone();
                full.model = model;
                incremental_consumer
                    .update_and_process_changes(update(&mut incremental), &mut IgnoreChanges);
                full_consumer
                    .update_and_process_changes(update_with(&mut full, true), &mut IgnoreChanges);
                assert_equivalent(&incremental_consumer, &full_consumer);
            }
        }
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
        let mut consumer = accesskit_consumer::Tree::new(update(&mut tree), true);

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
        consumer.update_and_process_changes(update(&mut tree), &mut IgnoreChanges);
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
        let before = tree.project(NodeId(1), row_id);
        let retained = Arc::clone(&tree.runs[&first].node);
        let changed = Arc::clone(&tree.runs[&second].node);
        assert!(tree.project(NodeId(1), row_id).is_empty());
        tree.model = state
            .apply(
                make_update(2, None, vec![row(second, 2, "C")]),
                crate::terminal::PresentationGeneration::test(2),
            )
            .unwrap()
            .as_ref()
            .clone();
        let after = tree.project(NodeId(1), row_id);
        assert_eq!(after.len(), 1);
        assert_eq!(before[1].0, after[0].0);
        assert_eq!(after[0].1.value(), Some("C"));
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
        assert_eq!(tree.project(NodeId(1), row_id).len(), 1);
        assert_eq!(tree.runs.len(), 1);
    }

    #[gpui::test]
    fn accesskit_publication_retains_notifications_until_visible_and_retires_selection_authority(
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
            adapter.set_hierarchy(false);
            assert_eq!(publish(&mut adapter, bounds, window), notifications);
            assert!(adapter.0.borrow().selection_sender.is_none());
        });
    }
}
