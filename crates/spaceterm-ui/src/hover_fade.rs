//! Hover that eases in and out.
//!
//! GPUI hover styles switch in a single frame. A [`HoverFade`] tracks the pointer over one region
//! and reports how far the region has eased toward its hovered look, so the owner paints each
//! hover-dependent color between its resting and hovered value. Entering eases in quickly and
//! leaving eases out more slowly, so a pointer sweeping across a list leaves a short trail instead
//! of a flicker. A reversal mid-transition continues from the current level. Reduced motion
//! switches at once. Hover never shows during a drag, as with GPUI hover styles.
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    App, ElementId, Entity, HitboxBehavior, MouseExitEvent, MouseMoveEvent, Rgba, Window, canvas,
};

use crate::ControlMotion;

/// How long a region takes to reach its hovered look.
pub(crate) const ENTER: Duration = Duration::from_millis(120);
/// How long a region takes to return to rest.
pub(crate) const EXIT: Duration = Duration::from_millis(220);

/// The hover of one region, as a level from 0 at rest to 1 hovered.
///
/// Create it during render with a key unique among its siblings, paint with [`Self::level`], and
/// mount [`Self::tracker`] as a child of the region. The region must be positioned, since the
/// tracker covers it absolutely.
#[derive(Clone)]
pub struct HoverFade {
    state: Entity<HoverRegion>,
}

impl HoverFade {
    /// The longest any hover change takes to finish.
    pub const SETTLE: Duration = if ENTER.as_nanos() > EXIT.as_nanos() {
        ENTER
    } else {
        EXIT
    };

    pub fn new(key: impl Into<ElementId>, window: &mut Window, cx: &mut App) -> Self {
        Self {
            state: window.use_keyed_state(key, cx, |_, _| HoverRegion::default()),
        }
    }

    /// How far the region has eased toward its hovered look this frame.
    ///
    /// Frames continue until the transition settles.
    pub fn level(&self, window: &mut Window, cx: &mut App) -> f32 {
        let now = cx.background_executor().now();
        let transition = self.state.read(cx).transition;
        let raw = transition.raw(now);
        if raw != transition.target() {
            window.request_animation_frame();
        }
        ease(raw)
    }

    /// Whether the pointer is over the region, regardless of how far the transition has run.
    pub fn is_hovered(&self, cx: &App) -> bool {
        self.state.read(cx).transition.hovered
    }

    /// The invisible element that follows the pointer over the region.
    ///
    /// It never blocks the pointer, so the region's own controls keep their hit testing.
    pub fn tracker(&self) -> impl IntoElement {
        let state = self.state.clone();
        canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |_, hitbox, window, cx| {
                let hovered = !cx.has_active_drag() && hitbox.is_hovered(window);
                let region = state.update(cx, |region, _| {
                    region.measured = hovered;
                    *region
                });
                if region.transition.hovered != hovered {
                    // The layout moved under a still pointer. Hover changes outside event
                    // dispatch, so it catches up on the next frame with the latest measurement.
                    let state = state.clone();
                    window.on_next_frame(move |_, cx| {
                        let measured = state.read(cx).measured;
                        set_hovered(&state, measured, cx);
                    });
                }
                let move_state = state.clone();
                window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
                    if phase.capture() {
                        let hovered = !cx.has_active_drag() && hitbox.is_hovered(window);
                        set_hovered(&move_state, hovered, cx);
                    }
                });
                let exit_state = state.clone();
                window.on_mouse_event(move |_: &MouseExitEvent, phase, _, cx| {
                    if phase.capture() {
                        set_hovered(&exit_state, false, cx);
                    }
                });
            },
        )
        .absolute()
        .inset_0()
    }
}

/// Finishes every hover transition in progress, since test windows have no frame loop.
#[cfg(test)]
pub(crate) fn settle(cx: &mut gpui::VisualTestContext) {
    cx.run_until_parked();
    cx.executor().advance_clock(HoverFade::SETTLE);
    cx.update(|window, cx| window.simulate_next_frame(cx));
    cx.run_until_parked();
}

/// Measures the pointer over the region and starts the transition toward it.
fn set_hovered(state: &Entity<HoverRegion>, hovered: bool, cx: &mut App) {
    let now = cx.background_executor().now();
    let motion = crate::control_motion(cx);
    state.update(cx, |region, cx| {
        region.measured = hovered;
        if region.transition.set_hovered(hovered, now, motion) {
            cx.notify();
        }
    });
}

/// One region's hover, as last measured and as currently shown.
#[derive(Clone, Copy, Default)]
struct HoverRegion {
    /// Whether the pointer was over the region at the latest paint or pointer event.
    measured: bool,
    transition: HoverTransition,
}

/// Paints straight between a resting and a hovered color.
///
/// Each end returns its own color exactly.
pub fn mix_rgba(rest: Rgba, hovered: Rgba, level: f32) -> Rgba {
    if level <= 0.0 {
        return rest;
    }
    if level >= 1.0 {
        return hovered;
    }
    let channel = |rest: f32, hovered: f32| rest * (1.0 - level) + hovered * level;
    Rgba {
        r: channel(rest.r, hovered.r),
        g: channel(rest.g, hovered.g),
        b: channel(rest.b, hovered.b),
        a: channel(rest.a, hovered.a),
    }
}

/// Where a region's hover stands, measured without easing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct HoverTransition {
    hovered: bool,
    /// The level the current transition started from.
    from: f32,
    /// When the current transition started, or `None` once a change applies at once.
    since: Option<Instant>,
}

impl HoverTransition {
    fn target(self) -> f32 {
        if self.hovered { 1.0 } else { 0.0 }
    }

    fn raw(self, now: Instant) -> f32 {
        let Some(since) = self.since else {
            return self.target();
        };
        let elapsed = now.saturating_duration_since(since).as_secs_f32();
        if self.hovered {
            (self.from + elapsed / ENTER.as_secs_f32()).min(1.0)
        } else {
            (self.from - elapsed / EXIT.as_secs_f32()).max(0.0)
        }
    }

    fn set_hovered(&mut self, hovered: bool, now: Instant, motion: ControlMotion) -> bool {
        if self.hovered == hovered {
            return false;
        }
        let from = self.raw(now);
        *self = Self {
            hovered,
            from,
            since: (motion == ControlMotion::Standard).then_some(now),
        };
        true
    }
}

/// Slow at both ends, so the change starts and settles without a visible step.
fn ease(raw: f32) -> f32 {
    raw * raw * (3.0 - 2.0 * raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_should_ease_in_faster_than_it_eases_out() {
        let start = Instant::now();
        let mut transition = HoverTransition::default();
        assert!(transition.set_hovered(true, start, ControlMotion::Standard));
        assert_eq!(transition.raw(start), 0.0);
        assert_eq!(transition.raw(start + ENTER / 2), 0.5);
        assert_eq!(transition.raw(start + ENTER), 1.0);

        let left = start + ENTER;
        assert!(transition.set_hovered(false, left, ControlMotion::Standard));
        assert_eq!(transition.raw(left + EXIT / 2), 0.5);
        assert_eq!(transition.raw(left + EXIT), 0.0);
        assert!(ENTER < EXIT);
    }

    #[test]
    fn a_reversal_should_continue_from_the_current_level() {
        let start = Instant::now();
        let mut transition = HoverTransition::default();
        transition.set_hovered(true, start, ControlMotion::Standard);
        let reversed = start + ENTER / 2;
        transition.set_hovered(false, reversed, ControlMotion::Standard);
        assert_eq!(transition.raw(reversed), 0.5);
        assert_eq!(transition.raw(reversed + EXIT / 2), 0.0);
    }

    #[test]
    fn reduced_motion_should_switch_hover_at_once() {
        let start = Instant::now();
        let mut transition = HoverTransition::default();
        transition.set_hovered(true, start, ControlMotion::Reduced);
        assert_eq!(transition.raw(start), 1.0);
        transition.set_hovered(false, start, ControlMotion::Reduced);
        assert_eq!(transition.raw(start), 0.0);
    }

    #[test]
    fn repeating_the_current_hover_should_change_nothing() {
        let start = Instant::now();
        let mut transition = HoverTransition::default();
        assert!(!transition.set_hovered(false, start, ControlMotion::Standard));
        assert_eq!(transition, HoverTransition::default());
    }

    #[test]
    fn easing_should_hold_both_ends_and_the_midpoint() {
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(0.5), 0.5);
        assert_eq!(ease(1.0), 1.0);
        assert!(ease(0.1) < 0.1);
    }

    #[test]
    fn mixing_should_reach_each_end() {
        let rest = gpui::rgba(0x00000000);
        let hovered = gpui::rgba(0xffffffff);
        assert_eq!(mix_rgba(rest, hovered, 0.0), rest);
        assert_eq!(mix_rgba(rest, hovered, 1.0), hovered);
        assert_eq!(mix_rgba(rest, hovered, 0.5).a, 0.5);
    }
}
