use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use thiserror::Error;

macro_rules! typed_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub(crate) struct $name(u64);

        impl $name {
            pub(super) const fn from_raw(value: u64) -> Self {
                Self(value)
            }

            #[cfg(test)]
            pub(crate) const fn new(value: u64) -> Self {
                Self::from_raw(value)
            }

            pub(crate) const fn get(self) -> u64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

typed_id!(WindowId);
typed_id!(PaneId);
typed_id!(SplitId);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PaneSize {
    width: f32,
    height: f32,
}

impl PaneSize {
    pub(crate) fn new(width: f32, height: f32) -> Result<Self, PaneSizeError> {
        if !width.is_finite() || width <= 0.0 {
            return Err(PaneSizeError::InvalidWidth(width));
        }
        if !height.is_finite() || height <= 0.0 {
            return Err(PaneSizeError::InvalidHeight(height));
        }

        Ok(Self { width, height })
    }

    pub(crate) const fn width(self) -> f32 {
        self.width
    }

    pub(crate) const fn height(self) -> f32 {
        self.height
    }

    fn extent(self, axis: SplitAxis) -> f32 {
        match axis {
            SplitAxis::Horizontal => self.width,
            SplitAxis::Vertical => self.height,
        }
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub(crate) enum PaneSizeError {
    #[error("Pane width must be finite and greater than zero, got {0}")]
    InvalidWidth(f32),
    #[error("Pane height must be finite and greater than zero, got {0}")]
    InvalidHeight(f32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SplitAxis {
    /// Places the first Pane on the left and the second Pane on the right.
    Horizontal,
    /// Places the first Pane above the second Pane.
    Vertical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FocusDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ZoomState {
    Restored,
    Zoomed(PaneId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClosePaneOutcome {
    PaneClosed {
        closed_pane_id: PaneId,
        focused_pane_id: PaneId,
    },
    CloseWindow,
}

#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub(crate) enum PaneError {
    #[error("Pane {0} does not belong to this Window")]
    PaneNotFound(PaneId),
    #[error("Split {0} does not belong to this Window")]
    SplitNotFound(SplitId),
    #[error("Pane ID space is exhausted")]
    PaneIdExhausted,
    #[error("split ID space is exhausted")]
    SplitIdExhausted,
    #[error("split divider size must be finite and non-negative, got {0}")]
    InvalidDividerSize(f32),
    #[error("split ratio must be finite, got {0}")]
    InvalidSplitRatio(f32),
    #[error("available Pane size {available:?} cannot fit required minimum {required:?}")]
    InsufficientSpace {
        available: PaneSize,
        required: PaneSize,
    },
    #[error("Pane layout minimum size exceeds the supported numeric range")]
    LayoutSizeOverflow,
    #[error("Pane {0} exists in the layout without an owned terminal")]
    MissingTerminal(PaneId),
}

#[derive(Clone, Debug)]
enum PaneNode {
    Leaf(PaneId),
    Split {
        id: SplitId,
        axis: SplitAxis,
        ratio: f32,
        first: Box<Self>,
        second: Box<Self>,
    },
}

#[derive(Clone, Copy)]
pub(crate) struct PaneTreeRef<'a> {
    node: &'a PaneNode,
}

impl<'a> PaneTreeRef<'a> {
    pub(crate) fn node(self) -> PaneNodeRef<'a> {
        match self.node {
            PaneNode::Leaf(pane_id) => PaneNodeRef::Leaf { pane_id: *pane_id },
            PaneNode::Split {
                id,
                axis,
                ratio,
                first,
                second,
            } => PaneNodeRef::Split {
                split_id: *id,
                axis: *axis,
                ratio: *ratio,
                first: Self { node: first },
                second: Self { node: second },
            },
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum PaneNodeRef<'a> {
    Leaf {
        pane_id: PaneId,
    },
    Split {
        split_id: SplitId,
        axis: SplitAxis,
        ratio: f32,
        first: PaneTreeRef<'a>,
        second: PaneTreeRef<'a>,
    },
}

pub(crate) struct TerminalWindow<T> {
    id: WindowId,
    root: PaneNode,
    terminals: BTreeMap<PaneId, T>,
    focused_pane_id: PaneId,
    zoom_state: ZoomState,
    minimum_pane_size: PaneSize,
    next_pane_id: u64,
    next_split_id: u64,
}

impl<T> TerminalWindow<T> {
    pub(crate) fn new(
        id: WindowId,
        minimum_pane_size: PaneSize,
        create_initial_terminal: impl FnOnce(PaneId) -> T,
    ) -> Self {
        let initial_pane_id = PaneId::from_raw(1);
        Self {
            id,
            root: PaneNode::Leaf(initial_pane_id),
            terminals: BTreeMap::from([(
                initial_pane_id,
                create_initial_terminal(initial_pane_id),
            )]),
            focused_pane_id: initial_pane_id,
            zoom_state: ZoomState::Restored,
            minimum_pane_size,
            next_pane_id: 2,
            next_split_id: 1,
        }
    }

    pub(crate) const fn id(&self) -> WindowId {
        self.id
    }

    pub(crate) const fn focused_pane_id(&self) -> PaneId {
        self.focused_pane_id
    }

    pub(crate) const fn zoom_state(&self) -> ZoomState {
        self.zoom_state
    }

    pub(crate) const fn minimum_pane_size(&self) -> PaneSize {
        self.minimum_pane_size
    }

    pub(crate) fn pane_count(&self) -> usize {
        self.terminals.len()
    }

    pub(crate) fn root(&self) -> PaneTreeRef<'_> {
        PaneTreeRef { node: &self.root }
    }

    pub(crate) fn terminal(&self, pane_id: PaneId) -> Option<&T> {
        self.terminals.get(&pane_id)
    }

    pub(crate) fn terminals(&self) -> impl ExactSizeIterator<Item = &T> {
        self.terminals.values()
    }

    pub(crate) fn focus_pane(&mut self, pane_id: PaneId) -> Result<(), PaneError> {
        if !self.root.contains_pane(pane_id) {
            return Err(PaneError::PaneNotFound(pane_id));
        }

        self.set_focused_pane(pane_id);
        Ok(())
    }

    pub(crate) fn focus_pane_in_direction(&mut self, direction: FocusDirection) -> Option<PaneId> {
        let pane_id = self
            .root
            .pane_in_direction(self.focused_pane_id, direction)?;
        self.set_focused_pane(pane_id);
        Some(pane_id)
    }

    fn set_focused_pane(&mut self, pane_id: PaneId) {
        self.focused_pane_id = pane_id;
        if matches!(self.zoom_state, ZoomState::Zoomed(_)) {
            self.zoom_state = ZoomState::Zoomed(pane_id);
        }
    }

    pub(crate) fn toggle_zoom(&mut self) -> ZoomState {
        self.zoom_state = match self.zoom_state {
            ZoomState::Restored => ZoomState::Zoomed(self.focused_pane_id),
            ZoomState::Zoomed(_) => ZoomState::Restored,
        };
        self.zoom_state
    }

    pub(crate) fn split_pane(
        &mut self,
        target_pane_id: PaneId,
        axis: SplitAxis,
        target_size: PaneSize,
        divider_size: f32,
        create_terminal: impl FnOnce(PaneId) -> T,
    ) -> Result<PaneId, PaneError> {
        self.validate_split(target_pane_id, axis, target_size, divider_size)?;
        let (new_pane_id, split_id, next_pane_id, next_split_id) = self.next_split_ids()?;
        let Some(new_root) = self
            .root
            .with_split(target_pane_id, new_pane_id, split_id, axis)
        else {
            unreachable!("a validated split target must remain in the Pane layout")
        };
        let terminal = create_terminal(new_pane_id);
        self.next_pane_id = next_pane_id;
        self.next_split_id = next_split_id;
        self.commit_split(new_root, new_pane_id, terminal);
        Ok(new_pane_id)
    }

    pub(crate) fn close_pane(&mut self, pane_id: PaneId) -> Result<ClosePaneOutcome, PaneError> {
        let removal = self
            .root
            .without_pane(pane_id)
            .ok_or(PaneError::PaneNotFound(pane_id))?;
        let Some(new_root) = removal.replacement else {
            return Ok(ClosePaneOutcome::CloseWindow);
        };
        let terminal = self
            .terminals
            .remove(&pane_id)
            .ok_or(PaneError::MissingTerminal(pane_id))?;

        self.root = new_root;
        if self.focused_pane_id == pane_id {
            self.focused_pane_id = removal.focus_fallback;
        }
        if self.zoom_state == ZoomState::Zoomed(pane_id) {
            self.zoom_state = ZoomState::Restored;
        }
        drop(terminal);

        Ok(ClosePaneOutcome::PaneClosed {
            closed_pane_id: pane_id,
            focused_pane_id: self.focused_pane_id,
        })
    }

    pub(crate) fn minimum_size(&self, divider_size: f32) -> Result<PaneSize, PaneError> {
        validate_divider_size(divider_size)?;
        self.root.minimum_size(self.minimum_pane_size, divider_size)
    }

    pub(crate) fn resize_split(
        &mut self,
        split_id: SplitId,
        available_size: PaneSize,
        divider_size: f32,
        requested_ratio: f32,
    ) -> Result<f32, PaneError> {
        validate_divider_size(divider_size)?;
        if !requested_ratio.is_finite() {
            return Err(PaneError::InvalidSplitRatio(requested_ratio));
        }

        let split = self
            .root
            .split(split_id)
            .ok_or(PaneError::SplitNotFound(split_id))?;
        let first_minimum = split
            .first
            .minimum_size(self.minimum_pane_size, divider_size)?;
        let second_minimum = split
            .second
            .minimum_size(self.minimum_pane_size, divider_size)?;
        let required = combined_minimum(first_minimum, second_minimum, split.axis, divider_size)?;
        ensure_space(available_size, required)?;

        let content_extent = available_size.extent(split.axis) - divider_size;
        let minimum_ratio = first_minimum.extent(split.axis) / content_extent;
        let maximum_ratio = 1.0 - second_minimum.extent(split.axis) / content_extent;
        let ratio = requested_ratio.clamp(minimum_ratio, maximum_ratio);

        let split = self
            .root
            .split_mut(split_id)
            .ok_or(PaneError::SplitNotFound(split_id))?;
        *split.ratio = ratio;
        Ok(ratio)
    }

    fn validate_split(
        &self,
        target_pane_id: PaneId,
        axis: SplitAxis,
        target_size: PaneSize,
        divider_size: f32,
    ) -> Result<(), PaneError> {
        if !self.root.contains_pane(target_pane_id) {
            return Err(PaneError::PaneNotFound(target_pane_id));
        }
        validate_divider_size(divider_size)?;
        let required = combined_minimum(
            self.minimum_pane_size,
            self.minimum_pane_size,
            axis,
            divider_size,
        )?;
        ensure_space(target_size, required)?;
        Ok(())
    }

    fn next_split_ids(&self) -> Result<(PaneId, SplitId, u64, u64), PaneError> {
        let next_pane_id = self
            .next_pane_id
            .checked_add(1)
            .ok_or(PaneError::PaneIdExhausted)?;
        let next_split_id = self
            .next_split_id
            .checked_add(1)
            .ok_or(PaneError::SplitIdExhausted)?;
        Ok((
            PaneId::from_raw(self.next_pane_id),
            SplitId::from_raw(self.next_split_id),
            next_pane_id,
            next_split_id,
        ))
    }

    fn commit_split(&mut self, root: PaneNode, pane_id: PaneId, terminal: T) {
        self.root = root;
        self.terminals.insert(pane_id, terminal);
        self.focused_pane_id = pane_id;
        self.zoom_state = ZoomState::Restored;
    }
}

struct SplitNodeRef<'a> {
    axis: SplitAxis,
    first: &'a PaneNode,
    second: &'a PaneNode,
}

struct SplitNodeMut<'a> {
    ratio: &'a mut f32,
}

struct PaneRemoval {
    replacement: Option<PaneNode>,
    focus_fallback: PaneId,
}

#[derive(Clone, Copy)]
struct NormalizedBounds {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

impl NormalizedBounds {
    const ROOT: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 1.0,
    };

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

impl PaneNode {
    fn contains_pane(&self, target: PaneId) -> bool {
        match self {
            Self::Leaf(pane_id) => *pane_id == target,
            Self::Split { first, second, .. } => {
                first.contains_pane(target) || second.contains_pane(target)
            }
        }
    }

    fn pane_in_direction(
        &self,
        focused_pane_id: PaneId,
        direction: FocusDirection,
    ) -> Option<PaneId> {
        let mut panes = Vec::new();
        self.collect_pane_bounds(NormalizedBounds::ROOT, &mut panes);
        let focused_bounds = panes
            .iter()
            .find_map(|(pane_id, bounds)| (*pane_id == focused_pane_id).then_some(*bounds))?;

        panes
            .into_iter()
            .filter_map(|(pane_id, bounds)| {
                directional_score(focused_bounds, bounds, direction).map(|score| (pane_id, score))
            })
            .min_by(|(first_id, first_score), (second_id, second_score)| {
                first_score
                    .compare(*second_score)
                    .then_with(|| first_id.cmp(second_id))
            })
            .map(|(pane_id, _)| pane_id)
    }

    fn collect_pane_bounds(
        &self,
        bounds: NormalizedBounds,
        panes: &mut Vec<(PaneId, NormalizedBounds)>,
    ) {
        match self {
            Self::Leaf(pane_id) => panes.push((*pane_id, bounds)),
            Self::Split {
                axis,
                ratio,
                first,
                second,
                ..
            } => {
                let (first_bounds, second_bounds) = match axis {
                    SplitAxis::Horizontal => {
                        let divider = bounds.left + (bounds.right - bounds.left) * ratio;
                        (
                            NormalizedBounds {
                                right: divider,
                                ..bounds
                            },
                            NormalizedBounds {
                                left: divider,
                                ..bounds
                            },
                        )
                    }
                    SplitAxis::Vertical => {
                        let divider = bounds.top + (bounds.bottom - bounds.top) * ratio;
                        (
                            NormalizedBounds {
                                bottom: divider,
                                ..bounds
                            },
                            NormalizedBounds {
                                top: divider,
                                ..bounds
                            },
                        )
                    }
                };
                first.collect_pane_bounds(first_bounds, panes);
                second.collect_pane_bounds(second_bounds, panes);
            }
        }
    }

    fn with_split(
        &self,
        target: PaneId,
        new_pane_id: PaneId,
        split_id: SplitId,
        axis: SplitAxis,
    ) -> Option<Self> {
        match self {
            Self::Leaf(pane_id) if *pane_id == target => Some(Self::Split {
                id: split_id,
                axis,
                ratio: 0.5,
                first: Box::new(Self::Leaf(*pane_id)),
                second: Box::new(Self::Leaf(new_pane_id)),
            }),
            Self::Leaf(_) => None,
            Self::Split {
                id,
                axis: current_axis,
                ratio,
                first,
                second,
            } => {
                if let Some(updated_first) = first.with_split(target, new_pane_id, split_id, axis) {
                    return Some(Self::Split {
                        id: *id,
                        axis: *current_axis,
                        ratio: *ratio,
                        first: Box::new(updated_first),
                        second: second.clone(),
                    });
                }
                second
                    .with_split(target, new_pane_id, split_id, axis)
                    .map(|updated_second| Self::Split {
                        id: *id,
                        axis: *current_axis,
                        ratio: *ratio,
                        first: first.clone(),
                        second: Box::new(updated_second),
                    })
            }
        }
    }

    fn without_pane(&self, target: PaneId) -> Option<PaneRemoval> {
        match self {
            Self::Leaf(pane_id) if *pane_id == target => Some(PaneRemoval {
                replacement: None,
                focus_fallback: *pane_id,
            }),
            Self::Leaf(_) => None,
            Self::Split {
                id,
                axis,
                ratio,
                first,
                second,
            } => {
                if let Some(removal) = first.without_pane(target) {
                    return Some(match removal.replacement {
                        Some(updated_first) => PaneRemoval {
                            replacement: Some(Self::Split {
                                id: *id,
                                axis: *axis,
                                ratio: *ratio,
                                first: Box::new(updated_first),
                                second: second.clone(),
                            }),
                            focus_fallback: removal.focus_fallback,
                        },
                        None => PaneRemoval {
                            replacement: Some((**second).clone()),
                            focus_fallback: second.first_pane_id(),
                        },
                    });
                }

                second
                    .without_pane(target)
                    .map(|removal| match removal.replacement {
                        Some(updated_second) => PaneRemoval {
                            replacement: Some(Self::Split {
                                id: *id,
                                axis: *axis,
                                ratio: *ratio,
                                first: first.clone(),
                                second: Box::new(updated_second),
                            }),
                            focus_fallback: removal.focus_fallback,
                        },
                        None => PaneRemoval {
                            replacement: Some((**first).clone()),
                            focus_fallback: first.last_pane_id(),
                        },
                    })
            }
        }
    }

    fn first_pane_id(&self) -> PaneId {
        match self {
            Self::Leaf(pane_id) => *pane_id,
            Self::Split { first, .. } => first.first_pane_id(),
        }
    }

    fn last_pane_id(&self) -> PaneId {
        match self {
            Self::Leaf(pane_id) => *pane_id,
            Self::Split { second, .. } => second.last_pane_id(),
        }
    }

    fn minimum_size(
        &self,
        leaf_minimum: PaneSize,
        divider_size: f32,
    ) -> Result<PaneSize, PaneError> {
        match self {
            Self::Leaf(_) => Ok(leaf_minimum),
            Self::Split {
                axis,
                first,
                second,
                ..
            } => combined_minimum(
                first.minimum_size(leaf_minimum, divider_size)?,
                second.minimum_size(leaf_minimum, divider_size)?,
                *axis,
                divider_size,
            ),
        }
    }

    fn split(&self, target: SplitId) -> Option<SplitNodeRef<'_>> {
        match self {
            Self::Leaf(_) => None,
            Self::Split {
                id,
                axis,
                first,
                second,
                ..
            } if *id == target => Some(SplitNodeRef {
                axis: *axis,
                first,
                second,
            }),
            Self::Split { first, second, .. } => {
                first.split(target).or_else(|| second.split(target))
            }
        }
    }

    fn split_mut(&mut self, target: SplitId) -> Option<SplitNodeMut<'_>> {
        match self {
            Self::Leaf(_) => None,
            Self::Split { id, ratio, .. } if *id == target => Some(SplitNodeMut { ratio }),
            Self::Split { first, second, .. } => {
                first.split_mut(target).or_else(|| second.split_mut(target))
            }
        }
    }
}

fn directional_score(
    focused: NormalizedBounds,
    candidate: NormalizedBounds,
    direction: FocusDirection,
) -> Option<DirectionalScore> {
    const EDGE_EPSILON: f32 = 0.000_001;

    let (is_in_direction, primary_distance, perpendicular_distance, overlap) = match direction {
        FocusDirection::Left => (
            candidate.right <= focused.left + EDGE_EPSILON,
            focused.left - candidate.right,
            (focused.center_y() - candidate.center_y()).abs(),
            focused.bottom.min(candidate.bottom) - focused.top.max(candidate.top),
        ),
        FocusDirection::Right => (
            candidate.left >= focused.right - EDGE_EPSILON,
            candidate.left - focused.right,
            (focused.center_y() - candidate.center_y()).abs(),
            focused.bottom.min(candidate.bottom) - focused.top.max(candidate.top),
        ),
        FocusDirection::Up => (
            candidate.bottom <= focused.top + EDGE_EPSILON,
            focused.top - candidate.bottom,
            (focused.center_x() - candidate.center_x()).abs(),
            focused.right.min(candidate.right) - focused.left.max(candidate.left),
        ),
        FocusDirection::Down => (
            candidate.top >= focused.bottom - EDGE_EPSILON,
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

fn validate_divider_size(divider_size: f32) -> Result<(), PaneError> {
    if divider_size.is_finite() && divider_size >= 0.0 {
        Ok(())
    } else {
        Err(PaneError::InvalidDividerSize(divider_size))
    }
}

fn combined_minimum(
    first: PaneSize,
    second: PaneSize,
    axis: SplitAxis,
    divider_size: f32,
) -> Result<PaneSize, PaneError> {
    let (width, height) = match axis {
        SplitAxis::Horizontal => (
            first.width + divider_size + second.width,
            first.height.max(second.height),
        ),
        SplitAxis::Vertical => (
            first.width.max(second.width),
            first.height + divider_size + second.height,
        ),
    };
    if !width.is_finite() || !height.is_finite() {
        return Err(PaneError::LayoutSizeOverflow);
    }
    Ok(PaneSize { width, height })
}

fn ensure_space(available: PaneSize, required: PaneSize) -> Result<(), PaneError> {
    if available.width >= required.width && available.height >= required.height {
        Ok(())
    } else {
        Err(PaneError::InsufficientSpace {
            available,
            required,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::*;

    const DIVIDER_SIZE: f32 = 1.0;

    struct DropProbe {
        id: u64,
        drops: Rc<RefCell<Vec<u64>>>,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.drops.borrow_mut().push(self.id);
        }
    }

    fn size(width: f32, height: f32) -> PaneSize {
        PaneSize::new(width, height).unwrap()
    }

    fn window<T>(terminal: T) -> TerminalWindow<T> {
        TerminalWindow::new(WindowId::new(7), size(100.0, 50.0), |_| terminal)
    }

    fn four_pane_window() -> TerminalWindow<()> {
        let mut window = window(());
        window
            .split_pane(
                PaneId::new(1),
                SplitAxis::Horizontal,
                size(500.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();
        window
            .split_pane(
                PaneId::new(1),
                SplitAxis::Vertical,
                size(250.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();
        window
            .split_pane(
                PaneId::new(2),
                SplitAxis::Vertical,
                size(250.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();
        window
    }

    fn topology(tree: PaneTreeRef<'_>) -> String {
        match tree.node() {
            PaneNodeRef::Leaf { pane_id } => pane_id.to_string(),
            PaneNodeRef::Split {
                split_id,
                axis,
                ratio,
                first,
                second,
            } => format!(
                "{split_id}:{axis:?}:{ratio:.2}({},{})",
                topology(first),
                topology(second)
            ),
        }
    }

    #[test]
    fn new_should_create_one_focused_pane() {
        let created_id = Cell::new(None);
        let window = TerminalWindow::new(WindowId::new(7), size(100.0, 50.0), |pane_id| {
            created_id.set(Some(pane_id));
        });

        assert_eq!(
            (
                window.pane_count(),
                window.focused_pane_id(),
                created_id.get()
            ),
            (1, PaneId::new(1), Some(PaneId::new(1)))
        );
    }

    #[test]
    fn split_pane_should_build_nested_right_and_down_layout() {
        let mut window = window(());
        window
            .split_pane(
                PaneId::new(1),
                SplitAxis::Horizontal,
                size(500.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();
        window
            .split_pane(
                PaneId::new(2),
                SplitAxis::Vertical,
                size(250.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();

        assert_eq!(
            topology(window.root()),
            "1:Horizontal:0.50(1,2:Vertical:0.50(2,3))"
        );
    }

    #[test]
    fn split_pane_should_focus_the_created_pane() {
        let mut window = window(());
        window
            .split_pane(
                PaneId::new(1),
                SplitAxis::Horizontal,
                size(500.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();

        assert_eq!(window.focused_pane_id(), PaneId::new(2));
    }

    #[test]
    fn closed_pane_and_split_ids_should_not_be_reused() {
        let mut window = window(());
        let second = window
            .split_pane(
                PaneId::new(1),
                SplitAxis::Horizontal,
                size(500.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();
        window.close_pane(second).unwrap();

        let third = window
            .split_pane(
                PaneId::new(1),
                SplitAxis::Vertical,
                size(500.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();

        assert_eq!(
            (third, topology(window.root())),
            (PaneId::new(3), "2:Vertical:0.50(1,3)".into())
        );
    }

    #[test]
    fn split_pane_should_validate_the_target_before_creating_terminal() {
        let mut window = window(());
        let creations = Cell::new(0);

        let result = window.split_pane(
            PaneId::new(99),
            SplitAxis::Horizontal,
            size(500.0, 400.0),
            DIVIDER_SIZE,
            |_| {
                creations.set(creations.get() + 1);
            },
        );

        assert_eq!(
            (
                result,
                creations.get(),
                window.next_pane_id,
                window.next_split_id,
            ),
            (Err(PaneError::PaneNotFound(PaneId::new(99))), 0, 2, 1)
        );
    }

    #[test]
    fn split_pane_should_validate_minimum_size_before_creating_terminal() {
        let mut window = window(());
        let creations = Cell::new(0);

        let result = window.split_pane(
            PaneId::new(1),
            SplitAxis::Horizontal,
            size(200.0, 50.0),
            DIVIDER_SIZE,
            |_| {
                creations.set(creations.get() + 1);
            },
        );

        assert_eq!(
            (
                result,
                creations.get(),
                window.next_pane_id,
                window.next_split_id,
            ),
            (
                Err(PaneError::InsufficientSpace {
                    available: size(200.0, 50.0),
                    required: size(201.0, 50.0),
                }),
                0,
                2,
                1,
            )
        );
    }

    #[test]
    fn split_pane_should_reject_exhausted_pane_ids_without_mutation() {
        let mut window = window(());
        window.next_pane_id = u64::MAX;
        let creations = Cell::new(0);

        let result = window.split_pane(
            PaneId::new(1),
            SplitAxis::Horizontal,
            size(500.0, 400.0),
            DIVIDER_SIZE,
            |_| creations.set(creations.get() + 1),
        );

        assert_eq!(
            (
                result,
                creations.get(),
                topology(window.root()),
                window.pane_count(),
                window.focused_pane_id(),
                window.next_pane_id,
                window.next_split_id,
            ),
            (
                Err(PaneError::PaneIdExhausted),
                0,
                "1".into(),
                1,
                PaneId::new(1),
                u64::MAX,
                1,
            )
        );
    }

    #[test]
    fn split_pane_should_reject_exhausted_split_ids_without_mutation() {
        let mut window = window(());
        window.next_split_id = u64::MAX;
        let creations = Cell::new(0);

        let result = window.split_pane(
            PaneId::new(1),
            SplitAxis::Horizontal,
            size(500.0, 400.0),
            DIVIDER_SIZE,
            |_| creations.set(creations.get() + 1),
        );

        assert_eq!(
            (
                result,
                creations.get(),
                topology(window.root()),
                window.pane_count(),
                window.focused_pane_id(),
                window.next_pane_id,
                window.next_split_id,
            ),
            (
                Err(PaneError::SplitIdExhausted),
                0,
                "1".into(),
                1,
                PaneId::new(1),
                2,
                u64::MAX,
            )
        );
    }

    #[test]
    fn focus_pane_should_reject_an_unknown_id_without_changing_focus() {
        let mut window = window(());

        let result = window.focus_pane(PaneId::new(99));

        assert_eq!(
            (result, window.focused_pane_id()),
            (
                Err(PaneError::PaneNotFound(PaneId::new(99))),
                PaneId::new(1)
            )
        );
    }

    #[test]
    fn focus_pane_in_direction_should_follow_nested_visual_neighbors() {
        let mut window = four_pane_window();
        window.focus_pane(PaneId::new(1)).unwrap();

        let focused_panes = [
            FocusDirection::Right,
            FocusDirection::Down,
            FocusDirection::Left,
            FocusDirection::Up,
        ]
        .map(|direction| window.focus_pane_in_direction(direction));

        assert_eq!(
            focused_panes,
            [
                Some(PaneId::new(2)),
                Some(PaneId::new(4)),
                Some(PaneId::new(3)),
                Some(PaneId::new(1)),
            ]
        );
    }

    #[test]
    fn focus_pane_in_direction_should_not_wrap_at_a_layout_edge() {
        let mut window = four_pane_window();
        window.focus_pane(PaneId::new(1)).unwrap();

        let focused_pane = window.focus_pane_in_direction(FocusDirection::Left);

        assert_eq!(
            (focused_pane, window.focused_pane_id()),
            (None, PaneId::new(1))
        );
    }

    #[test]
    fn focus_pane_in_direction_should_move_zoom_to_the_neighbor() {
        let mut window = four_pane_window();
        window.focus_pane(PaneId::new(1)).unwrap();
        window.toggle_zoom();

        let focused_pane = window.focus_pane_in_direction(FocusDirection::Right);

        assert_eq!(
            (focused_pane, window.zoom_state()),
            (Some(PaneId::new(2)), ZoomState::Zoomed(PaneId::new(2)))
        );
    }

    #[test]
    fn close_pane_should_request_window_close_for_the_last_pane() {
        let drops = Rc::new(RefCell::new(Vec::new()));
        let probe = DropProbe {
            id: 1,
            drops: drops.clone(),
        };
        let mut window = window(probe);

        let outcome = window.close_pane(PaneId::new(1)).unwrap();

        assert_eq!(outcome, ClosePaneOutcome::CloseWindow);
        assert!(drops.borrow().is_empty());

        drop(window);

        assert_eq!(*drops.borrow(), vec![1]);
    }

    #[test]
    fn close_pane_should_drop_its_terminal_exactly_once() {
        let drops = Rc::new(RefCell::new(Vec::new()));
        let mut window = window(DropProbe {
            id: 1,
            drops: drops.clone(),
        });
        window
            .split_pane(
                PaneId::new(1),
                SplitAxis::Horizontal,
                size(500.0, 400.0),
                DIVIDER_SIZE,
                |_| DropProbe {
                    id: 2,
                    drops: drops.clone(),
                },
            )
            .unwrap();

        window.close_pane(PaneId::new(2)).unwrap();

        assert_eq!(*drops.borrow(), vec![2]);

        drop(window);

        assert_eq!(*drops.borrow(), vec![2, 1]);
    }

    #[test]
    fn close_pane_should_focus_the_nearest_leaf_in_its_sibling() {
        let mut window = window(());
        window
            .split_pane(
                PaneId::new(1),
                SplitAxis::Horizontal,
                size(500.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();
        window
            .split_pane(
                PaneId::new(1),
                SplitAxis::Vertical,
                size(250.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();
        window.focus_pane(PaneId::new(2)).unwrap();

        window.close_pane(PaneId::new(2)).unwrap();

        assert_eq!(window.focused_pane_id(), PaneId::new(3));
    }

    #[test]
    fn focus_pane_should_move_zoom_to_the_new_focus() {
        let mut window = window(());
        window
            .split_pane(
                PaneId::new(1),
                SplitAxis::Horizontal,
                size(500.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();
        window.toggle_zoom();

        window.focus_pane(PaneId::new(1)).unwrap();

        assert_eq!(window.zoom_state(), ZoomState::Zoomed(PaneId::new(1)));
    }

    #[test]
    fn split_pane_should_restore_a_zoomed_layout() {
        let mut window = window(());
        window.toggle_zoom();

        window
            .split_pane(
                PaneId::new(1),
                SplitAxis::Horizontal,
                size(500.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();

        assert_eq!(window.zoom_state(), ZoomState::Restored);
    }

    #[test]
    fn close_pane_should_restore_when_the_zoomed_pane_closes() {
        let mut window = window(());
        window
            .split_pane(
                PaneId::new(1),
                SplitAxis::Horizontal,
                size(500.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();
        window.toggle_zoom();

        window.close_pane(PaneId::new(2)).unwrap();

        assert_eq!(window.zoom_state(), ZoomState::Restored);
    }

    #[test]
    fn minimum_size_should_accumulate_nested_split_constraints() {
        let mut window = window(());
        window
            .split_pane(
                PaneId::new(1),
                SplitAxis::Horizontal,
                size(500.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();
        window
            .split_pane(
                PaneId::new(2),
                SplitAxis::Vertical,
                size(250.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();

        assert_eq!(
            window.minimum_size(DIVIDER_SIZE).unwrap(),
            size(201.0, 101.0)
        );
    }

    #[test]
    fn resize_split_should_clamp_against_recursive_child_minimums() {
        let mut window = window(());
        window
            .split_pane(
                PaneId::new(1),
                SplitAxis::Horizontal,
                size(500.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();
        window
            .split_pane(
                PaneId::new(1),
                SplitAxis::Horizontal,
                size(250.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();

        let ratio = window
            .resize_split(SplitId::new(1), size(402.0, 100.0), DIVIDER_SIZE, 0.1)
            .unwrap();

        assert_eq!(ratio, 201.0 / 401.0);
    }

    #[test]
    fn resize_split_should_reject_an_impossible_extent_without_changing_ratio() {
        let mut window = window(());
        window
            .split_pane(
                PaneId::new(1),
                SplitAxis::Horizontal,
                size(500.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();

        let result = window.resize_split(SplitId::new(1), size(200.0, 50.0), DIVIDER_SIZE, 0.8);

        assert_eq!(
            (result.is_err(), topology(window.root())),
            (true, "1:Horizontal:0.50(1,2)".into())
        );
    }

    #[test]
    fn resize_split_should_reject_a_non_finite_ratio() {
        let mut window = window(());
        window
            .split_pane(
                PaneId::new(1),
                SplitAxis::Horizontal,
                size(500.0, 400.0),
                DIVIDER_SIZE,
                |_| (),
            )
            .unwrap();

        let result =
            window.resize_split(SplitId::new(1), size(500.0, 400.0), DIVIDER_SIZE, f32::NAN);

        assert!(matches!(result, Err(PaneError::InvalidSplitRatio(value)) if value.is_nan()));
    }
}
