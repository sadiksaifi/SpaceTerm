//! The one presentation of an empty, unavailable, or no-results state.
//!
//! A surface with nothing to show, or that can't show what it holds, says why with an icon, a
//! title, and a detail, and offers at most two actions: the first Primary, the second a link.
//! The surface owns the placement and its own edge; this element owns the order, type, spacing,
//! and width, so every such state reads the same.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, Font, Pixels, RenderOnce, Rgba, SharedString, Text, Window, div, px};

use crate::{Button, ButtonSize, ButtonVariant, CustomIconName, Icon, IconName};

/// The widest an empty state's text runs before it wraps, at the default spacing scale.
pub const EMPTY_STATE_MAXIMUM_WIDTH: f32 = 360.0;
const ICON_GAP: f32 = 8.0;
const DETAIL_GAP: f32 = 4.0;
const ACTIONS_GAP: f32 = 12.0;
const ACTION_GAP: f32 = 8.0;
/// The size the actions share, so a surface can measure them before it lays out.
pub const EMPTY_STATE_ACTION_SIZE: ButtonSize = ButtonSize::Regular;

type IconBuilder = Rc<dyn Fn(Rgba, Pixels) -> AnyElement>;

/// One resolved text role: its font, size, line box, and color.
#[derive(Clone, Debug, PartialEq)]
pub struct EmptyStateText {
    font: Font,
    size: Pixels,
    line_height: Pixels,
    color: Rgba,
}

impl EmptyStateText {
    pub fn new(font: Font, size: Pixels, line_height: Pixels, color: Rgba) -> Self {
        Self {
            font,
            size,
            line_height,
            color,
        }
    }
}

/// Whether the state asks the person to act. Only a state that needs action colors its icon.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EmptyStateTone {
    /// Nothing is wrong: the icon takes the muted color.
    #[default]
    Quiet,
    /// Something needs the person before the surface can show its content.
    Warning,
    /// Something failed, and the person must act.
    Error,
}

/// The type, icon paint, and spacing scale of the surface presenting an empty state.
///
/// The title takes the section role and the detail the secondary role. The icon is twice the
/// body line height.
#[derive(Clone, Debug, PartialEq)]
pub struct EmptyStateStyle {
    title: EmptyStateText,
    detail: EmptyStateText,
    icon_size: Pixels,
    quiet: Rgba,
    warning: Rgba,
    error: Rgba,
    spacing_scale: f32,
}

impl EmptyStateStyle {
    /// Every icon takes `muted` until [`Self::intent_colors`] supplies the action colors.
    pub fn new(
        title: EmptyStateText,
        detail: EmptyStateText,
        body_line_height: Pixels,
        muted: Rgba,
        spacing_scale: f32,
    ) -> Self {
        Self {
            title,
            detail,
            icon_size: body_line_height * 2.0,
            quiet: muted,
            warning: muted,
            error: muted,
            spacing_scale: crate::appearance::normalized_scale(spacing_scale),
        }
    }

    /// The icon colors of a state that needs action.
    pub fn intent_colors(mut self, warning: Rgba, error: Rgba) -> Self {
        self.warning = warning;
        self.error = error;
        self
    }

    fn space(&self, baseline: f32) -> Pixels {
        px(baseline * self.spacing_scale)
    }

    /// The widest the content runs.
    pub fn maximum_width(&self) -> Pixels {
        self.space(EMPTY_STATE_MAXIMUM_WIDTH)
    }

    fn icon_color(&self, tone: EmptyStateTone) -> Rgba {
        match tone {
            EmptyStateTone::Quiet => self.quiet,
            EmptyStateTone::Warning => self.warning,
            EmptyStateTone::Error => self.error,
        }
    }

    /// The detail's font, size, and line box, for measuring how it wraps.
    pub fn detail_font(&self) -> (&Font, Pixels, Pixels) {
        (&self.detail.font, self.detail.size, self.detail.line_height)
    }

    /// The height of a state with a one-line title, `detail_lines` lines of detail, and, when
    /// it has actions, actions `action_height` tall.
    pub fn height(&self, icon: bool, detail_lines: usize, action_height: Option<Pixels>) -> Pixels {
        let mut height = self.title.line_height;
        if icon {
            height += self.icon_size + self.space(ICON_GAP);
        }
        if detail_lines > 0 {
            height += self.space(DETAIL_GAP) + self.detail.line_height * detail_lines as f32;
        }
        if let Some(action_height) = action_height {
            height += self.space(ACTIONS_GAP) + action_height;
        }
        height
    }
}

/// An empty, unavailable, or no-results state: icon, title, detail, and up to two actions.
#[derive(IntoElement)]
pub struct EmptyState {
    style: EmptyStateStyle,
    icon: Option<IconBuilder>,
    tone: EmptyStateTone,
    title: SharedString,
    title_selector: Option<String>,
    supplement: Option<AnyElement>,
    detail: Option<AnyElement>,
    detail_line_limit: Option<usize>,
    primary: Option<Button>,
    link: Option<Button>,
    footer: Option<AnyElement>,
    debug_selector: Option<String>,
}

impl EmptyState {
    pub fn new(style: EmptyStateStyle, title: impl Into<SharedString>) -> Self {
        Self {
            style,
            icon: None,
            tone: EmptyStateTone::Quiet,
            title: title.into(),
            title_selector: None,
            supplement: None,
            detail: None,
            detail_line_limit: None,
            primary: None,
            link: None,
            footer: None,
            debug_selector: None,
        }
    }

    pub fn icon(self, name: IconName) -> Self {
        self.icon_with(Rc::new(move |tint, size| {
            Icon::new(name, size, tint).into_any_element()
        }))
    }

    pub fn custom_icon(self, name: CustomIconName) -> Self {
        self.icon_with(Rc::new(move |tint, size| {
            Icon::custom(name, size, tint).into_any_element()
        }))
    }

    /// An icon the caller builds at the given tint and size.
    pub fn icon_with(mut self, build: Rc<dyn Fn(Rgba, Pixels) -> AnyElement>) -> Self {
        self.icon = Some(build);
        self
    }

    pub fn tone(mut self, tone: EmptyStateTone) -> Self {
        self.tone = tone;
        self
    }

    /// One or two sentences under the title, each ending with a period.
    pub fn detail(self, detail: impl Into<SharedString>) -> Self {
        // Text with an id publishes itself, so assistive technology reads the detail.
        self.detail_element(Text::new("empty-state-detail".into(), detail.into()))
    }

    /// A detail that carries its own accessibility, styled as the detail.
    pub fn detail_element(mut self, detail: impl IntoElement) -> Self {
        self.detail = Some(detail.into_any_element());
        self
    }

    /// Bounds the detail to `lines`, for a surface whose height is measured.
    pub fn detail_line_limit(mut self, lines: usize) -> Self {
        self.detail_line_limit = Some(lines);
        self
    }

    /// A line between the title and the detail that names what the state is about, such as a
    /// path. It keeps the caller's own text style.
    pub fn supplement(mut self, supplement: impl IntoElement) -> Self {
        self.supplement = Some(supplement.into_any_element());
        self
    }

    /// The default action, presented as Primary. Give it its shortcut when it has one.
    pub fn primary(mut self, button: Button) -> Self {
        self.primary = Some(button);
        self
    }

    /// The second action, presented as a link.
    pub fn link(mut self, button: Button) -> Self {
        self.link = Some(button);
        self
    }

    /// Content under the actions that belongs to them, such as an action's progress.
    pub fn footer(mut self, footer: impl IntoElement) -> Self {
        self.footer = Some(footer.into_any_element());
        self
    }

    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    pub fn title_debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.title_selector = Some(selector.into());
        self
    }
}

impl RenderOnce for EmptyState {
    fn render(self, _: &mut Window, _: &mut gpui::App) -> impl IntoElement {
        let Self {
            style,
            icon,
            tone,
            title,
            title_selector,
            supplement,
            detail,
            detail_line_limit,
            primary,
            link,
            footer,
            debug_selector,
        } = self;
        let has_actions = primary.is_some() || link.is_some();
        div()
            .when_some(debug_selector, |state, selector| {
                state.debug_selector(move || selector.clone())
            })
            .w_full()
            .max_w(style.maximum_width())
            .flex()
            .flex_col()
            .items_center()
            .text_center()
            .when_some(icon, |state, icon| {
                let size = style.icon_size;
                state.child(
                    div()
                        .size(size)
                        .flex_shrink_0()
                        .mb(style.space(ICON_GAP))
                        .child(icon(style.icon_color(tone), size)),
                )
            })
            .child(
                div()
                    .when_some(title_selector, |title, selector| {
                        title.debug_selector(move || selector.clone())
                    })
                    .w_full()
                    .whitespace_normal()
                    .font(style.title.font.clone())
                    .text_size(style.title.size)
                    .line_height(style.title.line_height)
                    .text_color(style.title.color)
                    .child(Text::new("empty-state-title".into(), title)),
            )
            .when_some(supplement, |state, supplement| {
                state.child(
                    div()
                        .w_full()
                        .mt(style.space(DETAIL_GAP))
                        .whitespace_normal()
                        .child(supplement),
                )
            })
            .when_some(detail, |state, detail| {
                state.child(
                    div()
                        .w_full()
                        .mt(style.space(DETAIL_GAP))
                        .whitespace_normal()
                        .when_some(detail_line_limit, |detail, lines| detail.line_clamp(lines))
                        .font(style.detail.font.clone())
                        .text_size(style.detail.size)
                        .line_height(style.detail.line_height)
                        .text_color(style.detail.color)
                        .child(detail),
                )
            })
            .when(has_actions, |state| {
                state.child(
                    div()
                        .mt(style.space(ACTIONS_GAP))
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .items_center()
                        .justify_center()
                        .gap(style.space(ACTION_GAP))
                        .children(primary.map(|button| {
                            button
                                .variant(ButtonVariant::Primary)
                                .size(EMPTY_STATE_ACTION_SIZE)
                        }))
                        .children(link.map(|button| {
                            button
                                .variant(ButtonVariant::Link)
                                .size(EMPTY_STATE_ACTION_SIZE)
                        })),
                )
            })
            .children(footer)
    }
}
