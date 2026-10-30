//! Reported title structure and conservative animation evidence.

use std::sync::Arc;
use std::time::{Duration, Instant};

const ACTIVITY_DELAY: Duration = Duration::from_millis(200);
const ACTIVITY_FRESHNESS: Duration = Duration::from_secs(2);

/// Keeps an animation candidate from briefly replacing the program's stable icon.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum TitleGlyph {
    #[default]
    Reported,
    Retained(Option<Arc<str>>),
}

impl TitleGlyph {
    pub fn resolve<'a>(&'a self, reported: Option<&'a str>) -> Option<&'a str> {
        match self {
            Self::Reported => reported,
            Self::Retained(glyph) => glyph.as_deref(),
        }
    }
}

#[derive(Default)]
pub(super) struct TitleActivity {
    animation: Option<Animation>,
}

struct Animation {
    started: Instant,
    last_change: Instant,
    changed: bool,
    active: bool,
    stable_glyph: Option<Arc<str>>,
}

impl TitleActivity {
    pub(super) fn observe(&mut self, previous: &str, title: &str, now: Instant) {
        let next = reported_title(title);
        let Some(next_frame) = next.glyph.and_then(activity_frame) else {
            self.animation = None;
            return;
        };
        let previous = reported_title(previous);
        let previous_frame = previous.glyph.and_then(activity_frame);
        // Identical reports must not hide a static icon after confirmation expires.
        if self.animation.is_none() && previous_frame == Some(next_frame) {
            return;
        }
        let continues = previous.words == next.words
            && previous_frame.is_some_and(|(family, _)| family == next_frame.0);
        if let Some(animation) = &mut self.animation
            && continues
            && now < animation.last_change + ACTIVITY_FRESHNESS
        {
            if previous_frame != Some(next_frame) {
                animation.last_change = now;
                animation.changed = true;
            }
        } else {
            // Retain the stable icon when a title rename restarts confirmation.
            let stable_glyph = self.animation.as_ref().map_or_else(
                || previous.glyph.or(next.glyph).map(Arc::from),
                |animation| animation.stable_glyph.clone(),
            );
            self.animation = Some(Animation {
                started: now,
                last_change: now,
                changed: false,
                active: false,
                stable_glyph,
            });
        }
        self.advance(now);
    }

    pub(super) fn advance(&mut self, now: Instant) {
        if let Some(animation) = &mut self.animation {
            if now >= animation.last_change + ACTIVITY_FRESHNESS {
                self.animation = None;
            } else {
                animation.active = animation.changed && now >= animation.started + ACTIVITY_DELAY;
            }
        }
    }

    pub(super) fn active(&self) -> bool {
        self.animation
            .as_ref()
            .is_some_and(|animation| animation.active)
    }

    pub(super) fn glyph(&self) -> TitleGlyph {
        self.animation
            .as_ref()
            .map_or(TitleGlyph::Reported, |animation| {
                TitleGlyph::Retained(animation.stable_glyph.clone())
            })
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.animation.as_ref().map(|animation| {
            if animation.changed && !animation.active {
                animation.started + ACTIVITY_DELAY
            } else {
                animation.last_change + ACTIVITY_FRESHNESS
            }
        })
    }
}

fn activity_frame(glyph: &str) -> Option<(usize, char)> {
    let mut bases = glyph.chars().filter(|character| !is_glyph_mark(*character));
    let frame = bases.next()?;
    if bases.next().is_some() {
        return None;
    }
    ACTIVITY_FRAME_FAMILIES
        .iter()
        .position(|family| family.contains(frame))
        .map(|family| (family, frame))
}

/// Frames from generic activity animations that terminal programs commonly report in titles.
///
/// Keep this list finite. Braille also carries meaningful program artwork, so classifying its
/// whole Unicode block would erase icons that happen to use the same character set.
const ACTIVITY_FRAME_FAMILIES: [&str; 6] =
    ["⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏", "⣾⣽⣻⢿⡿⣟⣯⣷", "⢹⢺⢼⣸⣇⡧⡗⡏", "◐◓◑◒", "◰◳◲◱", "◴◷◶◵"];

#[cfg(test)]
pub use spaceterm_server::labels::ReportedTitle;
pub use spaceterm_server::labels::{is_glyph_mark, reported_title};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_frames_exclude_meaningful_and_unlisted_braille_glyphs() {
        for frame in ["⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏", "⣾⣽⣻⢿⡿⣟⣯⣷", "⢹⢺⢼⣸⣇⡧⡗⡏", "◐◓◑◒", "◰◳◲◱", "◴◷◶◵"]
            .iter()
            .flat_map(|family| family.chars())
        {
            assert!(activity_frame(&frame.to_string()).is_some());
        }
        assert!(activity_frame("◐\u{fe0f}").is_some());
        for glyph in ["✳", "🚀", "π", "◉", "⣿", "⡀", "⠋⠙", "\u{fe0f}"] {
            assert!(activity_frame(glyph).is_none(), "{glyph}");
        }
    }
}
