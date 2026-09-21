//! Presentation shared by every Settings Row.
//!
//! These are compositions of existing reusable controls, not new control families: a row is a
//! label, a control, and an optional reset affordance; a stepper is two icon buttons around a
//! readout. Interaction behavior stays in `spaceterm-ui`.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Rgba, SharedString, StyledText, Window, div, px, rgba};
use spaceterm_ui::{
    Button, ButtonSize, ButtonVariant, Icon, IconButton, IconName, Tooltip, highlight_ranges,
};

#[cfg(test)]
use crate::appearance::ChromeColors;
use crate::appearance::Color;
use crate::ui::appearance::ChromeAppearance;
use crate::ui::chrome_geometry::{HAIRLINE, RadiusRole};
use crate::ui::chrome_icons::{IconRole, InteractiveIconRole};
use crate::ui::chrome_typography::{ChromeTextStyle, ChromeTextStyleExt as _, TextRole};

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

/// The horizontal breathing room every row keeps inside the card that holds it.
///
/// Rows carry it rather than the card, so a fill a row paints, such as the one Settings Search
/// leaves on the row it reveals, reaches the card's own edges while the text stays clear of them.
/// The content column gives back exactly this much padding, so a group's title, a section's
/// heading, and a row's label all start on one edge.
const COMPACT_ROW_HORIZONTAL_INSET: f32 = 12.0;
const COMFORTABLE_ROW_HORIZONTAL_INSET: f32 = 14.0;
const COMPACT_ROW_VERTICAL_INSET: f32 = 8.0;
const COMFORTABLE_ROW_VERTICAL_INSET: f32 = 11.0;

pub(super) fn row_horizontal_inset(appearance: &ChromeAppearance) -> gpui::Pixels {
    px(if appearance.spacing_scale > 1.0 {
        COMFORTABLE_ROW_HORIZONTAL_INSET
    } else {
        COMPACT_ROW_HORIZONTAL_INSET
    })
}

/// Padding inside the card's border that leaves the requested visible inset from its outer edge.
fn row_horizontal_padding(appearance: &ChromeAppearance) -> gpui::Pixels {
    row_horizontal_inset(appearance) - px(HAIRLINE)
}

fn row_vertical_inset(appearance: &ChromeAppearance) -> gpui::Pixels {
    px(if appearance.spacing_scale > 1.0 {
        COMFORTABLE_ROW_VERTICAL_INSET
    } else {
        COMPACT_ROW_VERTICAL_INSET
    })
}

fn stepper_readout_min_width(appearance: &ChromeAppearance) -> gpui::Pixels {
    px(if appearance.spacing_scale > 1.0 {
        48.0
    } else {
        44.0
    })
}

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
fn reset_slot_width(appearance: &ChromeAppearance) -> gpui::Pixels {
    appearance
        .icons
        .interactive_target_size(InteractiveIconRole::Control)
}

/// The height of one line of a label, which the reset beside it is centered on.
///
/// This is the default text line box at the label's size. The reset shares that one line instead
/// of standing taller than it, and a label that wraps keeps the reset on its first line.
fn label_line_height(appearance: &ChromeAppearance) -> gpui::Pixels {
    appearance.typography.style(TextRole::Body).line_height
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

/// One titled run of related rows.
///
/// The group uses the document surface and its text pair. Its title and surrounding space carry
/// grouping. The fixed card edge and inset separators keep related rows legible without adding a
/// shadow, and the card clips revealed-row fills to its outer corners.
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
        let radius = RadiusRole::Card.pixels();
        let card_selector = format!("{selector}-card");
        let card_colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let card_background = appearance.surface(
            crate::appearance::SurfaceRole::Surface,
            appearance.colors.elevated_surface_background,
        );
        let card_edge = appearance
            .materials
            .edge(card_colors.elevated_surface_background, card_colors.border);
        let row_separator = appearance.separator(spaceterm_ui::ControlHost::Card);
        let row_inset = row_horizontal_padding(appearance);
        let separator_selector = card_selector.clone();
        let rows = self
            .rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let separator_selector = separator_selector.clone();
                div().relative().w_full().child(row).when(index > 0, |row| {
                    row.child(
                        div()
                            .debug_selector(move || {
                                format!("{separator_selector}-separator-{index}")
                            })
                            .absolute()
                            .top_0()
                            .left(row_inset)
                            .right_0()
                            .h(px(HAIRLINE))
                            .bg(gpui_color(row_separator)),
                    )
                })
            })
            .collect::<Vec<_>>();
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
                div().px(row_inset + px(HAIRLINE)).child(
                    div()
                        .debug_selector(move || title_selector.clone())
                        .chrome_text(appearance.typography.style(TextRole::Section))
                        .text_color(gpui_color(appearance.colors.text))
                        .child(self.title),
                ),
            )
            .child(
                spaceterm_ui::ControlHost::Card.mount(
                    div()
                        .debug_selector(move || card_selector.clone())
                        .relative()
                        .flex()
                        .flex_col()
                        .w_full()
                        .rounded(radius)
                        .border(px(HAIRLINE))
                        .border_color(gpui_color(card_edge))
                        // A revealed row fills to the card's own edges, so the card clips it back to
                        // its corners instead of letting a square fill escape a rounded shape.
                        .overflow_hidden()
                        // A card is a surface resting on the page's base, lighter or brighter than it.
                        .bg(gpui_color(card_background))
                        .children(rows),
                ),
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
    matched_indices: Vec<usize>,
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
            matched_indices: Vec::new(),
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

    pub(super) fn matched_indices(mut self, indices: Vec<usize>) -> Self {
        self.matched_indices = indices;
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
        _cx: &App,
    ) -> impl IntoElement {
        let selector = self.selector;
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let (foreground, secondary) = if self.highlighted {
            (
                colors.row_selected_foreground,
                colors.row_selected_secondary,
            )
        } else {
            (colors.text, colors.text_muted)
        };
        let matched = if self.highlighted {
            colors.row_selected_match
        } else {
            colors.row_match
        };
        let label_selector = format!("{selector}-label");
        let reset_slot_selector = format!("{selector}-reset-slot");
        let label_role = TextRole::Body;
        // Text left to size itself inside a row is measured once without a width and keeps that
        // answer, so it would neither wrap nor stay put. The label instead starts from the width
        // of its one line and gives up only what the reset's slot and the control need, and the
        // width it is left with is the one it wraps at.
        let label_width = appearance
            .typography
            .measure(label_role, self.label, window)
            .ceil();
        let reset_slot_width = reset_slot_width(appearance);
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
                .chrome_text(appearance.typography.style(TextRole::Secondary))
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
            .map(|description| {
                appearance
                    .typography
                    .measure(TextRole::Secondary, description, window)
                    .ceil()
            })
            .map_or(px(0.0), |width| {
                let label_with_reset =
                    label_width + appearance.spacing(RESET_GAP) + reset_slot_width;
                width.max(label_with_reset)
            });
        let label_ranges = highlight_ranges(self.label, &self.matched_indices);
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
                                .chrome_text(appearance.typography.style(label_role))
                                .text_color(gpui_color(foreground))
                                .whitespace_normal()
                                .child(highlighted_label(self.label, &label_ranges, matched)),
                        )
                        .child(
                            div()
                                .debug_selector(move || reset_slot_selector.clone())
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .w(reset_slot_width)
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
            .px(row_horizontal_padding(appearance))
            .py(row_vertical_inset(appearance))
            .when(self.highlighted, |row| {
                row.bg(gpui_color(highlighted_row_background(appearance)))
            })
            .children(label_above)
            .child(primary)
            .children(trailing_caption)
    }
}

pub(super) fn highlighted_row_background(appearance: &ChromeAppearance) -> Color {
    let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
    appearance.materials.paint(
        crate::appearance::SurfaceRole::Surface,
        colors.elevated_surface_background,
        colors.row_selected_background,
    )
}

fn highlighted_label(
    label: &'static str,
    ranges: &[std::ops::Range<usize>],
    matched: Color,
) -> AnyElement {
    StyledText::new(label)
        .with_highlights(
            ranges
                .iter()
                .cloned()
                .map(|range| (range, gpui_color(matched).into())),
        )
        .into_any_element()
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
                .chrome_text(appearance.typography.style(TextRole::Title))
                .text_color(gpui_color(appearance.colors.text))
                .child(title),
        )
        .child(
            div()
                .debug_selector(move || format!("{selector}-description"))
                .max_w(appearance.spacing(PROSE_MEASURE))
                .chrome_text(appearance.typography.style(TextRole::Body))
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
    icon_size: gpui::Pixels,
    enabled: bool,
    on_reset: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let icon_selector = SharedString::from(selector.clone());
    IconButton::new(
        icon_selector,
        SharedString::from(format!("Reset {setting} to default")),
        move |foreground| Icon::new(IconName::RotateCcw, icon_size, foreground).into_any_element(),
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

    pub(super) fn render(self, appearance: &ChromeAppearance) -> impl IntoElement {
        let text_style = appearance.typography.style(TextRole::Body).tabular();
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        StepperElement {
            readout_min_width: stepper_readout_min_width(appearance),
            control_height: appearance.typography.style(TextRole::Body).line_height
                + appearance.spacing(12.0),
            gap: appearance.spacing(8.0),
            text_style,
            icon_size: appearance.icons.metrics(IconRole::Control).glyph_size,
            separator: gpui_color(colors.border),
            foreground: gpui_color(if self.enabled {
                colors.input_text
            } else {
                colors.input_disabled_text
            }),
            control: self,
        }
    }
}

#[derive(IntoElement)]
struct StepperElement {
    control: Stepper,
    readout_min_width: gpui::Pixels,
    control_height: gpui::Pixels,
    gap: gpui::Pixels,
    text_style: ChromeTextStyle,
    icon_size: gpui::Pixels,
    separator: Rgba,
    foreground: Rgba,
}

impl gpui::RenderOnce for StepperElement {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let control = self.control;
        let accessibility_name = control.accessibility_name;
        let button_extent = self.control_height;
        let icon_size = self.icon_size;
        let step = |selector: String,
                    verb: &'static str,
                    icon: IconName,
                    delta: i32,
                    enabled: bool,
                    handler: Option<StepHandler>| {
            IconButton::new(
                SharedString::from(selector.clone()),
                SharedString::from(format!("{verb} {accessibility_name}")),
                move |foreground| Icon::new(icon, icon_size, foreground).into_any_element(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Regular)
            .disabled(!enabled)
            .tab_stop(true)
            .debug_selector(selector)
            .on_activate(move |_, window, cx| {
                if let Some(handler) = &handler {
                    handler(delta, window, cx);
                }
            })
        };
        let selector = control.selector;
        div()
            .debug_selector(move || selector.to_owned())
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .gap(self.gap)
            .child(
                div()
                    .flex_none()
                    .min_w(self.readout_min_width)
                    .text_align(gpui::TextAlign::Center)
                    // Tabular figures, so the readout holds still while a step runs: the digits of
                    // 9 and 10, or of 1.11 and 1.2, occupy the same width.
                    .chrome_text(&self.text_style)
                    .text_color(self.foreground)
                    .debug_selector({
                        let value_selector = format!("{selector}-value");
                        move || value_selector.clone()
                    })
                    .child(control.value),
            )
            .child(
                spaceterm_ui::field_surface(
                    SharedString::from(format!("{selector}-buttons")),
                    spaceterm_ui::FieldState::default().disabled(!control.enabled),
                    cx,
                )
                .debug_selector(move || format!("{selector}-buttons"))
                .relative()
                .flex()
                .flex_row()
                .items_center()
                .flex_none()
                .w(button_extent * 2.0)
                .h(button_extent)
                .rounded(RadiusRole::Control.pixels())
                .child(step(
                    format!("{selector}-decrease"),
                    "Decrease",
                    IconName::Minus,
                    -1,
                    control.enabled && control.can_decrease,
                    control.on_step.clone(),
                ))
                .child(step(
                    format!("{selector}-increase"),
                    "Increase",
                    IconName::Plus,
                    1,
                    control.enabled && control.can_increase,
                    control.on_step,
                ))
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(button_extent)
                        .w(px(HAIRLINE))
                        .bg(self.separator),
                ),
            )
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
    let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
    let background = appearance.materials.paint(
        crate::appearance::SurfaceRole::Surface,
        colors.elevated_surface_background,
        colors.element_background,
    );
    let border = appearance
        .materials
        .edge(colors.element_background, colors.border);
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .w(appearance.spacing(SWATCH_WIDTH))
        .h(appearance.spacing(14.0))
        .rounded(RadiusRole::ControlSmall.pixels())
        .overflow_hidden()
        // A strip is bounded by a hairline rather than left to its own colors: a scheme is free to
        // open on a near-background color, and the built-in dark scheme does, which would otherwise
        // leave the strip looking short of the column every other strip fills.
        .border(px(HAIRLINE))
        .border_color(gpui_color(border))
        .bg(gpui_color(background))
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
    let pair = appearance.semantic_text_pairs.badge;
    div()
        .flex_none()
        .flex()
        .items_center()
        .h(appearance.spacing(16.0))
        .px(appearance.spacing(6.0))
        .rounded(RadiusRole::ControlSmall.pixels())
        .bg(gpui_color(pair.background))
        .chrome_text(appearance.typography.style(TextRole::Badge))
        .text_color(gpui_color(pair.primary))
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
    use super::*;

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

    #[test]
    fn grouped_row_geometry_uses_exact_density_insets_and_a_fixed_card_radius() {
        let compact = ChromeAppearance {
            spacing_scale: 1.0,
            ..ChromeAppearance::default()
        };
        let comfortable = ChromeAppearance {
            spacing_scale: 1.25,
            ..compact.clone()
        };

        assert_eq!(row_horizontal_inset(&compact), px(12.0));
        assert_eq!(row_horizontal_padding(&compact), px(11.0));
        assert_eq!(row_vertical_inset(&compact), px(8.0));
        assert_eq!(row_horizontal_inset(&comfortable), px(14.0));
        assert_eq!(row_horizontal_padding(&comfortable), px(13.0));
        assert_eq!(row_vertical_inset(&comfortable), px(11.0));
        assert_eq!(stepper_readout_min_width(&compact), px(44.0));
        assert_eq!(stepper_readout_min_width(&comfortable), px(48.0));
        assert_eq!(RadiusRole::Card.pixels(), px(8.0));
    }
}
