//! Reusable GPUI controls for SpaceTerm.
//!
//! The crate owns interaction and editing behavior while the application supplies all product
//! colors and surrounding chrome from its canonical theme.

mod button;
mod middle_truncated_text;
mod text_input;

use gpui::App;

pub use button::{
    Button, ButtonActivation, ButtonActivationSource, ButtonMetrics, ButtonPaint, ButtonRole,
    ButtonShape, ButtonSize, ButtonSizes, ButtonTheme, ButtonVariant, ButtonVariantStyle,
    ButtonVariants, IconButton,
};
pub use middle_truncated_text::MiddleTruncatedText;
pub use text_input::{TextInput, TextInputEvent, TextInputStyle};

/// Registers shared control state, actions, and macOS key bindings.
pub fn init(cx: &mut App, button_theme: ButtonTheme) {
    cx.set_global(button_theme);
    text_input::init(cx);
}
