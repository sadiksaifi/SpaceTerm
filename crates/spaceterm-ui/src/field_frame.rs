//! Shared frame presentation for editable fields and composite input controls.
use gpui::prelude::*;
use gpui::{App, Div, ElementId, FocusHandle, Stateful, div, px};

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

/// How far the focus ring's outer edge sits beyond the frame's own outer edge.
///
/// The ring clears the frame border and then leaves a gap, so the two read as two marks rather
/// than one thick edge. Both values are logical points and neither follows the density scale: a
/// ring is a structural mark, and a comfortable window does not need a heavier one.
const FOCUS_RING_WIDTH: f32 = 1.0;
const FOCUS_RING_GAP: f32 = 2.0;
/// The frame border the ring has to clear.
const FRAME_BORDER_WIDTH: f32 = 1.0;
/// The radius a field keeps when its caller states none.
const DEFAULT_CORNER_RADIUS: f32 = 6.0;

/// Complete field surface and validation paints supplied by the application.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldFrameTheme {
    background: gpui::Rgba,
    border: gpui::Rgba,
    focused_border: gpui::Rgba,
    invalid_border: gpui::Rgba,
    disabled_background: gpui::Rgba,
    disabled_border: gpui::Rgba,
    focus_ring: Option<gpui::Rgba>,
    focus_ring_width: gpui::Pixels,
    corner_radius: gpui::Pixels,
}

impl FieldFrameTheme {
    /// Creates all independent field paints.
    ///
    /// Focus is a hollow ring outside the frame, and it composes with the frame rather than
    /// replacing it: an invalid field that gains focus keeps its invalid border and gains a ring.
    pub fn new(
        background: gpui::Rgba,
        border: gpui::Rgba,
        focused_border: gpui::Rgba,
        invalid_border: gpui::Rgba,
        disabled_background: gpui::Rgba,
        disabled_border: gpui::Rgba,
    ) -> Self {
        Self {
            background,
            border,
            focused_border,
            invalid_border,
            disabled_background,
            disabled_border,
            focus_ring: None,
            focus_ring_width: px(FOCUS_RING_WIDTH),
            corner_radius: px(DEFAULT_CORNER_RADIUS),
        }
    }

    /// Sets the ring paint, which is authored apart from the focused frame border.
    ///
    /// The frame border states "this field is where typing goes"; the ring states "the keyboard is
    /// here". They are the same decision only by default, so an appearance may move one without
    /// moving the other. Left unset, the ring follows the focused border.
    pub fn focus_ring(mut self, color: gpui::Rgba) -> Self {
        self.focus_ring = Some(color);
        self
    }

    /// Sets the focus-ring width independently of the field frame border and radius.
    pub fn focus_ring_width(mut self, width: gpui::Pixels) -> Self {
        self.focus_ring_width = width.max(px(0.0));
        self
    }

    /// Sets the field's own corner radius so the ring stays concentric with it.
    pub fn corner_radius(mut self, radius: gpui::Pixels) -> Self {
        self.corner_radius = radius;
        self
    }

    fn ring_color(self) -> gpui::Rgba {
        self.focus_ring.unwrap_or(self.focused_border)
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

    fn paint(self, state: FieldState, focused: bool) -> (gpui::Rgba, gpui::Rgba) {
        if state.disabled {
            (self.disabled_background, self.disabled_border)
        } else {
            (
                self.background,
                if state.invalid {
                    self.invalid_border
                } else if focused {
                    self.focused_border
                } else {
                    self.border
                },
            )
        }
    }
}

/// Creates the shared field frame while leaving geometry and children with its caller.
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
    cx: &App,
) -> Stateful<Div> {
    let theme = crate::floating_surface::hosted_text_input_theme(cx).frame;
    themed_field_frame(theme, id, focus, state)
}

/// The same frame for a control family that owns its own field paints.
pub(crate) fn themed_field_frame(
    theme: FieldFrameTheme,
    id: impl Into<ElementId>,
    focus: &FocusHandle,
    state: FieldState,
) -> Stateful<Div> {
    let id = id.into();
    let ring_id = ElementId::NamedChild(std::sync::Arc::new(id.clone()), "focus-ring".into());
    themed_field_surface(theme, id, state)
        .track_focus(focus)
        .when(!state.disabled, |frame| {
            frame.child(focus_ring(theme, ring_id, focus, state))
        })
}

/// The hollow ring a focused field draws outside its own frame.
///
/// It is a bordered element rather than a shadow. A zero-blur shadow is a filled rounded rect
/// behind the field, and every field on a translucent host shows it straight through the interior,
/// which turns focus into a colored plate and buries the text. A border paints only the edge, so
/// the interior keeps transmitting its host and the invalid frame stays visible underneath.
///
/// The element tracks the same handle the frame does, so focus resolves during prepaint without
/// the caller having to know whether the editor is focused when it builds its layout.
fn focus_ring(
    theme: FieldFrameTheme,
    id: ElementId,
    focus: &FocusHandle,
    state: FieldState,
) -> impl IntoElement {
    let ring = theme.ring_color();
    let outset = px(FOCUS_RING_GAP) + theme.focus_ring_width;
    #[cfg(not(feature = "appearance-exerciser"))]
    let pinned = false;
    #[cfg(feature = "appearance-exerciser")]
    let pinned = state.preview_focused;
    let _ = state;
    div()
        .id(id)
        .absolute()
        .top(-outset)
        .right(-outset)
        .bottom(-outset)
        .left(-outset)
        .rounded(theme.corner_radius + outset)
        .border(theme.focus_ring_width)
        .border_color(gpui::rgba(0))
        .when(pinned, |ring_element| ring_element.border_color(ring))
        .focus(move |style| style.border_color(ring))
        .track_focus(focus)
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
    #[cfg(not(feature = "appearance-exerciser"))]
    let focused = false;
    #[cfg(feature = "appearance-exerciser")]
    let focused = state.preview_focused;
    let (background, border) = theme.paint(state, focused);
    div()
        .id(id)
        .relative()
        .border(px(FRAME_BORDER_WIDTH))
        .bg(background)
        .border_color(border)
        .when(!state.disabled, |frame| {
            frame.focus(move |style| {
                // `paint` answers invalid before focused, so an invalid field keeps its own border
                // here and the ring outside carries the focus instead.
                let (_, border) = theme.paint(state, true);
                style.border_color(border)
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn field_states_preserve_validation_and_disable_all_interaction_paints() {
        let colors = [1, 2, 3, 4, 5, 6].map(gpui::rgba);
        let theme = FieldFrameTheme::new(
            colors[0], colors[1], colors[2], colors[3], colors[4], colors[5],
        );
        assert_eq!(
            theme.paint(FieldState::default(), false),
            (colors[0], colors[1])
        );
        assert_eq!(
            theme.paint(FieldState::default(), true),
            (colors[0], colors[2])
        );
        assert_eq!(
            theme.paint(FieldState::default().invalid(true), true),
            (colors[0], colors[3])
        );
        for focused in [false, true] {
            for invalid in [false, true] {
                assert_eq!(
                    theme.paint(
                        FieldState::default().invalid(invalid).disabled(true),
                        focused
                    ),
                    (colors[4], colors[5])
                );
            }
        }
    }

    /// The ring is its own paint, and a field that states none keeps following its focused border.
    #[test]
    fn focus_ring_is_independently_authorable_and_defaults_to_the_focused_border() {
        let colors = [1, 2, 3, 4, 5, 6, 7].map(gpui::rgba);
        let theme = FieldFrameTheme::new(
            colors[0], colors[1], colors[2], colors[3], colors[4], colors[5],
        );

        assert_eq!(theme.ring_color(), colors[2]);
        assert_eq!(theme.focus_ring(colors[6]).ring_color(), colors[6]);
        assert_eq!(
            theme
                .focus_ring(colors[6])
                .paint(FieldState::default(), true),
            theme.paint(FieldState::default(), true),
            "moving the ring must not move the frame"
        );
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
                themed_field_frame(self.theme, "field", &self.focus, self.state)
                    .w(px(200.0))
                    .h(px(32.0))
                    .rounded(self.theme.corner_radius),
            )
        }
    }

    #[gpui::test]
    fn focused_invalid_field_paints_a_hollow_outset_ring_and_keeps_its_frame(
        cx: &mut gpui::TestAppContext,
    ) {
        let fill = gpui::rgba(0x20202080);
        let invalid = gpui::rgba(0xcc0000ff);
        let ring = gpui::rgba(0x00aa00ff);
        let theme = FieldFrameTheme::new(
            fill,
            gpui::rgba(0x555555ff),
            gpui::rgba(0x0055ffff),
            invalid,
            fill,
            gpui::rgba(0x333333ff),
        )
        .focus_ring(ring);
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            FocusFixture {
                focus,
                state: FieldState::default().invalid(true),
                theme,
            }
        });
        cx.run_until_parked();
        let quads = cx.update(|window, _| window.painted_quads());
        let bounds_for = |color: gpui::Rgba| {
            quads
                .iter()
                .filter(|quad| quad.border_color == color.into())
                .map(|quad| quad.bounds.intersect(&quad.content_mask.bounds))
                .reduce(|bounds, next| bounds.union(&next))
                .unwrap()
        };
        let frame = bounds_for(invalid);
        let outline = bounds_for(ring);
        assert!(
            quads
                .iter()
                .any(|quad| quad.background == gpui::Background::from(fill)),
            "field fill must remain {fill:?}; painted {quads:?}"
        );
        assert!(
            quads
                .iter()
                .filter(|quad| quad.border_color == ring.into())
                .all(|quad| quad.background.is_transparent())
        );
        assert!(
            outline.left() < frame.left(),
            "frame={frame:?}; outline={outline:?}; all={quads:?}"
        );
        assert!(outline.top() < frame.top());
        assert!(outline.right() > frame.right());
        assert!(outline.bottom() > frame.bottom());

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
