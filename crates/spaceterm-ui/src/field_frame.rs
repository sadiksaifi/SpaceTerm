//! Shared frame presentation for editable fields and composite input controls.
use gpui::prelude::*;
use gpui::{App, Div, ElementId, FocusHandle, Stateful, div, px};

use crate::Ringed;

/// Caller-owned validation and availability, independent of keyboard focus.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FieldState {
    #[cfg(feature = "appearance-exerciser")]
    preview_focused: bool,
    disabled: bool,
    invalid: bool,
}

impl FieldState {
    /// Pins focus decoration without acquiring keyboard focus in the development gallery.
    #[cfg(feature = "appearance-exerciser")]
    pub fn preview_focus(mut self, focused: bool) -> Self {
        self.preview_focused = focused;
        self
    }

    /// Disables the complete frame, including hover and focus decoration.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    /// Retains the invalid border even while the editor owns keyboard focus.
    pub fn invalid(mut self, invalid: bool) -> Self {
        self.invalid = invalid;
        self
    }
}

/// The frame border every field draws, which its focus ring covers.
const FRAME_BORDER_WIDTH: f32 = 1.0;

/// Complete field surface and validation paints supplied by the application.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldFrameTheme {
    background: gpui::Rgba,
    border: gpui::Rgba,
    invalid_border: gpui::Rgba,
    disabled_background: gpui::Rgba,
    disabled_border: gpui::Rgba,
    focus_ring: gpui::Rgba,
}

impl FieldFrameTheme {
    /// Creates all independent field paints.
    ///
    /// Focus never changes the frame. The ring outside it states focus alone, so an invalid field
    /// that gains focus keeps its invalid border and gains a ring.
    pub fn new(
        background: gpui::Rgba,
        border: gpui::Rgba,
        invalid_border: gpui::Rgba,
        disabled_background: gpui::Rgba,
        disabled_border: gpui::Rgba,
        focus_ring: gpui::Rgba,
    ) -> Self {
        Self {
            background,
            border,
            invalid_border,
            disabled_background,
            disabled_border,
            focus_ring,
        }
    }

    pub(crate) fn ring_color(self) -> gpui::Rgba {
        self.focus_ring
    }

    pub(crate) fn transparent() -> Self {
        let transparent = gpui::rgba(0);
        Self::new(
            transparent,
            transparent,
            transparent,
            transparent,
            transparent,
            transparent,
        )
    }

    fn paint(self, state: FieldState) -> (gpui::Rgba, gpui::Rgba) {
        if state.disabled {
            (self.disabled_background, self.disabled_border)
        } else if state.invalid {
            (self.background, self.invalid_border)
        } else {
            (self.background, self.border)
        }
    }
}

/// Creates the shared field frame while leaving its remaining geometry and children with its
/// caller.
///
/// The frame owns its corner radius so the focus ring stays concentric with it; callers do not
/// round the returned element again.
///
/// Attach the handle the editor itself takes focus with, because that is the handle the frame
/// decorates. A frame given an enclosing handle instead would report focus for everything inside
/// that scope: a dialog that focuses its own root would light every field it contains as though the
/// reader were typing in all of them. The editor continues to own editing, enabled state, content,
/// and input-method composition. Callers must keep its enabled state synchronized with `state`.
pub fn field_frame(
    id: impl Into<ElementId>,
    focus: &FocusHandle,
    state: FieldState,
    corner_radius: gpui::Pixels,
    cx: &App,
) -> Ringed<Stateful<Div>> {
    let theme = crate::floating_surface::hosted_text_input_theme(cx).frame;
    themed_field_frame(theme, id, focus, state, corner_radius)
}

/// The same frame for a control family that owns its own field paints.
pub(crate) fn themed_field_frame(
    theme: FieldFrameTheme,
    id: impl Into<ElementId>,
    focus: &FocusHandle,
    state: FieldState,
    corner_radius: gpui::Pixels,
) -> Ringed<Stateful<Div>> {
    let id = id.into();
    let ring_id = crate::focus_ring::ring_id(&id);
    #[cfg(not(feature = "appearance-exerciser"))]
    let pinned = false;
    #[cfg(feature = "appearance-exerciser")]
    let pinned = state.preview_focused;
    let frame = themed_field_surface(theme, id, state)
        .rounded(corner_radius)
        .track_focus(focus);
    // The ring resolves focus while painting, so the caller need not know whether the editor is
    // focused when it builds its layout.
    let ring = (!state.disabled).then(|| {
        crate::focus_ring(
            ring_id,
            theme.ring_color(),
            corner_radius,
            px(FRAME_BORDER_WIDTH),
        )
        .tracking(focus, pinned)
    });
    Ringed::new(frame, ring)
}

/// Creates shared input presentation for a composite without one editor focus handle.
pub fn field_surface(id: impl Into<ElementId>, state: FieldState, cx: &App) -> Stateful<Div> {
    themed_field_surface(
        crate::floating_surface::hosted_text_input_theme(cx).frame,
        id,
        state,
    )
}

/// Creates the same surface for a control family that owns its own field paints.
pub(crate) fn themed_field_surface(
    theme: FieldFrameTheme,
    id: impl Into<ElementId>,
    state: FieldState,
) -> Stateful<Div> {
    let (background, border) = theme.paint(state);
    div()
        .id(id)
        .relative()
        .border(px(FRAME_BORDER_WIDTH))
        .bg(background)
        .border_color(border)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn field_states_preserve_validation_and_disable_all_paints() {
        let colors = [1, 2, 3, 4, 5, 6].map(gpui::rgba);
        let theme = FieldFrameTheme::new(
            colors[0], colors[1], colors[2], colors[3], colors[4], colors[5],
        );
        assert_eq!(theme.paint(FieldState::default()), (colors[0], colors[1]));
        assert_eq!(
            theme.paint(FieldState::default().invalid(true)),
            (colors[0], colors[2])
        );
        for invalid in [false, true] {
            assert_eq!(
                theme.paint(FieldState::default().invalid(invalid).disabled(true)),
                (colors[3], colors[4])
            );
        }
        assert_eq!(theme.ring_color(), colors[5]);
    }

    /// The union of the visible bounds of the painted quads that `matches` selects.
    fn painted_bounds(
        quads: &[gpui::Quad],
        matches: impl Fn(&gpui::Quad) -> bool,
    ) -> gpui::Bounds<gpui::ScaledPixels> {
        quads
            .iter()
            .filter(|quad| matches(quad))
            .map(|quad| quad.bounds.intersect(&quad.content_mask.bounds))
            .reduce(|bounds, next| bounds.union(&next))
            .expect("a matching quad was painted")
    }

    struct FocusFixture {
        focus: FocusHandle,
        state: FieldState,
        theme: FieldFrameTheme,
    }

    impl gpui::Render for FocusFixture {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            div().size_full().p(px(20.0)).child(
                themed_field_frame(self.theme, "field", &self.focus, self.state, px(6.0))
                    .w(px(200.0))
                    .h(px(32.0)),
            )
        }
    }

    #[gpui::test]
    fn focused_invalid_field_shows_only_the_ring_over_its_border(cx: &mut gpui::TestAppContext) {
        let fill = gpui::rgba(0x20202080);
        let invalid = gpui::rgba(0xcc0000ff);
        // Translucent, like the built-in ring, so a border under the band would show through.
        let ring = gpui::rgba(0x00aa007f);
        let theme = FieldFrameTheme::new(
            fill,
            gpui::rgba(0x555555ff),
            invalid,
            fill,
            gpui::rgba(0x333333ff),
            ring,
        );
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            FocusFixture {
                focus,
                state: FieldState::default().invalid(true),
                theme,
            }
        });
        let red = |quad: &gpui::Quad| {
            let color = gpui::Rgba::from(quad.border_color);
            (color.r - invalid.r).abs() < 0.01 && color.g < 0.01 && color.b < 0.01
        };
        let border_alpha = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, _| {
                window
                    .painted_quads()
                    .iter()
                    .filter(|quad| red(quad))
                    .map(|quad| quad.border_color.a)
                    .fold(0.0, f32::max)
            })
        };
        cx.run_until_parked();
        cx.executor().advance_clock(crate::focus_ring::ENTRANCE / 2);
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let entering = border_alpha(cx);
        assert!(
            entering > 0.0 && entering < 1.0,
            "the border fades as the band fades in, alpha {entering}"
        );

        crate::focus_ring::settle(cx);
        assert_eq!(border_alpha(cx), 0.0, "no border shows through the band");
        let quads = cx.update(|window, _| window.painted_quads());
        let is_ring = |quad: &&gpui::Quad| {
            let color = gpui::Rgba::from(quad.border_color);
            (color.g - ring.g).abs() < 0.01 && color.r < 0.01 && color.a > 0.0
        };
        assert!(
            quads
                .iter()
                .any(|quad| quad.background == gpui::Background::from(fill)),
            "field fill must remain {fill:?}; painted {quads:?}"
        );
        assert!(
            quads
                .iter()
                .filter(is_ring)
                .all(|quad| quad.background.is_transparent())
        );
        let frame = painted_bounds(&quads, |quad| {
            quad.background == gpui::Background::from(fill)
        });
        let outline = painted_bounds(&quads, |quad| is_ring(&quad));
        assert!(
            outline.left() < frame.left()
                && outline.top() < frame.top()
                && outline.right() > frame.right()
                && outline.bottom() > frame.bottom(),
            "frame={frame:?}; outline={outline:?}"
        );

        root.update(cx, |root, cx| {
            root.state = root.state.disabled(true);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.update(|window, _| {
            window
                .painted_quads()
                .iter()
                .all(|quad| quad.border_color != ring.into())
        }));
    }
}
