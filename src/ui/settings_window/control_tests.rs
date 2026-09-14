use std::rc::Rc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use crate::appearance::{
    Appearance, AppearanceDocument, AppearanceMode, ChromeFontFamily, SchemeId, TerminalFontFamily,
};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
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
        let paint = super::navigation_chip_paint(selected, true, &colors);
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
    let unavailable = super::navigation_chip_paint(false, false, &colors);
    assert_eq!(unavailable.hover_fill, None);
    assert_eq!(unavailable.hover_rim, None);
}

fn open_settings<'a>(
    document: &AppearanceDocument,
    cx: &'a mut TestAppContext,
) -> (Entity<SettingsWindow>, &'a mut VisualTestContext) {
    let (settings, changed) =
        crate::settings::UserSettings::load(MemoryStorage::with_document(document));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings, changed, Rc::new(platform), cx)
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
    let (settings, cx) = open_settings(&AppearanceDocument::default(), cx);

    cx.simulate_keystrokes("tab tab");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| settings.read(cx).navigation_focus.is_focused(window)));
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Appearance
    );

    cx.simulate_keystrokes("down down");
    cx.run_until_parked();

    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Terminal
    );
    assert!(cx.debug_bounds("settings-terminal-font-family").is_some());

    cx.simulate_keystrokes("up");
    cx.run_until_parked();

    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Interface
    );
    assert!(cx.debug_bounds("settings-chrome-font-family").is_some());
}

/// The list stops at both ends rather than wrapping, so holding an arrow lands somewhere stable.
#[gpui::test]
fn navigation_arrows_stop_at_both_ends_of_the_list(cx: &mut TestAppContext) {
    let (settings, cx) = open_settings(&AppearanceDocument::default(), cx);
    cx.simulate_keystrokes("tab tab");
    cx.run_until_parked();

    cx.simulate_keystrokes("up up");
    cx.run_until_parked();
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Appearance
    );

    cx.simulate_keystrokes("end down");
    cx.run_until_parked();
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::ColorSchemes
    );

    cx.simulate_keystrokes("home");
    cx.run_until_parked();
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Appearance
    );
}

/// Selecting with the pointer leaves no focus ring; reaching the list with the keyboard draws one.
///
/// A ring that an ordinary click leaves behind reads as an accessibility signal rather than as the
/// resting selection, and the selected material already says which section is current.
#[gpui::test]
fn pointer_selection_leaves_no_focus_ring_while_the_keyboard_draws_one(cx: &mut TestAppContext) {
    let (settings, cx) = open_settings(&AppearanceDocument::default(), cx);

    click("settings-navigation-settings-section-terminal", cx);
    redraw(cx);

    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Terminal
    );
    assert!(
        cx.debug_bounds("settings-navigation-chip-settings-section-terminal")
            .is_some(),
        "the selected section should keep its own material"
    );
    assert!(
        cx.debug_bounds("settings-navigation-focus-indicator")
            .is_none(),
        "a pointer selection should not leave a focus ring behind it"
    );

    assert!(!cx.update(|window, cx| settings.read(cx).navigation_focus.is_focused(window)));
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Terminal
    );

    // Keyboard navigation starts only when Tab enters the list.
    cx.simulate_keystrokes("tab tab down");
    cx.run_until_parked();

    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::ColorSchemes
    );
    let chip = cx
        .debug_bounds("settings-navigation-chip-settings-section-color-schemes")
        .expect("the selected section should keep its own material");
    let ring = cx
        .debug_bounds("settings-navigation-focus-indicator")
        .expect("the keyboard should report where it is in the list");
    assert!(
        ring.left() < chip.left()
            && ring.top() < chip.top()
            && ring.right() > chip.right()
            && ring.bottom() > chip.bottom(),
        "focus should ride outside the chip it belongs to, got {ring:?} around {chip:?}"
    );
}

/// Arrow keys move between the sections a query left something to present, and skip the rest.
#[gpui::test]
fn light_navigation_pointer_selection_survives_focus_changes_during_a_click(
    cx: &mut TestAppContext,
) {
    let mut document = AppearanceDocument::default();
    document.preferences.mode = AppearanceMode::Light;
    document.preferences.chrome.schemes.light =
        SchemeId::new("builtin.spaceterm.chrome.light").unwrap();
    let (settings, cx) = open_settings(&document, cx);
    cx.simulate_keystrokes("tab tab");
    cx.run_until_parked();

    let position = cx
        .debug_bounds("settings-navigation-settings-section-interface")
        .unwrap()
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_mouse_down(position, gpui::MouseButton::Left, Modifiers::none());
    // Native focus transitions can occur after the press callback and before its click callback.
    // Neither a blur nor a focus notification is evidence that the keyboard caused the event.
    cx.update(|window, cx| {
        settings.read(cx).focus_handle.focus(window);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        settings.read(cx).navigation_focus.focus(window);
    });
    cx.run_until_parked();
    assert!(!settings.read_with(cx, |settings, _| settings.navigation_focus_visible));
    cx.simulate_mouse_up(position, gpui::MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    redraw(cx);
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Interface
    );
    assert!(!settings.read_with(cx, |settings, _| settings.navigation_focus_visible));
    assert!(!cx.update(|window, cx| settings.read(cx).navigation_focus.is_focused(window)));
    assert!(!cx.update(|window, cx| settings.read(cx).navigation_has_visible_focus(window)));

    // The search input delegates its bound Tab action, which must restore keyboard modality.
    click("settings-search", cx);
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| settings.read(cx).navigation_focus.is_focused(window)));
    assert!(cx.update(|window, cx| settings.read(cx).navigation_has_visible_focus(window)));
    assert!(
        cx.debug_bounds("settings-navigation-focus-indicator")
            .is_some()
    );

    click("settings-navigation-settings-section-interface", cx);
    redraw(cx);
    assert!(!cx.update(|window, cx| settings.read(cx).navigation_focus.is_focused(window)));
    assert!(!cx.update(|window, cx| settings.read(cx).navigation_has_visible_focus(window)));
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Interface
    );
    cx.simulate_keystrokes("tab tab down");
    cx.run_until_parked();
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Terminal
    );
    assert!(cx.update(|window, cx| settings.read(cx).navigation_has_visible_focus(window)));
    assert!(
        cx.debug_bounds("settings-navigation-focus-indicator")
            .is_some()
    );
}

#[gpui::test]
fn unmatched_search_skips_navigation_and_reaches_the_footer(cx: &mut TestAppContext) {
    let (settings, cx) = open_settings(&AppearanceDocument::default(), cx);
    cx.dispatch_action(super::FocusSettingsSearch);
    cx.simulate_input("no matching setting");
    cx.run_until_parked();

    // Clear search remains reachable, then the footer action follows the empty detail pane.
    cx.simulate_keystrokes("tab tab");
    cx.run_until_parked();
    assert!(!cx.update(|window, cx| settings.read(cx).navigation_focus.is_focused(window)));
    cx.simulate_keystrokes("enter");
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    cx.run_until_parked();
    assert!(cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
}

#[gpui::test]
fn navigation_skips_sections_without_search_matches(cx: &mut TestAppContext) {
    let (settings, cx) = open_settings(&AppearanceDocument::default(), cx);
    cx.dispatch_action(super::FocusSettingsSearch);
    cx.simulate_input("weight");
    cx.run_until_parked();
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Interface,
        "search should land on the first section that can answer it"
    );

    // A populated search exposes its Clear search button before the navigation list.
    cx.simulate_keystrokes("tab tab");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| settings.read(cx).navigation_focus.is_focused(window)));

    cx.simulate_keystrokes("down");
    cx.run_until_parked();

    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Terminal,
        "the list should step over the sections the query emptied"
    );
}

#[gpui::test]
fn non_preset_weights_remain_selected_when_the_picker_is_accepted(cx: &mut TestAppContext) {
    let mut document = AppearanceDocument::default();
    document.preferences.chrome.typography.regular_weight = 450;
    document.preferences.terminal.typography.regular_weight = 900;
    let (settings, cx) = open_settings(&document, cx);

    click("settings-navigation-settings-section-interface", cx);
    click("settings-row-chrome-regular-weight-control", cx);
    assert!(
        cx.debug_bounds("settings-row-chrome-regular-weight-control-450")
            .is_some()
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    click("settings-navigation-settings-section-terminal", cx);
    click("settings-row-terminal-regular-weight-control", cx);
    assert!(
        cx.debug_bounds("settings-row-terminal-regular-weight-control-900")
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
fn unavailable_interface_font_remains_selected(cx: &mut TestAppContext) {
    let mut document = AppearanceDocument::default();
    document.preferences.chrome.typography.family = ChromeFontFamily::Named {
        family: "Unavailable Settings Test Font".to_owned(),
    };
    let (settings, cx) = open_settings(&document, cx);
    click("settings-navigation-settings-section-interface", cx);

    click("settings-chrome-font-family", cx);
    assert!(
        cx.debug_bounds("settings-chrome-font-unavailable")
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
    let mut document = AppearanceDocument::default();
    document.preferences.terminal.typography.family = TerminalFontFamily::Named {
        family: "Unavailable Settings Test Monospace".to_owned(),
    };
    let (settings, cx) = open_settings(&document, cx);
    click("settings-navigation-settings-section-terminal", cx);

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

#[gpui::test]
fn unavailable_scheme_ids_remain_selected(cx: &mut TestAppContext) {
    let mut document = AppearanceDocument::default();
    document.preferences.chrome.schemes.dark = SchemeId::new("user.missing-interface").unwrap();
    document.preferences.terminal.schemes.dark = SchemeId::new("user.missing-terminal").unwrap();
    let (settings, cx) = open_settings(&document, cx);

    click("settings-row-chrome-scheme-control", cx);
    assert!(
        cx.debug_bounds("settings-row-chrome-scheme-control-user.missing-interface")
            .is_some()
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    click("settings-row-terminal-scheme-control", cx);
    assert!(
        cx.debug_bounds("settings-row-terminal-scheme-control-user.missing-terminal")
            .is_some()
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert_eq!(
        settings.read_with(cx, |settings, _| settings.editor.document().clone()),
        document
    );
}
