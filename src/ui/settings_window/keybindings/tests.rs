//! The Keybindings section driven through its recorders.

use std::{rc::Rc, sync::Arc};

use gpui::{Bounds, Entity, Modifiers, Pixels, TestAppContext, VisualTestContext};

use crate::appearance::{Appearance, SettingsDocument};
use crate::keybindings::{Command, KeybindingPreferences, Shortcut};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::storage::StorageError;
use crate::ui::appearance_runtime;

use super::super::test_support::MemoryStorage;
use super::super::{SettingsRowId, SettingsSectionId, SettingsWindow, control_selector};
use super::ShortcutDescription;
use crate::ui::sidebar_window::form::CaptionTone;

fn open_keybindings(
    document: SettingsDocument,
    cx: &mut TestAppContext,
) -> (Entity<SettingsWindow>, &mut VisualTestContext) {
    open_keybindings_with(MemoryStorage::with_document(&document), cx)
}

fn open_keybindings_with(
    storage: Arc<MemoryStorage>,
    cx: &mut TestAppContext,
) -> (Entity<SettingsWindow>, &mut VisualTestContext) {
    open_keybindings_on(storage, |_| {}, cx)
}

/// Opens Keybindings after `install_desktop` replaces the testing Desktop Profile.
fn open_keybindings_on(
    storage: Arc<MemoryStorage>,
    install_desktop: impl FnOnce(&mut gpui::App),
    cx: &mut TestAppContext,
) -> (Entity<SettingsWindow>, &mut VisualTestContext) {
    let settings = crate::settings::UserSettings::load(storage);
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        install_desktop(cx);
    });
    let (window, cx) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            Default::default(),
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
        settings.editor.document().keybindings.get(command).cloned()
    })
}

fn description(
    window: &Entity<SettingsWindow>,
    command: Command,
    cx: &mut VisualTestContext,
) -> Option<ShortcutDescription> {
    window.read_with(cx, |settings, cx| {
        settings.shortcut_description(command, cx)
    })
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
        settings
            .shortcuts
            .search_input()
            .read(cx)
            .value()
            .to_owned()
    })
}

/// The explanation a search that found nothing shows.
fn no_results(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> Option<String> {
    window.read_with(cx, |settings, cx| {
        settings.no_shortcuts_found(cx).map(String::from)
    })
}

fn is_searching_by_shortcut(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> bool {
    window.read_with(cx, |settings, _| {
        settings.shortcuts.is_searching_by_shortcut()
    })
}

fn type_search(window: &Entity<SettingsWindow>, text: &str, cx: &mut VisualTestContext) {
    let input = window.read_with(cx, |settings, _| settings.shortcuts.search_input().clone());
    input.update_in(cx, |input, window, cx| {
        input.focus_handle().focus(window, cx)
    });
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
            tone: CaptionTone::Warning,
        })
    );
    assert_eq!(
        description(&window, Command::CloseTab, cx),
        Some(ShortcutDescription {
            text: "Its shortcut is now assigned to New Workspace.".into(),
            tone: CaptionTone::Warning,
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
            tone: CaptionTone::Error,
        })
    );

    cx.simulate_keystrokes("k");
    cx.run_until_parked();
    assert_eq!(
        description(&window, Command::NewWorkspace, cx),
        Some(ShortcutDescription {
            text: "K is sent to the terminal. Add Primary to use it as a shortcut.".into(),
            tone: CaptionTone::Error,
        })
    );

    // A function key types nothing, and the same wording holds for it.
    cx.simulate_keystrokes("f5");
    cx.run_until_parked();
    assert_eq!(
        description(&window, Command::NewWorkspace, cx).map(|description| description.text),
        Some("F5 is sent to the terminal. Add Primary to use it as a shortcut.".into())
    );

    // Escape ends the recording, and the refusal goes with it.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!is_recording(&window, Command::NewWorkspace, cx));
    assert_eq!(description(&window, Command::NewWorkspace, cx), None);
}

/// Replaces the key bindings with a Keymap Profile on US English that follows the Control-Shift
/// conventions and reserves the chords those desktops keep for themselves.
fn install_control_shift_profile(cx: &mut gpui::App) {
    use crate::keybindings::{
        KeymapProfile, Shortcut, SystemReservation, SystemReserved, TerminalConventions,
    };
    let reserved = |chord: &str, reason| SystemReserved {
        shortcut: Shortcut::parse(chord).expect("the reserved chord parses"),
        reason,
    };
    let profile = KeymapProfile::new(
        crate::platform::keyboard_layout::testing::us(),
        TerminalConventions::ControlShiftShortcuts,
        [],
        vec![
            reserved("ctrl-shift-u", SystemReservation::InputMethod),
            reserved("ctrl-insert", SystemReservation::Copy),
            reserved("shift-insert", SystemReservation::PasteSelection),
            reserved("ctrl-,", SystemReservation::Settings),
        ],
        vec![],
        vec![],
    )
    .expect("the Control-Shift profile is valid");
    cx.clear_key_bindings();
    cx.bind_keys(profile.control_bindings().iter().cloned());
    cx.bind_keys(profile.fixed_bindings().iter().cloned());
    crate::keybindings::runtime::install(profile, cx);
}

#[gpui::test]
fn control_shift_recording_accepts_and_refuses_chords_by_its_conventions(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings_on(
        MemoryStorage::with_document(&SettingsDocument::default()),
        install_control_shift_profile,
        cx,
    );
    // The profile has no defaults, so each recording only replaces the previous one.
    for chord in [
        "alt-1",
        "ctrl-alt-1",
        "ctrl-pageup",
        "ctrl-=",
        "ctrl-0",
        "ctrl--",
        "ctrl-f5",
        "shift-pageup",
        "shift-home",
        "f9",
        "shift-f3",
        "ctrl-shift-y",
    ] {
        record(&window, Command::CloseWorkspace, chord, cx);
        assert!(
            !is_recording(&window, Command::CloseWorkspace, cx),
            "{chord}"
        );
        assert_eq!(
            retained(&window, Command::CloseWorkspace, cx),
            Some(Some(shortcut(chord))),
            "{chord}"
        );
    }

    let accepted = retained(&window, Command::CloseWorkspace, cx);
    for (chord, refusal) in [
        (
            "ctrl-c",
            "Ctrl+C is reserved for programs running in the terminal.",
        ),
        (
            "ctrl-2",
            "Ctrl+2 is reserved for programs running in the terminal.",
        ),
        (
            "ctrl-[",
            "Ctrl+[ is reserved for programs running in the terminal.",
        ),
        (
            "alt-b",
            "Alt+B is reserved for programs running in the terminal.",
        ),
        (
            "alt-f4",
            "Alt+F4 is reserved for programs running in the terminal.",
        ),
        (
            "ctrl-alt-left",
            "Ctrl+Alt+LEFT is reserved for programs running in the terminal.",
        ),
        (
            "shift-left",
            "Shift+LEFT is sent to the terminal. Add Ctrl+Shift to use it as a shortcut.",
        ),
        (
            "ctrl-shift-u",
            "Ctrl+Shift+U is reserved by Operating System for the input method.",
        ),
        (
            "ctrl-insert",
            "Ctrl+INSERT is reserved by Operating System for Copy.",
        ),
        (
            "shift-insert",
            "Shift+INSERT is reserved by Operating System for Paste Selection.",
        ),
        (
            "ctrl-,",
            "Ctrl+, is reserved by Operating System for Settings.",
        ),
        (
            "cmd-t",
            "Primary+T is reserved by Operating System for desktop shortcuts.",
        ),
    ] {
        record(&window, Command::CloseWorkspace, chord, cx);
        assert!(
            is_recording(&window, Command::CloseWorkspace, cx),
            "{chord}"
        );
        assert_eq!(
            description(&window, Command::CloseWorkspace, cx),
            Some(ShortcutDescription {
                text: refusal.into(),
                tone: CaptionTone::Error,
            }),
            "{chord}"
        );
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(
            retained(&window, Command::CloseWorkspace, cx),
            accepted,
            "{chord}"
        );
    }
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
            tone: CaptionTone::Error,
        })
    );
}

#[cfg(feature = "developer-tools")]
#[gpui::test]
fn develop_menu_chords_are_refused_so_they_never_shadow_a_command(cx: &mut TestAppContext) {
    let owner = crate::application_identity::ApplicationIdentity::current().display_name();
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);
    for (chord, text) in [
        (
            "cmd-alt-a",
            format!("Primary+Alt+A is reserved by {owner} for the Developer Workbench."),
        ),
        (
            "cmd-alt-c",
            format!("Primary+Alt+C is reserved by {owner} for Toggle Appearance."),
        ),
    ] {
        record(&window, Command::NewWorkspace, chord, cx);

        assert!(is_recording(&window, Command::NewWorkspace, cx));
        assert_eq!(retained(&window, Command::NewWorkspace, cx), None);
        assert_eq!(
            description(&window, Command::NewWorkspace, cx),
            Some(ShortcutDescription {
                text: text.into(),
                tone: CaptionTone::Error,
            })
        );
    }
}

#[gpui::test]
fn a_bound_chord_while_recording_is_recorded_instead_of_closing_settings(cx: &mut TestAppContext) {
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
        settings.search.update(cx, |search, cx| {
            search.set_value("workspace".to_owned(), cx)
        });
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
fn an_override_reserved_here_is_explained_and_its_default_stays_active(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(
        with_keybindings(r#"{"create_tab":"ctrl-c","close_workspace":"cmd-q"}"#),
        cx,
    );

    assert_eq!(
        description(&window, Command::CreateTab, cx),
        Some(ShortcutDescription {
            text: "Ctrl+C is reserved for terminal input here, so the default shortcut is active."
                .into(),
            tone: CaptionTone::Warning,
        })
    );
    assert_eq!(
        window.read_with(cx, |settings, cx| settings
            .resolved_keymap(cx)
            .shortcut(Command::CreateTab)
            .cloned()),
        Some(shortcut("cmd-t"))
    );
    // Close Workspace has no default to fall back to.
    assert_eq!(
        description(&window, Command::CloseWorkspace, cx),
        Some(ShortcutDescription {
            text: "Primary+Q is reserved by Operating System for Quit here and isn't active."
                .into(),
            tone: CaptionTone::Error,
        })
    );
    // Both overrides stay retained until the person changes them.
    assert_eq!(
        retained(&window, Command::CreateTab, cx),
        Some(Some(shortcut("ctrl-c")))
    );
    assert_eq!(
        retained(&window, Command::CloseWorkspace, cx),
        Some(Some(shortcut("cmd-q")))
    );
    click("settings-row-shortcut-create-tab-reset", cx);
    assert_eq!(retained(&window, Command::CreateTab, cx), None);
    assert_eq!(description(&window, Command::CreateTab, cx), None);
}

#[gpui::test]
fn a_displaced_default_is_explained_and_its_reset_reclaims_it(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(with_keybindings(r#"{"close_workspace":"cmd-n"}"#), cx);

    assert_eq!(
        description(&window, Command::NewWorkspace, cx),
        Some(ShortcutDescription {
            text: "Its default shortcut is assigned to Close Workspace.".into(),
            tone: CaptionTone::Warning,
        })
    );
    click("settings-row-shortcut-new-workspace-reset", cx);

    assert_eq!(retained(&window, Command::NewWorkspace, cx), None);
    assert_eq!(retained(&window, Command::CloseWorkspace, cx), None);
    assert_eq!(description(&window, Command::NewWorkspace, cx), None);
}

#[gpui::test]
fn a_displaced_default_and_its_inactive_override_are_both_explained(cx: &mut TestAppContext) {
    let document = with_keybindings(r#"{"create_tab":"cmd-t","split_right":"ctrl-shift-t"}"#);
    let (window, cx) = open_keybindings_on(
        MemoryStorage::with_document(&document),
        |cx| {
            use crate::keybindings::{DefaultBinding, KeymapProfile, TerminalConventions};
            let profile = KeymapProfile::new(
                crate::platform::keyboard_layout::testing::us(),
                TerminalConventions::ControlShiftShortcuts,
                [
                    (
                        Command::CreateTab,
                        Some(DefaultBinding::new("ctrl-shift-t", &[])),
                    ),
                    (
                        Command::SplitRight,
                        Some(DefaultBinding::new("ctrl-shift-d", &[])),
                    ),
                ],
                vec![],
                vec![],
                vec![],
            )
            .unwrap();
            cx.clear_key_bindings();
            crate::keybindings::runtime::install(profile, cx);
        },
        cx,
    );

    assert_eq!(
        description(&window, Command::CreateTab, cx),
        Some(ShortcutDescription {
            text: "Primary+T is reserved by Operating System for desktop shortcuts here. Its default shortcut is assigned to Split Right.".into(),
            tone: CaptionTone::Warning,
        })
    );
    assert_eq!(
        retained(&window, Command::CreateTab, cx),
        Some(Some(shortcut("cmd-t")))
    );
    assert_eq!(
        window.read_with(cx, |settings, cx| settings
            .resolved_keymap(cx)
            .shortcut(Command::CreateTab)
            .cloned()),
        None
    );
    click("settings-row-shortcut-create-tab-reset", cx);

    assert_eq!(retained(&window, Command::CreateTab, cx), None);
    assert_eq!(retained(&window, Command::SplitRight, cx), Some(None));
    assert_eq!(description(&window, Command::CreateTab, cx), None);
    assert_eq!(
        window.read_with(cx, |settings, cx| settings
            .resolved_keymap(cx)
            .shortcut(Command::CreateTab)
            .cloned()),
        Some(shortcut("ctrl-shift-t"))
    );
    assert_eq!(
        window.read_with(cx, |settings, cx| settings
            .resolved_keymap(cx)
            .shortcut(Command::SplitRight)
            .cloned()),
        None
    );
}

#[gpui::test]
fn reset_all_restores_every_keybinding(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(
        with_keybindings(r#"{"new_workspace":"cmd-shift-y","close_tab":null}"#),
        cx,
    );
    click("settings-navigation-settings-section-advanced", cx);
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
    assert!(
        cx.debug_bounds("settings-keybindings-search-frame")
            .is_some()
    );
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
    assert!(
        cx.debug_bounds("settings-keybindings-search-frame")
            .is_some()
    );
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
    assert_eq!(
        no_results(&window, cx).as_deref(),
        Some("Ctrl+C is reserved for programs running in the terminal.")
    );
}

#[gpui::test]
fn searching_by_an_unused_shortcut_says_no_command_uses_it(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);

    click("settings-keybindings-search-by-shortcut", cx);
    cx.simulate_keystrokes("cmd-shift-y");
    cx.run_until_parked();

    assert!(found(&window, cx).is_empty());
    assert_eq!(
        no_results(&window, cx).as_deref(),
        Some("No command uses Primary+Shift+Y.")
    );
}

#[gpui::test]
fn searching_by_a_system_reserved_shortcut_names_its_owner(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);

    click("settings-keybindings-search-by-shortcut", cx);
    cx.simulate_keystrokes("cmd-q");
    cx.run_until_parked();

    assert!(found(&window, cx).is_empty());
    assert_eq!(
        no_results(&window, cx).as_deref(),
        Some("Primary+Q is reserved by Operating System for Quit.")
    );
}

#[gpui::test]
fn the_search_by_shortcut_toggle_is_the_tab_stop_after_the_search_field(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);
    type_search(&window, "", cx);

    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert!(!window.read_with(cx, |settings, cx| {
        settings.shortcuts.search_input().read(cx).is_focused()
    }));
    // A button activates when the key that pressed it is released.
    cx.simulate_keystrokes("space");
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("space").expect("keystroke"),
    });
    cx.run_until_parked();

    assert!(is_searching_by_shortcut(&window, cx));
    assert!(window.read_with(cx, |settings, cx| {
        settings.shortcuts.search_input().read(cx).is_focused()
    }));
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
fn a_reassignment_caption_leaves_when_reload_replaces_the_keybindings(cx: &mut TestAppContext) {
    let storage = MemoryStorage::with_document(&SettingsDocument::default());
    let (window, cx) = open_keybindings_with(Arc::clone(&storage), cx);
    storage.fail_writes(Some(StorageError::Conflict));
    record(&window, Command::NewWorkspace, "cmd-shift-w", cx);
    assert!(description(&window, Command::CloseTab, cx).is_some());

    storage.fail_writes(None);
    storage.repair();
    cx.update(|_, cx| window.update(cx, |settings, cx| settings.editor.reload(cx)));
    cx.run_until_parked();

    assert_eq!(retained(&window, Command::CloseTab, cx), None);
    assert_eq!(description(&window, Command::CloseTab, cx), None);
    assert_eq!(description(&window, Command::NewWorkspace, cx), None);
}

#[gpui::test]
fn leaving_the_window_stops_searching_by_shortcut(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);
    click("settings-keybindings-search-by-shortcut", cx);

    cx.deactivate_window();
    cx.run_until_parked();

    assert!(!is_searching_by_shortcut(&window, cx));
}

/// The window bounds of the Command's recorder in the latest frame.
fn recorder_bounds(command: Command, cx: &mut VisualTestContext) -> Bounds<Pixels> {
    let selector = control_selector(SettingsRowId::Shortcut(command)).leak();
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
}

fn focus_recorder(window: &Entity<SettingsWindow>, command: Command, cx: &mut VisualTestContext) {
    let recorder = window.read_with(cx, |settings, _| {
        settings.shortcuts.recorder(command).clone()
    });
    recorder.update_in(cx, |recorder, window, cx| {
        recorder.focus_handle().focus(window, cx)
    });
    cx.run_until_parked();
    // The frame that reports the focus scrolls, and the next one draws the scrolled rows.
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

#[gpui::test]
fn keyboard_focus_scrolls_a_hidden_shortcut_row_into_view(cx: &mut TestAppContext) {
    let (window, cx) = open_keybindings(SettingsDocument::default(), cx);
    let viewport = window.read_with(cx, |settings, _| settings.scroll.bounds());
    let first = Command::ALL[0];
    let last = Command::ALL[Command::ALL.len() - 1];
    assert!(
        recorder_bounds(last, cx).bottom() > viewport.bottom(),
        "the last row starts below the viewport"
    );

    focus_recorder(&window, last, cx);
    let shown = recorder_bounds(last, cx);
    assert!(shown.top() >= viewport.top() && shown.bottom() <= viewport.bottom());

    focus_recorder(&window, first, cx);
    let shown = recorder_bounds(first, cx);
    assert!(shown.top() >= viewport.top() && shown.bottom() <= viewport.bottom());
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
