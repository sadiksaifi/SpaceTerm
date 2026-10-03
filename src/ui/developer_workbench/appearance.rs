//! The Appearance section: preview edits beyond the toolbar's Appearance Mode, resets, and the
//! installed appearance's diagnostics.

use gpui::prelude::*;
use gpui::{AnyElement, Window, div};
use spaceterm_ui::{Menu, MenuEntry, SegmentedControl, SegmentedOption, Switch, ToggleSize};

use super::DeveloperWorkbench;
use super::preview::AppearancePreview;
use crate::appearance::{Appearance, ChromeDensity, ResetTarget, SettingsDocument};
use crate::ui::appearance::gpui_color;
use crate::ui::appearance::settings::SettingsAppearance;
use crate::ui::appearance_runtime;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::sidebar_window::form::{FormGroup, FormRow, FormRowLayout};

/// The transparency stops a capture compares. A document between stops selects none.
#[derive(Clone, Copy, Debug, PartialEq)]
enum TransparencyStop {
    Opaque,
    Default,
    Maximum,
}

impl TransparencyStop {
    const ALL: [Self; 3] = [Self::Opaque, Self::Default, Self::Maximum];

    fn value(self) -> f32 {
        match self {
            Self::Opaque => 0.0,
            Self::Default => SettingsDocument::default().preferences.window.transparency,
            Self::Maximum => 1.0,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Opaque => "Opaque",
            Self::Default => "Default",
            Self::Maximum => "Maximum",
        }
    }

    fn selector(self) -> &'static str {
        match self {
            Self::Opaque => "workbench-transparency-opaque",
            Self::Default => "workbench-transparency-default",
            Self::Maximum => "workbench-transparency-maximum",
        }
    }

    fn of(transparency: f32) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|stop| stop.value() == transparency)
    }
}

/// Every reset the Settings Document offers, grouped as the menu presents them.
fn reset_entries(cx: &gpui::App) -> Vec<MenuEntry<ResetTarget>> {
    let effective_theme = appearance_runtime::current(cx)
        .terminal
        .effective_theme
        .clone();
    let mut fields = vec![
        MenuEntry::action("Appearance Mode", ResetTarget::AppearanceMode),
        MenuEntry::action("Transparency", ResetTarget::Transparency),
        MenuEntry::action("Blur", ResetTarget::Blur),
        MenuEntry::action(
            "Light Terminal Theme",
            ResetTarget::TerminalTheme(Appearance::Light),
        ),
        MenuEntry::action(
            "Dark Terminal Theme",
            ResetTarget::TerminalTheme(Appearance::Dark),
        ),
        MenuEntry::action("Font Family", ResetTarget::TerminalFontFamily),
        MenuEntry::action("Font Size", ResetTarget::TerminalBaseSize),
        MenuEntry::action("Regular Weight", ResetTarget::TerminalRegularWeight),
        MenuEntry::action("Bold Weight", ResetTarget::TerminalBoldWeight),
        MenuEntry::action("Line Height", ResetTarget::TerminalLineHeight),
        MenuEntry::action("Italic", ResetTarget::TerminalItalic),
        MenuEntry::action("Bold as Bright", ResetTarget::TerminalBoldAsBright),
    ];
    fields.extend(
        ResetTarget::terminal_color_override(effective_theme, "foreground")
            .map(|target| MenuEntry::action("Foreground Color Override", target)),
    );
    vec![
        MenuEntry::section(
            "Groups",
            vec![
                MenuEntry::action("Density", ResetTarget::Density),
                MenuEntry::action("Terminal Colors", ResetTarget::TerminalColors),
                MenuEntry::action("Terminal Typography", ResetTarget::TerminalTypography),
                MenuEntry::action("Terminal Rendering", ResetTarget::TerminalRendering),
            ],
        ),
        MenuEntry::section("Fields", fields),
        MenuEntry::separator(),
        MenuEntry::action("All Appearance", ResetTarget::AllAppearance),
    ]
}

pub(super) fn render(
    workbench: &DeveloperWorkbench,
    surface: &SettingsAppearance,
    window: &Window,
    cx: &mut gpui::Context<DeveloperWorkbench>,
) -> Vec<AnyElement> {
    let appearance = &surface.chrome;
    let document = workbench.preview.document();
    let window_preferences = &document.preferences.window;

    let owner = cx.weak_entity();
    let density = SegmentedControl::new(
        "workbench-density",
        "Density",
        &window_preferences.density,
        vec![
            SegmentedOption::new(ChromeDensity::Compact, "Compact")
                .debug_selector("workbench-density-compact"),
            SegmentedOption::new(ChromeDensity::Comfortable, "Comfortable")
                .debug_selector("workbench-density-comfortable"),
        ],
    )
    .expect("two densities are within the bounded option set")
    .debug_selector("workbench-density")
    .on_change(move |change, _, cx| {
        let density = *change.requested();
        let _ = owner.update(cx, |workbench, cx| {
            workbench.apply(
                |preview| preview.set_density(density),
                "Density changed",
                cx,
            );
        });
    });
    let owner = cx.weak_entity();
    let transparency = SegmentedControl::new(
        "workbench-transparency",
        "Transparency",
        &TransparencyStop::of(window_preferences.transparency),
        TransparencyStop::ALL
            .into_iter()
            .map(|stop| {
                SegmentedOption::new(Some(stop), stop.label()).debug_selector(stop.selector())
            })
            .collect(),
    )
    .expect("three stops are within the bounded option set")
    .debug_selector("workbench-transparency")
    .on_change(move |change, _, cx| {
        let Some(stop) = *change.requested() else {
            return;
        };
        let _ = owner.update(cx, |workbench, cx| {
            workbench.apply(
                |preview| preview.set_transparency(stop.value()),
                "Transparency changed",
                cx,
            );
        });
    });
    let owner = cx.weak_entity();
    let blur = Switch::new("workbench-blur", "Blur", window_preferences.blur)
        .size(ToggleSize::Regular)
        .label_hidden(true)
        .debug_selector("workbench-blur")
        .on_change(move |change, _, cx| {
            let blur = change.requested();
            let _ = owner.update(cx, |workbench, cx| {
                workbench.apply(|preview| preview.set_blur(blur), "Blur changed", cx);
            });
        });
    let owner = cx.weak_entity();
    let bold_as_bright = Switch::new(
        "workbench-bold-as-bright",
        "Bold as bright",
        document.preferences.terminal.rendering.bold_as_bright,
    )
    .size(ToggleSize::Regular)
    .label_hidden(true)
    .debug_selector("workbench-bold-as-bright")
    .on_change(move |change, _, cx| {
        let enabled = change.requested();
        let _ = owner.update(cx, |workbench, cx| {
            workbench.apply(
                |preview| preview.set_bold_as_bright(enabled),
                "Bold as bright changed",
                cx,
            );
        });
    });
    let owner = cx.weak_entity();
    let alternate = Switch::new(
        "workbench-alternate-typography",
        "Alternate typography",
        AppearancePreview::alternate_typography(&document),
    )
    .size(ToggleSize::Regular)
    .label_hidden(true)
    .debug_selector("workbench-alternate-typography")
    .on_change(move |change, _, cx| {
        let alternate = change.requested();
        let _ = owner.update(cx, |workbench, cx| {
            workbench.apply(
                |preview| preview.set_alternate_typography(alternate),
                "Terminal typography changed",
                cx,
            );
        });
    });
    let owner = cx.weak_entity();
    let reset = Menu::new("workbench-reset", "Reset", reset_entries(cx))
        .debug_selector("workbench-reset")
        .on_activate(move |activation, _, cx| {
            let target = activation.action().clone();
            let _ = owner.update(cx, |workbench, cx| {
                workbench.apply(|preview| preview.reset(target), "Reset applied", cx);
            });
        });

    let window_rows = vec![
        FormRow::new("workbench-row-appearance-density", "Density", density)
            .render(appearance, window, cx)
            .into_any_element(),
        FormRow::new(
            "workbench-row-appearance-transparency",
            "Transparency",
            transparency,
        )
        .render(appearance, window, cx)
        .into_any_element(),
        FormRow::new("workbench-row-appearance-blur", "Blur", blur)
            .render(appearance, window, cx)
            .into_any_element(),
    ];
    let terminal_rows = vec![
        FormRow::new(
            "workbench-row-appearance-bold-as-bright",
            "Bold as bright",
            bold_as_bright,
        )
        .render(appearance, window, cx)
        .into_any_element(),
        FormRow::new(
            "workbench-row-appearance-alternate-typography",
            "Alternate typography",
            alternate,
        )
        .description("Menlo at 22 points with a 1.35 line height, and the other density.")
        .render(appearance, window, cx)
        .into_any_element(),
    ];
    let reset_rows = vec![
        FormRow::new("workbench-row-appearance-reset", "Reset in preview", reset)
            .description("Restores one field or group to its default. Imported themes remain.")
            .render(appearance, window, cx)
            .into_any_element(),
    ];
    let diagnostics_rows = vec![
        FormRow::new(
            "workbench-row-appearance-diagnostics",
            "Diagnostics",
            render_diagnostics(workbench, surface, cx),
        )
        .layout(FormRowLayout::Full)
        .render(appearance, window, cx)
        .into_any_element(),
    ];
    [
        ("workbench-group-window", "Window", window_rows),
        ("workbench-group-terminal", "Terminal", terminal_rows),
        ("workbench-group-reset", "Reset", reset_rows),
        (
            "workbench-group-diagnostics",
            "Diagnostics",
            diagnostics_rows,
        ),
    ]
    .into_iter()
    .map(|(selector, title, rows)| {
        FormGroup::new(selector.to_owned(), title, rows)
            .render(surface)
            .into_any_element()
    })
    .collect()
}

/// What the installed appearance resolved to. Its selector names the appearance generation, so a
/// test can see a repaint follow a system change.
fn render_diagnostics(
    workbench: &DeveloperWorkbench,
    surface: &SettingsAppearance,
    cx: &gpui::App,
) -> AnyElement {
    let appearance = &surface.chrome;
    let current = appearance_runtime::current(cx);
    let generation = current.generation.get();
    div()
        .debug_selector(move || format!("workbench-diagnostics-generation-{generation}"))
        .flex()
        .flex_col()
        .gap(appearance.spacing(2.0))
        .chrome_text(appearance.typography.style(TextRole::Secondary))
        .text_color(gpui_color(appearance.colors.text_secondary))
        .font_family(crate::bundled_font::FAMILY)
        .children(
            workbench
                .diagnostics(cx)
                .into_iter()
                .map(|line| div().whitespace_normal().child(line)),
        )
        .into_any_element()
}
