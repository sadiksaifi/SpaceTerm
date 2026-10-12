//! A centered symbol, title, explanation, and actions for a surface with nothing to show yet, in
//! the manner of a native content-unavailable view.

use gpui::prelude::*;
use gpui::{AnyElement, App, ElementId, Pixels, SharedString, Window, accesskit, div};
use spaceterm_ui::{Icon, IconName};

use crate::appearance::Color;
use crate::ui::appearance::{gpui_color, shared_chrome};
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};

/// The widest a content-unavailable column grows before its text wraps, unscaled.
const MAXIMUM_WIDTH: f32 = 360.0;

/// The text colors of a content-unavailable view, resolved for the surface it rests on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) struct ContentUnavailablePaint {
    pub(in crate::ui) title: Color,
    pub(in crate::ui) detail: Color,
}

/// One content-unavailable view. It names itself to accessibility with its title and explanation,
/// and its symbol is decorative.
#[derive(IntoElement)]
pub(in crate::ui) struct ContentUnavailable {
    id: ElementId,
    selector: &'static str,
    icon: IconName,
    title: SharedString,
    detail: SharedString,
    symbol: Option<Color>,
    paint: Option<ContentUnavailablePaint>,
    title_role: TextRole,
    symbol_size: f32,
    actions: Vec<AnyElement>,
}

impl ContentUnavailable {
    pub(in crate::ui) fn new(
        id: impl Into<ElementId>,
        selector: &'static str,
        icon: IconName,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            selector,
            icon,
            title: title.into(),
            detail: detail.into(),
            symbol: None,
            paint: None,
            title_role: TextRole::Section,
            symbol_size: 32.0,
            actions: Vec::new(),
        }
    }

    /// The symbol's color. It defaults to the Chrome icon color.
    pub(in crate::ui) fn symbol(mut self, color: Color) -> Self {
        self.symbol = Some(color);
        self
    }

    /// Text colors for a surface other than the Chrome window background.
    pub(in crate::ui) fn paint(mut self, paint: ContentUnavailablePaint) -> Self {
        self.paint = Some(paint);
        self
    }

    /// A larger title and symbol, for a view that fills a window rather than a Pane.
    pub(in crate::ui) fn prominent(mut self) -> Self {
        self.title_role = TextRole::Title;
        self.symbol_size = 44.0;
        self
    }

    /// The actions, in presentation order with the primary action last.
    pub(in crate::ui) fn actions(mut self, actions: Vec<AnyElement>) -> Self {
        self.actions = actions;
        self
    }
}

impl RenderOnce for ContentUnavailable {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let appearance = shared_chrome(cx);
        let paint = self.paint.unwrap_or(ContentUnavailablePaint {
            title: appearance.colors.text,
            detail: appearance.colors.text_secondary,
        });
        let symbol = self.symbol.unwrap_or(appearance.colors.icon);
        let selector = self.selector;
        let symbol_size: Pixels = appearance.spacing(self.symbol_size);
        div()
            .id(self.id)
            .role(accesskit::Role::Group)
            .aria_label(self.title.clone())
            .aria_description(self.detail.clone())
            .debug_selector(move || selector.to_owned())
            .max_w(appearance.spacing(MAXIMUM_WIDTH))
            .flex()
            .flex_col()
            .items_center()
            .text_center()
            .child(
                div()
                    .debug_selector(move || format!("{selector}-symbol"))
                    .text_color(gpui_color(symbol))
                    .child(Icon::inherited(self.icon, symbol_size)),
            )
            .child(
                div()
                    .debug_selector(move || format!("{selector}-title"))
                    .mt(appearance.spacing(12.0))
                    .whitespace_normal()
                    .chrome_text(appearance.typography.style(self.title_role))
                    .text_color(gpui_color(paint.title))
                    .child(self.title),
            )
            .child(
                div()
                    .debug_selector(move || format!("{selector}-detail"))
                    .mt(appearance.spacing(4.0))
                    .whitespace_normal()
                    .chrome_text(appearance.typography.style(TextRole::Body))
                    .text_color(gpui_color(paint.detail))
                    .child(self.detail),
            )
            .when(!self.actions.is_empty(), |content| {
                content.child(
                    div()
                        .debug_selector(move || format!("{selector}-actions"))
                        .mt(appearance.spacing(16.0))
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .justify_center()
                        .gap(appearance.spacing(8.0))
                        .children(self.actions),
                )
            })
    }
}
