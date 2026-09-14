//! Shared frame presentation for editable fields and composite input controls.
use gpui::prelude::*;
use gpui::{App, BoxShadow, Div, ElementId, FocusHandle, Stateful, div, point, px};

/// Caller-owned validation and availability, independent of keyboard focus.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FieldState {
    disabled: bool,
    invalid: bool,
}

impl FieldState {
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

/// Complete field surface and validation paints supplied by the application.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldFrameTheme {
    background: gpui::Rgba,
    border: gpui::Rgba,
    focused_border: gpui::Rgba,
    invalid_border: gpui::Rgba,
    disabled_background: gpui::Rgba,
    disabled_border: gpui::Rgba,
}

impl FieldFrameTheme {
    /// Creates all independent field paints. Focus is a separate outset ring on invalid fields.
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
        }
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
/// Attach the editor's focus handle; the frame follows descendant focus automatically. The editor
/// continues to own editing, enabled state, content, and input-method composition. Callers must keep
/// its enabled state synchronized with `state`.
pub fn field_frame(
    id: impl Into<ElementId>,
    focus: &FocusHandle,
    state: FieldState,
    cx: &App,
) -> Stateful<Div> {
    field_surface(id, state, cx).track_focus(focus)
}

/// Creates shared input presentation for a composite without one editor focus handle.
pub fn field_surface(id: impl Into<ElementId>, state: FieldState, cx: &App) -> Stateful<Div> {
    let theme = cx.global::<crate::TextInputTheme>().frame;
    let (background, border) = theme.paint(state, false);
    div()
        .id(id)
        .relative()
        .border(px(1.0))
        .bg(background)
        .border_color(border)
        .when(!state.disabled, |frame| {
            frame.in_focus(move |style| {
                let (_, border) = theme.paint(state, true);
                let style = style.border_color(border);
                if state.invalid {
                    style.shadow(vec![BoxShadow {
                        color: theme.focused_border.into(),
                        offset: point(px(0.0), px(0.0)),
                        blur_radius: px(0.0),
                        spread_radius: px(2.0),
                    }])
                } else {
                    style
                }
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
}
