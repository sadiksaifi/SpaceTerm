//! Presentation shared by every Settings Row.
//!
//! These are compositions of existing reusable controls, not new control families: a row is a
//! label, a control, and an optional reset affordance; a stepper is two icon buttons around a
//! readout. Interaction behavior stays in `spaceterm-ui`.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Rgba, SharedString, Window, div, px, rgba};
use spaceterm_ui::{
    Button, ButtonSize, ButtonTheme, ButtonVariant, Icon, IconButton, IconName, Tooltip,
};

#[cfg(test)]
use crate::appearance::ChromeColors;
use crate::appearance::Color;
use crate::ui::appearance::ChromeAppearance;

/// One stepper step, negative for decrement and positive for increment.
type StepHandler = Rc<dyn Fn(i32, &mut Window, &mut App)>;

pub(super) fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}

/// A field action rests on its field and uses complete neutral paints while interacting.
#[cfg(test)]
pub(super) fn field_action_style(colors: &ChromeColors) -> spaceterm_ui::ButtonVariantStyle {
    let paint = |background, foreground| {
        spaceterm_ui::ButtonPaint::new(gpui_color(background), gpui_color(foreground), rgba(0))
    };
    spaceterm_ui::ButtonVariantStyle::new(
        paint(colors.input_background, colors.input_text),
        paint(
            colors.ghost_element_hover,
            colors.ghost_element_hover_foreground,
        ),
        paint(
            colors.ghost_element_active,
            colors.ghost_element_active_foreground,
        ),
        paint(colors.input_disabled_background, colors.input_disabled_text),
    )
}

/// The width every scheme's color strip takes, so the names beside them share one column.
const SWATCH_WIDTH: f32 = 88.0;

/// The Settings type ramp.
///
/// Five sizes, each a clear step from the next, and every role on every page takes one of them.
/// The ramp is monotone in rank: a page outranks a run of rows, a run outranks the settings inside
/// it, and a setting outranks the guidance beside it. Nothing chooses a size of its own, so a new
/// row cannot invent a fifth step and no two roles can land close enough to read as the same
/// thing.
pub(super) mod text {
    /// The page's name, set as a large title at the head of the content surface.
    pub(crate) const TITLE: f32 = 22.0;
    /// The title of one run of related rows.
    ///
    /// It is larger and heavier than the labels under it: a heading that a label outweighs is not
    /// a heading, and with nothing ruled off, rank is the only thing telling a reader where a run
    /// begins.
    pub(crate) const GROUP: f32 = 13.0;
    /// One setting's label, one control's value, and one scheme's name.
    pub(crate) const BODY: f32 = 12.0;
    /// Guidance, metadata, and status: everything that explains something else.
    pub(crate) const SMALL: f32 = 11.0;
    /// A state a row carries, such as the scheme in use.
    pub(crate) const BADGE: f32 = 10.0;
}

/// The horizontal breathing room every row keeps inside the card that holds it.
///
/// Rows carry it rather than the card, so a fill a row paints, such as the one Settings Search
/// leaves on the row it reveals, reaches the card's own edges while the text stays clear of them.
/// The content column gives back exactly this much padding, so a group's title, a section's
/// heading, and a row's label all start on one edge.
pub(super) const ROW_INSET: f32 = 12.0;

/// The widest a run of explanatory prose is allowed to set.
///
/// A sentence spanning the whole pane is a long line to track back from, and the window is free to
/// be far wider than prose wants to be. Rows are unaffected: a control column has its own reason
/// to reach the right edge.
const PROSE_MEASURE: f32 = 460.0;

/// The least space between a label and the control it names, so the two never touch.
const LABEL_GAP: f32 = 16.0;

/// The space between a label and the reset that follows it, so the two read as one phrase.
const RESET_GAP: f32 = 4.0;

/// The size of a row's reset, whose square is also the slot held beside every label for it.
const RESET_SIZE: ButtonSize = ButtonSize::Compact;

/// The width held beside every label for its reset: the installed reset button's own square.
///
/// The slot is held whether or not the reset is present. A label measured against the space a
/// reset leaves would wrap onto another line the moment a reset appeared, growing the row and
/// moving the control centered beside it. The width comes from the control theme rather than a
/// number of its own, so the slot scales with the button it holds instead of letting a larger
/// button spill into the gap beside the label.
fn reset_slot_width(cx: &App) -> gpui::Pixels {
    cx.global::<ButtonTheme>().icon_button_size(RESET_SIZE)
}

/// The height of one line of a label, which the reset beside it is centered on.
///
/// This is the default text line box at the label's size. The reset shares that one line instead
/// of standing taller than it, and a label that wraps keeps the reset on its first line.
fn label_line_height(appearance: &ChromeAppearance) -> gpui::Pixels {
    appearance.text_size(text::BODY * 1.618)
}

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

/// The radius and vertical padding of the card one run of rows rests on.
///
/// The radius is shared with the notices that sit in the same column, so a page of cards and the
/// warnings above them are cut to one corner.
pub(super) const CARD_RADIUS: f32 = 10.0;
const CARD_PADDING_Y: f32 = 4.0;

/// One titled run of related rows.
///
/// The group uses the document surface and its text pair. Its title and surrounding space carry
/// grouping without an enclosing outline, so the only strokes on a page belong to controls. The
/// card remains as a clip for revealed rows.
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
        let radius = appearance.spacing(CARD_RADIUS);
        let card_selector = format!("{selector}-card");
        div()
            .debug_selector(move || selector.clone())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(6.0))
            .child(
                // The title keeps the rows' inset so it starts on their left edge, and the inset
                // sits on a wrapper so the title's own box is the type it sets, not the padding
                // around it.
                div().px(appearance.spacing(ROW_INSET)).child(
                    div()
                        .debug_selector(move || title_selector.clone())
                        .font(appearance.emphasis.clone())
                        .text_size(appearance.text_size(text::GROUP))
                        .text_color(gpui_color(appearance.colors.text))
                        .child(self.title),
                ),
            )
            .child(
                div()
                    .debug_selector(move || card_selector.clone())
                    .relative()
                    .flex()
                    .flex_col()
                    .w_full()
                    .py(appearance.spacing(CARD_PADDING_Y))
                    .rounded(radius)
                    // A revealed row fills to the card's own edges, so the card clips it back to
                    // its corners instead of letting a square fill escape a rounded shape.
                    .overflow_hidden()
                    // A card is a surface resting on the page's base, lighter or brighter than it.
                    .bg(gpui_color(appearance.surface(
                        crate::appearance::SurfaceRole::Surface,
                        appearance.colors.elevated_surface_background,
                    )))
                    .children(self.rows),
            )
    }
}

/// One labeled row: a label, its control, and an optional reset affordance.
///
/// The reset follows the label rather than the control. Most rows never carry one, so a column
/// reserved for it at the far end would stop every control short of the right edge and leave the
/// form wider on one side than the other. Beside the name it restores, the reset reads as a mark
/// that this setting was changed, the control stays on the one shared right edge whether or not a
/// reset is present, and keyboard focus reaches the reset just before the control it restores. The
/// reset's slot is held empty on a row without one, so the label wraps at the same width either way.
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
    ///
    /// A row without a label of its own has nowhere to carry one, so a full-width row drops it.
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

    pub(super) fn render(
        self,
        appearance: &ChromeAppearance,
        window: &Window,
        cx: &App,
    ) -> impl IntoElement {
        let selector = self.selector;
        let (foreground, secondary) = if self.highlighted {
            (
                appearance.colors.row_selected_foreground,
                appearance.colors.row_selected_secondary,
            )
        } else {
            (appearance.colors.text, appearance.colors.text_muted)
        };
        let label_selector = format!("{selector}-label");
        let reset_slot_selector = format!("{selector}-reset-slot");
        // Text left to size itself inside a row is measured once without a width and keeps that
        // answer, so it would neither wrap nor stay put. The label instead starts from the width
        // of its one line and gives up only what the reset's slot and the control need, and the
        // width it is left with is the one it wraps at.
        let label_width = appearance.measure(self.label, text::BODY, window).ceil();
        let above = self.layout == SettingsRowLayout::Above;
        let full = self.layout == SettingsRowLayout::Full;
        let description_selector = format!("{selector}-description");
        // One rule for the whole form: the label starts at the content's left edge, the control
        // ends at its right edge, and nothing is centered.
        let caption = |description: SharedString| {
            div()
                .debug_selector({
                    let description_selector = description_selector.clone();
                    move || description_selector.clone()
                })
                .text_size(appearance.text_size(text::SMALL))
                .text_color(gpui_color(secondary))
                .whitespace_normal()
                .child(description)
        };
        // Guidance belongs to the label, not to the row: under a tall control it would otherwise
        // come to rest between two rows, reading as a stray sentence belonging to neither. Stacked
        // with the label it stays anchored to the setting it explains, and it stops where the
        // control begins instead of running the width of the page.
        // Guidance is measured the same way. A column growing from nothing would wrap it one word
        // per line when measured, and a centered column that tall rises out of its own row.
        let label_basis = self
            .description
            .as_ref()
            .filter(|_| !above)
            .map(|description| appearance.measure(description, text::SMALL, window).ceil())
            .map_or(px(0.0), |width| {
                width.max(label_width + appearance.spacing(RESET_GAP) + reset_slot_width(cx))
            });
        let label = (!full).then(|| {
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .flex_1()
                .flex_basis(label_basis)
                .gap(appearance.spacing(2.0))
                // The line keeps an automatic minimum width. Taffy sizes this column under a
                // min-content constraint even when its own minimum is zero, and a zero minimum here
                // would lay the label out one glyph per line. Taffy's cache can then hand that
                // height back once wrapped guidance fills the column, stretching the row far below
                // its content.
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap(appearance.spacing(RESET_GAP))
                        .child(
                            div()
                                .debug_selector(move || label_selector.clone())
                                .flex_basis(label_width)
                                .flex_shrink()
                                .min_w_0()
                                .text_size(appearance.text_size(text::BODY))
                                .text_color(gpui_color(foreground))
                                .whitespace_normal()
                                .child(self.label),
                        )
                        .child(
                            div()
                                .debug_selector(move || reset_slot_selector.clone())
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .w(reset_slot_width(cx))
                                .h(label_line_height(appearance))
                                .children(self.reset),
                        ),
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
            .px(appearance.spacing(ROW_INSET))
            .py(appearance.spacing(8.0))
            .rounded(px(6.0))
            .when(self.highlighted, |row| {
                row.bg(gpui_color(appearance.surface(
                    crate::appearance::SurfaceRole::Surface,
                    appearance.colors.row_selected_background,
                )))
            })
            .children(label_above)
            .child(primary)
            .children(trailing_caption)
    }
}

/// The active section's large title with its explanation directly beneath it.
///
/// It carries no hitbox of its own, so the window-drag region behind it keeps the whole heading
/// available for native window movement.
pub(super) fn section_heading(
    selector: &'static str,
    title: &'static str,
    description: &'static str,
    appearance: &ChromeAppearance,
) -> impl IntoElement {
    div()
        .debug_selector(move || format!("{selector}-heading"))
        .flex()
        .flex_col()
        .w_full()
        .gap(appearance.spacing(4.0))
        .child(
            div()
                .debug_selector(move || format!("{selector}-title"))
                .truncate()
                .font(appearance.heading.clone())
                .text_size(appearance.text_size(text::TITLE))
                .line_height(gpui::relative(1.2))
                .text_color(gpui_color(appearance.colors.text))
                .child(title),
        )
        .child(
            div()
                .debug_selector(move || format!("{selector}-description"))
                .max_w(appearance.text_size(PROSE_MEASURE))
                .text_size(appearance.text_size(text::BODY))
                .text_color(gpui_color(appearance.colors.text_secondary))
                .whitespace_normal()
                .child(description),
        )
}

/// The affordance restoring one row's default value.
///
/// It is a quiet glyph sized to the label line it follows. Its accessible name says which setting
/// it restores, because focus can reach it apart from the label that shows this visually.
pub(super) fn reset_button(
    selector: String,
    setting: &'static str,
    enabled: bool,
    on_reset: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let icon_selector = SharedString::from(selector.clone());
    IconButton::new(
        icon_selector,
        SharedString::from(format!("Reset {setting} to default")),
        move |foreground| Icon::new(IconName::RotateCcw, px(11.0), foreground).into_any_element(),
    )
    .variant(ButtonVariant::Ghost)
    .size(RESET_SIZE)
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

    pub(super) fn render(self, appearance: &ChromeAppearance, cx: &App) -> impl IntoElement {
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
        spaceterm_ui::field_surface(
            selector,
            spaceterm_ui::FieldState::default().disabled(!self.enabled),
            cx,
        )
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
                // Tabular figures, so the readout holds still while a step runs: the digits of
                // 9 and 10, or of 1.11 and 1.2, occupy the same width.
                .font(appearance.tabular())
                .text_size(appearance.text_size(text::BODY))
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
        // A strip is bounded by a hairline rather than left to its own colors: a scheme is free to
        // open on a near-background color, and the built-in dark scheme does, which would otherwise
        // leave the strip looking short of the column every other strip fills.
        .border_1()
        .border_color(gpui_color(appearance.colors.border))
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
        .bg(gpui_color(appearance.surface(
            crate::appearance::SurfaceRole::Surface,
            appearance.colors.badge_background,
        )))
        .text_size(appearance.text_size(text::BADGE))
        .text_color(gpui_color(appearance.colors.badge_foreground))
        .child(label.into())
}

/// A text button used by the interchange and diagnostics rows.
pub(super) fn action_button(
    selector: &'static str,
    label: &'static str,
    enabled: bool,
    on_activate: impl Fn(&mut Window, &mut App) + 'static,
) -> Button {
    Button::new(selector, label)
        .variant(ButtonVariant::Outline)
        .size(ButtonSize::Regular)
        .disabled(!enabled)
        .tab_stop(true)
        .debug_selector(selector)
        .on_activate(move |_, window, cx| on_activate(window, cx))
}

#[cfg(test)]
mod tests {
    use super::text;

    #[test]
    fn field_action_preserves_a_light_field_and_opposite_hover_paint() {
        use crate::appearance::{ChromeColors, Color};
        let colors = ChromeColors {
            input_background: Color::rgb(0xffffff),
            input_text: Color::rgb(0x111111),
            text: Color::rgb(0xffffff),
            ghost_element_hover: Color::rgb(0x111111),
            ghost_element_hover_foreground: Color::rgb(0xffffff),
            ..ChromeColors::default()
        };
        let style = super::field_action_style(&colors);
        assert_eq!(
            style.normal().background(),
            super::gpui_color(colors.input_background)
        );
        assert_eq!(
            style.normal().foreground(),
            super::gpui_color(colors.input_text)
        );
        assert_eq!(
            style.hovered().background(),
            super::gpui_color(colors.ghost_element_hover)
        );
        assert_eq!(
            style.hovered().foreground(),
            super::gpui_color(colors.ghost_element_hover_foreground)
        );
    }

    /// The ramp is only a hierarchy while it stays ordered, and it is edited one constant at a
    /// time.
    #[test]
    fn the_type_ramp_stays_ordered_by_rank() {
        let ramp = [
            ("title", text::TITLE),
            ("group", text::GROUP),
            ("body", text::BODY),
            ("small", text::SMALL),
            ("badge", text::BADGE),
        ];
        for pair in ramp.windows(2) {
            let (outer, outer_size) = pair[0];
            let (inner, inner_size) = pair[1];
            assert!(
                outer_size > inner_size,
                "{outer} should set above {inner}, got {outer_size} over {inner_size}"
            );
        }
    }
}
