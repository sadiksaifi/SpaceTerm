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

/// The label column width, so every control in a section starts at the same offset.
const LABEL_WIDTH: f32 = 188.0;

/// The gap between the label column and the control it names.
const LABEL_GAP: f32 = 12.0;

/// The trailing column reserved for the reset affordance, so controls never shift when it appears.
const RESET_WIDTH: f32 = 28.0;

/// How a row arranges its label and its content.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum SettingsRowLayout {
    /// A right-aligned label beside its control, which is the form default.
    Beside,
    /// A label above content that needs the whole row, such as a list.
    Above,
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
        let label_width = appearance.text_size(LABEL_WIDTH);
        let label_gap = appearance.spacing(LABEL_GAP);
        let above = self.layout == SettingsRowLayout::Above;
        let label = div()
            .debug_selector(move || label_selector.clone())
            .flex_none()
            .text_color(gpui_color(appearance.colors.text_secondary))
            .whitespace_normal()
            .when(above, |label| label.font(appearance.emphasis.clone()))
            .when(!above, |label| {
                label.w(label_width).text_align(gpui::TextAlign::Right)
            })
            .child(self.label)
            .into_any_element();
        let (label_above, label_beside) = if above {
            (Some(label), None)
        } else {
            (None, Some(label))
        };
        // A label beside its control is centered against it, so a tall control and a short one both
        // read as one line. Guidance keeps its own line under the control column.
        let primary = div()
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .gap(label_gap)
            .children(label_beside)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .min_w_0()
                    .flex_1()
                    .child(self.control),
            )
            // The trailing column is reserved so a control never shifts when its reset appears. A
            // row that cannot be reset, such as a list, keeps the width for its own content.
            .when(!above || self.reset.is_some(), |line| {
                line.child(
                    div()
                        .w(appearance.text_size(RESET_WIDTH))
                        .flex_none()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .children(self.reset),
                )
            });
        let description = self.description.map(|description| {
            div()
                .text_size(appearance.text_size(11.0))
                .text_color(gpui_color(appearance.colors.text_muted))
                .whitespace_normal()
                .when(!above, |line| line.pl(label_width + label_gap))
                .child(description)
        });
        div()
            .debug_selector(move || selector.to_owned())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(4.0))
            .px(appearance.spacing(8.0))
            .py(appearance.spacing(5.0))
            .rounded(px(6.0))
            .when(self.highlighted, |row| {
                row.bg(gpui_color(appearance.colors.info_background))
            })
            .children(label_above)
            .child(primary)
            .children(description)
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
        .pb(appearance.spacing(6.0))
        .child(
            div()
                .font(appearance.heading.clone())
                .text_size(appearance.text_size(15.0))
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
pub(super) fn swatch_strip(
    selector: String,
    swatches: &[Color],
    appearance: &ChromeAppearance,
) -> impl IntoElement {
    let extent = appearance.text_size(13.0);
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .rounded(px(4.0))
        .overflow_hidden()
        .border_1()
        .border_color(gpui_color(appearance.colors.border_variant))
        .children(
            swatches
                .iter()
                .map(|color| div().w(extent).h(extent).bg(gpui_color(*color))),
        )
}

/// A short labeled classification, such as a scheme's kind or appearance.
///
/// The fill comes from the raised element role rather than the plain element background, which a
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
        .border_1()
        .border_color(gpui_color(appearance.colors.border_variant))
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
