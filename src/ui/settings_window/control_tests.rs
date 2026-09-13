use std::rc::Rc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use crate::appearance::{
    Appearance, AppearanceDocument, ChromeFontFamily, SchemeId, SchemeSelection, TerminalFontFamily,
};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::ui::appearance_runtime;

use super::test_support::MemoryStorage;
use super::{SettingsSectionId, SettingsWindow};

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

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let position = cx
        .debug_bounds(selector)
        .expect("control should render")
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

#[gpui::test]
fn tab_reaches_terminal_and_enter_activates_its_section(cx: &mut TestAppContext) {
    let (settings, cx) = open_settings(&AppearanceDocument::default(), cx);

    cx.simulate_keystrokes("tab tab tab tab");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| settings.read(cx).section_focus[2].is_focused(window)));
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Appearance
    );

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Terminal
    );
    assert!(cx.debug_bounds("settings-terminal-font-family").is_some());
}

#[gpui::test]
fn shift_tab_reaches_interface_and_space_activates_its_section(cx: &mut TestAppContext) {
    let (settings, cx) = open_settings(&AppearanceDocument::default(), cx);

    cx.simulate_keystrokes("tab tab tab tab shift-tab");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| settings.read(cx).section_focus[1].is_focused(window)));

    cx.simulate_keystrokes("space");
    cx.run_until_parked();

    assert_eq!(
        settings.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Interface
    );
    assert!(cx.debug_bounds("settings-chrome-font-family").is_some());
}

#[gpui::test]
fn tab_skips_sections_without_search_matches(cx: &mut TestAppContext) {
    let (settings, cx) = open_settings(&AppearanceDocument::default(), cx);
    cx.dispatch_action(super::FocusSettingsSearch);
    cx.simulate_input("line height");
    cx.run_until_parked();

    // A populated search exposes its Clear search button before the section entries.
    cx.simulate_keystrokes("tab tab");
    cx.run_until_parked();

    assert!(cx.update(|window, cx| settings.read(cx).section_focus[2].is_focused(window)));
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
    document.preferences.chrome.scheme = SchemeSelection::Fixed {
        id: SchemeId::new("user.missing-interface").unwrap(),
        appearance: Appearance::Dark,
    };
    document.preferences.terminal.scheme = SchemeSelection::Fixed {
        id: SchemeId::new("user.missing-terminal").unwrap(),
        appearance: Appearance::Dark,
    };
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
