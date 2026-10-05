use crate::ui::appearance::gpui_color;
use std::rc::Rc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use crate::appearance::{Appearance, AppearanceMode, TerminalFontFamily};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::settings::SettingsDocument;
use crate::ui::appearance_runtime;

use super::test_support::MemoryStorage;
use super::{SettingsSectionId, SettingsWindow};

#[test]
fn navigation_hover_changes_fill_without_adding_a_focus_like_rim() {
    use crate::appearance::{ChromeColors, Color};
    let colors = ChromeColors {
        row_selected_border: Color::rgb(0x888888),
        row_selected_hover_border: Color::rgb(0x0088ff),
        row_hover_border: Color::rgb(0x0088ff),
        ..ChromeColors::default()
    };
    for selected in [false, true] {
        let paint = crate::ui::sidebar_window::navigation_chip_paint(selected, true, &colors);
        assert_eq!(paint.hover_rim, None);
        assert_eq!(paint.rim, selected.then_some(colors.row_selected_border));
        assert_eq!(
            paint.hover_fill,
            Some(if selected {
                colors.row_selected_hover_background
            } else {
                colors.row_hover_background
            })
        );
    }
    let unavailable = crate::ui::sidebar_window::navigation_chip_paint(false, false, &colors);
    assert_eq!(unavailable.hover_fill, None);
    assert_eq!(unavailable.hover_rim, None);
}

#[test]
fn highlighted_row_materializes_against_its_card_host() {
    use crate::appearance::{
        AppearanceGeneration, AppearancePreferences, AvailableFonts, CompositionCapabilities,
        SurfaceRole, SystemAppearance, ThemeCatalog,
    };

    let mut preferences = AppearancePreferences::default();
    preferences.window.transparency = 1.0;
    let resolved = ThemeCatalog::default()
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::unavailable()
                .with_composition(CompositionCapabilities::new(true, true)),
            &AvailableFonts::default(),
        )
        .expect("built-in appearance should resolve");
    let appearance = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
    let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
    let expected = appearance.materials.paint(
        SurfaceRole::Surface,
        appearance.colors.elevated_surface_background,
        colors.row_selected_background,
    );

    assert_eq!(
        crate::ui::sidebar_window::form::highlighted_row_background(&appearance),
        expected
    );
}

#[gpui::test]
fn stepper_field_resolves_inside_its_rendered_card_host(cx: &mut TestAppContext) {
    use crate::appearance::Color;
    use crate::ui::appearance::ChromeAppearance;
    use crate::ui::appearance::settings::SettingsAppearance;
    use gpui::{
        Context, DivInspectorState, IntoElement as _, ParentElement as _, Render, ScrollDelta,
        ScrollWheelEvent, Styled as _, TouchPhase, Window, div, point, px,
    };
    use std::cell::RefCell;

    struct StepperCard(SettingsAppearance);
    impl Render for StepperCard {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
            let stepper =
                crate::ui::sidebar_window::form::Stepper::new("host-stepper", "Value", "1")
                    .render(&self.0.chrome)
                    .into_any_element();
            div().size_full().p(px(20.0)).child(
                crate::ui::sidebar_window::form::FormGroup::new(
                    "stepper-card".to_owned(),
                    "Value",
                    vec![stepper],
                )
                .render(&self.0),
            )
        }
    }

    cx.update(crate::ui::init).unwrap();
    let mut appearance = ChromeAppearance::default();
    appearance.control_colors.input_background = Color::rgb(0xcc2233);
    appearance.card_controls.colors.input_background = Color::rgb(0x228844);
    let expected = gpui_color(appearance.card_controls.colors.input_background);
    cx.update(|cx| {
        {
            let catalog = Box::new(crate::ui::control_theme_catalog::catalog(
                &appearance,
                spaceterm_ui::ControlMotion::Standard,
            ));
            spaceterm_ui::replace_control_theme_catalogs(
                cx,
                catalog.clone(),
                catalog.clone(),
                catalog.clone(),
                catalog,
            )
        }
        .unwrap()
    });
    let (_, cx) = cx.add_window_view(|_, _| StepperCard(SettingsAppearance::fallback(appearance)));
    cx.run_until_parked();
    let observed = Rc::new(RefCell::new(Vec::<DivInspectorState>::new()));
    let styles = Rc::clone(&observed);
    cx.update(|window, cx| {
        cx.register_inspector_element(move |_, _| {
            let styles = Rc::clone(&styles);
            move |_, state: &DivInspectorState, _, _| {
                styles.borrow_mut().push(state.clone());
                gpui::Empty
            }
        });
        cx.set_inspector_renderer(Box::new(|inspector, window, cx| {
            div()
                .children(inspector.render_inspector_states(window, cx))
                .into_any_element()
        }));
        window.toggle_inspector(cx);
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("host-stepper").is_some(),
        "Stepper must render in the card"
    );
    let value = cx
        .debug_bounds("host-stepper-value")
        .expect("Stepper must render its leading readout");
    let buttons = cx
        .debug_bounds("host-stepper-buttons")
        .expect("Stepper must render one joined button unit");
    assert!(
        value.size.width >= px(44.0),
        "the Compact readout must reserve at least 44 points, got {value:?}"
    );
    assert_eq!(
        buttons.size.width,
        buttons.size.height * 2.0,
        "the two Stepper buttons must divide one two-cell control-height unit"
    );
    cx.simulate_mouse_move(buttons.center(), None, Modifiers::none());
    cx.run_until_parked();
    for _ in 0..16 {
        if let Some(background) = observed
            .borrow()
            .iter()
            .rev()
            .find(|style| style.bounds == buttons)
            .map(|style| style.base_style.background.clone())
        {
            assert_eq!(
                background,
                Some(expected.into()),
                "Stepper frame must use Card input paint, not Window input paint"
            );
            return;
        }
        cx.simulate_event(ScrollWheelEvent {
            position: buttons.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(36.0))),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();
    }
    panic!("inspector did not expose the rendered Stepper frame");
}

fn open_settings<'a>(
    document: &SettingsDocument,
    cx: &'a mut TestAppContext,
) -> (Entity<SettingsWindow>, &'a mut VisualTestContext) {
    let settings = crate::settings::Settings::load(MemoryStorage::with_document(document));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
    });
    let (settings, cx) = cx.add_window_view(SettingsWindow::new);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (settings, cx)
}

/// Draws fresh geometry. GPUI retains removed selectors, so this cannot prove disappearance.
fn redraw(cx: &mut VisualTestContext) {
    cx.update(|_, cx| cx.refresh_windows());
    cx.run_until_parked();
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let position = cx
        .debug_bounds(selector)
        .expect("control should render")
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

/// The navigation list is one keyboard stop whose arrows move between sections.
///
/// Tab reaching every entry in turn would make the keyboard walk the sidebar before it could reach
/// a setting, and it is what gave an ordinary pointer click somewhere to leave a focus ring.
#[gpui::test]
fn tab_reaches_the_navigation_list_and_arrows_move_through_its_sections(cx: &mut TestAppContext) {
    let (settings, cx) = open_settings(&SettingsDocument::default(), cx);

    cx.simulate_keystrokes("tab tab");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| settings.read(cx).navigation.list_focus().is_focused(window)));
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Interface
    );

    cx.simulate_keystrokes("down");
    cx.run_until_parked();

    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Font
    );
    assert!(cx.debug_bounds("settings-terminal-font-family").is_some());

    cx.simulate_keystrokes("up");
    cx.run_until_parked();

    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Interface
    );
    assert!(cx.debug_bounds("settings-density").is_some());
}

/// The list stops at both ends rather than wrapping, so holding an arrow lands somewhere stable.
#[gpui::test]
fn navigation_arrows_stop_at_both_ends_of_the_list(cx: &mut TestAppContext) {
    let (settings, cx) = open_settings(&SettingsDocument::default(), cx);
    cx.simulate_keystrokes("tab tab");
    cx.run_until_parked();

    cx.simulate_keystrokes("up up");
    cx.run_until_parked();
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Interface
    );

    cx.simulate_keystrokes("end down");
    cx.run_until_parked();
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Advanced
    );

    cx.simulate_keystrokes("home");
    cx.run_until_parked();
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Interface
    );
}

/// Whether the selected navigation chip paints the accent fill that marks a list with keyboard
/// focus.
fn navigation_selection_is_emphasized(chip: &'static str, cx: &mut VisualTestContext) -> bool {
    let chip = cx
        .debug_bounds(chip)
        .expect("the selected section should paint its chip");
    cx.update(|window, cx| {
        let settings = crate::ui::appearance::settings::shared(cx);
        let accent = settings
            .chrome
            .host_colors(spaceterm_ui::ControlHost::Panel)
            .primary_background;
        let fill = gpui::Background::from(gpui_color(accent));
        let bounds = chip.scale(window.scale_factor());
        window
            .painted_quads()
            .iter()
            .any(|quad| quad.bounds == bounds && quad.background == fill)
    })
}

/// Pointer selection keeps the resting selection; keyboard focus emphasizes it in the accent
/// color, the way an AppKit source list does, and draws no ring.
#[gpui::test]
fn navigation_selection_is_emphasized_only_under_keyboard_navigation(cx: &mut TestAppContext) {
    let (settings, cx) = open_settings(&SettingsDocument::default(), cx);

    click("settings-navigation-settings-section-font", cx);
    redraw(cx);

    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Font
    );
    assert!(
        cx.debug_bounds("settings-navigation-chip-settings-section-font")
            .is_some(),
        "the selected section should keep its own material"
    );
    assert!(
        !navigation_selection_is_emphasized("settings-navigation-chip-settings-section-font", cx),
        "a pointer selection should not look keyboard focused"
    );

    assert!(!cx.update(|window, cx| settings.read(cx).navigation.list_focus().is_focused(window)));
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Font
    );

    // Keyboard navigation starts only when Tab enters the list.
    cx.simulate_keystrokes("tab tab down");
    cx.run_until_parked();

    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Themes
    );
    assert!(
        navigation_selection_is_emphasized("settings-navigation-chip-settings-section-themes", cx),
        "keyboard navigation must emphasize the selected section"
    );
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert!(!navigation_selection_is_emphasized(
        "settings-navigation-chip-settings-section-themes",
        cx
    ));
}

/// Arrow keys move between the sections a query left something to present, and skip the rest.
#[gpui::test]
fn light_navigation_pointer_selection_survives_focus_changes_during_a_click(
    cx: &mut TestAppContext,
) {
    let mut document = SettingsDocument::default();
    document.preferences.mode = AppearanceMode::Light;
    let (settings, cx) = open_settings(&document, cx);
    cx.simulate_keystrokes("tab tab");
    cx.run_until_parked();

    let position = cx
        .debug_bounds("settings-navigation-settings-section-font")
        .unwrap()
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_mouse_down(position, gpui::MouseButton::Left, Modifiers::none());
    // Native focus transitions can occur after the press callback and before its click callback.
    // Neither a blur nor a focus notification is evidence that the keyboard caused the event.
    cx.update(|window, cx| {
        let focus = settings.read(cx).focus_handle.clone();
        focus.focus(window, cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        let focus = settings.read(cx).navigation.list_focus().clone();
        focus.focus(window, cx);
    });
    cx.run_until_parked();
    assert!(!settings.read_with(cx, |settings, _| settings.navigation.focus_visible()));
    cx.simulate_mouse_up(position, gpui::MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    redraw(cx);
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Font
    );
    assert!(!settings.read_with(cx, |settings, _| settings.navigation.focus_visible()));
    assert!(!cx.update(|window, cx| settings.read(cx).navigation.list_focus().is_focused(window)));
    assert!(!cx.update(|window, cx| settings.read(cx).navigation.has_visible_focus(window)));

    // The search input delegates its bound Tab action, which must restore keyboard modality.
    click("settings-search", cx);
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| settings.read(cx).navigation.list_focus().is_focused(window)));
    assert!(cx.update(|window, cx| settings.read(cx).navigation.has_visible_focus(window)));
    assert!(navigation_selection_is_emphasized(
        "settings-navigation-chip-settings-section-font",
        cx
    ));

    click("settings-navigation-settings-section-font", cx);
    redraw(cx);
    assert!(!cx.update(|window, cx| settings.read(cx).navigation.list_focus().is_focused(window)));
    assert!(!cx.update(|window, cx| settings.read(cx).navigation.has_visible_focus(window)));
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Font
    );
    cx.simulate_keystrokes("tab tab down");
    cx.run_until_parked();
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Themes
    );
    assert!(cx.update(|window, cx| settings.read(cx).navigation.has_visible_focus(window)));
    assert!(navigation_selection_is_emphasized(
        "settings-navigation-chip-settings-section-themes",
        cx
    ));
}

#[gpui::test]
fn unmatched_search_skips_navigation(cx: &mut TestAppContext) {
    let (settings, cx) = open_settings(&SettingsDocument::default(), cx);
    cx.dispatch_action(super::FocusSettingsSearch);
    cx.simulate_input("no matching setting");
    cx.run_until_parked();

    // The in-field clear mark follows the native search-field convention and is not a separate
    // traversal stop, and a query that empties every section leaves navigation nothing to offer.
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert!(!cx.update(|window, cx| settings.read(cx).navigation.list_focus().is_focused(window)));
}

#[gpui::test]
fn navigation_skips_sections_without_search_matches(cx: &mut TestAppContext) {
    let (settings, cx) = open_settings(&SettingsDocument::default(), cx);
    cx.dispatch_action(super::FocusSettingsSearch);
    cx.simulate_input("weight");
    cx.run_until_parked();
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Font,
        "search should land on the first section that can answer it"
    );

    // The in-field clear mark is pointer-only; Escape clears from the keyboard, while Tab moves
    // directly into the navigation list.
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| settings.read(cx).navigation.list_focus().is_focused(window)));

    cx.simulate_keystrokes("down");
    cx.run_until_parked();

    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Font,
        "the list should step over the sections the query emptied"
    );
}

#[gpui::test]
fn non_preset_weights_remain_selected_when_the_picker_is_accepted(cx: &mut TestAppContext) {
    let mut document = SettingsDocument::default();

    document.preferences.terminal.typography.regular_weight = 450;
    let (settings, cx) = open_settings(&document, cx);

    click("settings-navigation-settings-section-font", cx);
    click("settings-row-terminal-regular-weight-control", cx);
    assert!(
        cx.debug_bounds("settings-row-terminal-regular-weight-control-450")
            .is_some()
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    click("settings-navigation-settings-section-font", cx);
    click("settings-row-terminal-regular-weight-control", cx);
    assert!(
        cx.debug_bounds("settings-row-terminal-regular-weight-control-450")
            .is_some()
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert_eq!(
        settings.read_with(cx, |settings, _| settings.editor.document().clone()),
        document
    );
}

#[gpui::test]
fn unavailable_terminal_font_remains_selected(cx: &mut TestAppContext) {
    let mut document = SettingsDocument::default();
    document.preferences.terminal.typography.family = TerminalFontFamily::Named {
        family: "Unavailable Settings Test Monospace".to_owned(),
    };
    let (settings, cx) = open_settings(&document, cx);
    click("settings-navigation-settings-section-font", cx);

    click("settings-terminal-font-family", cx);
    assert!(
        cx.debug_bounds("settings-terminal-font-unavailable")
            .is_some()
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert_eq!(
        settings.read_with(cx, |settings, _| settings.editor.document().clone()),
        document
    );
}

/// A Settings file that selects an uninstalled theme opens with that slot on the built-in theme,
/// which the list shows in use.
#[gpui::test]
fn a_missing_theme_selection_opens_on_the_builtin_theme(cx: &mut TestAppContext) {
    let mut document = SettingsDocument::default();
    document.preferences.mode = crate::appearance::AppearanceMode::Dark;
    let bytes = crate::settings::export_settings(&document)
        .unwrap()
        .replace("builtin.spaceterm.dark", "user.missing-terminal")
        .into_bytes();
    let settings = crate::settings::Settings::load(MemoryStorage::with_bytes(bytes));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
    });
    let (settings, cx) = cx.add_window_view(SettingsWindow::new);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    click("settings-navigation-settings-section-themes", cx);
    assert!(
        cx.debug_bounds("settings-theme-row-builtin.spaceterm.dark-in-use")
            .is_some()
    );
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.editor.document().clone()),
        document
    );
}
