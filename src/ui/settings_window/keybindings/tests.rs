//! The Keybindings section driven through its recorders.

use std::rc::Rc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use crate::appearance::{Appearance, SettingsDocument};
use crate::keybindings::{Command, KeybindingPreferences, Shortcut};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::ui::appearance_runtime;

use super::super::test_support::MemoryStorage;
use super::super::{SettingsRowId, SettingsSectionId, SettingsWindow};
use super::ShortcutDescription;

fn open_keybindings(
    document: SettingsDocument,
    cx: &mut TestAppContext,
) -> (Entity<SettingsWindow>, &mut VisualTestContext) {
    let settings = crate::settings::UserSettings::load(MemoryStorage::with_document(&document));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
    });
    let (window, cx) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            None,
            None,
            window,
            cx,
        )
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    click("settings-navigation-settings-section-keybindings", cx);
    (window, cx)
}

fn with_keybindings(source: &str) -> SettingsDocument {
    SettingsDocument {
        keybindings: serde_json::from_str::<KeybindingPreferences>(source)
            .expect("fixture keybindings"),
        ..SettingsDocument::default()
    }
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let position = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

fn record(
    window: &Entity<SettingsWindow>,
    command: Command,
    keystrokes: &str,
    cx: &mut VisualTestContext,
) {
    let recorder = window.read_with(cx, |settings, _| {
        settings.shortcuts.recorder(command).clone()
    });
    recorder.update_in(cx, |recorder, window, cx| {
        recorder.start_recording(window, cx);
    });
    cx.run_until_parked();
    cx.simulate_keystrokes(keystrokes);
    cx.run_until_parked();
}

fn is_recording(
    window: &Entity<SettingsWindow>,
    command: Command,
    cx: &mut VisualTestContext,
) -> bool {
    window.read_with(cx, |settings, cx| {
        settings.shortcuts.recorder(command).read(cx).is_recording()
    })
}

/// The retained override: absent for the default, `Some(None)` for Unassigned.
fn retained(
    window: &Entity<SettingsWindow>,
    command: Command,
    cx: &mut VisualTestContext,
) -> Option<Option<Shortcut>> {
    window.read_with(cx, |settings, _| {
        settings
            .editor
            .document()
            .keybindings
            .get(command)
            .cloned()
    })
}

fn description(
    window: &Entity<SettingsWindow>,
    command: Command,
    cx: &mut VisualTestContext,
) -> Option<ShortcutDescription> {
    window.read_with(cx, |settings, cx| settings.shortcut_description(command, cx))
}

/// The Commands whose rows the Keybindings search keeps, in row order.
fn found(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> Vec<Command> {
    window.read_with(cx, |settings, cx| {
        let mut rows = settings.rows_for(SettingsSectionId::Keybindings);
        settings.retain_found_shortcuts(&mut rows, cx);
        rows.into_iter()
            .filter_map(|row| match row {
                SettingsRowId::Shortcut(command) => Some(command),
                _ => None,
            })
            .collect()
    })
}

fn search_value(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> String {
    window.read_with(cx, |settings, cx| {
        settings.shortcuts.search_input().read(cx).value().to_owned()
    })
}

fn is_searching_by_shortcut(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> bool {
    window.read_with(cx, |settings, _| settings.shortcuts.is_searching_by_shortcut())
}

fn type_search(window: &Entity<SettingsWindow>, text: &str, cx: &mut VisualTestContext) {
    let input = window.read_with(cx, |settings, _| settings.shortcuts.search_input().clone());
    input.update_in(cx, |input, window, cx| input.focus_handle().focus(window, cx));
    cx.simulate_input(text);
    cx.run_until_parked();
}

fn shortcut(source: &str) -> Shortcut {
    Shortcut::parse(source).expect("fixture shortcut")
}

#[gpui::test]
fn every_command_has_a_row_in_the_keybindings_section(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);
    assert_eq!(
        window.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Keybindings
    );
    let rows = window.read_with(cx, |settings, _| {
        settings.rows_for(SettingsSectionId::Keybindings)
    });
    assert_eq!(rows.len(), Command::ALL.len());
    assert!(
        cx.debug_bounds("settings-row-shortcut-new-workspace-control")
            .is_some()
    );
    // Defaults offer nothing to reset.
    assert!(
        cx.debug_bounds("settings-row-shortcut-new-workspace-reset")
            .is_none()
    );
}

#[gpui::test]
fn clicking_a_shortcut_records_the_next_chord_into_the_draft(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);
    click("settings-row-shortcut-new-workspace-control", cx);
    assert!(is_recording(&window, Command::NewWorkspace, cx));
    cx.simulate_keystrokes("cmd-shift-y");
    cx.run_until_parked();

    assert!(!is_recording(&window, Command::NewWorkspace, cx));
    assert_eq!(
        retained(&window, Command::NewWorkspace, cx),
        Some(Some(shortcut("cmd-shift-y")))
    );
    assert_eq!(description(&window, Command::NewWorkspace, cx), None);
    assert!(
        cx.debug_bounds("settings-row-shortcut-new-workspace-reset")
            .is_some()
    );
}

#[gpui::test]
fn recording_another_commands_shortcut_reassigns_it(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);
    record(&window, Command::NewWorkspace, "cmd-shift-w", cx);

    assert_eq!(
        retained(&window, Command::NewWorkspace, cx),
        Some(Some(shortcut("cmd-shift-w")))
    );
    assert_eq!(retained(&window, Command::CloseTab, cx), Some(None));
    assert_eq!(
        description(&window, Command::NewWorkspace, cx),
        Some(ShortcutDescription {
            text: "Removed from Close Tab.".into(),
            warning: true,
        })
    );
    assert_eq!(
        description(&window, Command::CloseTab, cx),
        Some(ShortcutDescription {
            text: "Its shortcut is now assigned to New Workspace.".into(),
            warning: true,
        })
    );
}

#[gpui::test]
fn a_terminal_reserved_chord_is_refused_and_recording_continues(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);
    record(&window, Command::NewWorkspace, "ctrl-c", cx);

    assert!(is_recording(&window, Command::NewWorkspace, cx));
    assert_eq!(retained(&window, Command::NewWorkspace, cx), None);
    assert_eq!(
        description(&window, Command::NewWorkspace, cx),
        Some(ShortcutDescription {
            text: "Ctrl+C is reserved for programs running in the terminal.".into(),
            warning: true,
        })
    );

    cx.simulate_keystrokes("k");
    cx.run_until_parked();
    assert_eq!(
        description(&window, Command::NewWorkspace, cx),
        Some(ShortcutDescription {
            text: "K types into the terminal. Add Primary to use it as a shortcut.".into(),
            warning: true,
        })
    );

    // Escape ends the recording, and the refusal goes with it.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!is_recording(&window, Command::NewWorkspace, cx));
    assert_eq!(description(&window, Command::NewWorkspace, cx), None);
}

#[gpui::test]
fn a_system_reserved_chord_is_refused_without_being_performed(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);
    record(&window, Command::NewWorkspace, "cmd-q", cx);

    assert!(is_recording(&window, Command::NewWorkspace, cx));
    assert_eq!(retained(&window, Command::NewWorkspace, cx), None);
    assert_eq!(
        description(&window, Command::NewWorkspace, cx),
        Some(ShortcutDescription {
            text: "Primary+Q is reserved by Operating System for Quit.".into(),
            warning: true,
        })
    );
}

#[gpui::test]
fn a_bound_chord_while_recording_is_recorded_instead_of_closing_settings(
    cx: &mut TestAppContext,
) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);
    record(&window, Command::NewWorkspace, "cmd-w", cx);

    assert!(cx.debug_bounds("settings-window-surface").is_some());
    assert_eq!(
        retained(&window, Command::NewWorkspace, cx),
        Some(Some(shortcut("cmd-w")))
    );
    assert_eq!(retained(&window, Command::ClosePane, cx), Some(None));
}

#[gpui::test]
fn escape_while_recording_keeps_the_settings_search(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);
    window.update_in(cx, |settings, _, cx| {
        settings
            .search
            .update(cx, |search, cx| search.set_value("workspace".to_owned(), cx));
    });
    cx.run_until_parked();
    record(&window, Command::NewWorkspace, "escape", cx);

    assert!(!is_recording(&window, Command::NewWorkspace, cx));
    assert_eq!(
        window.read_with(cx, |settings, _| settings.query.clone()),
        "workspace"
    );
    assert_eq!(retained(&window, Command::NewWorkspace, cx), None);
}

#[gpui::test]
fn delete_unassigns_and_the_row_reset_restores_the_default(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);
    record(&window, Command::NewWorkspace, "backspace", cx);
    assert_eq!(retained(&window, Command::NewWorkspace, cx), Some(None));

    click("settings-row-shortcut-new-workspace-reset", cx);

    assert_eq!(retained(&window, Command::NewWorkspace, cx), None);
    assert!(
        cx.debug_bounds("settings-row-shortcut-new-workspace-reset")
            .is_none()
    );
}

#[gpui::test]
fn a_displaced_default_is_explained_and_its_reset_reclaims_it(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(with_keybindings(r#"{"close_workspace":"cmd-n"}"#), cx);

    assert_eq!(
        description(&window, Command::NewWorkspace, cx),
        Some(ShortcutDescription {
            text: "Its default shortcut is assigned to Close Workspace.".into(),
            warning: true,
        })
    );
    click("settings-row-shortcut-new-workspace-reset", cx);

    assert_eq!(retained(&window, Command::NewWorkspace, cx), None);
    assert_eq!(retained(&window, Command::CloseWorkspace, cx), None);
    assert_eq!(description(&window, Command::NewWorkspace, cx), None);
}

#[gpui::test]
fn reset_all_restores_every_keybinding(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(
        with_keybindings(r#"{"new_workspace":"cmd-shift-y","close_tab":null}"#),
        cx,
    );
    click("settings-reset-all", cx);
    click("modal-action-settings-reset-all-confirm", cx);

    assert!(window.read_with(cx, |settings, _| {
        settings
            .editor
            .document()
            .keybindings
            .iter()
            .next()
            .is_none()
    }));
}

#[gpui::test]
fn searching_by_text_keeps_the_commands_it_names(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);
    assert!(cx.debug_bounds("settings-keybindings-search-frame").is_some());
    assert_eq!(found(&window, cx).len(), Command::ALL.len());

    type_search(&window, "split", cx);

    assert_eq!(
        found(&window, cx),
        vec![Command::SplitRight, Command::SplitDown]
    );
    assert!(cx.debug_bounds("settings-keybindings-no-results").is_none());

    type_search(&window, " sideways", cx);

    assert!(found(&window, cx).is_empty());
    assert!(cx.debug_bounds("settings-keybindings-no-results").is_some());
    // The search stays in place so the query can be corrected.
    assert!(cx.debug_bounds("settings-keybindings-search-frame").is_some());
}

#[gpui::test]
fn searching_by_shortcut_finds_the_command_the_chord_runs(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);

    click("settings-keybindings-search-by-shortcut", cx);
    assert!(is_searching_by_shortcut(&window, cx));
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();

    assert_eq!(found(&window, cx), vec![Command::CreateTab]);
    assert_eq!(search_value(&window, cx), "Primary+T");
    // Recording continues, so the next chord replaces the first.
    assert!(is_searching_by_shortcut(&window, cx));
    cx.simulate_keystrokes("cmd-d");
    cx.run_until_parked();
    assert_eq!(found(&window, cx), vec![Command::SplitRight]);

    // Escape stops recording and keeps what it found.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!is_searching_by_shortcut(&window, cx));
    assert_eq!(found(&window, cx), vec![Command::SplitRight]);
    assert_eq!(
        window.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Keybindings
    );
}

#[gpui::test]
fn searching_by_shortcut_captures_a_chord_that_closes_the_window(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);

    click("settings-keybindings-search-by-shortcut", cx);
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();

    assert_eq!(found(&window, cx), vec![Command::ClosePane]);
    assert!(cx.debug_bounds("settings-window-surface").is_some());
}

#[gpui::test]
fn a_chord_no_command_uses_finds_nothing(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);

    click("settings-keybindings-search-by-shortcut", cx);
    cx.simulate_keystrokes("ctrl-c");
    cx.run_until_parked();

    assert!(found(&window, cx).is_empty());
    assert!(cx.debug_bounds("settings-keybindings-no-results").is_some());
}

#[gpui::test]
fn editing_a_found_shortcut_searches_by_text_again(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);
    click("settings-keybindings-search-by-shortcut", cx);
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();

    click("settings-keybindings-search-by-shortcut", cx);
    assert!(!is_searching_by_shortcut(&window, cx));
    click("settings-keybindings-search-clear", cx);
    type_search(&window, "zoom", cx);

    assert_eq!(found(&window, cx), vec![Command::TogglePaneZoom]);
}

#[gpui::test]
fn leaving_the_section_stops_searching_by_shortcut(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);
    click("settings-keybindings-search-by-shortcut", cx);

    click("settings-navigation-settings-section-interface", cx);

    assert!(!is_searching_by_shortcut(&window, cx));
}

#[gpui::test]
fn clicking_away_while_recording_cancels_without_a_change(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);
    let recorder = window.read_with(cx, |settings, _| {
        settings.shortcuts.recorder(Command::NewWorkspace).clone()
    });
    recorder.update_in(cx, |recorder, window, cx| {
        recorder.start_recording(window, cx);
    });
    cx.run_until_parked();

    click("settings-keybindings-search-frame", cx);

    assert!(!is_recording(&window, Command::NewWorkspace, cx));
    assert_eq!(retained(&window, Command::NewWorkspace, cx), None);
    // The keys that follow go where focus went, not to the recorder.
    cx.simulate_input("tab");
    cx.run_until_parked();
    assert_eq!(search_value(&window, cx), "tab");
    assert_eq!(retained(&window, Command::NewWorkspace, cx), None);
}
