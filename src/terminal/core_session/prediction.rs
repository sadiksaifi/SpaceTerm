//! The Pane worker uses the portable predictor with desktop snapshots.
use super::ScreenSnapshot;
pub(super) use spaceterm_terminal_client::prediction::Overlay;
pub(super) type Predictor = spaceterm_terminal_client::prediction::Predictor<ScreenSnapshot>;

#[cfg(test)]
use super::{ActiveScreenSnapshot, CursorPositionSnapshot, RowSnapshot, TerminalUnderlineSnapshot};
#[cfg(test)]
use spaceterm_core::pane::{InteractiveInput, LineState, Request};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::time::{Duration, Instant};

#[cfg(test)]
#[path = "prediction_tests.rs"]
mod tests;

#[cfg(all(
    test,
    feature = "native-tests",
    any(target_os = "macos", target_os = "linux")
))]
#[path = "prediction_unix_tests.rs"]
mod unix_tests;
