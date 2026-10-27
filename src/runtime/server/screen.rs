//! Screen frames, presentation style and palette for the Development host.
use super::screen_encoder::{ScreenCapture, ScreenEncoder};
use super::*;
use spaceterm_server::protocol::interim::ScreenFrame;

impl OwnedPane {
    /// Shows or hides the Pane. A change restarts updates from a keyframe.
    pub(super) fn presentation_demand(&mut self, visible: bool) {
        if self.presentable != visible {
            self.presentable = visible;
            self.runtime.set_presentable(visible);
            self.screen_encoder = ScreenEncoder::default();
            self.sent = None;
            if !visible {
                self.runtime.end_server_accessibility_demand();
            }
        }
    }

    /// The screen frame a poll returns, or `None` when the controller already has it.
    /// `changes` holds the controller's last applied frame when it asks for changes. Otherwise
    /// the poll receives a keyframe once per screen, which leaves the change baseline alone.
    pub(super) fn screen_frame(&mut self, changes: Option<Option<u64>>) -> Option<Reply> {
        let screen = self.screen.as_ref().filter(|_| self.presentable)?;
        if changes.is_none() && self.sent == Some(screen.generation) {
            return None;
        }
        let capture = ScreenCapture {
            screen: Arc::clone(screen),
            history: self.history.observe(screen),
            generation: self.screen_generation,
            accessibility_revision: self.accessibility.revision(),
        };
        let base = changes.flatten();
        let frame = self.frame + 1;
        let update = match changes {
            Some(_) => self.screen_encoder.encode(&capture, frame, base)?,
            None => ScreenEncoder::default().encode(&capture, frame, None)?,
        };
        self.frame = frame;
        self.sent = Some(capture.screen.generation);
        self.accessibility.take_changed_revision();
        Some(Reply::Screen(Box::new(ScreenFrame {
            frame,
            base,
            update,
        })))
    }

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
