//! Presentation shared by every Settings Row.
//!
//! These are compositions of existing reusable controls, not new control families: a row is a
//! label, a control, and an optional reset affordance; a stepper is two icon buttons around a
//! readout. Interaction behavior stays in `spaceterm-ui`.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Rgba, SharedString, Window, div, px, rgba};
use spaceterm_ui::{Button, ButtonSize, ButtonVariant, Icon, IconButton, IconName, Tooltip};

use crate::appearance::Color;
use crate::ui::appearance::ChromeAppearance;

/// One stepper step, negative for decrement and positive for increment.
type StepHandler = Rc<dyn Fn(i32, &mut Window, &mut App)>;

pub(super) fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}

/// The width every scheme's color strip takes, so the names beside them share one column.
const SWATCH_WIDTH: f32 = 88.0;

/// The least space between a label and the control it names, so the two never touch.
const LABEL_GAP: f32 = 16.0;

/// The trailing column every row ends with, so a row action never shifts the control beside it and
/// every control on every page shares one right edge.
pub(super) const TRAILING_WIDTH: f32 = 28.0;

/// How a row arranges its label and its content.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum SettingsRowLayout {
    /// A right-aligned label beside its control, which is the form default.
    Beside,
    /// A label above content that needs the whole row, such as a list.
    Above,
    /// Content spanning the row with no label of its own, for a group whose title already names
    /// it. Repeating that title on the only row inside the box says the same thing twice.
    Full,
}

/// One titled run of related rows.
///
/// Grouping is the structure a settings form is read by, and here it is carried entirely by space
/// and by the title: rows within a run sit closer together than one run sits to the next. Nothing
/// is ruled off. A frame would have to be drawn as a border, because a scheme is free to resolve
/// the window, panel, and elevated surfaces to one color and the built-in dark scheme does exactly
/// that; and a hairline between every pair of rows adds a line for every reading the eye already
/// gets from the gap.
pub(super) struct SettingsGroup {
    selector: String,
    title: &'static str,
    rows: Vec<AnyElement>,
}

impl SettingsGroup {
    pub(super) fn new(selector: String, title: &'static str, rows: Vec<AnyElement>) -> Self {
        Self {
            selector,
            title,
            rows,
        }
    }

    pub(super) fn render(self, appearance: &ChromeAppearance) -> impl IntoElement {
        let selector = self.selector.clone();
        let title_selector = format!("{selector}-title");
        div()
            .debug_selector(move || selector.clone())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(4.0))
            .child(
                div()
                    .debug_selector(move || title_selector.clone())
                    .font(appearance.emphasis.clone())
                    .text_size(appearance.text_size(11.0))
                    .text_color(gpui_color(appearance.colors.text_secondary))
                    .child(self.title),
            )
            .child(div().flex().flex_col().w_full().children(self.rows))
    }
}

/// One labeled row: a right-aligned label, its control, and an optional reset affordance.
pub(super) struct SettingsRow {
    selector: &'static str,
    label: &'static str,
    description: Option<SharedString>,
    control: AnyElement,
    reset: Option<AnyElement>,
    highlighted: bool,
    layout: SettingsRowLayout,
}

impl SettingsRow {
    pub(super) fn new(
        selector: &'static str,
        label: &'static str,
        control: impl IntoElement,
    ) -> Self {
        Self {
            selector,
            label,
            description: None,
            control: control.into_any_element(),
            reset: None,
            highlighted: false,
            layout: SettingsRowLayout::Beside,
        }
    }

    /// Adds one line of guidance below the control.
    pub(super) fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Adds the affordance restoring this row's default. Present only when it differs.
    pub(super) fn reset(mut self, reset: Option<impl IntoElement>) -> Self {
        self.reset = reset.map(IntoElement::into_any_element);
        self
    }

    /// Marks the row Settings Search revealed, so the eye lands on it.
    pub(super) fn highlighted(mut self, highlighted: bool) -> Self {
        self.highlighted = highlighted;
        self
    }

    /// Chooses where the label sits. Content that needs the whole row takes the label above it.
    pub(super) fn layout(mut self, layout: SettingsRowLayout) -> Self {
        self.layout = layout;
        self
    }

    pub(super) fn render(self, appearance: &ChromeAppearance) -> impl IntoElement {
        let selector = self.selector;
        let label_selector = format!("{selector}-label");
        let above = self.layout == SettingsRowLayout::Above;
        let full = self.layout == SettingsRowLayout::Full;
        // One rule for the whole form: the label starts at the content's left edge, the control
        // ends at its right edge, and nothing is centered.
        let caption = |description: SharedString| {
            div()
                .text_size(appearance.text_size(11.0))
                .text_color(gpui_color(appearance.colors.text_muted))
                .whitespace_normal()
                .child(description)
        };
        // Guidance belongs to the label, not to the row: under a tall control it would otherwise
        // come to rest between two rows, reading as a stray sentence belonging to neither. Stacked
        // with the label it stays anchored to the setting it explains, and it stops where the
        // control begins instead of running the width of the page.
        let label = (!full).then(|| {
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .flex_1()
                .gap(appearance.spacing(2.0))
                .child(
                    div()
                        .debug_selector(move || label_selector.clone())
                        .text_color(gpui_color(appearance.colors.text))
                        .whitespace_normal()
                        .child(self.label),
                )
                .children(self.description.clone().filter(|_| !above).map(&caption))
                .into_any_element()
        });
        let (label_above, label_beside) = if above { (label, None) } else { (None, label) };
        let primary = div()
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .gap(appearance.spacing(LABEL_GAP))
            .children(label_beside)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .min_w_0()
                    .when(above || full, |content| content.flex_1())
                    .when(!(above || full), |content| content.flex_none())
                    .child(self.control),
            )
            .child(
                div()
                    .w(appearance.text_size(TRAILING_WIDTH))
                    .flex_none()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .children(self.reset),
            );
        // A row whose content takes the whole width has no label column to stack guidance in, so
        // it keeps its own line underneath.
        let trailing_caption = self.description.filter(|_| above || full).map(caption);
        div()
            .debug_selector(move || selector.to_owned())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(3.0))
            .py(appearance.spacing(8.0))
            .when(self.highlighted, |row| {
                row.bg(gpui_color(appearance.colors.info_background))
            })
            .children(label_above)
            .child(primary)
            .children(trailing_caption)
    }
}

/// A section heading with its explanation.
pub(super) fn section_header(
    selector: &'static str,
    title: &'static str,
    description: &'static str,
    appearance: &ChromeAppearance,
) -> impl IntoElement {
    div()
        .debug_selector(move || selector.to_owned())
        .flex()
        .flex_col()
        .w_full()
        .gap(appearance.spacing(3.0))
        .child(
            div()
                .font(appearance.heading.clone())
                .text_size(appearance.text_size(17.0))
                .text_color(gpui_color(appearance.colors.text))
                .child(title),
        )
        .child(
            div()
                .text_size(appearance.text_size(11.0))
                .text_color(gpui_color(appearance.colors.text_muted))
                .whitespace_normal()
                .child(description),
        )
}

/// The affordance restoring one row's default value.
pub(super) fn reset_button(
    selector: String,
    enabled: bool,
    on_reset: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let icon_selector = SharedString::from(selector.clone());
    IconButton::new(icon_selector, "Reset to default", move |foreground| {
        Icon::new(IconName::RotateCcw, px(12.0), foreground).into_any_element()
    })
    .variant(ButtonVariant::Ghost)
    .size(ButtonSize::Small)
    .disabled(!enabled)
    .tab_stop(true)
    .debug_selector(selector.clone())
    .tooltip(Tooltip::new(
        SharedString::from(format!("{selector}-tooltip")),
        "Reset to default",
    ))
    .on_activate(move |_, window, cx| on_reset(window, cx))
}

/// A bounded numeric control: decrement, a readout, increment.
///
/// Every value it can request is inside the range the Settings document already validates, so the
/// control cannot compose an invalid document.
pub(super) struct Stepper {
    selector: &'static str,
    accessibility_name: &'static str,
    value: SharedString,
    can_decrease: bool,
    can_increase: bool,
    enabled: bool,
    on_step: Option<StepHandler>,
}

impl Stepper {
    pub(super) fn new(
        selector: &'static str,
        accessibility_name: &'static str,
        value: impl Into<SharedString>,
    ) -> Self {
        Self {
            selector,
            accessibility_name,
            value: value.into(),
            can_decrease: true,
            can_increase: true,
            enabled: true,
            on_step: None,
        }
    }

    /// Disables the ends of the range so the control cannot request a rejected value.
    pub(super) fn bounds(mut self, can_decrease: bool, can_increase: bool) -> Self {
        self.can_decrease = can_decrease;
        self.can_increase = can_increase;
        self
    }

    pub(super) fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Handles one step, negative for decrement and positive for increment.
    pub(super) fn on_step(
        mut self,
        handler: impl Fn(i32, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_step = Some(Rc::new(handler));
        self
    }

    pub(super) fn render(self, appearance: &ChromeAppearance) -> impl IntoElement {
        let accessibility_name = self.accessibility_name;
        let step = |selector: String,
                    verb: &'static str,
                    icon: IconName,
                    delta: i32,
                    enabled: bool,
                    handler: Option<StepHandler>| {
            IconButton::new(
                SharedString::from(selector.clone()),
                SharedString::from(format!("{verb} {accessibility_name}")),
                move |foreground| Icon::new(icon, px(11.0), foreground).into_any_element(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Compact)
            .disabled(!enabled)
            .tab_stop(true)
            .debug_selector(selector)
            .on_activate(move |_, window, cx| {
                if let Some(handler) = &handler {
                    handler(delta, window, cx);
                }
            })
        };
        let selector = self.selector;
        div()
            .debug_selector(move || selector.to_owned())
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .w(appearance.text_size(132.0))
            .gap(appearance.spacing(2.0))
            .px(appearance.spacing(2.0))
            .h(appearance.height(28.0, 12.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(gpui_color(appearance.colors.input_border))
            .bg(gpui_color(appearance.colors.input_background))
            .child(step(
                format!("{selector}-decrease"),
                "Decrease",
                IconName::Minus,
                -1,
                self.enabled && self.can_decrease,
                self.on_step.clone(),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_align(gpui::TextAlign::Center)
                    .text_size(appearance.text_size(12.0))
                    .text_color(gpui_color(if self.enabled {
                        appearance.colors.input_text
                    } else {
                        appearance.colors.input_disabled_text
                    }))
                    .debug_selector({
                        let value_selector = format!("{selector}-value");
                        move || value_selector.clone()
                    })
                    .child(self.value),
            )
            .child(step(
                format!("{selector}-increase"),
                "Increase",
                IconName::Plus,
                1,
                self.enabled && self.can_increase,
                self.on_step,
            ))
    }
}

/// A left-to-right strip of representative scheme colors.
///
/// Every strip is the same size whatever a scheme offers, so the names beside them line up in one
/// column. A scheme with fewer colors shows wider bands rather than a shorter strip.
pub(super) fn swatch_strip(
    selector: String,
    swatches: &[Color],
    appearance: &ChromeAppearance,
) -> impl IntoElement {
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .w(appearance.text_size(SWATCH_WIDTH))
        .h(appearance.text_size(14.0))
        .rounded(px(4.0))
        .overflow_hidden()
        .bg(gpui_color(appearance.colors.element_background))
        .children(
            swatches
                .iter()
                .map(|color| div().flex_1().h_full().bg(gpui_color(*color))),
        )
}

/// A short status a row carries, such as a scheme being the one in use.
///
/// It is filled rather than outlined, so it reads as a state rather than as one more frame. The
/// fill comes from the raised element role rather than the plain element background, which a
/// scheme may resolve to the window background and would leave the badge invisible.
pub(super) fn badge(
    label: impl Into<SharedString>,
    appearance: &ChromeAppearance,
) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .h(appearance.text_size(16.0))
        .px(appearance.spacing(6.0))
        .rounded(px(4.0))
        .bg(gpui_color(appearance.colors.element_active))
        .text_size(appearance.text_size(10.0))
        .text_color(gpui_color(appearance.colors.text_secondary))
        .child(label.into())
}

/// A text button used by the interchange and diagnostics rows.
pub(super) fn action_button(
    selector: &'static str,
    label: &'static str,
    enabled: bool,
    on_activate: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    Button::new(selector, label)
        .variant(ButtonVariant::Outline)
        .size(ButtonSize::Regular)
        .disabled(!enabled)
        .tab_stop(true)
        .debug_selector(selector)
        .on_activate(move |_, window, cx| on_activate(window, cx))
}
