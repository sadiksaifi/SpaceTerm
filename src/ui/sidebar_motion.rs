//! Motion of the Workspace sidebar as it shows and hides.

use std::time::Instant;

use gpui::App;

use super::drag_and_drop::{Ease, PRESENCE_DURATION, now, presence_motion};

/// How far the Workspace sidebar is revealed, easing toward whether it is shown.
///
/// Only a change announced by [`SidebarMotion::expect_change`] moves the sidebar, so a resize
/// that collapses it, or a window that opens with it hidden, presents the change at once. A change
/// during motion eases from where the sidebar was drawn.
#[derive(Debug)]
pub(crate) struct SidebarMotion {
    /// Whether the last frame showed the sidebar.
    shown: bool,
    /// The reveal the last frame drew.
    drawn: f32,
    /// The reveal the current motion started from, and its ease.
    motion: Option<(f32, Ease)>,
    /// Whether the next change of visibility comes from an operation that announced it.
    expected: bool,
}

/// How one frame presents the sidebar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SidebarFrame {
    /// How much of the sidebar shows, from 0 when hidden to 1 when shown.
    pub(crate) reveal: f32,
    /// Whether the sidebar is still moving.
    pub(crate) moving: bool,
}

impl SidebarFrame {
    /// The value `reveal` of the way from `hidden` to `shown`.
    pub(crate) fn between<T>(self, hidden: T, shown: T) -> T
    where
        T: std::ops::Add<Output = T> + std::ops::Sub<Output = T> + std::ops::Mul<f32, Output = T>,
        T: Copy,
    {
        hidden + (shown - hidden) * self.reveal
    }
}

impl SidebarMotion {
    pub(crate) fn new(shown: bool) -> Self {
        Self {
            shown,
            drawn: if shown { 1.0 } else { 0.0 },
            motion: None,
            expected: false,
        }
    }

    /// Announces that the next frame's visibility comes from a toggle, so the sidebar moves.
    pub(crate) fn expect_change(&mut self) {
        self.expected = true;
    }

    /// The frame that presents the sidebar `shown` or hidden.
    pub(crate) fn present(&mut self, shown: bool, cx: &App) -> SidebarFrame {
        self.present_at(shown, now(cx), presence_motion(cx))
    }

    fn present_at(
        &mut self,
        shown: bool,
        now: Instant,
        motion: spaceterm_ui::ControlMotion,
    ) -> SidebarFrame {
        let expected = std::mem::take(&mut self.expected);
        if shown != self.shown {
            self.shown = shown;
            self.motion = expected.then(|| (self.drawn, Ease::new(PRESENCE_DURATION, now, motion)));
        }
        // Motion reduced during a change ends it.
        if motion == spaceterm_ui::ControlMotion::Reduced {
            self.motion = None;
        }
        self.motion = self.motion.filter(|(_, ease)| !ease.finished(now));
        let target = if shown { 1.0 } else { 0.0 };
        self.drawn = self
            .motion
            .map_or(target, |(from, ease)| from + (target - from) * ease.at(now));
        SidebarFrame {
            reveal: self.drawn,
            moving: self.motion.is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use spaceterm_ui::ControlMotion;

    use super::*;

    #[test]
    fn a_toggle_should_slide_the_sidebar_in_and_out() {
        let start = Instant::now();
        let mut motion = SidebarMotion::new(false);
        motion.present_at(false, start, ControlMotion::Standard);
        motion.expect_change();

        let opening = [Duration::ZERO, PRESENCE_DURATION / 8, PRESENCE_DURATION]
            .map(|at| motion.present_at(true, start + at, ControlMotion::Standard));
        motion.expect_change();
        let closed = start + PRESENCE_DURATION;
        let closing = [Duration::ZERO, PRESENCE_DURATION / 8, PRESENCE_DURATION]
            .map(|at| motion.present_at(false, closed + at, ControlMotion::Standard));

        for (frames, from, to) in [(opening, 0.0, 1.0), (closing, 1.0, 0.0)] {
            assert_eq!(
                frames.map(|frame| frame.moving),
                [true, true, false],
                "{frames:?}"
            );
            assert_eq!((frames[0].reveal, frames[2].reveal), (from, to));
            assert!(
                frames[1].reveal > 0.0 && frames[1].reveal < 1.0,
                "{frames:?}"
            );
        }
    }

    #[test]
    fn a_toggle_during_motion_should_reverse_from_where_the_sidebar_is_drawn() {
        let start = Instant::now();
        let mut motion = SidebarMotion::new(false);
        motion.expect_change();
        motion.present_at(true, start, ControlMotion::Standard);
        let drawn = motion.present_at(true, start + PRESENCE_DURATION / 8, ControlMotion::Standard);
        motion.expect_change();

        let reversed = motion.present_at(
            false,
            start + PRESENCE_DURATION / 8,
            ControlMotion::Standard,
        );

        assert_eq!(
            reversed,
            SidebarFrame {
                reveal: drawn.reveal,
                moving: true
            }
        );
    }

    #[test]
    fn a_change_nobody_announced_should_present_the_sidebar_at_rest() {
        let start = Instant::now();
        let mut motion = SidebarMotion::new(true);
        motion.present_at(true, start, ControlMotion::Standard);

        let frame = motion.present_at(false, start, ControlMotion::Standard);

        assert_eq!(
            frame,
            SidebarFrame {
                reveal: 0.0,
                moving: false
            }
        );
    }

    #[test]
    fn reduced_motion_should_present_a_toggle_at_once() {
        let start = Instant::now();
        let mut motion = SidebarMotion::new(false);
        motion.expect_change();
        motion.present_at(true, start, ControlMotion::Standard);

        let frame = motion.present_at(true, start + PRESENCE_DURATION / 8, ControlMotion::Reduced);

        assert_eq!(
            frame,
            SidebarFrame {
                reveal: 1.0,
                moving: false
            }
        );
    }
}
