//! Adapts the Scrollback view to the desktop's native graphics.
pub(super) mod route;
#[cfg(test)]
mod tests;

use super::ScreenSnapshot;
use crate::appearance::Color;
use crate::terminal::{HyperlinkTarget, PresentationGeneration};
use spaceterm_terminal_client::scrollback as state;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

pub(crate) use state::{
    AccessibilityView, CopyOutcome, EpochSelection, Grid, PageRequest, PointerEvent,
};
pub(super) type DecodedPage = state::DecodedPage<HyperlinkTarget, Color>;

#[derive(Default)]
pub(super) struct Scrollback(state::Scrollback<ScreenSnapshot>);

impl Scrollback {
    pub(super) fn present(
        &mut self,
        generation: PresentationGeneration,
    ) -> Option<Arc<ScreenSnapshot>> {
        let state::Presented {
            mut screen,
            scrolled_rows,
        } = self.0.present(generation)?;
        if scrolled_rows > 0 {
            let shift = i32::try_from(scrolled_rows).unwrap_or(i32::MAX);
            let screen = Arc::make_mut(&mut screen);
            screen.graphics.placements = screen
                .graphics
                .placements
                .iter()
                .map(|placement| {
                    let mut placement = placement.clone();
                    placement.viewport_row = placement.viewport_row.saturating_add(shift);
                    placement
                })
                .collect();
        }
        Some(screen)
    }
}

impl Deref for Scrollback {
    type Target = state::Scrollback<ScreenSnapshot>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Scrollback {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
