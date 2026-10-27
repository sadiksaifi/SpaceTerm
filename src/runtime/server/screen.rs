//! Screen generations, presentation style and palette for the Development host.
use super::*;

impl OwnedPane {
    /// Advances the wire screen generation when `screen` replaces a different snapshot.
    pub(super) fn advance_screen_generation(&mut self, screen: &ScreenSnapshot) {
        if self
            .screen
            .as_ref()
            .is_none_or(|old| old.generation != screen.generation)
        {
            self.screen_generation = self.screen_generation.saturating_add(1);
        }
    }
}

pub(super) fn appearance(
    style: Option<PresentationStyle>,
    generation: u64,
) -> Result<TerminalAppearanceUpdate, TerminalFailure> {
    let mut update = TerminalAppearanceUpdate::builtin()?;
    update.generation = AppearanceGeneration::new(generation);
    if let Some(style) = style {
        let resolved = Arc::make_mut(&mut update.appearance);
        resolved.appearance = if style.light {
            Appearance::Light
        } else {
            Appearance::Dark
        };
        resolved.colors.foreground = from_rgb(style.foreground);
        resolved.colors.background = from_rgb(style.background);
        resolved.colors.cursor = from_rgb(style.cursor);
        resolved.colors.cursor_text = style.cursor_text.map(from_rgb);
        resolved.colors.normal = std::array::from_fn(|i| from_rgb(style.palette[i]));
        resolved.colors.bright = std::array::from_fn(|i| from_rgb(style.palette[i + 8]));
        resolved.bold_as_bright = style.bold_as_bright;
    }
    Ok(update)
}
fn from_rgb(c: [u8; 3]) -> super::super::Color {
    super::super::Color::from_rgb_components(c[0], c[1], c[2])
}
