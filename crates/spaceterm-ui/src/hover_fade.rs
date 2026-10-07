//! Hover that eases in and out, because GPUI hover styles switch in a single frame.
use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    App, ElementId, Entity, Global, Hitbox, HitboxBehavior, MouseExitEvent, MouseMoveEvent, Rgba,
    Window, WindowId, canvas,
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
                let hovered = pointer_over(&hitbox, window, cx);
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
                        set_pointer_outside(window, false, cx);
                        let hovered = pointer_over(&hitbox, window, cx);
                        set_hovered(&move_state, hovered, cx);
                    }
                });
                let exit_state = state.clone();
                window.on_mouse_event(move |_: &MouseExitEvent, phase, window, cx| {
                    if phase.capture() {
                        set_pointer_outside(window, true, cx);
                        set_hovered(&exit_state, false, cx);
                    }
                });
            },
        )
        .absolute()
        .inset_0()
    }
}

/// How far a flag has eased toward on, from 0 off to 1 on, such as controls that show while their
/// Pane has focus.
///
/// Call it every render with a key unique in the window. The first render takes the flag's value
/// at once; each later change eases like a hover unless `animate` is false. Frames continue until
/// the transition settles.
pub fn eased_flag(
    key: impl Into<ElementId>,
    on: bool,
    animate: bool,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    let now = cx.background_executor().now();
    let motion = if animate {
        crate::control_motion(cx)
    } else {
        ControlMotion::Reduced
    };
    let state = window.use_keyed_state(key, cx, |_, _| HoverTransition {
        hovered: on,
        from: 0.0,
        since: None,
    });
    let transition = state.update(cx, |transition, _| {
        transition.set_hovered(on, now, motion);
        *transition
    });
    let raw = transition.raw(now);
    if raw != transition.target() {
        window.request_animation_frame();
    }
    ease(raw)
}

/// Finishes every hover transition in progress, since test windows have no frame loop.
#[cfg(test)]
pub(crate) fn settle(cx: &mut gpui::VisualTestContext) {
    cx.run_until_parked();
    cx.executor().advance_clock(ENTER.max(EXIT));
    cx.update(|window, cx| window.simulate_next_frame(cx));
    cx.run_until_parked();
}

/// Whether the pointer rests over `hitbox` and can hover it.
///
/// GPUI keeps the last position and hit test after the pointer leaves a window, so a region under
/// that position would otherwise read as hovered on every later paint.
fn pointer_over(hitbox: &Hitbox, window: &Window, cx: &App) -> bool {
    !cx.has_active_drag()
        && !cx.try_global::<PointerOutside>().is_some_and(|outside| {
            outside
                .windows
                .contains(&window.window_handle().window_id())
        })
        && hitbox.is_hovered(window)
}

fn set_pointer_outside(window: &Window, outside: bool, cx: &mut App) {
    let window_id = window.window_handle().window_id();
    if outside {
        cx.default_global::<PointerOutside>()
            .windows
            .insert(window_id);
    } else if cx
        .try_global::<PointerOutside>()
        .is_some_and(|pointer| pointer.windows.contains(&window_id))
    {
        cx.global_mut::<PointerOutside>().windows.remove(&window_id);
    }
}

/// The windows the pointer has left since its last move inside them.
#[derive(Default)]
struct PointerOutside {
    windows: HashSet<WindowId>,
}

impl Global for PointerOutside {}

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

/// Paints a resting color partway to its hovered color.
///
/// Channels mix premultiplied by alpha, so fading from or to a transparent fill changes only its
/// coverage and never passes through a darker color. Each end returns its own color exactly.
pub fn mix_rgba(rest: Rgba, hovered: Rgba, level: f32) -> Rgba {
    if level <= 0.0 {
        return rest;
    }
    if level >= 1.0 {
        return hovered;
    }
    let a = rest.a * (1.0 - level) + hovered.a * level;
    if a <= 0.0 {
        return Rgba::default();
    }
    let channel = |from: f32, to: f32| (from * rest.a * (1.0 - level) + to * hovered.a * level) / a;
    Rgba {
        r: channel(rest.r, hovered.r),
        g: channel(rest.g, hovered.g),
        b: channel(rest.b, hovered.b),
        a,
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

    #[test]
    fn fading_in_a_transparent_fill_should_keep_its_color() {
        let hovered = gpui::rgba(0xd2d2d2ff);
        let halfway = mix_rgba(gpui::rgba(0x00000000), hovered, 0.5);
        assert_eq!(
            (halfway.r, halfway.g, halfway.b),
            (hovered.r, hovered.g, hovered.b)
        );
        assert_eq!(halfway.a, 0.5);
    }
}
