//! Reported title structure and conservative animation evidence.

use std::sync::Arc;
use std::time::{Duration, Instant};

const ACTIVITY_DELAY: Duration = Duration::from_millis(200);
const ACTIVITY_FRESHNESS: Duration = Duration::from_secs(2);

/// Keeps an animation candidate from briefly replacing the program's stable icon.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum TitleGlyph {
    #[default]
    Reported,
    Retained(Option<Arc<str>>),
}

impl TitleGlyph {
    pub(crate) fn resolve<'a>(&'a self, reported: Option<&'a str>) -> Option<&'a str> {
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
        // Once an unchanged candidate expires, identical reports must not repeatedly hide a
        // static icon behind another confirmation interval.
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
            // A title rename may restart confirmation while its loader is already visible in
            // the raw title. Carry the original stable icon across that restart.
            let stable_glyph = self.animation.as_ref().map_or_else(
                || {
                    previous
                        .glyph
                        .filter(|glyph| activity_frame(glyph).is_none())
                        .map(Arc::from)
                },
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

/// How many characters the first word of a title can hold and still be a glyph rather than a word.
const MAXIMUM_GLYPH_CHARS: usize = 2;

/// Characters a title keeps, because a Session can be named after them.
const TITLE_WORD_CHARS: &str = "~/\\._-:@$#([{'\"";

/// Characters that separate a program's glyph from the words after it.
const TITLE_SEPARATOR_CHARS: &str = "-\u{2013}\u{2014}\u{00b7}|:";

/// What a program put at the front of the title it reported, and the words that follow it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReportedTitle<'a> {
    /// The program's own glyph, when it reported one host chrome can draw.
    pub(crate) glyph: Option<&'a str>,
    /// The title without that glyph.
    pub(crate) words: &'a str,
}

/// Splits the glyph a program draws at the front of its own title from the words after it.
///
/// Programs often open the title they report with their own icon or a spinner frame. One Session
/// gets one glyph, so the program's takes the place of the glyph host chrome would draw rather than
/// sitting beside it, and never appears twice. Only a first word that names nothing on its own is
/// taken: a short decorative sequence that is not a word a Session could be named after. No glyph
/// is recognised by name, so this stays the same for every program. The active chrome font's
/// shaper decides whether the candidate occupies the Session's one glyph slot.
pub(crate) fn reported_title(title: &str) -> ReportedTitle<'_> {
    let title = title.trim();
    let plain = ReportedTitle {
        glyph: None,
        words: title,
    };
    let Some(split) = title.find(char::is_whitespace) else {
        return plain;
    };
    let (first, rest) = title.split_at(split);
    if !is_glyph_word(first) {
        return plain;
    }
    let words = title_words_after_glyph(rest);
    if words.is_empty() {
        return plain;
    }
    ReportedTitle {
        glyph: is_glyph_candidate(first).then_some(first),
        words,
    }
}

/// Removes whitespace and one standalone delimiter token after a reported glyph.
///
/// Punctuation attached to the next opaque word is content, such as `--help` or `:memory`.
fn title_words_after_glyph(rest: &str) -> &str {
    let words = rest.trim_start();
    let Some(split) = words.find(char::is_whitespace) else {
        return words.trim_end();
    };
    let (first, remainder) = words.split_at(split);
    if first.chars().count() == 1
        && first
            .chars()
            .all(|character| TITLE_SEPARATOR_CHARS.contains(character))
    {
        remainder.trim()
    } else {
        words.trim_end()
    }
}

/// Whether the first word of a title decorates it rather than naming what the Session is doing.
fn is_glyph_word(word: &str) -> bool {
    let bases = word
        .chars()
        .filter(|character| !is_glyph_mark(*character))
        .count();
    // Several letters name something, in whatever script they are written. Several symbols
    // together are still decoration.
    let decorative = bases == 1 || word.chars().all(|character| !character.is_alphanumeric());
    (1..=MAXIMUM_GLYPH_CHARS).contains(&bases)
        && decorative
        && word.chars().all(|character| {
            !character.is_ascii_alphanumeric() && !TITLE_WORD_CHARS.contains(character)
        })
}

/// Whether a character belongs to the glyph before it rather than standing as one of its own.
pub(crate) fn is_glyph_mark(character: char) -> bool {
    matches!(
        u32::from(character),
        // Variation selectors, the zero-width joiner, and the skin tone modifiers.
        0xFE00..=0xFE0F | 0x200D | 0x1F3FB..=0x1F3FF | 0xE0100..=0xE01EF
    )
}

/// Whether a title prefix has the structural shape of a reported glyph candidate.
///
/// A Private-Use character is drawn by the font a program expects rather than by the font the host
/// paints its chrome in, so it would paint as a missing-glyph box. An ASCII one says less than the
/// Session's own glyph does. Actual Chrome-font support is checked after shaping.
fn is_glyph_candidate(glyph: &str) -> bool {
    glyph.chars().all(|character| {
        !character.is_ascii()
            && !matches!(
                u32::from(character),
                0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_frames_exclude_meaningful_and_unlisted_braille_glyphs() {
        for frame in ACTIVITY_FRAME_FAMILIES
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
