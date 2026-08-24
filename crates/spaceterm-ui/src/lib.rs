//! Reusable GPUI controls for SpaceTerm.
//!
//! The crate owns interaction and editing behavior while the application supplies all product
//! colors and surrounding chrome from its canonical theme.

mod middle_truncated_text;
mod text_input;

use gpui::App;

pub use middle_truncated_text::MiddleTruncatedText;
pub use text_input::{TextInput, TextInputEvent, TextInputStyle};

/// Registers actions and macOS key bindings used by SpaceTerm controls.
pub fn init(cx: &mut App) {
    text_input::init(cx);
}
