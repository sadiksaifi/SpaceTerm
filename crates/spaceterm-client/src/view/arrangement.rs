//! Personal Pane arrangements and personal Tab order.
//!
//! An arrangement is a flat node table in compact visual pre-order: index zero
//! is the root and every Split precedes its children. Validation, traversal,
//! mutation and destruction are iterative, so a maximal nested Split chain
//! cannot exhaust the stack. Arrangements and Tab order hold no Root authority:
//! personal moves, resizes, focus, zoom and activation never change a published
//! Root Tab or Root Pane, and nothing here creates shared organization,
//! acquires control or starts a Terminal Session.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::{Range, RangeInclusive};

use spaceterm_core::authority::CreationOrdinal;
use spaceterm_core::identity::{PaneId, TabId, WorkspaceId, WorktreeId};
use spaceterm_core::organization::{
    AcceptedPaneCreation, PaneCreation, PaneRecord, SplitAxis, SplitEdge, TabRecord,
};
use spaceterm_core::organization_retained::TabCreationDefault;

use super::types::{
    LayoutRevision, MAX_TAB_PANES, MAX_VIEW_TABS, SplitId, ViewCounter, ViewError, ViewResource,
};

/// Nodes in a complete binary arrangement of the maximum Pane count.
pub(crate) const MAX_TAB_NODES: usize = 2 * MAX_TAB_PANES - 1;
const DEFAULT_RATIO: f32 = 0.5;
/// The largest `f32` below one.
const MAX_RATIO: f32 = 1.0 - f32::EPSILON / 2.0;
const EDGE_EPSILON: f32 = 0.000_001;
const TAB_PANE_LIMIT: ViewError = ViewError::ResourceLimit {
    resource: ViewResource::TabPanes,
};
const TAB_LIMIT: ViewError = ViewError::ResourceLimit {
    resource: ViewResource::Tabs,
};

/// A finite, positive extent in presentation units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneSize {
    width: f32,
    height: f32,
}

impl PaneSize {
    pub fn new(width: f32, height: f32) -> Result<Self, ViewError> {
        if positive(width) && positive(height) {
            Ok(Self { width, height })
        } else {
            Err(ViewError::InvalidGeometry)
        }
    }

    pub const fn width(self) -> f32 {
        self.width
    }

    pub const fn height(self) -> f32 {
        self.height
    }

    fn extent(self, axis: SplitAxis) -> f32 {
        match axis {
            SplitAxis::Horizontal => self.width,
            SplitAxis::Vertical => self.height,
        }
    }

    fn holds(self, required: Self) -> bool {
        self.width >= required.width && self.height >= required.height
    }
}

/// Presentation constants supplied with each geometric operation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArrangementMetrics {
    pub minimum_leaf: PaneSize,
    /// Divider thickness. It must be finite and non-negative.
    pub divider: f32,
}

impl ArrangementMetrics {
    fn checked(self) -> Result<Self, ViewError> {
        if self.divider.is_finite() && self.divider >= 0.0 {
            Ok(self)
        } else {
            Err(ViewError::InvalidGeometry)
        }
    }

    /// The minimum holding `first` and `second` along `axis`, or None past `f32`.
    fn combine(self, first: PaneSize, second: PaneSize, axis: SplitAxis) -> Option<PaneSize> {
        let (width, height) = match axis {
            SplitAxis::Horizontal => (
                first.width + self.divider + second.width,
                first.height.max(second.height),
            ),
            SplitAxis::Vertical => (
                first.width.max(second.width),
                first.height + self.divider + second.height,
            ),
        };
        (width.is_finite() && height.is_finite()).then_some(PaneSize { width, height })
    }
}

/// The side of a target Pane that a placed Pane takes. Directional edges are
/// view-local; the Server records only a Split axis and a before/after edge.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PaneEdge {
    Left,
    Right,
    Top,
    Bottom,
}

impl PaneEdge {
    pub const fn from_split(axis: SplitAxis, edge: SplitEdge) -> Self {
        match (axis, edge) {
            (SplitAxis::Horizontal, SplitEdge::Before) => Self::Left,
            (SplitAxis::Horizontal, SplitEdge::After) => Self::Right,
            (SplitAxis::Vertical, SplitEdge::Before) => Self::Top,
            (SplitAxis::Vertical, SplitEdge::After) => Self::Bottom,
        }
    }

    pub const fn axis(self) -> SplitAxis {
        match self {
            Self::Left | Self::Right => SplitAxis::Horizontal,
            Self::Top | Self::Bottom => SplitAxis::Vertical,
        }
    }

    /// Whether the placed Pane precedes its target along the axis.
    pub const fn leads(self) -> bool {
        matches!(self, Self::Left | Self::Top)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FocusDirection {
    Left,
    Right,
    Up,
    Down,
}

/// A Zoomed Pane is always the Focused Pane of an arrangement with several Panes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ZoomState {
    Restored,
    Zoomed(PaneId),
}

/// One step through a personal sequence.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TabStep {
    Previous,
    Next,
}

/// One node of a [`CreationLayout`]. Child indices refer to the same table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreationNode {
    Pane(PaneId),
    Split {
        axis: SplitAxis,
        first: u32,
        second: u32,
    },
}

/// Temporary presentation adapter for one Tab's normalized surviving creation
/// default. It is not a shared layout: it has no Split identities or ratios.
/// Consumers validate it against exact Tab membership before use.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreationLayout {
    pub root: u32,
    pub nodes: Vec<CreationNode>,
}

impl CreationLayout {
    /// Replays the default's normalized order, splitting each surviving source.
    ///
    /// Creation ordinals never choose the replay order: deleting a source can
    /// rebase a later Pane ahead of an earlier one.
    pub fn from_default(
        default: &TabCreationDefault,
        panes: &[PaneRecord],
    ) -> Result<Self, ViewError> {
        if default.replay_order().len() > MAX_TAB_PANES {
            return Err(TAB_PANE_LIMIT);
        }
        default
            .validate(panes)
            .map_err(|_| ViewError::InvalidCreationTopology)?;
        let links: BTreeMap<PaneId, PaneCreation> = panes
            .iter()
            .filter(|pane| pane.tab == default.tab())
            .map(|pane| (pane.id, pane.creation))
            .collect();
        Self::replay(default.replay_order(), &links)
    }

    /// Replays a default against the exact creation links of its Tab's Panes,
    /// already selected by the caller. Equal counts plus a successful replay
    /// prove exact membership without scanning every Pane of the Server.
    pub(crate) fn from_tab_links(
        default: &TabCreationDefault,
        links: &BTreeMap<PaneId, PaneCreation>,
    ) -> Result<Self, ViewError> {
        let order = default.replay_order();
        if order.len() > MAX_TAB_PANES || links.len() > MAX_TAB_PANES {
            return Err(TAB_PANE_LIMIT);
        }
        if links.len() != order.len() {
            return Err(ViewError::InvalidCreationTopology);
        }
        Self::replay(order, links)
    }

    fn replay(order: &[PaneId], links: &BTreeMap<PaneId, PaneCreation>) -> Result<Self, ViewError> {
        let mut nodes = Vec::with_capacity((2 * order.len()).saturating_sub(1));
        let mut leaves = BTreeMap::new();
        for &pane in order {
            if leaves.contains_key(&pane) {
                return Err(ViewError::InvalidCreationTopology);
            }
            match links.get(&pane) {
                Some(PaneCreation::Initial) if nodes.is_empty() => {
                    leaves.insert(pane, slot(nodes.len()));
                    nodes.push(CreationNode::Pane(pane));
                }
                Some(&PaneCreation::SplitOf { source, axis, edge }) => {
                    let at = *leaves
                        .get(&source)
                        .ok_or(ViewError::InvalidCreationTopology)?;
                    let kept = slot(nodes.len());
                    let added = slot(nodes.len() + 1);
                    nodes.push(CreationNode::Pane(source));
                    nodes.push(CreationNode::Pane(pane));
                    let (first, second) = match edge {
                        SplitEdge::Before => (added, kept),
                        SplitEdge::After => (kept, added),
                    };
                    nodes[at as usize] = CreationNode::Split {
                        axis,
                        first,
                        second,
                    };
                    leaves.insert(source, kept);
                    leaves.insert(pane, added);
                }
                _ => return Err(ViewError::InvalidCreationTopology),
            }
        }
        if nodes.is_empty() {
            return Err(ViewError::InvalidCreationTopology);
        }
        Ok(Self { root: 0, nodes })
    }
}

/// Original placement of one accepted Pane creation. It applies only to the
/// snapshot of the publication that accepted it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PanePlacement {
    pub pane: PaneId,
    pub source: PaneId,
    pub edge: PaneEdge,
    pub ordinal: CreationOrdinal,
}

impl PanePlacement {
    pub const fn from_accepted(creation: AcceptedPaneCreation) -> Self {
        Self {
            pane: creation.pane(),
            source: creation.source(),
            edge: PaneEdge::from_split(creation.axis(), creation.edge()),
            ordinal: creation.ordinal(),
        }
    }
}

/// One flat arrangement node. Child indices refer to the same node table.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ArrangementNode {
    Pane(PaneId),
    Split {
        id: SplitId,
        axis: SplitAxis,
        ratio: f32,
        first: u32,
        second: u32,
    },
}

impl ArrangementNode {
    const fn pane(&self) -> Option<PaneId> {
        match *self {
            Self::Pane(pane) => Some(pane),
            Self::Split { .. } => None,
        }
    }
}

/// Flat retained form of one arrangement. Import validates every field before
/// the record becomes live state. Layout revisions are not retained.
#[derive(Clone, Debug, PartialEq)]
pub struct ArrangementRecord {
    pub root: u32,
    pub nodes: Vec<ArrangementNode>,
    /// The last Split ID issued, or zero before any.
    pub split_high_water: u64,
    pub focused: PaneId,
    /// Panes this client focused, each once, least to most recently focused.
    /// It omits Panes never focused here and need not contain `focused`.
    pub focus_history: Vec<PaneId>,
    pub zoomed: Option<PaneId>,
}

/// Structural result of applying authoritative membership to an arrangement.
/// Fallback focus is restoration, never explicit selection.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArrangementChange {
    /// Removed Panes in prior visual order.
    pub removed: Vec<PaneId>,
    /// Inserted Panes in insertion order.
    pub inserted: Vec<PaneId>,
    pub focus_changed: bool,
    pub zoom_cleared: bool,
}

impl ArrangementChange {
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.inserted.is_empty()
    }
}

/// Read-only view of one arrangement subtree.
#[derive(Clone, Copy, Debug)]
pub struct PaneTreeRef<'a> {
    nodes: &'a [ArrangementNode],
    index: u32,
}

impl<'a> PaneTreeRef<'a> {
    pub fn node(self) -> PaneNodeRef<'a> {
        match self.nodes[self.index as usize] {
            ArrangementNode::Pane(pane) => PaneNodeRef::Pane(pane),
            ArrangementNode::Split {
                id,
                axis,
                ratio,
                first,
                second,
            } => PaneNodeRef::Split {
                id,
                axis,
                ratio,
                first: Self {
                    nodes: self.nodes,
                    index: first,
                },
                second: Self {
                    nodes: self.nodes,
                    index: second,
                },
            },
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum PaneNodeRef<'a> {
    Pane(PaneId),
    Split {
        id: SplitId,
        axis: SplitAxis,
        ratio: f32,
        first: PaneTreeRef<'a>,
        second: PaneTreeRef<'a>,
    },
}

/// One client's placement of a Tab's shared Panes, Split axes and ratios, focus
/// and zoom. Each represented Pane occurs exactly once and belongs to the Tab's
/// Server. Failed operations leave the tree, IDs, focus, zoom and layout
/// revision unchanged.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneArrangement {
    tab: TabId,
    /// Compact pre-order. Index zero is the root.
    nodes: Vec<ArrangementNode>,
    /// The last Split ID issued, or zero before any.
    split_high_water: u64,
    focused: PaneId,
    /// Panes this client focused, each once, least to most recently focused.
    /// Explicit, initial and fallback focus record a Pane; history cycling does
    /// not, so the Focused Pane can be absent.
    focus_history: Vec<PaneId>,
    zoom: ZoomState,
    /// Advances with every geometry change. It is not retained.
    layout: LayoutRevision,
}

impl PaneArrangement {
    /// Clones a creation layout with fresh Split IDs and default ratios.
    /// Initial focus is restoration, never explicit use.
    pub fn from_creation(
        tab: TabId,
        layout: &CreationLayout,
        initial_focus: PaneId,
    ) -> Result<Self, ViewError> {
        let shape = CreationShape::new(tab, layout)?;
        if initial_focus.server() != tab.server() {
            return Err(ViewError::WrongServer);
        }
        if !shape.leaves.contains(&initial_focus) {
            return Err(ViewError::TargetMissing);
        }
        let (nodes, split_high_water) = clone_creation(&layout.nodes, &shape.order, 0, 0)?;
        Ok(Self {
            tab,
            nodes,
            split_high_water,
            focused: initial_focus,
            focus_history: vec![initial_focus],
            zoom: ZoomState::Restored,
            layout: LayoutRevision::INITIAL,
        })
    }

    /// Validates a retained record for `tab` and normalizes it to pre-order.
    pub fn import(tab: TabId, record: &ArrangementRecord) -> Result<Self, ViewError> {
        let invalid = ViewError::InvalidPersistedState;
        if record.focus_history.len() > MAX_TAB_PANES {
            return Err(TAB_PANE_LIMIT);
        }
        let walk = walk(record.nodes.len(), record.root, |index| {
            match record.nodes[index] {
                ArrangementNode::Pane(pane) => Step::Pane(pane),
                ArrangementNode::Split { first, second, .. } => Step::Split(first, second),
            }
        })
        .map_err(|fault| fault.error(invalid))?;
        if walk.leaves.iter().any(|pane| pane.server() != tab.server()) {
            return Err(invalid);
        }
        let mut ids = BTreeSet::new();
        for node in &record.nodes {
            if let ArrangementNode::Split { id, ratio, .. } = *node
                && (!valid_ratio(ratio) || id.get() > record.split_high_water || !ids.insert(id))
            {
                return Err(invalid);
            }
        }
        let leaves: BTreeSet<PaneId> = walk.leaves.iter().copied().collect();
        let mut history = BTreeSet::new();
        if !leaves.contains(&record.focused)
            || record
                .focus_history
                .iter()
                .any(|pane| !leaves.contains(pane) || !history.insert(*pane))
        {
            return Err(invalid);
        }
        let zoom = match record.zoomed {
            None => ZoomState::Restored,
            Some(pane) if pane == record.focused && leaves.len() > 1 => ZoomState::Zoomed(pane),
            Some(_) => return Err(invalid),
        };
        Ok(Self {
            tab,
            nodes: compact_in(&record.nodes, &walk.order),
            split_high_water: record.split_high_water,
            focused: record.focused,
            focus_history: record.focus_history.clone(),
            zoom,
            layout: LayoutRevision::INITIAL,
        })
    }

    pub fn export(&self) -> ArrangementRecord {
        ArrangementRecord {
            root: 0,
            nodes: self.nodes.clone(),
            split_high_water: self.split_high_water,
            focused: self.focused,
            focus_history: self.focus_history.clone(),
            zoomed: match self.zoom {
                ZoomState::Zoomed(pane) => Some(pane),
                ZoomState::Restored => None,
            },
        }
    }

    pub const fn tab(&self) -> TabId {
        self.tab
    }

    pub fn tree(&self) -> PaneTreeRef<'_> {
        PaneTreeRef {
            nodes: &self.nodes,
            index: 0,
        }
    }

    /// Always present: an arrangement holds at least one Pane.
    pub const fn focused_pane(&self) -> Option<PaneId> {
        Some(self.focused)
    }

    pub const fn zoom(&self) -> ZoomState {
        self.zoom
    }

    pub fn focus_history(&self) -> &[PaneId] {
        &self.focus_history
    }

    /// Panes in visual order.
    pub fn pane_ids(&self) -> Vec<PaneId> {
        self.leaves().collect()
    }

    pub fn pane_count(&self) -> usize {
        self.nodes.len().div_ceil(2)
    }

    pub fn contains_pane(&self, pane: PaneId) -> bool {
        position_of(&self.nodes, pane).is_some()
    }

    pub const fn layout_revision(&self) -> LayoutRevision {
        self.layout
    }

    pub const fn split_high_water(&self) -> u64 {
        self.split_high_water
    }

    /// Explicitly focuses a Pane and reports whether the Focused Pane changed.
    /// Focus moves the Pane to the end of history and carries zoom with it.
    pub fn focus_pane(&mut self, pane: PaneId) -> Result<bool, ViewError> {
        self.member(pane)?;
        let changed = pane != self.focused;
        self.set_focus(pane, true)?;
        Ok(changed)
    }

    /// Focuses the nearest visual neighbor in `direction`, without wrapping.
    pub fn focus_in_direction(
        &mut self,
        direction: FocusDirection,
    ) -> Result<Option<PaneId>, ViewError> {
        let Some(pane) = self.pane_in_direction(direction) else {
            return Ok(None);
        };
        self.set_focus(pane, true)?;
        Ok(Some(pane))
    }

    /// Cycles toward older entries of the focus cycle without reordering
    /// history. A zoom transfer that would exhaust the layout revision leaves
    /// focus unchanged.
    pub fn focus_previous(&mut self) -> Option<PaneId> {
        self.focus_in_history(TabStep::Previous)
    }

    /// Cycles toward newer entries of the focus cycle without reordering
    /// history. A zoom transfer that would exhaust the layout revision leaves
    /// focus unchanged.
    pub fn focus_next(&mut self) -> Option<PaneId> {
        self.focus_in_history(TabStep::Next)
    }

    /// Zooms the Focused Pane or restores the full layout. A single Pane never
    /// zooms, and an exhausted layout revision refuses the change.
    pub fn toggle_zoom(&mut self) -> bool {
        if self.pane_count() <= 1 {
            return false;
        }
        let Ok(layout) = next_layout(self.layout) else {
            return false;
        };
        self.zoom = match self.zoom {
            ZoomState::Restored => ZoomState::Zoomed(self.focused),
            ZoomState::Zoomed(_) => ZoomState::Restored,
        };
        self.layout = layout;
        true
    }

    /// Moves a Pane beside `target`, splitting the target so the Pane takes
    /// `edge`, and reports whether the arrangement changed.
    ///
    /// A changed move focuses the moved Pane and restores the full layout. A
    /// move that rebuilds the same arrangement changes nothing and keeps every
    /// Split ID and ratio.
    pub fn move_pane(
        &mut self,
        pane: PaneId,
        target: PaneId,
        edge: PaneEdge,
        target_size: PaneSize,
        metrics: ArrangementMetrics,
    ) -> Result<bool, ViewError> {
        self.member(pane)?;
        self.check_receive(target, edge, target_size, metrics)?;
        if pane == target {
            return Ok(false);
        }
        let id = self.next_split_id()?;
        let mut nodes = self.nodes.clone();
        remove_leaves(&mut nodes, &BTreeSet::from([pane]));
        let mut nodes = compact(&nodes);
        let at = position_of(&nodes, target).ok_or(ViewError::TargetMissing)?;
        let added = slot(nodes.len());
        nodes.push(ArrangementNode::Pane(pane));
        split_leaf(&mut nodes, at, added, id, edge.axis(), edge.leads());
        let nodes = compact(&nodes);
        if same_structure(&nodes, &self.nodes) {
            return Ok(false);
        }
        let layout = next_layout(self.layout)?;
        self.nodes = nodes;
        self.split_high_water = id.get();
        self.zoom = ZoomState::Restored;
        self.record_focus(pane);
        self.layout = layout;
        Ok(true)
    }

    /// Whether a Pane moved onto `edge` of `target` fits within `target_size`.
    pub fn can_receive_pane(
        &self,
        target: PaneId,
        edge: PaneEdge,
        target_size: PaneSize,
        metrics: ArrangementMetrics,
    ) -> bool {
        self.check_receive(target, edge, target_size, metrics)
            .is_ok()
    }

    /// Changes one Split's axis within its `available` region and clamps its
    /// ratio into the new axis range. Reports whether the axis changed.
    pub fn set_split_axis(
        &mut self,
        split: SplitId,
        axis: SplitAxis,
        available: PaneSize,
        metrics: ArrangementMetrics,
    ) -> Result<bool, ViewError> {
        let index = self.split_position(split)?;
        let metrics = metrics.checked()?;
        let ArrangementNode::Split {
            id,
            axis: current,
            ratio,
            first,
            second,
        } = self.nodes[index]
        else {
            return Err(ViewError::TargetMissing);
        };
        if current == axis {
            return Ok(false);
        }
        let range = self.ratio_range(axis, first, second, available, metrics)?;
        let ratio = ratio.max(*range.start()).min(*range.end());
        let layout = next_layout(self.layout)?;
        self.nodes[index] = ArrangementNode::Split {
            id,
            axis,
            ratio,
            first,
            second,
        };
        self.layout = layout;
        Ok(true)
    }

    /// Sets a Split ratio, clamped so each side keeps its recursive minimum
    /// within `available`, and returns the applied ratio.
    pub fn resize_split(
        &mut self,
        split: SplitId,
        available: PaneSize,
        metrics: ArrangementMetrics,
        requested_ratio: f32,
    ) -> Result<f32, ViewError> {
        if !requested_ratio.is_finite() {
            return Err(ViewError::InvalidRatio);
        }
        let range = self.split_ratio_range(split, available, metrics)?;
        let ratio = requested_ratio.max(*range.start()).min(*range.end());
        let index = self.split_position(split)?;
        let ArrangementNode::Split { ratio: current, .. } = self.nodes[index] else {
            return Err(ViewError::TargetMissing);
        };
        if current.to_bits() == ratio.to_bits() {
            return Ok(ratio);
        }
        let layout = next_layout(self.layout)?;
        if let ArrangementNode::Split { ratio: stored, .. } = &mut self.nodes[index] {
            *stored = ratio;
        }
        self.layout = layout;
        Ok(ratio)
    }

    /// The ratios a Split may take while each side keeps its recursive minimum.
    pub fn split_ratio_range(
        &self,
        split: SplitId,
        available: PaneSize,
        metrics: ArrangementMetrics,
    ) -> Result<RangeInclusive<f32>, ViewError> {
        let index = self.split_position(split)?;
        let metrics = metrics.checked()?;
        let ArrangementNode::Split {
            axis,
            first,
            second,
            ..
        } = self.nodes[index]
        else {
            return Err(ViewError::TargetMissing);
        };
        self.ratio_range(axis, first, second, available, metrics)
    }

    /// The smallest size that holds the full, unzoomed arrangement.
    pub fn minimum_size(&self, metrics: ArrangementMetrics) -> Result<PaneSize, ViewError> {
        let metrics = metrics.checked()?;
        self.minima(metrics)
            .first()
            .copied()
            .flatten()
            .ok_or(ViewError::InvalidGeometry)
    }

    #[cfg(test)]
    pub(crate) fn set_layout_revision_for_test(&mut self, layout: LayoutRevision) {
        self.layout = layout;
    }

    fn leaves(&self) -> impl Iterator<Item = PaneId> + '_ {
        self.nodes.iter().filter_map(ArrangementNode::pane)
    }

    fn member(&self, pane: PaneId) -> Result<usize, ViewError> {
        if pane.server() != self.tab.server() {
            return Err(ViewError::WrongServer);
        }
        position_of(&self.nodes, pane).ok_or(ViewError::TargetMissing)
    }

    fn split_position(&self, split: SplitId) -> Result<usize, ViewError> {
        self.nodes
            .iter()
            .position(|node| matches!(*node, ArrangementNode::Split { id, .. } if id == split))
            .ok_or(ViewError::TargetMissing)
    }

    fn next_split_id(&self) -> Result<SplitId, ViewError> {
        self.split_high_water
            .checked_add(1)
            .and_then(SplitId::new)
            .ok_or(ViewCounter::SplitId.exhausted())
    }

    fn check_receive(
        &self,
        target: PaneId,
        edge: PaneEdge,
        target_size: PaneSize,
        metrics: ArrangementMetrics,
    ) -> Result<(), ViewError> {
        self.member(target)?;
        let metrics = metrics.checked()?;
        let required = metrics
            .combine(metrics.minimum_leaf, metrics.minimum_leaf, edge.axis())
            .ok_or(ViewError::InvalidGeometry)?;
        if target_size.holds(required) {
            Ok(())
        } else {
            Err(ViewError::InsufficientSpace)
        }
    }

    /// Focuses `pane`, transferring zoom. Explicit focus records history;
    /// history cycling keeps its order.
    fn set_focus(&mut self, pane: PaneId, record: bool) -> Result<(), ViewError> {
        let layout = match self.zoom {
            ZoomState::Zoomed(zoomed) if zoomed != pane => Some(next_layout(self.layout)?),
            _ => None,
        };
        if record {
            self.record_focus(pane);
        } else {
            self.focused = pane;
        }
        if let Some(layout) = layout {
            self.zoom = ZoomState::Zoomed(pane);
            self.layout = layout;
        }
        Ok(())
    }

    fn record_focus(&mut self, pane: PaneId) {
        self.focus_history.retain(|candidate| *candidate != pane);
        self.focus_history.push(pane);
        self.focused = pane;
    }

    fn focus_in_history(&mut self, step: TabStep) -> Option<PaneId> {
        let cycle = self.focus_cycle();
        let len = cycle.len();
        if len <= 1 {
            return None;
        }
        let current = cycle.iter().position(|pane| *pane == self.focused)?;
        let next = match step {
            TabStep::Previous => current.checked_sub(1).unwrap_or(len - 1),
            TabStep::Next => (current + 1) % len,
        };
        let pane = cycle[next];
        self.set_focus(pane, false).ok()?;
        Some(pane)
    }

    /// Every Pane once, least to most recent: Panes never focused here in
    /// reverse visual order, then focus history. Panes inserted from the
    /// replica start unfocused, so Previous and Next Pane reach them this way.
    /// The first unfocused visual leaf is the most recent of them.
    fn focus_cycle(&self) -> Vec<PaneId> {
        let focused: BTreeSet<PaneId> = self.focus_history.iter().copied().collect();
        let mut cycle: Vec<PaneId> = self
            .leaves()
            .filter(|pane| !focused.contains(pane))
            .collect();
        cycle.reverse();
        cycle.extend_from_slice(&self.focus_history);
        cycle
    }

    fn ratio_range(
        &self,
        axis: SplitAxis,
        first: u32,
        second: u32,
        available: PaneSize,
        metrics: ArrangementMetrics,
    ) -> Result<RangeInclusive<f32>, ViewError> {
        let minima = self.minima(metrics);
        let (Some(first), Some(second)) = (minima[first as usize], minima[second as usize]) else {
            return Err(ViewError::InvalidGeometry);
        };
        let required = metrics
            .combine(first, second, axis)
            .ok_or(ViewError::InvalidGeometry)?;
        // This is the f32 check that `minimum_size` advertises, and it alone
        // decides whether the children fit. Every size it accepts has a
        // nonempty range of valid ratios.
        if !available.holds(required) {
            return Err(ViewError::InsufficientSpace);
        }
        // Bounds are computed in f64 and each rounded to f32 once. Rounding is
        // monotonic, so bounds that are ordered in f64 stay ordered, and
        // exactly fitting children give a one-ratio range.
        let content = f64::from(available.extent(axis)) - f64::from(metrics.divider);
        let first = f64::from(first.extent(axis));
        let second = f64::from(second.extent(axis));
        let (low, high) = (first / content, (content - second) / content);
        // The f32 check can round an overfull sum down, by any amount relative
        // to `content` when the divider dwarfs the children. Children that
        // overlap in f64 share the content in proportion to their minima.
        let (low, high) = if content > 0.0 && low <= high {
            (low, high)
        } else {
            let share = first / (first + second);
            (share, share)
        };
        Ok(rounded_ratio(low)..=rounded_ratio(high))
    }

    /// Recursive minimum of every node, or None where it exceeds `f32`.
    /// Children follow their Split, so a reverse scan is a post-order walk.
    fn minima(&self, metrics: ArrangementMetrics) -> Vec<Option<PaneSize>> {
        let mut minima = vec![None; self.nodes.len()];
        for index in (0..self.nodes.len()).rev() {
            minima[index] = match self.nodes[index] {
                ArrangementNode::Pane(_) => Some(metrics.minimum_leaf),
                ArrangementNode::Split {
                    axis,
                    first,
                    second,
                    ..
                } => match (minima[first as usize], minima[second as usize]) {
                    (Some(first), Some(second)) => metrics.combine(first, second, axis),
                    _ => None,
                },
            };
        }
        minima
    }

    /// The nearest other Pane in `direction`. The source is never a candidate.
    fn pane_in_direction(&self, direction: FocusDirection) -> Option<PaneId> {
        let leaves = self.leaf_bounds();
        let focused = leaves
            .iter()
            .find_map(|(pane, bounds)| (*pane == self.focused).then_some(*bounds))?;
        leaves
            .into_iter()
            .filter(|(pane, _)| *pane != self.focused)
            .filter_map(|(pane, bounds)| {
                directional_score(focused, bounds, direction).map(|score| (pane, score))
            })
            .min_by(|(left, left_score), (right, right_score)| {
                left_score
                    .compare(*right_score)
                    .then_with(|| left.cmp(right))
            })
            .map(|(pane, _)| pane)
    }

    /// Normalized visual bounds of every leaf. Parents precede children.
    fn leaf_bounds(&self) -> Vec<(PaneId, Bounds)> {
        let mut regions = vec![Bounds::ROOT; self.nodes.len()];
        let mut leaves = Vec::with_capacity(self.pane_count());
        for (index, node) in self.nodes.iter().enumerate() {
            let region = regions[index];
            match *node {
                ArrangementNode::Pane(pane) => leaves.push((pane, region)),
                ArrangementNode::Split {
                    axis,
                    ratio,
                    first,
                    second,
                    ..
                } => {
                    let (leading, trailing) = region.split(axis, ratio);
                    regions[first as usize] = leading;
                    regions[second as usize] = trailing;
                }
            }
        }
        leaves
    }
}

/// Crate-private structural reconciliation. These operations follow the
/// authoritative replica; they are never local create or close actions.
impl PaneArrangement {
    /// Removes Panes that are no longer members without inserting new ones.
    /// `None` means no Pane would survive; the caller drops the arrangement.
    pub(crate) fn prune(
        &mut self,
        surviving: impl Fn(PaneId) -> bool,
    ) -> Result<Option<ArrangementChange>, ViewError> {
        let removed: Vec<PaneId> = self.leaves().filter(|pane| !surviving(*pane)).collect();
        if removed.is_empty() {
            return Ok(Some(ArrangementChange::default()));
        }
        if removed.len() == self.pane_count() {
            return Ok(None);
        }
        let layout = next_layout(self.layout)?;
        let mut next = self.clone();
        let Some(mut change) = next.remove_panes(&removed) else {
            return Ok(None);
        };
        change.zoom_cleared |= next.settle_zoom();
        next.layout = layout;
        *self = next;
        Ok(Some(change))
    }

    /// Applies exact Tab membership from a validated complete replica.
    ///
    /// Removed leaves collapse only their parent Split. New Panes split their
    /// accepted source's personal leaf; without a placed source, the
    /// lowest-ordinal missing Pane brings its missing creation subtree beside the
    /// lowest-ordinal placed leaf under its nearest creation ancestor. Surviving
    /// Split IDs, ratios, focus and zoom are preserved unless removal forces a
    /// fallback. With no survivor the layout is cloned with fresh Split IDs.
    /// Unchanged membership is idempotent.
    pub(crate) fn reconcile(
        &mut self,
        layout: &CreationLayout,
        placements: &[PanePlacement],
        ordinal: impl Fn(PaneId) -> Option<CreationOrdinal>,
    ) -> Result<ArrangementChange, ViewError> {
        let target = CreationShape::new(self.tab, layout)?;
        let members: BTreeSet<PaneId> = target.leaves.iter().copied().collect();
        let present: BTreeSet<PaneId> = self.leaves().collect();
        let removed: Vec<PaneId> = self
            .leaves()
            .filter(|pane| !members.contains(pane))
            .collect();
        let missing = members.difference(&present).count();
        if removed.is_empty() && missing == 0 {
            return Ok(ArrangementChange::default());
        }
        // Each inserted Pane adds exactly one Split, so both counters are
        // checked before any effect.
        let revision = next_layout(self.layout)?;
        u64::try_from(missing)
            .ok()
            .and_then(|count| self.split_high_water.checked_add(count))
            .ok_or(ViewCounter::SplitId.exhausted())?;
        let mut next = self.clone();
        let change = match next.remove_panes(&removed) {
            Some(mut change) => {
                change.inserted = next.insert_missing(&target, layout, placements, &ordinal)?;
                change.zoom_cleared |= next.settle_zoom();
                change
            }
            None => next.rebuild(&target, layout, removed)?,
        };
        next.layout = revision;
        *self = next;
        Ok(change)
    }

    /// Removes leaves, repairs focus and clears the zoom of a removed Pane.
    /// Returns None, unchanged, when no leaf would survive. The one-Pane zoom
    /// rule waits for [`Self::settle_zoom`] on the completed result, because a
    /// resynchronized snapshot can remove one Pane and insert another.
    fn remove_panes(&mut self, removed: &[PaneId]) -> Option<ArrangementChange> {
        let removed_set: BTreeSet<PaneId> = removed.iter().copied().collect();
        let first_survivor = self.leaves().find(|pane| !removed_set.contains(pane))?;
        // History holds only Panes focused here, so a Pane that was merely
        // observed never outranks the first surviving visual leaf.
        let fallback = removed_set.contains(&self.focused).then(|| {
            self.sibling_fallback(&removed_set)
                .or_else(|| {
                    self.focus_history
                        .iter()
                        .rev()
                        .copied()
                        .find(|pane| !removed_set.contains(pane))
                })
                .unwrap_or(first_survivor)
        });
        remove_leaves(&mut self.nodes, &removed_set);
        self.nodes = compact(&self.nodes);
        self.focus_history
            .retain(|pane| !removed_set.contains(pane));
        if let Some(pane) = fallback {
            self.record_focus(pane);
        }
        let zoom_cleared =
            matches!(self.zoom, ZoomState::Zoomed(pane) if removed_set.contains(&pane));
        if zoom_cleared {
            self.zoom = ZoomState::Restored;
        }
        Some(ArrangementChange {
            removed: removed.to_vec(),
            inserted: Vec::new(),
            focus_changed: fallback.is_some(),
            zoom_cleared,
        })
    }

    /// Restores a completed one-Pane arrangement, which is never zoomed, and
    /// reports whether that cleared zoom.
    fn settle_zoom(&mut self) -> bool {
        let cleared = self.pane_count() == 1 && matches!(self.zoom, ZoomState::Zoomed(_));
        if cleared {
            self.zoom = ZoomState::Restored;
        }
        cleared
    }

    /// The nearest surviving leaf of the Focused Pane's sibling subtree.
    fn sibling_fallback(&self, removed: &BTreeSet<PaneId>) -> Option<PaneId> {
        let index = slot(position_of(&self.nodes, self.focused)?);
        let (sibling, leading) = self.nodes.iter().find_map(|node| match *node {
            ArrangementNode::Split { first, second, .. } if first == index => Some((second, true)),
            ArrangementNode::Split { first, second, .. } if second == index => Some((first, false)),
            _ => None,
        })?;
        let mut leaves = self.nodes[subtree(&self.nodes, sibling as usize)]
            .iter()
            .filter_map(ArrangementNode::pane);
        if leading {
            leaves.find(|pane| !removed.contains(pane))
        } else {
            leaves.rev().find(|pane| !removed.contains(pane))
        }
    }

    fn rebuild(
        &mut self,
        target: &CreationShape,
        layout: &CreationLayout,
        removed: Vec<PaneId>,
    ) -> Result<ArrangementChange, ViewError> {
        let focus = *target
            .leaves
            .first()
            .ok_or(ViewError::InvalidCreationTopology)?;
        let (nodes, split_high_water) =
            clone_creation(&layout.nodes, &target.order, 0, self.split_high_water)?;
        let zoom_cleared = matches!(self.zoom, ZoomState::Zoomed(_));
        self.nodes = nodes;
        self.split_high_water = split_high_water;
        self.focused = focus;
        self.focus_history = vec![focus];
        self.zoom = ZoomState::Restored;
        Ok(ArrangementChange {
            removed,
            inserted: target.leaves.clone(),
            focus_changed: true,
            zoom_cleared,
        })
    }

    fn insert_missing(
        &mut self,
        target: &CreationShape,
        layout: &CreationLayout,
        placements: &[PanePlacement],
        ordinal: &impl Fn(PaneId) -> Option<CreationOrdinal>,
    ) -> Result<Vec<PaneId>, ViewError> {
        let mut placed: BTreeSet<PaneId> = self.leaves().collect();
        let mut missing: BTreeSet<PaneId> = target
            .leaves
            .iter()
            .copied()
            .filter(|pane| !placed.contains(pane))
            .collect();
        let mut accepted: Vec<&PanePlacement> = placements
            .iter()
            .filter(|placement| missing.contains(&placement.pane))
            .collect();
        accepted.sort_by_key(|placement| (placement.ordinal, placement.pane));
        let mut inserted = Vec::with_capacity(missing.len());
        while !missing.is_empty() {
            for placement in &accepted {
                if !missing.contains(&placement.pane) || !placed.contains(&placement.source) {
                    continue;
                }
                let at = position_of(&self.nodes, placement.source)
                    .ok_or(ViewError::InvalidCreationTopology)?;
                let id = self.allocate_split()?;
                let added = slot(self.nodes.len());
                self.nodes.push(ArrangementNode::Pane(placement.pane));
                let edge = placement.edge;
                split_leaf(&mut self.nodes, at, added, id, edge.axis(), edge.leads());
                missing.remove(&placement.pane);
                placed.insert(placement.pane);
                inserted.push(placement.pane);
            }
            let Some(pane) = missing
                .iter()
                .copied()
                .min_by_key(|pane| ordinal_key(*pane, ordinal))
            else {
                break;
            };
            let fallback = target.fallback(layout, pane, &placed, ordinal)?;
            let at = position_of(&self.nodes, fallback.anchor)
                .ok_or(ViewError::InvalidCreationTopology)?;
            let id = self.allocate_split()?;
            let base = slot(self.nodes.len());
            let order = creation_preorder(&layout.nodes, fallback.subtree);
            let (subtree, split_high_water) =
                clone_creation(&layout.nodes, &order, base, self.split_high_water)?;
            for pane in subtree.iter().filter_map(ArrangementNode::pane) {
                if !missing.remove(&pane) {
                    return Err(ViewError::InvalidCreationTopology);
                }
                placed.insert(pane);
                inserted.push(pane);
            }
            self.split_high_water = split_high_water;
            self.nodes.extend(subtree);
            split_leaf(&mut self.nodes, at, base, id, fallback.axis, fallback.leads);
        }
        self.nodes = compact(&self.nodes);
        Ok(inserted)
    }

    fn allocate_split(&mut self) -> Result<SplitId, ViewError> {
        let id = self.next_split_id()?;
        self.split_high_water = id.get();
        Ok(id)
    }
}

/// A creation layout validated against one Tab.
struct CreationShape {
    /// Node indices in visual pre-order.
    order: Vec<u32>,
    /// Leaves in visual order.
    leaves: Vec<PaneId>,
    /// Parent of each node; the root has none.
    parents: Vec<Option<u32>>,
    leaf_at: BTreeMap<PaneId, u32>,
}

/// Where a missing creation subtree joins the personal arrangement.
struct Fallback {
    subtree: u32,
    anchor: PaneId,
    axis: SplitAxis,
    leads: bool,
}

impl CreationShape {
    fn new(tab: TabId, layout: &CreationLayout) -> Result<Self, ViewError> {
        let walk = walk(layout.nodes.len(), layout.root, |index| {
            match layout.nodes[index] {
                CreationNode::Pane(pane) => Step::Pane(pane),
                CreationNode::Split { first, second, .. } => Step::Split(first, second),
            }
        })
        .map_err(|fault| fault.error(ViewError::InvalidCreationTopology))?;
        if walk.leaves.iter().any(|pane| pane.server() != tab.server()) {
            return Err(ViewError::WrongServer);
        }
        let mut parents = vec![None; layout.nodes.len()];
        let mut leaf_at = BTreeMap::new();
        for &index in &walk.order {
            match layout.nodes[index as usize] {
                CreationNode::Pane(pane) => {
                    leaf_at.insert(pane, index);
                }
                CreationNode::Split { first, second, .. } => {
                    parents[first as usize] = Some(index);
                    parents[second as usize] = Some(index);
                }
            }
        }
        Ok(Self {
            order: walk.order,
            leaves: walk.leaves,
            parents,
            leaf_at,
        })
    }

    /// Walks up from a missing Pane to the nearest creation ancestor whose
    /// opposite child holds a placed leaf. Everything below that ancestor on the
    /// Pane's side is still missing and moves as one subtree.
    fn fallback(
        &self,
        layout: &CreationLayout,
        pane: PaneId,
        placed: &BTreeSet<PaneId>,
        ordinal: &impl Fn(PaneId) -> Option<CreationOrdinal>,
    ) -> Result<Fallback, ViewError> {
        let mut child = *self
            .leaf_at
            .get(&pane)
            .ok_or(ViewError::InvalidCreationTopology)?;
        while let Some(parent) = self.parents[child as usize] {
            let CreationNode::Split {
                axis,
                first,
                second,
            } = layout.nodes[parent as usize]
            else {
                break;
            };
            let (opposite, leads) = if first == child {
                (second, true)
            } else {
                (first, false)
            };
            let anchor = creation_preorder(&layout.nodes, opposite)
                .into_iter()
                .filter_map(|index| match layout.nodes[index as usize] {
                    CreationNode::Pane(leaf) if placed.contains(&leaf) => Some(leaf),
                    _ => None,
                })
                .min_by_key(|leaf| ordinal_key(*leaf, ordinal));
            if let Some(anchor) = anchor {
                return Ok(Fallback {
                    subtree: child,
                    anchor,
                    axis,
                    leads,
                });
            }
            child = parent;
        }
        Err(ViewError::InvalidCreationTopology)
    }
}

/// Personal Tab order and activation for one attached Workspace.
///
/// One order serves every Worktree. Scoped operations rewrite only the slots of
/// the Tab's Worktree, so a Worktree's Tabs need not be contiguous and remotely
/// created Tabs append without disturbing surviving order. Worktree membership
/// is read from the replica's Tab records for each operation, never retained.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TabOrder {
    workspace: WorkspaceId,
    order: Vec<TabId>,
    active: Option<TabId>,
    /// Tabs this client activated, each once, least to most recently
    /// activated. Ends with `active`. Explicit, initial and fallback activation
    /// record a Tab; observing or reordering one does not.
    activation_history: Vec<TabId>,
}

/// Flat retained form of one Workspace's Tab order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TabOrderRecord {
    pub order: Vec<TabId>,
    pub active: Option<TabId>,
    /// Tabs of `order` this client activated, each once, ending with `active`.
    pub activation_history: Vec<TabId>,
}

/// Result of applying authoritative Tab membership. A changed Active Tab is
/// restoration, never explicit selection.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TabOrderChange {
    /// Removed Tabs in prior personal order.
    pub removed: Vec<TabId>,
    pub active_changed: bool,
}

impl TabOrder {
    pub const fn new(workspace: WorkspaceId) -> Self {
        Self {
            workspace,
            order: Vec::new(),
            active: None,
            activation_history: Vec::new(),
        }
    }

    /// Orders the Workspace's Tabs by creation ordinal. Initial activation is
    /// restoration, never explicit use.
    pub fn from_creation(
        workspace: WorkspaceId,
        tabs: &[TabRecord],
        initial_active: TabId,
    ) -> Result<Self, ViewError> {
        let mut members: Vec<&TabRecord> = tabs
            .iter()
            .filter(|record| record.workspace == workspace)
            .collect();
        if members.len() > MAX_VIEW_TABS {
            return Err(TAB_LIMIT);
        }
        if initial_active.server() != workspace.server()
            || members
                .iter()
                .any(|record| record.id.server() != workspace.server())
        {
            return Err(ViewError::WrongServer);
        }
        members.sort_by_key(|record| (record.creation_ordinal, record.id));
        let order: Vec<TabId> = members.iter().map(|record| record.id).collect();
        let mut unique = BTreeSet::new();
        if !order.iter().all(|tab| unique.insert(*tab)) {
            return Err(ViewError::WrongMembership);
        }
        if !order.contains(&initial_active) {
            return Err(ViewError::TargetMissing);
        }
        Ok(Self {
            workspace,
            order,
            active: Some(initial_active),
            activation_history: vec![initial_active],
        })
    }

    /// Validates a retained record for `workspace`.
    pub fn import(workspace: WorkspaceId, record: &TabOrderRecord) -> Result<Self, ViewError> {
        let invalid = ViewError::InvalidPersistedState;
        if record.order.len() > MAX_VIEW_TABS {
            return Err(TAB_LIMIT);
        }
        let mut members = BTreeSet::new();
        if record
            .order
            .iter()
            .any(|tab| tab.server() != workspace.server() || !members.insert(*tab))
        {
            return Err(invalid);
        }
        let mut activated = BTreeSet::new();
        if record
            .activation_history
            .iter()
            .any(|tab| !members.contains(tab) || !activated.insert(*tab))
        {
            return Err(invalid);
        }
        let consistent = match record.active {
            None => record.order.is_empty() && record.activation_history.is_empty(),
            Some(active) => record.activation_history.last() == Some(&active),
        };
        if !consistent {
            return Err(invalid);
        }
        Ok(Self {
            workspace,
            order: record.order.clone(),
            active: record.active,
            activation_history: record.activation_history.clone(),
        })
    }

    pub fn export(&self) -> TabOrderRecord {
        TabOrderRecord {
            order: self.order.clone(),
            active: self.active,
            activation_history: self.activation_history.clone(),
        }
    }

    pub const fn workspace(&self) -> WorkspaceId {
        self.workspace
    }

    pub fn order(&self) -> &[TabId] {
        &self.order
    }

    /// None exactly when the order is empty.
    pub const fn active(&self) -> Option<TabId> {
        self.active
    }

    pub fn activation_history(&self) -> &[TabId] {
        &self.activation_history
    }

    pub fn contains(&self, tab: TabId) -> bool {
        self.order.contains(&tab)
    }

    /// One Worktree's Tabs in personal order.
    pub fn tabs_in(&self, tabs: &[TabRecord], worktree: Option<WorktreeId>) -> Vec<TabId> {
        let scopes = self.scopes(tabs);
        self.order
            .iter()
            .copied()
            .filter(|tab| scopes.get(tab) == Some(&worktree))
            .collect()
    }

    /// The most recently activated Tab of one Worktree, or its first Tab in
    /// personal order when none was activated here. None only when the
    /// Worktree has no Tabs.
    pub fn most_recent_in(
        &self,
        tabs: &[TabRecord],
        worktree: Option<WorktreeId>,
    ) -> Option<TabId> {
        let scopes = self.scopes(tabs);
        let in_scope = |tab: &TabId| scopes.get(tab) == Some(&worktree);
        self.activation_history
            .iter()
            .rev()
            .copied()
            .find(in_scope)
            .or_else(|| self.order.iter().copied().find(in_scope))
    }

    /// The Tab one step from the Active Tab, wrapping within its Worktree.
    pub fn neighbor_of_active(&self, step: TabStep, tabs: &[TabRecord]) -> Option<TabId> {
        let active = self.active?;
        let scopes = self.scopes(tabs);
        let scope = *scopes.get(&active)?;
        let sequence: Vec<TabId> = self
            .order
            .iter()
            .copied()
            .filter(|tab| scopes.get(tab) == Some(&scope))
            .collect();
        let index = sequence.iter().position(|tab| *tab == active)?;
        let len = sequence.len();
        let neighbor = match step {
            TabStep::Previous => (index + len - 1) % len,
            TabStep::Next => (index + 1) % len,
        };
        sequence.get(neighbor).copied()
    }

    /// Activates a member Tab and reports whether the Active Tab changed.
    pub fn activate(&mut self, tab: TabId) -> Result<bool, ViewError> {
        self.check_member(tab)?;
        let changed = self.active != Some(tab);
        self.set_active(tab);
        Ok(changed)
    }

    /// Moves a Tab to `position` within its Worktree's sequence and reports
    /// whether the order changed. Other Worktrees keep their slots.
    pub fn move_tab(
        &mut self,
        tab: TabId,
        position: usize,
        tabs: &[TabRecord],
    ) -> Result<bool, ViewError> {
        self.check_member(tab)?;
        let scopes = self.scopes(tabs);
        let scope = *scopes.get(&tab).ok_or(ViewError::TargetMissing)?;
        let slots: Vec<usize> = self
            .order
            .iter()
            .enumerate()
            .filter(|(_, candidate)| scopes.get(*candidate) == Some(&scope))
            .map(|(index, _)| index)
            .collect();
        if position >= slots.len() {
            return Err(ViewError::TargetMissing);
        }
        let mut sequence: Vec<TabId> = slots.iter().map(|&index| self.order[index]).collect();
        let current = sequence
            .iter()
            .position(|candidate| *candidate == tab)
            .ok_or(ViewError::TargetMissing)?;
        if current == position {
            return Ok(false);
        }
        let moved = sequence.remove(current);
        sequence.insert(position, moved);
        for (index, tab) in slots.into_iter().zip(sequence) {
            self.order[index] = tab;
        }
        Ok(true)
    }

    /// Moves a Tab one step within its Worktree's sequence without wrapping.
    pub fn step_tab(
        &mut self,
        tab: TabId,
        step: TabStep,
        tabs: &[TabRecord],
    ) -> Result<bool, ViewError> {
        self.check_member(tab)?;
        let scope = *self
            .scopes(tabs)
            .get(&tab)
            .ok_or(ViewError::TargetMissing)?;
        let sequence = self.tabs_in(tabs, scope);
        let current = sequence
            .iter()
            .position(|candidate| *candidate == tab)
            .ok_or(ViewError::TargetMissing)?;
        let position = match step {
            TabStep::Previous => current.checked_sub(1),
            TabStep::Next => Some(current + 1).filter(|position| *position < sequence.len()),
        };
        match position {
            Some(position) => self.move_tab(tab, position, tabs),
            None => Ok(false),
        }
    }

    /// Appends a newly observed Tab without activating it. An empty order
    /// activates it as restoration.
    pub(crate) fn append(&mut self, tab: TabId) -> Result<bool, ViewError> {
        if tab.server() != self.workspace.server() {
            return Err(ViewError::WrongServer);
        }
        if self.contains(tab) {
            return Ok(false);
        }
        if self.order.len() >= MAX_VIEW_TABS {
            return Err(TAB_LIMIT);
        }
        self.order.push(tab);
        if self.active.is_none() {
            self.set_active(tab);
        }
        Ok(true)
    }

    /// Removes Tabs absent from the Workspace's current records.
    ///
    /// A removed Active Tab falls back to the next surviving Tab of its old
    /// Worktree sequence, then the preceding one, then the most recent
    /// surviving Tab activated here, then the first remaining Tab in personal
    /// order. `old_active_worktree` is the removed Active Tab's Worktree from
    /// the replica that last contained it, when known.
    pub(crate) fn retain(
        &mut self,
        tabs: &[TabRecord],
        old_active_worktree: Option<Option<WorktreeId>>,
    ) -> TabOrderChange {
        let scopes = self.scopes(tabs);
        let removed: Vec<TabId> = self
            .order
            .iter()
            .copied()
            .filter(|tab| !scopes.contains_key(tab))
            .collect();
        if removed.is_empty() {
            return TabOrderChange::default();
        }
        let previous = std::mem::take(&mut self.order);
        self.order = previous
            .iter()
            .copied()
            .filter(|tab| scopes.contains_key(tab))
            .collect();
        self.activation_history
            .retain(|tab| scopes.contains_key(tab));
        let Some(active) = self.active.filter(|active| !scopes.contains_key(active)) else {
            return TabOrderChange {
                removed,
                active_changed: false,
            };
        };
        let fallback = old_active_worktree
            .and_then(|worktree| {
                let sequence: Vec<TabId> = previous
                    .iter()
                    .copied()
                    .filter(|tab| *tab == active || scopes.get(tab) == Some(&worktree))
                    .collect();
                let index = sequence.iter().position(|tab| *tab == active)?;
                sequence
                    .get(index + 1)
                    .or_else(|| index.checked_sub(1).and_then(|before| sequence.get(before)))
                    .copied()
            })
            .or_else(|| self.activation_history.last().copied())
            .or_else(|| self.order.first().copied());
        match fallback {
            Some(tab) => self.set_active(tab),
            None => self.active = None,
        }
        TabOrderChange {
            removed,
            active_changed: true,
        }
    }

    fn check_member(&self, tab: TabId) -> Result<(), ViewError> {
        if tab.server() != self.workspace.server() {
            return Err(ViewError::WrongServer);
        }
        if self.contains(tab) {
            Ok(())
        } else {
            Err(ViewError::TargetMissing)
        }
    }

    fn set_active(&mut self, tab: TabId) {
        self.activation_history
            .retain(|candidate| *candidate != tab);
        self.activation_history.push(tab);
        self.active = Some(tab);
    }

    fn scopes(&self, tabs: &[TabRecord]) -> BTreeMap<TabId, Option<WorktreeId>> {
        tabs.iter()
            .filter(|record| record.workspace == self.workspace)
            .map(|record| (record.id, record.worktree))
            .collect()
    }
}

/// Reachability facts for a candidate node table.
struct Walk {
    order: Vec<u32>,
    leaves: Vec<PaneId>,
}

enum Step {
    Pane(PaneId),
    Split(u32, u32),
}

#[derive(Clone, Copy)]
enum Fault {
    Malformed,
    TooLarge,
}

impl Fault {
    const fn error(self, malformed: ViewError) -> ViewError {
        match self {
            Self::Malformed => malformed,
            Self::TooLarge => TAB_PANE_LIMIT,
        }
    }
}

/// Visits an untrusted node table from `root` with an explicit stack. Every node
/// must be reached exactly once, which rejects out-of-range indices, shared
/// children, cycles and unreachable nodes. Leaves must be unique.
fn walk(len: usize, root: u32, step: impl Fn(usize) -> Step) -> Result<Walk, Fault> {
    if len > MAX_TAB_NODES {
        return Err(Fault::TooLarge);
    }
    let root = root as usize;
    if root >= len {
        return Err(Fault::Malformed);
    }
    let mut reached = vec![false; len];
    let mut order = Vec::with_capacity(len);
    let mut leaves = Vec::with_capacity(len.div_ceil(2));
    let mut unique = BTreeSet::new();
    let mut stack = vec![root];
    reached[root] = true;
    while let Some(index) = stack.pop() {
        order.push(slot(index));
        match step(index) {
            Step::Pane(pane) => {
                if !unique.insert(pane) {
                    return Err(Fault::Malformed);
                }
                leaves.push(pane);
            }
            Step::Split(first, second) => {
                for child in [second, first] {
                    let child = child as usize;
                    if child >= len || reached[child] {
                        return Err(Fault::Malformed);
                    }
                    reached[child] = true;
                    stack.push(child);
                }
            }
        }
    }
    if order.len() == len {
        Ok(Walk { order, leaves })
    } else {
        Err(Fault::Malformed)
    }
}

/// Node tables stay within a few multiples of [`MAX_TAB_NODES`], far inside `u32`.
fn slot(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

fn positive(value: f32) -> bool {
    value.is_finite() && value > 0.0
}

/// Finite and strictly between zero and one. NaN fails both comparisons.
fn valid_ratio(ratio: f32) -> bool {
    ratio > 0.0 && ratio < 1.0
}

/// Rounds an f64 ratio within zero and one to the nearest valid f32 ratio.
/// Extreme proportions can round to zero or one, so the result is clamped to
/// the smallest normal f32 and the largest f32 below one. The rounding and the
/// clamp are both monotonic.
fn rounded_ratio(ratio: f64) -> f32 {
    (ratio as f32).clamp(f32::MIN_POSITIVE, MAX_RATIO)
}

fn next_layout(layout: LayoutRevision) -> Result<LayoutRevision, ViewError> {
    layout
        .checked_successor()
        .ok_or(ViewCounter::LayoutRevision.exhausted())
}

/// Deterministic anchor and insertion order: surviving creation ordinal, then
/// Pane identity, with unknown ordinals last.
fn ordinal_key(
    pane: PaneId,
    ordinal: &impl Fn(PaneId) -> Option<CreationOrdinal>,
) -> (bool, Option<CreationOrdinal>, PaneId) {
    let value = ordinal(pane);
    (value.is_none(), value, pane)
}

fn position_of(nodes: &[ArrangementNode], pane: PaneId) -> Option<usize> {
    nodes.iter().position(|node| node.pane() == Some(pane))
}

/// Pre-order indices of a trusted table reachable from index zero.
fn preorder(nodes: &[ArrangementNode]) -> Vec<u32> {
    let mut order = Vec::with_capacity(nodes.len());
    let mut stack = vec![0_u32];
    while let Some(index) = stack.pop() {
        order.push(index);
        if let Some(ArrangementNode::Split { first, second, .. }) = nodes.get(index as usize) {
            stack.push(*second);
            stack.push(*first);
        }
    }
    order
}

/// Pre-order indices of a validated creation subtree.
fn creation_preorder(nodes: &[CreationNode], root: u32) -> Vec<u32> {
    let mut order = Vec::new();
    let mut stack = vec![root];
    while let Some(index) = stack.pop() {
        order.push(index);
        if let Some(CreationNode::Split { first, second, .. }) = nodes.get(index as usize) {
            stack.push(*second);
            stack.push(*first);
        }
    }
    order
}

/// The index range of one subtree in a compact pre-order table.
fn subtree(nodes: &[ArrangementNode], start: usize) -> Range<usize> {
    let mut end = start;
    let mut pending = 1_usize;
    while pending > 0 && end < nodes.len() {
        pending -= 1;
        if matches!(nodes[end], ArrangementNode::Split { .. }) {
            pending += 2;
        }
        end += 1;
    }
    start..end
}

fn compact(nodes: &[ArrangementNode]) -> Vec<ArrangementNode> {
    compact_in(nodes, &preorder(nodes))
}

/// Rebuilds the nodes listed in pre-order `order` as a compact table.
fn compact_in(nodes: &[ArrangementNode], order: &[u32]) -> Vec<ArrangementNode> {
    let mut position = vec![0_u32; nodes.len()];
    for (new, &old) in order.iter().enumerate() {
        position[old as usize] = slot(new);
    }
    order
        .iter()
        .map(|&old| match nodes[old as usize] {
            ArrangementNode::Split {
                id,
                axis,
                ratio,
                first,
                second,
            } => ArrangementNode::Split {
                id,
                axis,
                ratio,
                first: position[first as usize],
                second: position[second as usize],
            },
            pane => pane,
        })
        .collect()
}

/// Clones validated creation nodes in pre-order `order` at index `base`, issuing
/// Split IDs after `high_water` with default ratios.
fn clone_creation(
    nodes: &[CreationNode],
    order: &[u32],
    base: u32,
    mut high_water: u64,
) -> Result<(Vec<ArrangementNode>, u64), ViewError> {
    let position: BTreeMap<u32, u32> = order
        .iter()
        .enumerate()
        .map(|(offset, &index)| (index, base.saturating_add(slot(offset))))
        .collect();
    let at = |index: u32| {
        position
            .get(&index)
            .copied()
            .ok_or(ViewError::InvalidCreationTopology)
    };
    let mut cloned = Vec::with_capacity(order.len());
    for &index in order {
        cloned.push(match nodes[index as usize] {
            CreationNode::Pane(pane) => ArrangementNode::Pane(pane),
            CreationNode::Split {
                axis,
                first,
                second,
            } => {
                high_water = high_water
                    .checked_add(1)
                    .ok_or(ViewCounter::SplitId.exhausted())?;
                ArrangementNode::Split {
                    id: SplitId::new(high_water).ok_or(ViewCounter::SplitId.exhausted())?,
                    axis,
                    ratio: DEFAULT_RATIO,
                    first: at(first)?,
                    second: at(second)?,
                }
            }
        });
    }
    Ok((cloned, high_water))
}

/// Replaces the leaf at `at` with a Split of that leaf and the node at `added`.
fn split_leaf(
    nodes: &mut Vec<ArrangementNode>,
    at: usize,
    added: u32,
    id: SplitId,
    axis: SplitAxis,
    leads: bool,
) {
    let kept = slot(nodes.len());
    nodes.push(nodes[at]);
    let (first, second) = if leads { (added, kept) } else { (kept, added) };
    nodes[at] = ArrangementNode::Split {
        id,
        axis,
        ratio: DEFAULT_RATIO,
        first,
        second,
    };
}

/// Removes leaves from a trusted table rooted at index zero, collapsing each
/// removed leaf's parent into its sibling. Surviving Splits keep their IDs and
/// ratios. Detached slots remain until compaction. A root leaf is never removed;
/// callers keep at least one survivor.
fn remove_leaves(nodes: &mut [ArrangementNode], removed: &BTreeSet<PaneId>) {
    let mut parents = vec![None; nodes.len()];
    let mut leaves = BTreeMap::new();
    for (index, node) in nodes.iter().enumerate() {
        match *node {
            ArrangementNode::Split { first, second, .. } => {
                parents[first as usize] = Some(index);
                parents[second as usize] = Some(index);
            }
            ArrangementNode::Pane(pane) if removed.contains(&pane) => {
                leaves.insert(pane, index);
            }
            ArrangementNode::Pane(_) => {}
        }
    }
    for pane in removed {
        let Some(index) = leaves.remove(pane) else {
            continue;
        };
        let Some(parent) = parents[index] else {
            continue;
        };
        let ArrangementNode::Split { first, second, .. } = nodes[parent] else {
            continue;
        };
        let sibling = if first as usize == index {
            second
        } else {
            first
        };
        let moved = nodes[sibling as usize];
        nodes[parent] = moved;
        match moved {
            ArrangementNode::Split { first, second, .. } => {
                parents[first as usize] = Some(parent);
                parents[second as usize] = Some(parent);
            }
            ArrangementNode::Pane(kept) => {
                if let Some(at) = leaves.get_mut(&kept) {
                    *at = parent;
                }
            }
        }
    }
}

/// Whether two compact tables place the same Panes in the same Splits,
/// whatever their Split IDs and ratios.
fn same_structure(left: &[ArrangementNode], right: &[ArrangementNode]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|pair| match pair {
            (ArrangementNode::Pane(left), ArrangementNode::Pane(right)) => left == right,
            (
                ArrangementNode::Split {
                    axis,
                    first,
                    second,
                    ..
                },
                ArrangementNode::Split {
                    axis: other_axis,
                    first: other_first,
                    second: other_second,
                    ..
                },
            ) => axis == other_axis && first == other_first && second == other_second,
            _ => false,
        })
}

#[derive(Clone, Copy)]
struct Bounds {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

impl Bounds {
    const ROOT: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 1.0,
    };

    fn split(self, axis: SplitAxis, ratio: f32) -> (Self, Self) {
        match axis {
            SplitAxis::Horizontal => {
                let divider = self.left + (self.right - self.left) * ratio;
                (
                    Self {
                        right: divider,
                        ..self
                    },
                    Self {
                        left: divider,
                        ..self
                    },
                )
            }
            SplitAxis::Vertical => {
                let divider = self.top + (self.bottom - self.top) * ratio;
                (
                    Self {
                        bottom: divider,
                        ..self
                    },
                    Self {
                        top: divider,
                        ..self
                    },
                )
            }
        }
    }

    fn center_x(self) -> f32 {
        (self.left + self.right) / 2.0
    }

    fn center_y(self) -> f32 {
        (self.top + self.bottom) / 2.0
    }
}

#[derive(Clone, Copy)]
struct DirectionalScore {
    primary_distance: f32,
    perpendicular_distance: f32,
    overlap: f32,
}

impl DirectionalScore {
    fn compare(self, other: Self) -> Ordering {
        self.primary_distance
            .total_cmp(&other.primary_distance)
            .then_with(|| {
                self.perpendicular_distance
                    .total_cmp(&other.perpendicular_distance)
            })
            .then_with(|| other.overlap.total_cmp(&self.overlap))
    }
}

/// Scores a candidate that lies past the focused Pane's edge in `direction`.
///
/// The edge test tolerates [`EDGE_EPSILON`], so a Pane narrower than that
/// tolerance would also accept itself and the Panes just behind it. A candidate
/// must therefore also reach strictly past the focused Pane's far edge. That
/// second test compares stored bounds without arithmetic, so adjacent Panes
/// stay ordered even where their f32 centers round together. It holds for every
/// candidate wider than the tolerance.
fn directional_score(
    focused: Bounds,
    candidate: Bounds,
    direction: FocusDirection,
) -> Option<DirectionalScore> {
    let (is_in_direction, primary_distance, perpendicular_distance, overlap) = match direction {
        FocusDirection::Left => (
            candidate.right <= focused.left + EDGE_EPSILON && candidate.left < focused.left,
            focused.left - candidate.right,
            (focused.center_y() - candidate.center_y()).abs(),
            focused.bottom.min(candidate.bottom) - focused.top.max(candidate.top),
        ),
        FocusDirection::Right => (
            candidate.left >= focused.right - EDGE_EPSILON && candidate.right > focused.right,
            candidate.left - focused.right,
            (focused.center_y() - candidate.center_y()).abs(),
            focused.bottom.min(candidate.bottom) - focused.top.max(candidate.top),
        ),
        FocusDirection::Up => (
            candidate.bottom <= focused.top + EDGE_EPSILON && candidate.top < focused.top,
            focused.top - candidate.bottom,
            (focused.center_x() - candidate.center_x()).abs(),
            focused.right.min(candidate.right) - focused.left.max(candidate.left),
        ),
        FocusDirection::Down => (
            candidate.top >= focused.bottom - EDGE_EPSILON && candidate.bottom > focused.bottom,
            candidate.top - focused.bottom,
            (focused.center_x() - candidate.center_x()).abs(),
            focused.right.min(candidate.right) - focused.left.max(candidate.left),
        ),
    };

    (is_in_direction && overlap > EDGE_EPSILON).then_some(DirectionalScore {
        primary_distance: primary_distance.max(0.0),
        perpendicular_distance,
        overlap,
    })
}
