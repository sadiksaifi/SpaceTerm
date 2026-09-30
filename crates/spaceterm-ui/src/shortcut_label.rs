//! Displayed Shortcut text whose key glyphs share one optical center.
use gpui::{App, IntoElement, RenderOnce, SharedString, Window};

use crate::optical_text::OpticalText;

/// Shows a displayed Shortcut in the inherited text style.
///
/// Key glyphs the shortcut font lacks are centered on the font's own key glyphs, and the label's
/// box spans the glyphs' ink, so spacing around it measures to what the reader sees.
#[derive(IntoElement)]
pub struct ShortcutLabel {
    text: SharedString,
}

impl ShortcutLabel {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self { text: text.into() }
    }
}

impl RenderOnce for ShortcutLabel {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        OpticalText::new(self.text).centering_fallback_glyphs()
    }
}
