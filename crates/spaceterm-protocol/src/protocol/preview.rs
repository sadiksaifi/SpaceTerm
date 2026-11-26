//! Bounded, channel-free previews of a Pane's visible terminal text.

use super::message::check;
use super::screen::{Row, StyleRef, flags};
use super::{Fatal, limits};
use crate::hierarchy::Id;
use serde::{Deserialize, Serialize};

/// Most Panes one request reads.
pub const MAX_PANES: usize = 8;
/// Most visible rows and columns taken from the top-left of each screen.
pub const MAX_ROWS: u16 = 24;
pub const MAX_COLUMNS: u16 = 80;

/// One result for each requested Pane, in request order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanePreview {
    pub pane: Id,
    pub state: State,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Missing,
    NotStarted,
    Live {
        session: Id,
        /// `None` if a complete current screen is unavailable before the read's deadline.
        screen: Option<Screen>,
    },
    Ended {
        session: Option<Id>,
        /// The last screen kept by the Machine, if any. An unsuccessful shell exit or End All
        /// Sessions discards the screen.
        screen: Option<Screen>,
    },
}

/// The visible grid, cropped to the preview's row and column limits. Runs use default or inline
/// styles and contain no links. Palette entries are resolved to RGB by the Machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    pub columns: u16,
    pub row_count: u16,
    pub cropped: bool,
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub reversed: bool,
    /// Missing rows and columns are blank in the default style.
    pub rows: Vec<Row>,
}

pub(super) fn validate_request(panes: &[Id]) -> Result<(), Fatal> {
    check((1..=MAX_PANES).contains(&panes.len()))?;
    check(
        panes
            .iter()
            .enumerate()
            .all(|(i, pane)| !panes[..i].contains(pane)),
    )
}

pub(super) fn validate_reply(previews: &[PanePreview]) -> Result<(), Fatal> {
    check((1..=MAX_PANES).contains(&previews.len()))?;
    for preview in previews {
        if let State::Live {
            screen: Some(screen),
            ..
        }
        | State::Ended {
            screen: Some(screen),
            ..
        } = &preview.state
        {
            screen.validate()?;
        }
    }
    Ok(())
}

impl Screen {
    fn validate(&self) -> Result<(), Fatal> {
        check((1..=MAX_COLUMNS).contains(&self.columns))?;
        check((1..=MAX_ROWS).contains(&self.row_count))?;
        check(self.rows.len() <= usize::from(self.row_count))?;
        check(
            self.rows
                .windows(2)
                .all(|pair| pair[0].index < pair[1].index),
        )?;
        for row in &self.rows {
            check(row.index < self.row_count && row.runs.len() <= usize::from(self.columns))?;
            let mut width = 0;
            for run in &row.runs {
                check(run.link.is_none())?;
                check(match run.style {
                    StyleRef::Default => true,
                    StyleRef::Inline(style) => style.valid() && style.flags & flags::SELECTED == 0,
                    StyleRef::Id(_) => false,
                })?;
                check(run.text.len() <= usize::from(self.columns) * limits::MAX_CELL_BYTES)?;
                width += run.width();
                check(width <= usize::from(self.columns) && run.covers_text())?;
            }
        }
        Ok(())
    }
}
