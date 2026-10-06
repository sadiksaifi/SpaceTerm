//! The Controls section: every production control family pinned in each interaction state.
//! Focus and drag handlers stay unarmed so a capture shows a state's paint without input.

use gpui::prelude::*;
use gpui::{AnyElement, App, Div, Entity, SharedString, Window, div, px};
use spaceterm_ui::{
    Button, ButtonSize, ButtonVariant, Checkbox, CheckboxState, ComboBox, ComboBoxItem,
    CommandPalette, ControlPreviewState, DeterminateProgress, FieldState, FrameSpinner, Icon,
    IconButton, IconName, OverlayScrollbar, ProgressBar, ProgressRing, ProgressSize, ProgressState,
    ResizeAxis, ResizeHandle, ScrollMetrics, SegmentedControl, SegmentedOption, Switch, TextInput,
};

use super::single_line_field;
use crate::ui::appearance::settings::SettingsAppearance;
use crate::ui::appearance::{
    ChromeAppearance, DisabledControlDiagnostic, FloatingControlFamily, gpui_color,
};
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::sidebar_window::form::{FormGroup, FormRow, FormRowLayout};
use crate::ui::terminal_pane::StatusIntent;
use crate::ui::terminal_status::{StatusColors, StatusGlyph, TerminalProgress};
use crate::ui::workspace_chrome::{WorkspaceChromeStatus, WorkspaceChromeStatusHosts};

/// The state columns. Disabled pins the normal paint of a disabled control.
const STATES: [(&str, ControlPreviewState); 5] = [
    ("Normal", ControlPreviewState::Normal),
    ("Hover", ControlPreviewState::Hovered),
    ("Pressed", ControlPreviewState::Pressed),
    ("Disabled", ControlPreviewState::Normal),
    ("Focused", ControlPreviewState::Focused),
];
const DISABLED_COLUMN: usize = 3;
/// One scrollbar per state column, named for the state it pins.
const SCROLLBAR_NAMES: [&str; 5] = [
    "workbench-scrollbar-normal",
    "workbench-scrollbar-hover",
    "workbench-scrollbar-pressed",
    "workbench-scrollbar-disabled",
    "workbench-scrollbar-focused",
];
const FIELD_STATES: [&str; 5] = [
    "Placeholder",
    "Focused",
    "Invalid",
    "Disabled",
    "Invalid and focused",
];
/// The value each non-empty field fixture holds, and the empty one's placeholder. Each fits the
/// narrowest field, so no field clips its text or scrolls it to keep the caret in view.
pub(super) const FIELD_SAMPLE: &str = "Sample text";
pub(super) const FIELD_PLACEHOLDER: &str = "Placeholder";
const LABEL_WIDTH: f32 = 112.0;
/// The widest a state column grows. Columns share narrower rows equally, so a larger density still
/// fits the card.
const COLUMN_WIDTH: f32 = 142.0;
const COLUMN_GAP: f32 = 8.0;

/// Controls that keep state across frames: field contents and scrollbar geometry.
pub(super) struct ControlStates {
    pub(super) fields: Vec<Entity<TextInput>>,
    scrollbars: Vec<Entity<OverlayScrollbar<f32>>>,
}

impl ControlStates {
    pub(super) fn new<T: 'static>(window: &mut Window, cx: &mut Context<T>) -> Self {
        let fields = FIELD_STATES
            .iter()
            .enumerate()
            .map(|(index, _)| {
                cx.new(|cx| {
                    TextInput::new(
                        SharedString::from(format!("workbench-field-{index}")),
                        "Field state fixture",
                        if index == 0 { "" } else { FIELD_SAMPLE },
                        window,
                        cx,
                    )
                    .placeholder(FIELD_PLACEHOLDER)
                    .debug_selector(format!("workbench-field-{index}"))
                    .enabled(index != DISABLED_COLUMN)
                })
            })
            .collect();
        let scrollbars = STATES
            .iter()
            .zip(SCROLLBAR_NAMES)
            .map(|((_, state), name)| {
                cx.new(|cx| {
                    let mut scrollbar = OverlayScrollbar::new(name)
                        .persistent()
                        .preview_state(*state);
                    scrollbar.sync(ScrollMetrics::for_pixels(0.0, 60.0, 240.0, 60.0), cx);
                    scrollbar
                })
            })
            .collect();
        Self { fields, scrollbars }
    }

    pub(super) fn render(
        &self,
        palette: &Entity<CommandPalette<u8>>,
        surface: &SettingsAppearance,
        window: &Window,
        cx: &App,
    ) -> Vec<AnyElement> {
        let appearance = &surface.chrome;
        let icon_size = appearance.icons.metrics(IconRole::Control).glyph_size;
        let mut groups = Vec::new();
        if let Some(diagnostics) = control_diagnostics(appearance) {
            groups.push(diagnostics.into_any_element());
        }
        let buttons = [
            ("Primary", ButtonVariant::Primary),
            ("Destructive", ButtonVariant::Destructive),
            ("Secondary", ButtonVariant::Secondary),
        ]
        .into_iter()
        .enumerate()
        .map(|(row, (label, variant))| {
            state_row(label, appearance, |column, state| {
                Button::new(("workbench-button", row * STATES.len() + column), "Action")
                    .variant(variant)
                    .size(ButtonSize::Regular)
                    .debug_selector(format!("workbench-button-{row}-{column}"))
                    .disabled(column == DISABLED_COLUMN)
                    .preview_state(state)
                    .leading(move |color| {
                        Icon::new(IconName::Plus, icon_size, color).into_any_element()
                    })
                    .on_activate(|_, _, _| {})
                    .into_any_element()
            })
        })
        .collect();
        groups.push(
            StateMatrix::new(
                "workbench-group-buttons",
                "workbench-row-buttons",
                "Buttons",
                header(STATES.map(|(label, _)| label), appearance),
                buttons,
            )
            .render(surface, window, cx),
        );
        let mut selection = vec![state_row("Segmented", appearance, |column, state| {
            SegmentedControl::new(
                ("workbench-segmented", column),
                "Selection fixture",
                &true,
                vec![
                    SegmentedOption::new(false, "Off"),
                    SegmentedOption::new(true, "On"),
                ],
            )
            .expect("two options are within the bounded option set")
            .full_width(true)
            .disabled(column == DISABLED_COLUMN)
            .preview_state(state)
            .on_change(|_, _, _| {})
            .into_any_element()
        })];
        selection.extend(
            [
                ("Unchecked", CheckboxState::Unchecked),
                ("Checked", CheckboxState::Checked),
                ("Mixed", CheckboxState::Mixed),
            ]
            .into_iter()
            .enumerate()
            .map(|(row, (label, value))| {
                state_row(label, appearance, |column, state| {
                    Checkbox::new(
                        ("workbench-checkbox", row * STATES.len() + column),
                        "Choice",
                        value,
                    )
                    .disabled(column == DISABLED_COLUMN)
                    .preview_state(state)
                    .on_change(|_, _, _| {})
                    .into_any_element()
                })
            }),
        );
        selection.push(state_row("Switch", appearance, |column, state| {
            Switch::new(("workbench-switch", column), "Enabled", true)
                .disabled(column == DISABLED_COLUMN)
                .preview_state(state)
                .on_change(|_, _, _| {})
                .into_any_element()
        }));
        groups.push(
            StateMatrix::new(
                "workbench-group-selection",
                "workbench-row-selection",
                "Selection",
                header(STATES.map(|(label, _)| label), appearance),
                selection,
            )
            .render(surface, window, cx),
        );
        let text_fields = self.fields.iter().enumerate().map(|(index, input)| {
            let frame = spaceterm_ui::field_frame(
                ("workbench-field-frame", index),
                &input.read(cx).focus_handle(),
                FieldState::default()
                    .disabled(index == DISABLED_COLUMN)
                    .invalid(index == 2 || index == 4)
                    .preview_focus(index == 1 || index == 4),
                RadiusRole::Control.pixels(),
                cx,
            );
            single_line_field(frame, appearance)
                .debug_selector(move || format!("workbench-field-frame-{index}"))
                .w_full()
                .child(input.clone())
                .into_any_element()
        });
        let fields = vec![
            cells("Text field", appearance, text_fields),
            state_row("Resize, scroll", appearance, |column, state| {
                div()
                    .relative()
                    .w_full()
                    .h(px(70.0))
                    .child(
                        div().h(px(60.0)).child(
                            ResizeHandle::new(
                                ("workbench-resize", column),
                                "Resize fixture",
                                ResizeAxis::Horizontal,
                                0.0,
                            )
                            .disabled(column == DISABLED_COLUMN)
                            .preview_state(state)
                            .on_event(|_, _, _| {}),
                        ),
                    )
                    .when(column != DISABLED_COLUMN, |cell| {
                        cell.child(self.scrollbars[column].clone())
                    })
                    .into_any_element()
            }),
        ];
        groups.push(
            StateMatrix::new(
                "workbench-group-fields",
                "workbench-row-fields",
                "Fields and scrolling",
                header(FIELD_STATES, appearance),
                fields,
            )
            .render(surface, window, cx),
        );
        groups.push(progress_group(surface, window, cx));
        groups.push(status_group(surface, window, cx));
        groups.push(lists_group(palette, icon_size, surface, window, cx));
        groups
    }
}

/// One state matrix: a card holding a single full-width row of column headers and state rows.
struct StateMatrix {
    group: &'static str,
    row: &'static str,
    title: &'static str,
    header: Div,
    rows: Vec<Div>,
}

impl StateMatrix {
    fn new(
        group: &'static str,
        row: &'static str,
        title: &'static str,
        header: Div,
        rows: Vec<Div>,
    ) -> Self {
        Self {
            group,
            row,
            title,
            header,
            rows,
        }
    }

    fn render(self, surface: &SettingsAppearance, window: &Window, cx: &App) -> AnyElement {
        let appearance = &surface.chrome;
        let matrix = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(appearance.spacing(10.0))
            .child(self.header)
            .children(self.rows);
        FormGroup::new(
            self.group.to_owned(),
            self.title,
            vec![
                FormRow::new(self.row, self.title, matrix)
                    .layout(FormRowLayout::Full)
                    .render(appearance, window, cx)
                    .into_any_element(),
            ],
        )
        .render(surface)
        .into_any_element()
    }
}

fn header<const N: usize>(labels: [&'static str; N], appearance: &ChromeAppearance) -> Div {
    div()
        .flex()
        .gap(px(COLUMN_GAP))
        .chrome_text(appearance.typography.style(TextRole::Secondary))
        .text_color(gpui_color(appearance.colors.text_muted))
        .child(div().w(px(LABEL_WIDTH)).flex_none())
        .children(
            labels
                .into_iter()
                .map(|label| column().truncate().child(label)),
        )
}

/// One state column. Header and state rows use the same columns, so their cells stay aligned.
fn column() -> Div {
    div()
        .flex_1()
        .flex_basis(px(0.0))
        .min_w_0()
        .max_w(px(COLUMN_WIDTH))
}

fn cells(
    label: &'static str,
    appearance: &ChromeAppearance,
    cells: impl Iterator<Item = AnyElement>,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(COLUMN_GAP))
        .child(
            div()
                .w(px(LABEL_WIDTH))
                .flex_none()
                .text_color(gpui_color(appearance.colors.text_secondary))
                .child(label),
        )
        .children(cells.map(|cell| column().child(cell)))
}

fn state_row(
    label: &'static str,
    appearance: &ChromeAppearance,
    cell: impl Fn(usize, ControlPreviewState) -> AnyElement,
) -> Div {
    cells(
        label,
        appearance,
        STATES
            .iter()
            .enumerate()
            .map(|(column, (_, state))| cell(column, *state)),
    )
}

fn progress_group(surface: &SettingsAppearance, window: &Window, cx: &App) -> AnyElement {
    let appearance = &surface.chrome;
    let determinate = |value: f64| {
        ProgressState::Determinate(
            DeterminateProgress::new(value).expect("progress fixtures are finite"),
        )
    };
    let columns = [
        ("Zero", determinate(0.0)),
        ("Partial", determinate(0.4)),
        ("Full", determinate(1.0)),
        ("Indeterminate", ProgressState::Indeterminate),
    ];
    let mut rows = Vec::new();
    for (row, (label, size)) in [
        ("Bar, compact", ProgressSize::Compact),
        ("Bar, regular", ProgressSize::Regular),
    ]
    .into_iter()
    .enumerate()
    {
        rows.push(cells(
            label,
            appearance,
            columns.iter().enumerate().map(|(column, (_, state))| {
                ProgressBar::new(
                    ("workbench-progress-bar", row * columns.len() + column),
                    "Progress fixture",
                    *state,
                )
                .size(size)
                .into_any_element()
            }),
        ));
    }
    for (row, (label, size)) in [
        ("Ring, compact", ProgressSize::Compact),
        ("Ring, regular", ProgressSize::Regular),
    ]
    .into_iter()
    .enumerate()
    {
        rows.push(cells(
            label,
            appearance,
            columns.iter().enumerate().map(|(column, (_, state))| {
                ProgressRing::new(
                    ("workbench-progress-ring", row * columns.len() + column),
                    "Progress fixture",
                    *state,
                )
                .size(size)
                .into_any_element()
            }),
        ));
    }
    // The frame spinner belongs to terminal status slots and has no determinate form, so it fills
    // only the indeterminate column.
    rows.push(cells(
        "Terminal spinner",
        appearance,
        columns.iter().map(|(_, state)| {
            div()
                .when(*state == ProgressState::Indeterminate, |cell| {
                    cell.child(
                        FrameSpinner::new("workbench-frame-spinner", "Progress fixture")
                            .size(ProgressSize::Compact),
                    )
                })
                .into_any_element()
        }),
    ));
    StateMatrix::new(
        "workbench-group-progress",
        "workbench-row-progress",
        "Progress",
        header(columns.map(|(label, _)| label), appearance),
        rows,
    )
    .render(surface, window, cx)
}

/// Every status mark that carries a semantic color, so Differentiate Without Color shows each
/// family's non-color cue.
fn status_group(surface: &SettingsAppearance, window: &Window, cx: &App) -> AnyElement {
    let appearance = &surface.chrome;
    let host = appearance.control_host_background(spaceterm_ui::ControlHost::Card);
    let differentiate_without_color = appearance.capabilities.differentiate_without_color;
    let caption = |mark: AnyElement, label: &'static str| {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(appearance.spacing(4.0))
            .child(mark)
            .child(
                div()
                    .w_full()
                    .truncate()
                    .text_center()
                    .chrome_text(appearance.typography.style(TextRole::Secondary))
                    .text_color(gpui_color(appearance.colors.text_muted))
                    .child(label),
            )
            .into_any_element()
    };
    let hosts = WorkspaceChromeStatusHosts::new(host, host);
    let workspace = WorkspaceChromeStatus::ALL
        .into_iter()
        .enumerate()
        .map(|(index, status)| {
            caption(
                div()
                    .debug_selector(move || format!("workbench-status-workspace-{index}"))
                    .child(status.mark(appearance, hosts, "workbench-status-workspace"))
                    .into_any_element(),
                status.label(),
            )
        });
    let status_colors = appearance.colors.status(host);
    let glyph_size = appearance.icons.metrics(IconRole::Status).glyph_size;
    let terminal = [
        ("Idle", TerminalProgress::None),
        ("Known", TerminalProgress::Normal(40)),
        ("Unknown", TerminalProgress::Indeterminate),
        ("Error", TerminalProgress::Error(40)),
        ("Paused", TerminalProgress::Paused(40)),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (label, progress))| {
        caption(
            div()
                .text_color(gpui_color(appearance.colors.text))
                .child(
                    StatusGlyph {
                        icon: IconName::Terminal,
                        reported: None,
                        size: glyph_size,
                        progress,
                        attention: false,
                        id: ("workbench-status-terminal", index).into(),
                        selector_prefix: format!("workbench-status-terminal-{index}"),
                        colors: StatusColors {
                            host: gpui_color(host),
                            attention: gpui_color(status_colors.attention),
                            busy: gpui_color(status_colors.busy),
                            error: gpui_color(status_colors.error),
                            paused: gpui_color(status_colors.paused),
                        },
                        differentiate_without_color,
                    }
                    .render(),
                )
                .into_any_element(),
            label,
        )
    });
    let notices = StatusIntent::ALL
        .into_iter()
        .enumerate()
        .map(|(index, intent)| {
            caption(
                div()
                    .debug_selector(move || format!("workbench-status-notice-{index}"))
                    .child(Icon::new(
                        intent.glyph(differentiate_without_color),
                        glyph_size,
                        gpui_color(intent.color(&appearance.colors)),
                    ))
                    .into_any_element(),
                intent.label(),
            )
        });
    StateMatrix::new(
        "workbench-group-status",
        "workbench-row-status",
        "Status",
        div(),
        vec![
            cells("Workspace", appearance, workspace),
            cells("Terminal", appearance, terminal),
            cells("Notice", appearance, notices),
        ],
    )
    .render(surface, window, cx)
}

fn lists_group(
    palette: &Entity<CommandPalette<u8>>,
    icon_size: gpui::Pixels,
    surface: &SettingsAppearance,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let appearance = &surface.chrome;
    let palette = palette.clone();
    let rows = vec![
        FormRow::new(
            "workbench-row-controls-palette",
            "Command palette",
            Button::new("workbench-open-palette", "Open Palette")
                .variant(ButtonVariant::Outline)
                .debug_selector("workbench-open-palette")
                .on_activate(move |_, window, cx| {
                    palette.update(cx, |palette, cx| {
                        palette.open(window, cx);
                        palette.set_query("Open", cx);
                    });
                }),
        )
        .description("Icons, secondary text, a disabled result, and label matches for “Open”.")
        .render(appearance, window, cx)
        .into_any_element(),
        FormRow::new(
            "workbench-row-controls-combo",
            "Combo box rows",
            ComboBox::new(
                "workbench-combo",
                "Row states",
                Some(1_u8),
                "Choose",
                vec![
                    ComboBoxItem::new(1, "Disabled selected row")
                        .description("Visual state only; unavailable to navigation")
                        .disabled(true)
                        .preview_selected(true),
                    ComboBoxItem::new(2, "Enabled row")
                        .description("Keyboard and pointer acceptance stay live"),
                ],
            )
            .on_accept(|_, _, _| {}),
        )
        .render(appearance, window, cx)
        .into_any_element(),
        FormRow::new(
            "workbench-row-controls-icon-button",
            "Icon-only button",
            IconButton::new("workbench-icon-button", "Icon-only action", move |color| {
                Icon::new(IconName::Plus, icon_size, color).into_any_element()
            })
            .variant(ButtonVariant::Primary)
            .on_activate(|_, _, _| {}),
        )
        .render(appearance, window, cx)
        .into_any_element(),
    ];
    FormGroup::new(
        "workbench-group-controls-lists".to_owned(),
        "Lists and actions",
        rows,
    )
    .render(surface)
    .into_any_element()
}

fn family_label(family: FloatingControlFamily) -> &'static str {
    match family {
        FloatingControlFamily::Element => "Element",
        FloatingControlFamily::GhostElement => "GhostElement",
        FloatingControlFamily::Toggle => "Toggle",
        FloatingControlFamily::Segmented => "Segmented",
        FloatingControlFamily::Input => "Input",
    }
}

/// What the installed appearance could not satisfy, as bounded family names. `true` marks a
/// readability failure, which is shown in the error color.
pub(super) fn control_diagnostic_lines(appearance: &ChromeAppearance) -> Vec<(String, bool)> {
    let mut lines = Vec::new();
    let separator_hosts = appearance.separator_ceiling_fallbacks();
    if !separator_hosts.is_empty() {
        lines.push((
            format!(
                "Separator quietness ceiling relaxed: {}",
                separator_hosts.join(", ")
            ),
            false,
        ));
    }
    let disabled = |select: fn(&DisabledControlDiagnostic) -> Option<FloatingControlFamily>| {
        appearance
            .disabled_diagnostics
            .iter()
            .filter_map(select)
            .map(family_label)
            .collect::<Vec<_>>()
            .join(", ")
    };
    for (families, message, severe) in [
        (
            disabled(|diagnostic| match *diagnostic {
                DisabledControlDiagnostic::ContrastFloor { family } => Some(family),
                _ => None,
            }),
            "Disabled contrast floor unmet",
            true,
        ),
        (
            disabled(|diagnostic| match *diagnostic {
                DisabledControlDiagnostic::Separation { family } => Some(family),
                _ => None,
            }),
            "Disabled separation target unmet",
            false,
        ),
        (
            disabled(|diagnostic| match *diagnostic {
                DisabledControlDiagnostic::SharedPaint { family } => Some(family),
                _ => None,
            }),
            "Disabled shared paint unmet",
            false,
        ),
        (
            disabled(|diagnostic| match *diagnostic {
                DisabledControlDiagnostic::SelectedStep { family } => Some(family),
                _ => None,
            }),
            "Disabled selected step unmet",
            false,
        ),
    ] {
        if !families.is_empty() {
            lines.push((format!("{message}: {families}"), severe));
        }
    }
    let hosts = [
        ("Window", &appearance.window_control_fallbacks),
        ("TitleBar", &appearance.title_bar_controls.fallback_families),
        ("Panel", &appearance.panel_controls.fallback_families),
        ("Card", &appearance.card_controls.fallback_families),
        ("Floating", &appearance.floating_fallbacks),
    ]
    .into_iter()
    .filter(|(_, families)| !families.is_empty())
    .map(|(host, families)| {
        let families = families
            .iter()
            .copied()
            .map(family_label)
            .collect::<Vec<_>>()
            .join(", ");
        format!("{host}: {families}")
    })
    .collect::<Vec<_>>();
    if !hosts.is_empty() {
        lines.push((
            format!("Control constraint fallback families: {}", hosts.join("; ")),
            false,
        ));
    }
    lines
}

/// The constraint notice above the matrix. It is absent when the appearance satisfied every
/// control constraint.
pub(super) fn control_diagnostics(appearance: &ChromeAppearance) -> Option<Div> {
    let lines = control_diagnostic_lines(appearance);
    (!lines.is_empty()).then(|| {
        div()
            .debug_selector(|| "workbench-control-diagnostics".to_owned())
            .flex()
            .flex_col()
            .px(crate::ui::sidebar_window::form::row_horizontal_inset(
                appearance,
            ))
            .children(lines.into_iter().map(|(message, severe)| {
                // Under Differentiate Without Color a severe line also leads with the error glyph.
                let glyph =
                    (severe && appearance.capabilities.differentiate_without_color).then(|| {
                        div().flex_none().child(Icon::inherited(
                            IconName::CircleAlert,
                            appearance.icons.metrics(IconRole::Status).glyph_size,
                        ))
                    });
                div()
                    .flex()
                    .items_start()
                    .gap(appearance.spacing(6.0))
                    .chrome_text(appearance.typography.style(TextRole::Secondary))
                    .text_color(gpui_color(if severe {
                        appearance.colors.error
                    } else {
                        appearance.colors.text_muted
                    }))
                    .children(glyph)
                    .child(div().min_w_0().flex_1().whitespace_normal().child(message))
            }))
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{Render, TestAppContext};

    use super::*;
    use crate::ui::appearance::{InstalledChrome, PreparedControlHost};

    fn joined(appearance: &ChromeAppearance) -> String {
        control_diagnostic_lines(appearance)
            .into_iter()
            .map(|(message, _)| message)
            .collect::<Vec<_>>()
            .join("; ")
    }

    #[test]
    fn fallback_diagnostics_name_only_the_bounded_families() {
        let appearance = ChromeAppearance {
            window_control_fallbacks: vec![FloatingControlFamily::Segmented],
            title_bar_controls: PreparedControlHost {
                fallback_families: vec![FloatingControlFamily::GhostElement],
                ..ChromeAppearance::default().title_bar_controls
            },
            panel_controls: PreparedControlHost {
                fallback_families: vec![FloatingControlFamily::Input],
                ..ChromeAppearance::default().panel_controls
            },
            card_controls: PreparedControlHost {
                fallback_families: vec![FloatingControlFamily::Segmented],
                ..ChromeAppearance::default().card_controls
            },
            floating_fallbacks: vec![
                FloatingControlFamily::Element,
                FloatingControlFamily::GhostElement,
                FloatingControlFamily::Toggle,
            ],
            ..ChromeAppearance::default()
        };

        assert_eq!(
            joined(&appearance),
            "Control constraint fallback families: Window: Segmented; TitleBar: GhostElement; Panel: Input; Card: Segmented; Floating: Element, GhostElement, Toggle"
        );
    }

    #[test]
    fn disabled_diagnostics_distinguish_readability_from_separation() {
        let appearance = ChromeAppearance {
            disabled_diagnostics: vec![
                DisabledControlDiagnostic::ContrastFloor {
                    family: FloatingControlFamily::Element,
                },
                DisabledControlDiagnostic::Separation {
                    family: FloatingControlFamily::Toggle,
                },
                DisabledControlDiagnostic::SharedPaint {
                    family: FloatingControlFamily::Input,
                },
                DisabledControlDiagnostic::SelectedStep {
                    family: FloatingControlFamily::Segmented,
                },
            ],
            ..ChromeAppearance::default()
        };

        assert_eq!(
            joined(&appearance),
            "Disabled contrast floor unmet: Element; Disabled separation target unmet: Toggle; Disabled shared paint unmet: Input; Disabled selected step unmet: Segmented"
        );
        assert!(control_diagnostic_lines(&appearance)[0].1);
        assert_eq!(
            control_diagnostic_lines(&appearance)
                .iter()
                .map(|line| line.1)
                .collect::<Vec<_>>(),
            [true, false, false, false]
        );
    }

    struct DiagnosticsFixture;

    impl Render for DiagnosticsFixture {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let appearance = crate::ui::appearance::chrome(cx);
            div().children(control_diagnostics(appearance))
        }
    }

    #[gpui::test]
    fn diagnostics_disappear_after_an_appearance_update(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(InstalledChrome::single(Arc::new(ChromeAppearance {
                floating_fallbacks: vec![FloatingControlFamily::GhostElement],
                ..ChromeAppearance::default()
            })));
        });
        let (_, cx) = cx.add_window_view(|_, _| DiagnosticsFixture);
        cx.run_until_parked();
        assert!(cx.debug_bounds("workbench-control-diagnostics").is_some());

        cx.update(|window, cx| {
            cx.set_global(InstalledChrome::single(Arc::new(
                ChromeAppearance::default(),
            )));
            window.refresh();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("workbench-control-diagnostics").is_none());
    }
}
