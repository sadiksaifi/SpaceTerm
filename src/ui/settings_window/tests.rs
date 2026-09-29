use std::{rc::Rc, sync::Arc};

use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px};

use crate::appearance::{
    Appearance, AppearanceMode, ChromeDensity, SettingsDocument, builtin_fallback_theme,
};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::{
    OperatingSystemWindowDragPlatform, RecordingOperatingSystemWindowDragPlatform,
};
use crate::settings::storage::StorageError;
use crate::theme_registry::ZedThemeRegistry;
use crate::theme_registry::testing::{MemoryTransport, extension_archive};
use crate::ui::appearance_runtime;

use super::editor::{COMMIT_DELAY, SaveStatus};
use super::{SettingsRowId, SettingsSectionId, SettingsWindow};

use super::test_support::MemoryStorage;

struct Harness {
    storage: Arc<MemoryStorage>,
    settings: crate::settings::UserSettings,
    platform: RecordingAppearancePlatform,
}

fn open_settings(
    cx: &mut TestAppContext,
) -> (Entity<SettingsWindow>, Harness, &mut VisualTestContext) {
    open_settings_with(
        cx,
        MemoryStorage::with_document(&SettingsDocument::default()),
    )
}

fn open_settings_with(
    cx: &mut TestAppContext,
    storage: Arc<MemoryStorage>,
) -> (Entity<SettingsWindow>, Harness, &mut VisualTestContext) {
    open_settings_with_drag(
        cx,
        storage,
        Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
    )
}

fn open_settings_with_drag(
    cx: &mut TestAppContext,
    storage: Arc<MemoryStorage>,
    window_drag: Rc<dyn OperatingSystemWindowDragPlatform>,
) -> (Entity<SettingsWindow>, Harness, &mut VisualTestContext) {
    open_settings_with_capabilities(cx, storage, window_drag, None)
}

fn open_settings_with_registry(
    cx: &mut TestAppContext,
    transport: Arc<MemoryTransport>,
) -> (Entity<SettingsWindow>, Harness, &mut VisualTestContext) {
    open_settings_with_capabilities(
        cx,
        MemoryStorage::with_document(&SettingsDocument::default()),
        Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
        Some(ZedThemeRegistry::new(transport)),
    )
}

fn open_settings_with_capabilities(
    cx: &mut TestAppContext,
    storage: Arc<MemoryStorage>,
    window_drag: Rc<dyn OperatingSystemWindowDragPlatform>,
    registry: Option<ZedThemeRegistry>,
) -> (Entity<SettingsWindow>, Harness, &mut VisualTestContext) {
    let settings = crate::settings::UserSettings::load(storage.clone());
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings.clone(), Rc::new(platform.clone()), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
    });
    let (window, cx) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(window_drag, None, registry, window, cx)
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (
        window,
        Harness {
            storage,
            settings,
            platform,
        },
        cx,
    )
}

fn right_click(selector: &'static str, cx: &mut VisualTestContext) {
    let position = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
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

/// Selects one navigation entry, because each section is its own view.
fn select_section(section: SettingsSectionId, cx: &mut VisualTestContext) {
    let selector: &'static str = match section {
        SettingsSectionId::Interface => "settings-navigation-settings-section-interface",
        SettingsSectionId::Font => "settings-navigation-settings-section-font",
        SettingsSectionId::Themes => "settings-navigation-settings-section-themes",
        SettingsSectionId::Keybindings => "settings-navigation-settings-section-keybindings",
        SettingsSectionId::Privacy => "settings-navigation-settings-section-privacy",
        SettingsSectionId::Updates => "settings-navigation-settings-section-updates",
        SettingsSectionId::Advanced => "settings-navigation-settings-section-advanced",
    };
    click(selector, cx);
}

/// Runs the debounce out so a scheduled write happens.
fn settle(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(COMMIT_DELAY * 2);
    cx.run_until_parked();
}

fn status(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> SaveStatus {
    window.read_with(cx, |window, _| window.editor.status())
}

fn set_query(window: &Entity<SettingsWindow>, query: &str, cx: &mut VisualTestContext) {
    let query = query.to_owned();
    cx.update(|_, cx| {
        window.update(cx, |window, cx| {
            let search = window.search.clone();
            search.update(cx, |search, cx| {
                search.set_value(query.clone(), cx);
            });
        });
    });
    cx.run_until_parked();
}

// Save model ---------------------------------------------------------------------------------

#[gpui::test]
fn an_edit_previews_at_once_and_writes_after_it_settles(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    let before = cx.update(|_, cx| appearance_runtime::current(cx).generation.get());

    click("settings-density-comfortable", cx);

    // The preview is live before anything is written.
    assert_eq!(harness.storage.writes(), 0);
    assert_eq!(status(&window, cx), SaveStatus::Saving);
    assert_eq!(
        document_of(&window, cx).preferences.window.density,
        ChromeDensity::Comfortable
    );
    let previewing = cx.update(|_, cx| appearance_runtime::current(cx).generation.get());
    assert!(
        previewing > before,
        "a preview should repaint the application"
    );

    settle(cx);

    assert_eq!(harness.storage.writes(), 1);
    assert_eq!(status(&window, cx), SaveStatus::Saved);
    assert_eq!(
        harness
            .storage
            .document()
            .expect("the retained document should parse")
            .preferences
            .window
            .density,
        ChromeDensity::Comfortable
    );
}

#[gpui::test]
fn several_rapid_changes_produce_one_write_carrying_the_last_value(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Font, cx);

    click("settings-terminal-base-size-increase", cx);
    click("settings-terminal-base-size-increase", cx);
    click("settings-terminal-base-size-increase", cx);
    assert_eq!(harness.storage.writes(), 0);

    settle(cx);

    assert_eq!(harness.storage.writes(), 1);
    assert_eq!(
        document_of(&window, cx)
            .preferences
            .terminal
            .typography
            .base_size,
        21.0
    );
}

#[gpui::test]
fn an_edit_that_cannot_reach_the_preview_is_re_pushed_rather_than_lost(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    // Another owner holds the transaction, so the window cannot begin its own preview.
    let blocking = harness
        .settings
        .begin_preview(harness.settings.snapshot().committed.revision)
        .expect("the fixture should obtain the first preview");

    click("settings-density-comfortable", cx);

    // The draft carries the change even though the live preview could not.
    assert_eq!(
        document_of(&window, cx).preferences.window.density,
        ChromeDensity::Comfortable
    );
    settle(cx);
    assert_eq!(
        harness.storage.writes(),
        0,
        "a blocked transaction must not be written around"
    );

    drop(blocking);
    settle(cx);

    assert_eq!(harness.storage.writes(), 1);
    assert_eq!(
        harness
            .storage
            .document()
            .expect("the retained document should parse")
            .preferences
            .window
            .density,
        ChromeDensity::Comfortable
    );
    assert_eq!(status(&window, cx), SaveStatus::Saved);
}

#[gpui::test]
fn a_failed_write_keeps_the_change_applied_and_retry_writes_it(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    harness.storage.fail_writes(Some(StorageError::Unavailable));

    click("settings-density-comfortable", cx);
    settle(cx);

    assert!(matches!(status(&window, cx), SaveStatus::Failed(_)));
    // The change is still previewing, so the application still shows it.
    assert_eq!(
        harness
            .settings
            .snapshot()
            .candidate
            .preferences
            .window
            .density,
        ChromeDensity::Comfortable
    );
    assert!(cx.debug_bounds("settings-banner").is_some());

    harness.storage.fail_writes(None);
    click("settings-banner-retry", cx);
    cx.run_until_parked();
    settle(cx);

    assert_eq!(status(&window, cx), SaveStatus::Saved);
    assert_eq!(
        harness
            .storage
            .document()
            .expect("the retained document should parse")
            .preferences
            .window
            .density,
        ChromeDensity::Comfortable
    );
}

#[gpui::test]
fn an_unreadable_document_refuses_edits_until_it_is_reloaded(cx: &mut TestAppContext) {
    let storage = Arc::new(MemoryStorage::default());
    storage.corrupt();
    let (window, harness, cx) = open_settings_with(cx, storage);

    assert!(matches!(status(&window, cx), SaveStatus::Unavailable(_)));
    assert!(!window.read_with(cx, |window, _| window.editor.editable()));
    assert!(cx.debug_bounds("settings-banner").is_some());

    // Controls are disabled, so nothing reaches the document.
    click("settings-density-comfortable", cx);
    assert_eq!(
        document_of(&window, cx).preferences.window.density,
        ChromeDensity::Compact
    );
    assert_eq!(harness.storage.writes(), 0);

    harness.storage.repair();
    click("settings-banner-reload", cx);
    cx.run_until_parked();

    assert_eq!(status(&window, cx), SaveStatus::Saved);
    assert!(window.read_with(cx, |window, _| window.editor.editable()));
}

#[gpui::test]
fn malformed_settings_reset_keeps_a_backup_and_resumes_editing(cx: &mut TestAppContext) {
    let storage = Arc::new(MemoryStorage::default());
    storage.corrupt();
    let (window, harness, cx) = open_settings_with(cx, storage);
    assert!(cx.debug_bounds("settings-banner-reset-settings").is_some());

    click("settings-banner-reset-settings", cx);
    click("modal-action-settings-recovery-confirm-cancel", cx);
    cx.run_until_parked();
    assert_eq!(harness.storage.backup(), None);
    assert!(!window.read_with(cx, |window, _| window.editor.editable()));

    click("settings-banner-reset-settings", cx);
    click("modal-action-settings-recovery-confirm-reset", cx);
    cx.run_until_parked();

    assert_eq!(
        harness.storage.backup().as_deref(),
        Some(super::test_support::CORRUPT_DOCUMENT)
    );
    assert!(
        harness.storage.document().is_some(),
        "the reset writes a readable default document"
    );
    assert_eq!(status(&window, cx), SaveStatus::Saved);
    assert!(cx.debug_bounds("settings-banner").is_none());
    click("settings-density-comfortable", cx);
    assert_eq!(
        document_of(&window, cx).preferences.window.density,
        ChromeDensity::Comfortable
    );
}

#[gpui::test]
fn only_malformed_settings_offer_a_reset(cx: &mut TestAppContext) {
    let storage = Arc::new(MemoryStorage::default());
    storage.fail_reads(Some(StorageError::Unsafe));
    let (window, _harness, cx) = open_settings_with(cx, storage);

    assert!(matches!(status(&window, cx), SaveStatus::Unavailable(_)));
    assert!(cx.debug_bounds("settings-banner-reload").is_some());
    assert!(cx.debug_bounds("settings-banner-reset-settings").is_none());
}

#[gpui::test]
fn a_document_published_without_identity_pauses_editing_until_reload(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    harness.storage.drop_identity(true);

    click("settings-density-comfortable", cx);
    settle(cx);

    assert!(matches!(
        status(&window, cx),
        SaveStatus::Unavailable(crate::settings::SettingsError::Storage(
            StorageError::Conflict
        ))
    ));
    assert!(!window.read_with(cx, |window, _| window.editor.editable()));
}

#[gpui::test]
fn closing_the_window_writes_a_change_that_has_not_settled(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    click("settings-density-comfortable", cx);
    assert_eq!(harness.storage.writes(), 0);

    cx.update(|_, cx| {
        window.update(cx, |window, cx| window.editor.flush(cx));
    });
    cx.run_until_parked();

    assert_eq!(harness.storage.writes(), 1);
    assert_eq!(
        harness
            .storage
            .document()
            .expect("the retained document should parse")
            .preferences
            .window
            .density,
        ChromeDensity::Comfortable
    );
}

// Appearance Mode ----------------------------------------------------------------------------

#[gpui::test]
fn appearance_mode_switches_both_surfaces_without_changing_any_theme_slot(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Themes, cx);
    let before = document_of(&window, cx).preferences;
    for (selector, mode) in [
        ("settings-appearance-mode-light", AppearanceMode::Light),
        ("settings-appearance-mode-auto", AppearanceMode::Auto),
        ("settings-appearance-mode-dark", AppearanceMode::Dark),
    ] {
        click(selector, cx);
        let after = document_of(&window, cx).preferences;
        assert_eq!(after.mode, mode);
        assert_eq!(after.window, before.window);
        assert_eq!(after.terminal, before.terminal);
    }
}

/// The page always shows the theme in use and the gallery. Only Auto shows a card per slot.
#[gpui::test]
fn every_mode_shows_the_theme_in_use_and_auto_shows_both_slots(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Themes, cx);
    for (mode, automatic) in [
        (AppearanceMode::Light, false),
        (AppearanceMode::Auto, true),
        (AppearanceMode::Dark, false),
    ] {
        cx.update(|_, cx| window.update(cx, |settings, cx| settings.set_appearance_mode(mode, cx)));
        cx.run_until_parked();
        let rows = window.read_with(cx, |settings, _| {
            settings.rows_for(SettingsSectionId::Themes)
        });
        assert_eq!(
            rows,
            vec![
                SettingsRowId::AppearanceMode,
                SettingsRowId::TerminalTheme,
                SettingsRowId::InstalledThemes
            ]
        );
        for slot in ["settings-theme-slot-light", "settings-theme-slot-dark"] {
            assert_eq!(cx.debug_bounds(slot).is_some(), automatic, "{mode:?} {slot}");
        }
        assert_eq!(
            cx.debug_bounds("settings-current-theme-name").is_some(),
            !automatic
        );
    }
}

#[gpui::test]
fn changing_one_theme_slot_preserves_the_mode_and_other_slot(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    for mode in [
        AppearanceMode::Light,
        AppearanceMode::Dark,
        AppearanceMode::Auto,
    ] {
        cx.update(|_, cx| window.update(cx, |settings, cx| settings.set_appearance_mode(mode, cx)));
        {
            for slot in [Appearance::Light, Appearance::Dark] {
                let before = document_of(&window, cx).preferences;
                let id = crate::appearance::ThemeId::new(
                    format!("custom.{slot:?}").to_ascii_lowercase(),
                )
                .unwrap();
                let mut expected = before.clone();
                expected.terminal.themes.set(slot, id.clone());
                cx.update(|_, cx| {
                    window.update(cx, |settings, cx| settings.set_theme(slot, id, cx))
                });
                assert_eq!(document_of(&window, cx).preferences, expected);
            }
        }
    }
}

#[gpui::test]
fn resetting_appearance_mode_preserves_all_theme_choices(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Themes, cx);
    click("settings-appearance-mode-light", cx);
    let mut expected = document_of(&window, cx).preferences;
    expected.mode = AppearanceMode::default();
    click("settings-row-appearance-mode-reset", cx);
    assert_eq!(document_of(&window, cx).preferences, expected);
}

/// A tile applies its theme to the slot the gallery shows: the fixed mode's slot, or the slot
/// selected under Auto. The other slot and the mode stay as they were.
#[gpui::test]
fn a_gallery_tile_applies_its_theme_to_the_displayed_slot(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Themes, cx);
    for (mode, slot) in [
        (AppearanceMode::Light, Appearance::Light),
        (AppearanceMode::Dark, Appearance::Dark),
        (AppearanceMode::Auto, Appearance::Light),
        (AppearanceMode::Auto, Appearance::Dark),
    ] {
        cx.update(|_, cx| {
            window.update(cx, |settings, cx| {
                settings.set_appearance_mode(mode, cx);
                settings.set_theme(
                    slot,
                    crate::appearance::ThemeId::new("user.unavailable").unwrap(),
                    cx,
                );
            })
        });
        cx.run_until_parked();
        if mode == AppearanceMode::Auto {
            click(
                leaked_owned(format!("settings-theme-slot-{slot:?}").to_ascii_lowercase()),
                cx,
            );
        }
        assert_eq!(
            window.read_with(cx, |settings, cx| settings.theme_slot(cx)),
            slot
        );
        let before = document_of(&window, cx).preferences;
        let chosen = builtin_fallback_theme(slot);
        click(
            leaked_owned(format!("settings-theme-tile-{}", chosen.as_str())),
            cx,
        );
        let mut expected = before;
        expected.terminal.themes.set(slot, chosen);
        assert_eq!(document_of(&window, cx).preferences, expected);
    }
}

/// The same selection and removal operations remain reachable without a pointer.
#[gpui::test]
fn keyboard_navigation_selects_slots_applies_themes_and_opens_removal(cx: &mut TestAppContext) {
    let family = br##"{"themes":[{"name":"Sample Dark","appearance":"dark","style":{}}]}"##;
    let mut document = SettingsDocument {
        terminal_themes: crate::appearance::translate_zed_family(family).unwrap(),
        ..Default::default()
    };
    document.preferences.mode = AppearanceMode::Auto;
    let imported = document.terminal_themes[0].id.clone();
    let (settings, _, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    set_query(&settings, "theme", cx);
    cx.update(|window, cx| window.focus(&settings.read(cx).focus_handle.clone(), cx));

    // Search, section navigation, then the Light slot.
    cx.simulate_keystrokes("tab tab tab enter");
    cx.run_until_parked();
    assert_eq!(
        settings.read_with(cx, |settings, cx| settings.theme_slot(cx)),
        Appearance::Light
    );
    cx.simulate_keystrokes("tab space");
    cx.run_until_parked();
    assert_eq!(
        settings.read_with(cx, |settings, cx| settings.theme_slot(cx)),
        Appearance::Dark
    );

    // The built-in theme leads the gallery, followed by the imported theme.
    cx.simulate_keystrokes("tab tab enter");
    cx.run_until_parked();
    assert_eq!(
        document_of(&settings, cx).preferences.terminal.themes.dark,
        imported
    );
    assert_eq!(
        document_of(&settings, cx).preferences.mode,
        AppearanceMode::Auto
    );

    cx.simulate_keystrokes("shift-tab space");
    cx.run_until_parked();
    assert_eq!(
        document_of(&settings, cx).preferences.terminal.themes.dark,
        builtin_fallback_theme(Appearance::Dark)
    );
    cx.simulate_keystrokes("tab enter");
    cx.run_until_parked();
    assert_eq!(
        document_of(&settings, cx).preferences.terminal.themes.dark,
        imported
    );

    cx.simulate_keystrokes("shift-f10 down enter");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("modal-action-settings-remove-theme-confirm")
            .is_some()
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(installed_count(&settings, cx), 1);
}

/// Removing the theme in use asks first, then returns its slot to the built-in theme.
#[gpui::test]
fn removing_the_theme_in_use_returns_its_slot_to_the_built_in_theme(cx: &mut TestAppContext) {
    let mut document = SettingsDocument {
        terminal_themes: crate::appearance::translate_zed_family(IMPORTABLE_FAMILY)
            .expect("fixture Zed family"),
        ..Default::default()
    };
    let imported = document
        .terminal_themes
        .iter()
        .find(|theme| theme.appearance == Appearance::Light)
        .expect("fixture light theme")
        .id
        .clone();
    document.preferences.mode = AppearanceMode::Light;
    document.preferences.terminal.themes.light = imported.clone();
    let (window, _harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    select_section(SettingsSectionId::Themes, cx);
    let tile = leaked_owned(format!("settings-theme-tile-{}", imported.as_str()));

    right_click(tile, cx);
    click(
        leaked_owned(format!("settings-theme-tile-{}-remove", imported.as_str())),
        cx,
    );
    assert_eq!(
        document_of(&window, cx).preferences.terminal.themes.light,
        imported,
        "removal waits for confirmation"
    );
    click("modal-action-settings-remove-theme-confirm", cx);

    let after = document_of(&window, cx);
    assert!(after.terminal_themes.iter().all(|theme| theme.id != imported));
    assert_eq!(
        after.preferences.terminal.themes.light,
        SettingsDocument::default().preferences.terminal.themes.light
    );
    assert!(cx.debug_bounds(tile).is_none());
}

/// Built-in themes offer only to be used, since there is nothing to remove.
#[gpui::test]
fn a_built_in_tile_offers_no_removal(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    // Choosing another theme leaves the built-in one something to use.
    window.update(cx, |settings, cx| {
        settings.set_appearance_mode(AppearanceMode::Dark, cx);
        settings.set_theme(
            Appearance::Dark,
            crate::appearance::ThemeId::new("user.unavailable").unwrap(),
            cx,
        );
    });
    select_section(SettingsSectionId::Themes, cx);
    let builtin = builtin_fallback_theme(Appearance::Dark);
    right_click(
        leaked_owned(format!("settings-theme-tile-{}", builtin.as_str())),
        cx,
    );

    assert!(
        cx.debug_bounds(leaked_owned(format!(
            "settings-theme-tile-{}-use",
            builtin.as_str()
        )))
        .is_some()
    );
    assert!(
        cx.debug_bounds(leaked_owned(format!(
            "settings-theme-tile-{}-remove",
            builtin.as_str()
        )))
        .is_none()
    );
}

// Reset --------------------------------------------------------------------------------------

#[gpui::test]
fn a_row_reset_appears_only_once_the_row_differs_and_restores_the_default(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    assert!(!window.read_with(cx, |window, cx| {
        window.differs_from_default(SettingsRowId::Density, cx)
    }));

    click("settings-density-comfortable", cx);

    assert!(window.read_with(cx, |window, cx| {
        window.differs_from_default(SettingsRowId::Density, cx)
    }));

    click("settings-row-density-reset", cx);

    assert_eq!(
        document_of(&window, cx).preferences.window.density,
        ChromeDensity::Compact
    );
    assert!(!window.read_with(cx, |window, cx| {
        window.differs_from_default(SettingsRowId::Density, cx)
    }));
}

/// A reset follows the name it restores, and the control holds the row's right edge either way.
///
/// Most rows never carry a reset. A column held open for one would stop every control short of
/// the edge and leave more space on the right of the form than on its left, and a reset that took
/// its place only when present would move the control out from under the pointer that changed it.
#[gpui::test]
fn a_row_reset_follows_its_label_and_leaves_the_control_on_the_row_edge(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Font, cx);

    let bounds = |selector: &'static str, cx: &mut VisualTestContext| {
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} should render"))
    };
    let row = bounds("settings-row-terminal-italic", cx);
    let label = bounds("settings-row-terminal-italic-label", cx);
    let control = bounds("settings-terminal-italic", cx);
    assert!(
        cx.debug_bounds("settings-row-terminal-italic-reset")
            .is_none()
    );
    assert_eq!(
        row.right() - control.right(),
        label.left() - row.left(),
        "a control should keep the same inset from the right as its label keeps from the left"
    );

    click("settings-terminal-italic", cx);

    let reset = bounds("settings-row-terminal-italic-reset", cx);
    assert_eq!(
        bounds("settings-terminal-italic", cx),
        control,
        "a control should not move when its reset appears"
    );
    assert_eq!(
        bounds("settings-row-terminal-italic", cx).size.height,
        row.size.height,
        "a row should keep its height when its reset appears"
    );
    let label = bounds("settings-row-terminal-italic-label", cx);
    assert!(
        reset.left() >= label.right() && reset.left() - label.right() < px(12.0),
        "a reset should follow its label closely, got {reset:?} after {label:?}"
    );
    assert!(
        reset.right() < control.left(),
        "a reset should stay clear of the control it restores"
    );
    let label_middle = label.top() + label.size.height / 2.0;
    let reset_middle = reset.top() + reset.size.height / 2.0;
    assert!(
        (label_middle - reset_middle).abs() < px(1.0),
        "a reset should sit on its label's line, got {reset:?} beside {label:?}"
    );

    click("settings-row-terminal-italic-reset", cx);

    // A test frame keeps every selector it has ever drawn, so the reset's absence is read from the
    // row's state rather than from its bounds.
    assert!(!window.read_with(cx, |window, cx| {
        window.differs_from_default(SettingsRowId::TerminalItalic, cx)
    }));
    assert_eq!(bounds("settings-terminal-italic", cx), control);
}

/// A reset that appears beside a label never changes where that label wraps.
///
/// A long label at a large size sits at its wrapping threshold across a band of window widths. If
/// the reset took its space only when present, the label would gain a line inside that band as
/// soon as the setting changed, growing the row and moving the control centered beside it. The
/// reset scales with the type, so its held slot has to scale with it too, or the visible button
/// would spill out of the slot and over the gap beside the label.
#[gpui::test]
fn a_row_reset_leaves_a_wrapping_label_and_its_control_in_place(cx: &mut TestAppContext) {
    const ROW: &str = "settings-row-terminal-bold-as-bright";
    const LABEL: &str = "settings-row-terminal-bold-as-bright-label";
    const CONTROL: &str = "settings-terminal-bold-as-bright";
    const RESET: &str = "settings-row-terminal-bold-as-bright-reset";
    const RESET_SLOT: &str = "settings-row-terminal-bold-as-bright-reset-slot";

    let mut document = SettingsDocument::default();
    // The fixed Chrome type with the roomiest density, where the reset is furthest from its
    // default size.

    document.preferences.window.density = ChromeDensity::Comfortable;
    let (window, _harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    select_section(SettingsSectionId::Font, cx);

    // A tall window keeps the row on screen at every width, so only the width decides wrapping.
    let resize = |width: f32, cx: &mut VisualTestContext| {
        cx.simulate_resize(gpui::size(px(width), px(2400.0)));
        cx.run_until_parked();
    };
    let geometry = |cx: &mut VisualTestContext| {
        let mut bounds = |selector: &'static str| {
            cx.debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} should render"))
        };
        (bounds(ROW), bounds(LABEL), bounds(CONTROL))
    };

    // Find where the label first wraps, then look across a band wider than a reset on either side.
    let mut widths = (560..=1400).step_by(8).map(|width| width as f32);
    let single_line = {
        resize(1400.0, cx);
        geometry(cx).1.size.height
    };
    let threshold = widths
        .find(|&width| {
            resize(width, cx);
            geometry(cx).1.size.height < single_line * 1.5
        })
        .expect("the label should fit on one line in a wide window");
    let band: Vec<f32> = (-40..=40)
        .step_by(4)
        .map(|offset| threshold + offset as f32)
        .collect();
    let measure = |cx: &mut VisualTestContext| {
        band.iter()
            .map(|&width| {
                resize(width, cx);
                geometry(cx)
            })
            .collect::<Vec<_>>()
    };

    let without_reset = measure(cx);
    assert!(
        without_reset
            .iter()
            .any(|(_, label, _)| label.size.height > single_line * 1.5)
            && without_reset
                .iter()
                .any(|(_, label, _)| label.size.height < single_line * 1.5),
        "the band should cross the label's wrapping threshold"
    );

    resize(1400.0, cx);
    click(CONTROL, cx);
    assert!(window.read_with(cx, |window, cx| {
        window.differs_from_default(SettingsRowId::TerminalBoldAsBright, cx)
    }));
    let with_reset = measure(cx);

    for ((width, before), after) in band.iter().zip(&without_reset).zip(&with_reset) {
        assert_eq!(
            before, after,
            "at width {width}, the row, its label, and its control should not move when a reset \
             appears"
        );
    }
    let (row, label, control) = with_reset[0];
    assert_eq!(
        row.right() - control.right(),
        label.left() - row.left(),
        "a control should keep the same inset from the right as its label keeps from the left"
    );

    for &width in &band {
        resize(width, cx);
        let mut bounds = |selector: &'static str| {
            cx.debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} should render"))
        };
        let (label, slot, reset, control) = (
            bounds(LABEL),
            bounds(RESET_SLOT),
            bounds(RESET),
            bounds(CONTROL),
        );
        assert!(
            reset.size.width > px(20.0),
            "at width {width}, the reset should have scaled past its default size, got {reset:?}"
        );
        assert!(
            slot.left() <= reset.left() && reset.right() <= slot.right(),
            "at width {width}, the reset should stay within its held slot, got {reset:?} in \
             {slot:?}"
        );
        assert!(
            slot.left() > label.right() && reset.left() > label.right(),
            "at width {width}, the reset should keep its gap after the label, got {reset:?} after \
             {label:?}"
        );
        assert!(
            reset.right() < control.left(),
            "at width {width}, the reset should stay clear of the control it restores"
        );
    }
}

/// Reset All says "all", so the imported catalog goes back to empty alongside the preferences.
/// The two reset together: a selection naming an imported theme is valid only while that theme
/// is installed, so clearing one without the other would leave the document contradicting itself.
#[gpui::test]
fn resetting_everything_restores_defaults_and_empties_the_installed_catalog(
    cx: &mut TestAppContext,
) {
    let mut document = SettingsDocument::default();
    document.preferences.window.density = ChromeDensity::Comfortable;
    document.terminal_themes = crate::appearance::translate_zed_family(IMPORTABLE_FAMILY)
        .expect("fixture Zed family");
    let (window, harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    assert!(
        installed_count(&window, cx) > 0,
        "the fixture should install one theme"
    );

    select_section(SettingsSectionId::Advanced, cx);
    click("settings-reset-all", cx);
    click("modal-action-settings-reset-all-confirm", cx);
    settle(cx);

    let after = document_of(&window, cx);
    assert_eq!(after.preferences.window.density, ChromeDensity::Compact);
    assert!(
        after.terminal_themes.is_empty(),
        "Reset All should empty the imported catalog, got {} themes",
        after.terminal_themes.len()
    );
    let retained = harness
        .storage
        .document()
        .expect("the retained document should parse");
    assert!(
        retained.terminal_themes.is_empty(),
        "the emptied catalog should reach storage"
    );
}

/// A theme the preferences select cannot survive the catalog that defines it, so the reset must
/// return the selection to a built-in theme in the same edit the catalog is emptied by.
#[gpui::test]
fn resetting_everything_releases_a_selected_imported_theme(cx: &mut TestAppContext) {
    let mut document = SettingsDocument {
        terminal_themes: crate::appearance::translate_zed_family(IMPORTABLE_FAMILY)
            .expect("fixture Zed family"),
        ..Default::default()
    };
    let imported = document.terminal_themes[0].id.clone();
    document.preferences.terminal.themes.light = imported.clone();
    let (window, _harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    assert_eq!(
        document_of(&window, cx).preferences.terminal.themes.light,
        imported,
        "the fixture should select the imported theme"
    );

    select_section(SettingsSectionId::Advanced, cx);
    click("settings-reset-all", cx);
    click("modal-action-settings-reset-all-confirm", cx);
    settle(cx);

    let after = document_of(&window, cx);
    assert_eq!(
        after.preferences.terminal.themes.light,
        SettingsDocument::default()
            .preferences
            .terminal
            .themes
            .light
    );
    assert!(after.terminal_themes.is_empty());
    after
        .validate()
        .expect("the reset document should stay valid");
}

// Search and navigation ----------------------------------------------------------------------

#[gpui::test]
fn search_narrows_the_detail_pane_to_matching_rows(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);

    set_query(&window, "line height", cx);

    let rows = window.read_with(cx, |window, _| window.rows_for(SettingsSectionId::Font));
    assert_eq!(rows, vec![SettingsRowId::TerminalLineHeight]);
    assert!(
        window
            .read_with(cx, |window, _| window
                .rows_for(SettingsSectionId::Interface))
            .is_empty()
    );
    assert!(
        cx.debug_bounds("settings-row-terminal-line-height")
            .is_some()
    );
}

#[gpui::test]
fn search_reveals_the_first_match(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);

    set_query(&window, "leading", cx);

    assert_eq!(
        window.read_with(cx, |window, _| window.revealed),
        Some(SettingsRowId::TerminalLineHeight)
    );
}

#[gpui::test]
fn search_highlight_keeps_the_setting_label_geometry_stable(cx: &mut TestAppContext) {
    use std::cell::RefCell;

    use gpui::{DivInspectorState, IntoElement as _, ParentElement as _, div};

    let (window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Themes, cx);
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
    let geometry = |cx: &mut VisualTestContext| {
        let row = cx
            .debug_bounds("settings-row-appearance-mode")
            .expect("the appearance mode row should render");
        let label = cx
            .debug_bounds("settings-row-appearance-mode-label")
            .expect("the appearance mode label should render");
        observed.borrow_mut().clear();
        cx.simulate_mouse_move(point(px(0.0), px(0.0)), None, Modifiers::none());
        cx.run_until_parked();
        observed.borrow_mut().clear();
        cx.simulate_mouse_move(label.center(), None, Modifiers::none());
        cx.run_until_parked();
        let mut text_style = observed
            .borrow()
            .iter()
            .rev()
            .find_map(|state| (state.bounds == label).then(|| state.base_style.text.clone()))
            .expect("the inspector should expose the rendered label text style");
        text_style.color = None;
        text_style.background_color = None;
        (
            label.left() - row.left(),
            label.top() - row.top(),
            label.size,
            text_style,
        )
    };
    let resting = geometry(cx);

    set_query(&window, "automatic", cx);

    assert_eq!(
        geometry(cx),
        resting,
        "search selection may add fill and match color, but must not move or resize its label"
    );
}

#[gpui::test]
fn a_theme_search_reveals_the_theme_in_use_in_every_mode(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    set_query(&window, "theme", cx);
    for mode in [
        AppearanceMode::Light,
        AppearanceMode::Auto,
        AppearanceMode::Dark,
    ] {
        cx.update(|_, cx| window.update(cx, |settings, cx| settings.set_appearance_mode(mode, cx)));

        assert_eq!(
            window.read_with(cx, |window, _| window.revealed),
            Some(SettingsRowId::TerminalTheme)
        );
    }
}

#[gpui::test]
fn a_theme_search_navigates_only_to_sections_it_matches(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);

    set_query(&window, "theme", cx);

    assert!(!window.read_with(cx, |window, _| {
        window
            .navigable_sections()
            .contains(&SettingsSectionId::Interface)
    }));
}

#[gpui::test]
fn an_unmatched_query_reports_that_nothing_matched(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);

    set_query(&window, "kubernetes", cx);

    assert!(cx.debug_bounds("settings-no-results").is_some());
    assert_eq!(window.read_with(cx, |window, _| window.revealed), None);
}

#[gpui::test]
fn clearing_search_restores_every_row(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    set_query(&window, "line height", cx);

    click("settings-search-clear", cx);

    assert!(window.read_with(cx, |window, _| window.query.is_empty()));
    assert!(window.read_with(cx, |window, cx| window.search.read(cx).is_focused()));
    assert!(
        !window
            .read_with(cx, |window, _| window
                .rows_for(SettingsSectionId::Interface))
            .is_empty()
    );
}

#[gpui::test]
fn selecting_a_section_makes_it_active(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);

    click("settings-navigation-settings-section-font", cx);

    assert_eq!(
        window.read_with(cx, |window, _| window.active_section),
        SettingsSectionId::Font
    );
}

#[gpui::test]
fn every_section_presents_its_own_rows_when_it_is_selected(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);

    for section in SettingsSectionId::ALL {
        select_section(section, cx);

        assert!(
            cx.debug_bounds(leaked(section.selector())).is_some(),
            "{section:?} should render"
        );
        for row in window.read_with(cx, |window, _| window.rows_for(section)) {
            assert!(
                cx.debug_bounds(leaked(row.descriptor().selector)).is_some(),
                "{row:?} should render"
            );
        }
    }
}

#[gpui::test]
fn the_detail_pane_presents_only_the_selected_section(cx: &mut TestAppContext) {
    // Sections are separate views rather than one scrolling document, so a section that was never
    // selected has never been rendered.
    let (window, _harness, cx) = open_settings(cx);

    assert_eq!(
        window.read_with(cx, |window, _| window.active_section),
        SettingsSectionId::Interface
    );
    assert!(cx.debug_bounds("settings-section-interface").is_some());
    assert!(cx.debug_bounds("settings-section-font").is_none());
    assert!(cx.debug_bounds("settings-section-themes").is_none());
    assert!(cx.debug_bounds("settings-row-terminal-italic").is_none());
}

#[gpui::test]
fn the_search_shortcut_focuses_the_search_field(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    assert!(!window.read_with(cx, |window, cx| window.search.read(cx).is_focused()));

    cx.simulate_keystrokes("cmd-f");
    cx.run_until_parked();

    assert!(window.read_with(cx, |window, cx| window.search.read(cx).is_focused()));
}

#[gpui::test]
fn escape_blurs_an_empty_search(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    cx.simulate_keystrokes("cmd-f");
    cx.run_until_parked();

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert!(!window.read_with(cx, |window, cx| window.search.read(cx).is_focused()));
}

#[gpui::test]
fn escape_with_an_empty_search_preserves_navigation_focus(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    cx.simulate_keystrokes("cmd-f tab");
    cx.run_until_parked();
    assert!(cx.update(|gpui_window, cx| window.read(cx).navigation_focus.is_focused(gpui_window)));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert!(cx.update(|gpui_window, cx| window.read(cx).navigation_focus.is_focused(gpui_window)));
}

#[gpui::test]
fn clicking_the_settings_titlebar_blurs_search(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    cx.simulate_keystrokes("cmd-f");
    cx.run_until_parked();

    click("settings-detail-drag-region", cx);

    assert!(!window.read_with(cx, |window, cx| window.search.read(cx).is_focused()));
}

#[gpui::test]
fn dragging_the_settings_titlebar_blurs_search_without_interrupting_window_movement(
    cx: &mut TestAppContext,
) {
    let records = Rc::new(RecordingOperatingSystemWindowDragPlatform::default());
    let window_drag: Rc<dyn OperatingSystemWindowDragPlatform> = records.clone();
    let (window, _harness, cx) = open_settings_with_drag(
        cx,
        MemoryStorage::with_document(&SettingsDocument::default()),
        window_drag,
    );
    cx.simulate_keystrokes("cmd-f");
    let drag_target = cx
        .debug_bounds("settings-detail-drag-region-hitbox")
        .expect("Settings heading drag target")
        .center();

    cx.simulate_mouse_down(drag_target, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        point(drag_target.x + px(8.0), drag_target.y),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_mouse_up(drag_target, MouseButton::Left, Modifiers::none());

    assert_eq!(
        (
            window.read_with(cx, |window, cx| window.search.read(cx).is_focused()),
            records.counts(),
        ),
        (false, (1, 1, 1, 0))
    );
}

#[gpui::test]
fn escape_clears_an_active_search(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    set_query(&window, "line", cx);
    assert!(!window.read_with(cx, |window, _| window.query.is_empty()));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert!(window.read_with(cx, |window, _| window.query.is_empty()));
}

#[gpui::test]
fn escape_blurs_an_active_search(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    set_query(&window, "line", cx);
    cx.simulate_keystrokes("cmd-f");
    cx.run_until_parked();

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert!(!window.read_with(cx, |window, cx| window.search.read(cx).is_focused()));
}

#[gpui::test]
fn escape_inside_a_confirmation_dismisses_it_rather_than_clearing_search(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    set_query(&window, "factory", cx);
    click("settings-reset-all", cx);
    assert!(
        cx.update(|gpui_window, cx| spaceterm_ui::window_modal_is_open(gpui_window, cx)),
        "the reset confirmation should be presented"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert!(!cx.update(|gpui_window, cx| spaceterm_ui::window_modal_is_open(gpui_window, cx)));
    assert!(
        !window.read_with(cx, |window, _| window.query.is_empty()),
        "the modal owns Escape while it is open, so the search query survives"
    );
}

/// Reset All is reached by a single click, and what it destroys cannot be given back, so the click may only ever open the confirmation. Nothing is written until the
/// confirmation is answered, which is what makes a mistaken click harmless.
#[gpui::test]
fn pressing_reset_all_only_opens_the_confirmation(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    click("settings-density-comfortable", cx);
    settle(cx);
    let writes = harness.storage.writes();

    select_section(SettingsSectionId::Advanced, cx);
    click("settings-reset-all", cx);
    settle(cx);

    assert!(
        cx.update(|gpui_window, cx| spaceterm_ui::window_modal_is_open(gpui_window, cx)),
        "the reset confirmation should be presented"
    );
    assert_eq!(
        document_of(&window, cx).preferences.window.density,
        ChromeDensity::Comfortable,
        "the unanswered confirmation must not have reset anything"
    );
    assert_eq!(
        harness.storage.writes(),
        writes,
        "the unanswered confirmation must not have written anything"
    );
}

#[gpui::test]
fn cancelling_the_reset_confirmation_changes_nothing(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    click("settings-density-comfortable", cx);
    settle(cx);
    let writes = harness.storage.writes();

    select_section(SettingsSectionId::Advanced, cx);
    click("settings-reset-all", cx);
    click("modal-action-settings-reset-all-cancel", cx);
    settle(cx);

    assert_eq!(
        document_of(&window, cx).preferences.window.density,
        ChromeDensity::Comfortable
    );
    assert_eq!(harness.storage.writes(), writes);
}

#[gpui::test]
fn confirming_the_reset_restores_defaults(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    click("settings-density-comfortable", cx);
    settle(cx);

    select_section(SettingsSectionId::Advanced, cx);
    click("settings-reset-all", cx);
    click("modal-action-settings-reset-all-confirm", cx);
    settle(cx);

    assert_eq!(
        document_of(&window, cx).preferences.window.density,
        ChromeDensity::Compact
    );
    assert_eq!(
        harness
            .storage
            .document()
            .expect("the retained document should parse")
            .preferences
            .window
            .density,
        ChromeDensity::Compact
    );
}

// Content --------------------------------------------------------------------------------------

#[test]
fn the_terminal_font_list_hides_the_private_default_and_keeps_system_choices() {
    use crate::appearance::{AvailableFont, AvailableFonts, FontClass};

    let fonts = AvailableFonts {
        installed: vec![
            AvailableFont {
                family: "SpaceTerm Default".into(),
                class: FontClass::Monospace,
                resolution_identity: "bundled".into(),
            },
            AvailableFont {
                family: "JetBrainsMono Nerd Font".into(),
                class: FontClass::Monospace,
                resolution_identity: "system-installed".into(),
            },
            AvailableFont {
                family: "Menlo".into(),
                class: FontClass::Monospace,
                resolution_identity: "menlo".into(),
            },
            AvailableFont {
                family: "Helvetica Neue".into(),
                class: FontClass::Proportional,
                resolution_identity: "helvetica".into(),
            },
        ],
        ..AvailableFonts::default()
    };

    assert_eq!(
        super::terminal_font_families(&fonts),
        vec![
            String::from("JetBrainsMono Nerd Font"),
            String::from("Menlo")
        ]
    );
}

#[gpui::test]
fn a_stepper_stops_at_the_ends_of_its_validated_range(cx: &mut TestAppContext) {
    let mut document = SettingsDocument::default();
    document.preferences.terminal.typography.base_size = 8.0;
    let (window, _harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    select_section(SettingsSectionId::Font, cx);

    // The decrement is disabled at the bottom of the range, so it cannot request a rejected value.
    click("settings-terminal-base-size-decrease", cx);

    assert_eq!(
        document_of(&window, cx)
            .preferences
            .terminal
            .typography
            .base_size,
        8.0
    );
}

#[gpui::test]
fn line_height_steps_stay_on_the_step_grid(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);

    select_section(SettingsSectionId::Font, cx);
    click("settings-terminal-line-height-increase", cx);

    let height = document_of(&window, cx)
        .preferences
        .terminal
        .typography
        .line_height;
    assert!(
        (height - 1.15).abs() < 0.001,
        "the default 1.111 should snap to 1.15, got {height}"
    );
}

#[gpui::test]
fn settings_backdrop_tracks_blur_and_accessibility_live(cx: &mut TestAppContext) {
    let (_window, harness, cx) = open_settings(cx);
    assert_eq!(harness.platform.backdrop_presence(), vec![false]);

    harness
        .platform
        .set_native_window_transparency_supported(true);
    cx.run_until_parked();
    assert_eq!(harness.platform.backdrop_presence(), vec![false, true]);

    click("settings-blur", cx);
    assert_eq!(
        harness.platform.backdrop_presence(),
        vec![false, true, false]
    );
    click("settings-blur", cx);
    harness.platform.set_reduce_transparency(true);
    cx.run_until_parked();
    assert_eq!(
        harness.platform.backdrop_presence(),
        vec![false, true, false, true, false]
    );
    harness.platform.set_reduce_transparency(false);
    cx.run_until_parked();
    assert_eq!(
        harness.platform.backdrop_presence(),
        vec![false, true, false, true, false, true]
    );
}

#[gpui::test]
fn backdrop_guidance_tracks_capability_recovery_without_losing_retained_choices(
    cx: &mut TestAppContext,
) {
    let (window, harness, cx) = open_settings(cx);
    let guidance = |row, cx: &mut VisualTestContext| {
        window.read_with(cx, |settings, cx| settings.row_description(row, cx))
    };
    assert!(
        guidance(SettingsRowId::Transparency, cx)
            .unwrap()
            .contains("Floating surfaces still use your transparency choice")
    );
    assert!(
        guidance(SettingsRowId::Blur, cx)
            .unwrap()
            .contains("Floating surfaces still use your blur choice")
    );

    // The fallback must not disable editing the choices that will apply when support returns.
    click("settings-transparency-increase", cx);
    click("settings-blur", cx);
    settle(cx);
    let retained = harness.storage.document().unwrap().preferences.window;
    assert_eq!(retained.transparency, 0.4);
    assert!(!retained.blur);

    harness
        .platform
        .set_native_window_transparency_supported(true);
    cx.run_until_parked();
    assert_eq!(document_of(&window, cx).preferences.window, retained);
    assert_eq!(
        guidance(SettingsRowId::Blur, cx),
        Some("Soften the desktop behind the window and content behind floating surfaces.")
    );
    assert!(
        guidance(SettingsRowId::Transparency, cx)
            .unwrap()
            .contains("0 is opaque; 1 is maximum transparency")
    );
    assert_eq!(
        cx.update(|_, cx| appearance_runtime::current(cx).chrome.composition.effective),
        crate::appearance::WindowBackgroundAppearance::Transparent
    );

    harness
        .platform
        .set_native_window_transparency_supported(false);
    cx.run_until_parked();
    assert!(
        guidance(SettingsRowId::Transparency, cx)
            .unwrap()
            .contains("Floating surfaces still use your transparency choice")
    );
    assert_eq!(document_of(&window, cx).preferences.window, retained);
    assert_eq!(
        harness.storage.document().unwrap().preferences.window,
        retained
    );
}

#[gpui::test]
fn backdrop_guidance_only_promises_opacity_for_the_resolved_material_policy(
    cx: &mut TestAppContext,
) {
    let (window, harness, cx) = open_settings(cx);
    let retained = document_of(&window, cx).preferences.window;
    harness
        .platform
        .set_native_window_transparency_supported(true);
    harness.platform.set_reduce_transparency(true);
    cx.run_until_parked();
    let (transparency, blur) = window.read_with(cx, |settings, cx| {
        (
            settings
                .row_description(SettingsRowId::Transparency, cx)
                .unwrap(),
            settings
                .row_description(SettingsRowId::Blur, cx)
                .unwrap(),
        )
    });
    assert!(transparency.contains("window and floating surfaces opaque"));
    assert!(blur.contains("disable window and floating-surface blur"));

    harness.platform.set_reduce_transparency(false);
    harness.platform.set_increase_contrast(true);
    cx.run_until_parked();
    for row in [SettingsRowId::Transparency, SettingsRowId::Blur] {
        let guidance = window
            .read_with(cx, |settings, cx| settings.row_description(row, cx))
            .unwrap();
        assert!(!guidance.contains("currently keep"), "{row:?}: {guidance}");
        assert!(
            !guidance.contains("currently disable"),
            "{row:?}: {guidance}"
        );
    }

    harness.platform.set_increase_contrast(false);
    harness.platform.set_show_borders(true);
    cx.run_until_parked();
    for row in [SettingsRowId::Transparency, SettingsRowId::Blur] {
        let guidance = window
            .read_with(cx, |settings, cx| settings.row_description(row, cx))
            .unwrap();
        assert!(!guidance.contains("currently keep"), "{row:?}: {guidance}");
        assert!(
            !guidance.contains("currently disable"),
            "{row:?}: {guidance}"
        );
    }
    assert_eq!(document_of(&window, cx).preferences.window, retained);
}

#[gpui::test]
fn backdrop_guidance_identifies_zero_transparency_without_claiming_a_system_override(
    cx: &mut TestAppContext,
) {
    let mut document = SettingsDocument::default();
    document.preferences.window.transparency = 0.0;
    let (window, harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    for supported in [false, true] {
        harness
            .platform
            .set_native_window_transparency_supported(supported);
        cx.run_until_parked();
        let (transparency, blur) = window.read_with(cx, |settings, cx| {
            (
                settings
                    .row_description(SettingsRowId::Transparency, cx)
                    .unwrap(),
                settings
                    .row_description(SettingsRowId::Blur, cx)
                    .unwrap(),
            )
        });
        assert!(transparency.contains("opaque at 0"));
        assert!(blur.contains("Increase Transparency above 0"));
        assert!(!transparency.contains("accessibility"));
        assert!(!blur.contains("accessibility"));
        assert_eq!(
            document_of(&window, cx).preferences.window,
            document.preferences.window
        );
    }
}

#[gpui::test]
fn transparency_stepper_persists_bounds_blur_and_reset(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    assert_eq!(
        document_of(&window, cx).preferences.window.transparency,
        0.35
    );
    for _ in 0..8 {
        click("settings-transparency-decrease", cx);
    }
    assert_eq!(
        document_of(&window, cx).preferences.window.transparency,
        0.0
    );
    for _ in 0..21 {
        click("settings-transparency-increase", cx);
    }
    assert_eq!(
        document_of(&window, cx).preferences.window.transparency,
        1.0
    );
    click("settings-blur", cx);
    settle(cx);
    let saved = harness.storage.document().unwrap();
    assert_eq!(saved.preferences.window.transparency, 1.0);
    assert!(!saved.preferences.window.blur);
    window.update(cx, |settings, cx| {
        settings.edit(
            |draft| {
                draft
                    .preferences
                    .reset(crate::appearance::ResetTarget::Transparency)
            },
            cx,
        )
    });
    settle(cx);
    let saved = harness.storage.document().unwrap();
    assert_eq!(saved.preferences.window.transparency, 0.35);
    assert!(!saved.preferences.window.blur);
}

/// Nothing is wrong by default, and a warning that says so is noise rather than information.
#[gpui::test]
fn the_themes_page_warns_only_when_something_could_not_be_resolved(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Themes, cx);

    assert_eq!(cx.debug_bounds("settings-diagnostics-notice"), None);
    assert!(
        cx.debug_bounds("settings-current-theme").is_some(),
        "the page leads with the theme in use"
    );
}

// Layout ---------------------------------------------------------------------------------------

/// Every row belongs to a titled box, and every box frames the rows the catalog put in it.
///
/// Grouping is the structure the page is read by, so a row that escapes its box, or a box drawn
/// for rows that are not inside it, is a layout defect rather than a matter of taste.
#[gpui::test]
fn every_row_sits_inside_the_titled_group_that_names_it(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);

    for section in SettingsSectionId::ALL {
        select_section(section, cx);

        for row in window.read_with(cx, |window, _| window.rows_for(section)) {
            // A section's untitled lead group presents itself, as the theme in use does.
            if row.descriptor().group.is_empty() {
                continue;
            }
            let group = super::group_selector(section, row.descriptor().group);
            let frame = cx
                .debug_bounds(leaked_owned(group.clone()))
                .unwrap_or_else(|| panic!("{group} should render"));
            let bounds = cx
                .debug_bounds(leaked(row.descriptor().selector))
                .unwrap_or_else(|| panic!("{row:?} should render"));

            // Half a pixel of slack, because the group's height and its rows' heights are each
            // rounded from the same scaled spacing and can disagree in the last half pixel.
            let slack = px(0.5);
            assert!(
                bounds.top() >= frame.top() - slack && bounds.bottom() <= frame.bottom() + slack,
                "{row:?} should sit inside {group}, got {bounds:?} in {frame:?}"
            );
            assert!(
                cx.debug_bounds(leaked_owned(format!("{group}-title")))
                    .is_some(),
                "{group} should carry its title"
            );
        }
    }
}

/// Group cards keep one content column and separate runs more than adjacent rows.
#[gpui::test]
fn grouped_cards_preserve_content_alignment_and_spacing(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Font, cx);

    let pane = cx
        .debug_bounds("settings-section-font")
        .expect("the section should render");
    let rows = window.read_with(cx, |window, _| window.rows_for(SettingsSectionId::Font));
    let mut cards: Vec<(String, gpui::Bounds<gpui::Pixels>)> = Vec::new();
    for row in &rows {
        let group = super::group_selector(SettingsSectionId::Font, row.descriptor().group);
        let card = cx
            .debug_bounds(leaked_owned(format!("{group}-card")))
            .unwrap_or_else(|| panic!("{group} should rest on a card"));
        let label = cx
            .debug_bounds(leaked_owned(format!("{}-label", row.descriptor().selector)))
            .unwrap_or_else(|| panic!("{row:?} should carry a label"));

        assert!(
            card.left() < label.left() && card.right() > label.right(),
            "{group} should clear the text it holds, got {card:?} around {label:?}"
        );
        assert!(
            card.left() >= pane.left() && card.right() <= pane.right(),
            "{group} should stay inside the content column, got {card:?} in {pane:?}"
        );
        if cards.last().is_none_or(|(last, _)| *last != group) {
            cards.push((group, card));
        }
    }

    assert!(
        cards.len() > 1,
        "the Terminal section should present more than one card"
    );
    let widest_row_gap = rows
        .windows(2)
        .filter(|pair| pair[0].descriptor().group == pair[1].descriptor().group)
        .map(|pair| {
            let top = cx
                .debug_bounds(leaked(pair[0].descriptor().selector))
                .expect("row bounds");
            let bottom = cx
                .debug_bounds(leaked(pair[1].descriptor().selector))
                .expect("row bounds");
            bottom.top() - top.bottom()
        })
        .fold(px(0.0), gpui::Pixels::max);
    for pair in cards.windows(2) {
        let gap = pair[1].1.top() - pair[0].1.bottom();
        assert!(
            gap > widest_row_gap,
            "cards should separate more than the rows inside one do, got {gap:?} against \
             {widest_row_gap:?}"
        );
        assert_eq!(
            pair[0].1.left(),
            pair[1].1.left(),
            "every card should share one left edge"
        );
    }
}

#[gpui::test]
fn grouped_rows_use_a_leading_inset_hairline_without_an_inter_row_gap(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    let card = cx
        .debug_bounds("settings-section-interface-group-window-card")
        .expect("the Window group must render its card");
    let top = cx
        .debug_bounds("settings-row-transparency")
        .expect("the Transparency row must render");
    let bottom = cx
        .debug_bounds("settings-row-blur")
        .expect("the Blur row must render");
    let separator = cx
        .debug_bounds("settings-section-interface-group-window-card-separator-2")
        .expect("adjacent Window rows must render a separator");

    assert_eq!(bottom.top(), top.bottom());
    assert_eq!(separator.size.height, px(1.0));
    assert_eq!(separator.left() - card.left(), px(12.0));
    assert_eq!(separator.right(), card.right() - px(1.0));
    cx.update(|window, cx| {
        let appearance = crate::ui::appearance::chrome(cx);
        let divider = crate::ui::appearance::gpui_color(
            appearance.separator(spaceterm_ui::ControlHost::Card),
        );
        let scale = window.scale_factor();
        let quads = window.painted_quads();
        assert!(quads.iter().any(|quad| {
            quad.bounds.intersect(&quad.content_mask.bounds) == separator.scale(scale)
                && quad.background == gpui::Background::from(divider)
        }));
    });
}

/// The footer closes the content column alone, so the sidebar runs unbroken to the window's
/// bottom edge and the save status ends on the content gutter.
fn assert_the_footer_closes_only_the_content_column(
    document: SettingsDocument,
    cx: &mut TestAppContext,
) {
    let (_window, _harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    for section in SettingsSectionId::ALL {
        select_section(section, cx);
        let surface = cx.debug_bounds("settings-window-surface").unwrap();
        let sidebar = cx.debug_bounds("settings-sidebar").unwrap();
        let footer = cx.debug_bounds("settings-footer").unwrap();
        let status = cx.debug_bounds("settings-save-status").unwrap();
        let heading = cx
            .debug_bounds(leaked_owned(format!("{}-heading", section.selector())))
            .unwrap();
        assert_eq!(
            sidebar.bottom(),
            surface.bottom(),
            "the sidebar should reach the window's bottom edge in {section:?}"
        );
        assert_eq!(
            (footer.left(), footer.right()),
            (sidebar.right(), surface.right()),
            "the footer should span the content column alone in {section:?}"
        );
        assert_eq!(
            status.right(),
            heading.right(),
            "the save status should end on the content gutter in {section:?}"
        );
    }
    assert!(
        cx.debug_bounds("settings-sidebar-footer-divider").is_none(),
        "no rule should cross the sidebar above the window's bottom edge"
    );
}

#[gpui::test]
fn the_footer_closes_only_the_content_column_on_every_settings_page(cx: &mut TestAppContext) {
    assert_the_footer_closes_only_the_content_column(SettingsDocument::default(), cx);
}

#[gpui::test]
fn the_footer_tracks_the_content_gutter_with_comfortable_density(cx: &mut TestAppContext) {
    let mut document = SettingsDocument::default();

    document.preferences.window.density = ChromeDensity::Comfortable;
    assert_the_footer_closes_only_the_content_column(document, cx);
}

/// The footer is a status line. Actions on Settings as a whole live in the Advanced section.
#[gpui::test]
fn the_footer_carries_only_the_save_status(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    let footer = cx.debug_bounds("settings-footer").unwrap();

    for selector in ["settings-reset-all", "settings-document-export", "settings-import"] {
        assert!(
            cx.debug_bounds(selector)
                .is_none_or(|bounds| !footer.intersects(&bounds)),
            "{selector} should not sit in the footer"
        );
    }
}

fn assert_client_chrome_geometry(document: SettingsDocument, cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    let surface = cx
        .debug_bounds("settings-window-surface")
        .expect("window surface");
    let sidebar = cx.debug_bounds("settings-sidebar").expect("sidebar");
    let sidebar_titlebar = cx
        .debug_bounds("settings-sidebar-titlebar")
        .expect("traffic-light strip");
    let search = cx
        .debug_bounds("settings-search-frame")
        .expect("search field");
    let heading = cx
        .debug_bounds("settings-detail-heading")
        .expect("detail heading");
    let title = cx
        .debug_bounds("settings-section-interface-title")
        .expect("section title");
    let description = cx
        .debug_bounds("settings-section-interface-description")
        .expect("section description");
    let detail = cx.debug_bounds("settings-detail").expect("detail pane");
    let expected_height = cx.update(|_, cx| crate::ui::appearance::chrome(cx).top_height());

    assert_eq!(
        (sidebar.top(), heading.top()),
        (surface.top(), surface.top()),
        "sidebar and content surfaces must both reach the window's top edge"
    );
    assert_eq!(
        (sidebar_titlebar.left(), sidebar_titlebar.right()),
        (sidebar.left(), sidebar.right()),
        "the traffic-light strip must be the sidebar's own material"
    );
    assert!(
        (sidebar_titlebar.size.height - expected_height).abs() <= px(1.0),
        "the traffic-light strip should preserve its scaled height after pixel rounding"
    );
    assert!(
        search.top() >= sidebar_titlebar.bottom(),
        "Search must be the first control below the traffic lights"
    );
    assert_eq!(
        (heading.left(), heading.right()),
        (detail.left(), detail.right())
    );
    assert!(
        title.left() > sidebar.right() && description.top() >= title.bottom(),
        "the title leads the content column with its description directly below"
    );
    assert!(
        detail.top() >= heading.bottom(),
        "settings content follows the fixed heading"
    );
}

#[gpui::test]
fn client_chrome_extends_both_columns_to_the_top_edge(cx: &mut TestAppContext) {
    assert_client_chrome_geometry(SettingsDocument::default(), cx);
}

#[gpui::test]
fn client_chrome_geometry_tracks_comfortable_density(cx: &mut TestAppContext) {
    let mut document = SettingsDocument::default();

    document.preferences.window.density = ChromeDensity::Comfortable;
    assert_client_chrome_geometry(document, cx);
}

#[gpui::test]
fn active_section_owns_the_large_heading_and_its_description(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);

    for section in SettingsSectionId::ALL {
        select_section(section, cx);
        let title_selector = leaked_owned(format!("{}-title", section.selector()));
        let description_selector = leaked_owned(format!("{}-description", section.selector()));
        let title = cx
            .debug_bounds(title_selector)
            .unwrap_or_else(|| panic!("{section:?} must own the visible heading"));
        let description = cx
            .debug_bounds(description_selector)
            .unwrap_or_else(|| panic!("{section:?} must retain its description"));
        assert!(
            description.top() >= title.bottom() && description.left() == title.left(),
            "{section:?} description should sit directly below its title on one edge"
        );
    }
}

#[gpui::test]
fn settings_titlebar_forwards_one_threshold_crossing_to_native_window_movement(
    cx: &mut TestAppContext,
) {
    let records = Rc::new(RecordingOperatingSystemWindowDragPlatform::default());
    let window_drag: Rc<dyn OperatingSystemWindowDragPlatform> = records.clone();
    let (_window, _harness, cx) = open_settings_with_drag(
        cx,
        MemoryStorage::with_document(&SettingsDocument::default()),
        window_drag,
    );
    let drag_target = cx
        .debug_bounds("settings-detail-drag-region-hitbox")
        .expect("Settings heading drag target")
        .center();

    cx.simulate_mouse_down(drag_target, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        point(drag_target.x + px(2.0), drag_target.y),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_mouse_move(
        point(drag_target.x + px(8.0), drag_target.y),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_mouse_move(
        point(drag_target.x + px(16.0), drag_target.y),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_mouse_up(drag_target, MouseButton::Left, Modifiers::none());

    assert_eq!(records.counts(), (1, 1, 1, 0));

    let search = cx
        .debug_bounds("settings-search-frame")
        .expect("search field")
        .center();
    cx.simulate_click(search, Modifiers::none());
    assert_eq!(
        records.counts(),
        (1, 1, 1, 0),
        "Search interaction must remain outside the titlebar drag owner"
    );
}

/// A run's title outranks every label inside it, and shares the labels' left edge.
///
/// The card edge bounds the run, while the title still establishes its name and rank. A title a
/// label outweighs inverts that reading, and it is the kind of inversion that survives review
/// because each piece looks reasonable on its own.
#[gpui::test]
fn a_group_title_outranks_the_labels_it_contains(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);

    for section in SettingsSectionId::ALL {
        select_section(section, cx);

        for row in window.read_with(cx, |window, _| window.rows_for(section)) {
            if row.descriptor().group.is_empty() {
                continue;
            }
            let group = super::group_selector(section, row.descriptor().group);
            let title = cx
                .debug_bounds(leaked_owned(format!("{group}-title")))
                .unwrap_or_else(|| panic!("{group} should carry its title"));
            let Some(label) =
                cx.debug_bounds(leaked_owned(format!("{}-label", row.descriptor().selector)))
            else {
                continue;
            };

            assert!(
                title.size.height > label.size.height,
                "{group} should set its title above the rank of {row:?}, got {title:?} over \
                 {label:?}"
            );
            assert_eq!(
                title.left(),
                label.left(),
                "{group} should start its title on the same edge as {row:?}"
            );
        }
    }
}

/// One alignment rule for the whole form: labels share a left edge, controls share a right edge.
///
/// This is the property that makes a settings page look designed rather than assembled. It is
/// asserted numerically because it is the kind of thing that decays one row at a time. A row's own
/// box reaches past its text on both sides, because the fill Settings Search leaves on a revealed
/// row has to clear the text rather than run against it, so what has to agree is the inset every
/// row keeps, not the box edge itself.
#[gpui::test]
fn every_row_shares_one_left_edge_for_labels_and_one_right_edge_for_controls(
    cx: &mut TestAppContext,
) {
    let (window, _harness, cx) = open_settings(cx);

    for section in SettingsSectionId::ALL {
        select_section(section, cx);

        let mut left: Option<(SettingsRowId, gpui::Pixels)> = None;
        let mut right: Option<(SettingsRowId, gpui::Pixels)> = None;
        let mut inset: Option<(SettingsRowId, gpui::Pixels)> = None;
        for row in window.read_with(cx, |window, _| window.rows_for(section)) {
            let bounds = cx
                .debug_bounds(leaked(row.descriptor().selector))
                .unwrap_or_else(|| panic!("{row:?} should render"));
            if let Some(label) =
                cx.debug_bounds(leaked_owned(format!("{}-label", row.descriptor().selector)))
            {
                if let Some((first, edge)) = left {
                    assert_eq!(
                        label.left(),
                        edge,
                        "{row:?} starts its label at a different edge than {first:?}"
                    );
                } else {
                    left = Some((row, label.left()));
                }
                let own = label.left() - bounds.left();
                assert!(
                    own > gpui::px(0.0),
                    "{row:?} should hold its label clear of the row's own edge, got {own:?}"
                );
                if let Some((first, expected)) = inset {
                    assert_eq!(
                        own, expected,
                        "{row:?} insets its label further than {first:?}"
                    );
                } else {
                    inset = Some((row, own));
                }
            }
            if let Some((first, edge)) = right {
                assert_eq!(
                    bounds.right(),
                    edge,
                    "{row:?} ends at a different edge than {first:?}"
                );
            } else {
                right = Some((row, bounds.right()));
            }
        }
        // The library's rows carry no label of their own: their group title names them. Every
        // section still shares the one right edge, which is what the loop above checked.
        assert!(right.is_some(), "{section:?} should present a row");
    }
}

/// Guidance stays with the setting it explains rather than coming to rest between two rows.
///
/// Under a tall control, a caption on the row's own line ends up nearer the row below it than the
/// one it belongs to, and runs the width of the page on the way. Stacked under its label it stays
/// anchored and it stops where the control begins.
#[gpui::test]
fn guidance_sits_under_its_label_and_stops_before_the_control(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Themes, cx);

    let label = cx
        .debug_bounds("settings-row-appearance-mode-label")
        .expect("the appearance label should render");
    let control = cx
        .debug_bounds("settings-appearance-mode")
        .expect("the appearance control should render");
    let row = cx
        .debug_bounds("settings-row-appearance-mode")
        .expect("the appearance row should render");
    let next = cx
        .debug_bounds("settings-row-terminal-theme")
        .expect("the theme in use should render under the appearance row");

    // The caption is the only thing in this row under the label, so the label column's own extent
    // is what the assertions below measure.
    assert!(
        label.bottom() < control.bottom(),
        "guidance should sit under the label, got {label:?} against {control:?}"
    );
    assert!(
        row.bottom() <= next.top() + px(0.5),
        "guidance should stay inside its own row, got {row:?} against {next:?}"
    );
    assert!(
        control.left() >= label.left(),
        "guidance should stop before the control, got {label:?} against {control:?}"
    );
}

/// Grouping survives without a rule between every pair of rows.
///
/// Nothing is ruled off, so space is the only thing telling a reader where one group ends. Rows
/// inside a group must therefore sit closer together than a group sits to the one after it, or the
/// page becomes one undifferentiated list.
#[gpui::test]
fn a_group_reads_as_a_group_because_its_rows_sit_closer_than_its_neighbours(
    cx: &mut TestAppContext,
) {
    let (window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Font, cx);

    let rows = window.read_with(cx, |window, _| window.rows_for(SettingsSectionId::Font));
    let bounds = rows
        .iter()
        .map(|row| {
            let descriptor = row.descriptor();
            let bounds = cx
                .debug_bounds(leaked(descriptor.selector))
                .unwrap_or_else(|| panic!("{row:?} should render"));
            (descriptor.group, bounds)
        })
        .collect::<Vec<_>>();

    let mut within = Vec::new();
    let mut between = Vec::new();
    for pair in bounds.windows(2) {
        let gap = pair[1].1.top() - pair[0].1.bottom();
        if pair[0].0 == pair[1].0 {
            within.push(gap);
        } else {
            between.push(gap);
        }
    }
    assert!(!within.is_empty() && !between.is_empty());

    let widest_within = within.iter().copied().fold(px(0.0), gpui::Pixels::max);
    let narrowest_between = between
        .iter()
        .copied()
        .fold(px(f32::MAX), gpui::Pixels::min);
    assert!(
        narrowest_between > widest_within,
        "groups should separate more than their own rows do, got {between:?} against {within:?}"
    );
}

/// A selector is bezeled like the steppers and segmented controls beside it.
///
/// A ghost trigger occupies the same width but draws no edge, so it reads as ending short of its
/// neighbours even when the geometry agrees. The page has one control treatment or it has none.
#[gpui::test]
fn a_selector_carries_the_same_bezel_as_the_controls_beside_it(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Font, cx);

    let selector = cx
        .debug_bounds("settings-row-terminal-regular-weight-control")
        .expect("the weight selector should render");
    let stepper = cx
        .debug_bounds("settings-terminal-base-size")
        .expect("the size stepper should render");

    assert_eq!(
        selector.right(),
        stepper.right(),
        "a selector and a stepper should end at one edge"
    );
    assert_eq!(
        selector.size.height, stepper.size.height,
        "a selector and a stepper should share one height"
    );
}

/// A selector shows its value next to its chevron rather than at the far end of an empty bezel.
///
/// A reserving trigger is as wide as its popup whatever it holds, which reads as an empty field
/// with a stranded chevron. Hugging is what makes a column of them read as values.
#[gpui::test]
fn a_selector_takes_only_the_width_its_value_needs(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);

    set_query(&window, "typeface", cx);
    let trigger = cx
        .debug_bounds("settings-terminal-font-family")
        .expect("the terminal font selector should render");
    let value = cx
        .debug_bounds("combo-box-trigger-label")
        .expect("the selector should show its value");

    // Everything the trigger holds beyond its value is its own padding, its chevron, and the gap
    // between them, which together are far narrower than the popup it would otherwise reserve.
    let slack = trigger.size.width - value.size.width;
    assert!(
        slack < px(64.0),
        "the trigger should hug its value, got {trigger:?} around {value:?}"
    );
}

#[gpui::test]
fn a_row_label_is_centered_against_the_control_it_names(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Font, cx);

    // The terminal weight row pairs a short label with the tallest control in the form.
    let label = cx
        .debug_bounds("settings-row-terminal-regular-weight-label")
        .expect("the terminal weight label should render");
    let control = cx
        .debug_bounds("settings-row-terminal-regular-weight-control")
        .expect("the terminal weight control should render");

    assert!(
        (label.center().y - control.center().y).abs() < px(1.0),
        "the label should sit on the control's line, got {label:?} and {control:?}"
    );
}

#[gpui::test]
fn a_control_hugs_its_content_rather_than_filling_the_row(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);

    let row = cx
        .debug_bounds("settings-row-density")
        .expect("the density row should render");
    let control = cx
        .debug_bounds("settings-density")
        .expect("the density control should render");

    assert!(
        control.size.width * 2.0 < row.size.width,
        "two densities should not span the row, got {control:?} in {row:?}"
    );
}

#[gpui::test]
fn a_switch_row_presents_the_switch_without_repeating_the_label(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Font, cx);

    let switch = cx
        .debug_bounds("settings-terminal-italic")
        .expect("the italic switch should render");
    let indicator = cx
        .debug_bounds("settings-terminal-italic-indicator")
        .expect("the switch indicator should render");

    assert_eq!(
        switch.size.width, indicator.size.width,
        "the row label already names the switch, so the switch carries no label of its own"
    );
}

#[gpui::test]
fn every_selector_and_stepper_shares_one_control_height(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);

    // The controls live on three pages, so each page is visited before its own are measured.
    let mut heights = Vec::new();
    for (section, selectors) in [
        (
            SettingsSectionId::Interface,
            ["settings-density"].as_slice(),
        ),
        (
            SettingsSectionId::Font,
            [
                "settings-terminal-font-family",
                "settings-terminal-base-size",
                "settings-row-terminal-regular-weight-control",
            ]
            .as_slice(),
        ),
    ] {
        select_section(section, cx);
        for selector in selectors {
            heights.push(
                cx.debug_bounds(leaked(selector))
                    .unwrap_or_else(|| panic!("{selector} should render"))
                    .size
                    .height,
            );
        }
    }

    assert!(
        heights.windows(2).all(|pair| pair[0] == pair[1]),
        "a selector, a segmented control, and a stepper should share one height, got {heights:?}"
    );
}

#[gpui::test]
fn an_open_selector_matches_its_filter_and_row_heights(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Font, cx);

    click("settings-row-terminal-regular-weight-control", cx);

    let filter = cx
        .debug_bounds("combo-box-input-row")
        .expect("the filter row should render");
    let first = cx
        .debug_bounds("settings-row-terminal-regular-weight-control-300")
        .expect("the first weight should render");
    let second = cx
        .debug_bounds("settings-row-terminal-regular-weight-control-400")
        .expect("the second weight should render");

    assert_eq!(filter.size.height, first.size.height);
    assert_eq!(first.size.height, second.size.height);
}

#[gpui::test]
fn an_open_selector_shows_its_filter_glyph_and_marks_the_current_value(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Font, cx);

    click("settings-row-terminal-regular-weight-control", cx);

    assert!(
        cx.debug_bounds("combo-box-input-leading").is_some(),
        "the filter should carry the same search glyph as the window search field"
    );
    // The default regular weight is the second of the six offered weights.
    assert!(
        cx.debug_bounds("combo-box-row-1-check").is_some(),
        "the current value should be marked"
    );
    assert_eq!(cx.debug_bounds("combo-box-row-0-check"), None);
}

#[gpui::test]
fn pressing_a_segment_keeps_the_control_height(cx: &mut TestAppContext) {
    // GPUI replaces an element's text style when an interaction refinement carries one, so a
    // refinement that forgot the segment's line height would reflow the row under the pointer.
    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Themes, cx);
    let light = cx
        .debug_bounds("settings-appearance-mode-light")
        .expect("the light card should render")
        .center();
    let idle = cx
        .debug_bounds("settings-appearance-mode")
        .expect("the control should render");

    cx.simulate_mouse_move(light, None, Modifiers::none());
    cx.run_until_parked();
    let hovered = cx.debug_bounds("settings-appearance-mode");
    cx.simulate_mouse_down(light, gpui::MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    let pressed = cx.debug_bounds("settings-appearance-mode");
    cx.simulate_mouse_up(light, gpui::MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    assert_eq!(hovered, Some(idle), "hover should not resize the control");
    assert_eq!(pressed, Some(idle), "a press should not resize the control");
}

#[gpui::test]
fn a_card_segment_keeps_space_under_its_label(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Themes, cx);

    let card = cx
        .debug_bounds("settings-appearance-mode-light")
        .expect("the light card should render");
    let label = cx
        .debug_bounds("settings-appearance-mode-light-label")
        .expect("the card label should render");

    assert!(
        card.bottom() - label.bottom() >= px(6.0),
        "the label should not sit on the card edge, got {card:?} and {label:?}"
    );
}

// Helpers --------------------------------------------------------------------------------------

const IMPORTABLE_FAMILY: &[u8] = br##"{"name":"Sample","themes":[{"name":"Sample","appearance":"light","style":{"terminal.foreground":"#112233"}}]}"##;

fn document_of(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> SettingsDocument {
    window.read_with(cx, |window, _| window.editor.document().clone())
}

fn installed_count(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> usize {
    window.read_with(cx, |window, _| {
        window.editor.document().terminal_themes.len()
    })
}

/// `debug_bounds` takes a `'static` selector, and section and row selectors are already static
/// strings behind accessors.
fn leaked(selector: &'static str) -> &'static str {
    selector
}

/// A composed selector with the lifetime `debug_bounds` asks for.
///
/// A test builds a handful of these and then ends, so leaking them costs nothing worth managing.
fn leaked_owned(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn request_window_close(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) {
    cx.update(|native, cx| {
        let intent = super::CloseIntent::Window(native.window_handle());
        window.update(cx, |settings, cx| settings.request_close(intent, cx));
    });
    cx.run_until_parked();
}

#[gpui::test]
fn closing_during_a_write_retains_the_window_until_the_newer_edit_is_saved(
    cx: &mut TestAppContext,
) {
    let (window, harness, cx) = open_settings(cx);
    click("settings-density-comfortable", cx);
    let (job, finished) = cx.update(|_, cx| {
        window.update(cx, |settings, cx| settings.editor.start_deferred_commit(cx))
    });
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.edit(
                |document| document.preferences.terminal.typography.base_size = 21.0,
                cx,
            )
        })
    });
    let handle = cx.update(|native, _| native.window_handle());

    request_window_close(&window, cx);
    request_window_close(&window, cx);
    assert!(cx.cx.update(|cx| cx.windows().contains(&handle)));
    assert!(window.read_with(cx, |settings, _| settings.close_after_save.is_some()));

    finished.try_send(job.run()).unwrap();
    cx.run_until_parked();

    assert_eq!(
        harness
            .storage
            .document()
            .unwrap()
            .preferences
            .terminal
            .typography
            .base_size,
        21.0
    );
    assert_eq!(harness.storage.writes(), 2);
    assert!(!cx.cx.update(|cx| cx.windows().contains(&handle)));
}

#[gpui::test]
fn a_failed_close_retains_the_draft_and_its_recovery_controls(cx: &mut TestAppContext) {
    for failure in [StorageError::Unavailable, StorageError::Conflict] {
        let (window, harness, cx) = open_settings(cx);
        click("settings-density-comfortable", cx);
        harness.storage.fail_writes(Some(failure));
        let handle = cx.update(|native, _| native.window_handle());

        request_window_close(&window, cx);
        request_window_close(&window, cx);

        assert!(cx.cx.update(|cx| cx.windows().contains(&handle)));
        assert_eq!(
            document_of(&window, cx).preferences.window.density,
            ChromeDensity::Comfortable
        );
        assert!(cx.debug_bounds("settings-banner").is_some());
        assert!(window.read_with(cx, |settings, _| settings.close_after_save.is_none()));
        harness.storage.fail_writes(None);
        cx.update(|_, cx| window.update(cx, |settings, cx| settings.editor.reload(cx)));
        request_window_close(&window, cx);
    }
}

#[gpui::test]
fn a_failed_application_quit_save_reports_failure(cx: &mut TestAppContext) {
    let (_, harness, cx) = open_settings(cx);
    click("settings-density-comfortable", cx);
    harness.storage.fail_writes(Some(StorageError::Unavailable));
    let outcome = Rc::new(std::cell::Cell::new(None));
    let recorded_outcome = Rc::clone(&outcome);

    cx.cx.update(|cx| {
        super::quit_when_saved(
            cx,
            crate::app::ApplicationQuitAfterSave::new(move |_, outcome| {
                recorded_outcome.set(Some(outcome));
            }),
        );
    });
    cx.run_until_parked();

    assert_eq!(
        outcome.get(),
        Some(crate::app::ApplicationQuitSaveOutcome::Failed)
    );
}

#[gpui::test]
fn application_quit_waits_for_the_latest_settings_edit(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    click("settings-density-comfortable", cx);
    let (job, finished) = cx.update(|_, cx| {
        window.update(cx, |settings, cx| settings.editor.start_deferred_commit(cx))
    });
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.edit(
                |document| document.preferences.terminal.typography.base_size = 21.0,
                cx,
            )
        })
    });

    let completions = Rc::new(std::cell::Cell::new(0));
    let recorded_completions = Rc::clone(&completions);
    cx.cx.update(|cx| {
        super::quit_when_saved(
            cx,
            crate::app::ApplicationQuitAfterSave::new(move |_, outcome| {
                if matches!(outcome, crate::app::ApplicationQuitSaveOutcome::Saved) {
                    recorded_completions.set(recorded_completions.get() + 1);
                }
            }),
        );
    });
    assert!(window.read_with(cx, |settings, _| matches!(
        settings.close_after_save.as_ref(),
        Some(super::CloseIntent::Application(_))
    )));
    finished.try_send(job.run()).unwrap();
    cx.run_until_parked();

    assert_eq!(
        harness
            .storage
            .document()
            .unwrap()
            .preferences
            .terminal
            .typography
            .base_size,
        21.0
    );
    assert_eq!(harness.storage.writes(), 2);
    assert_eq!(completions.get(), 1);
    assert!(window.read_with(cx, |settings, _| settings.close_after_save.is_none()));
}

#[gpui::test]
fn native_shutdown_saves_an_edit_before_its_debounce_runs(cx: &mut TestAppContext) {
    let (_, harness, cx) = open_settings(cx);
    click("settings-density-comfortable", cx);
    assert_eq!(harness.storage.writes(), 0);

    cx.cx.update(|cx| cx.shutdown());

    assert_eq!(
        harness
            .storage
            .document()
            .unwrap()
            .preferences
            .window
            .density,
        ChromeDensity::Comfortable
    );
    assert_eq!(harness.storage.writes(), 1);
}

#[gpui::test]
fn native_shutdown_drains_background_writes_without_a_foreground_callback(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    click("settings-density-comfortable", cx);
    let (job, finished) = cx.update(|_, cx| {
        window.update(cx, |settings, cx| settings.editor.start_deferred_commit(cx))
    });
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.edit(
                |document| document.preferences.terminal.typography.base_size = 21.0,
                cx,
            )
        })
    });
    finished.try_send(job.run()).unwrap();

    // The storage result is ready, but GPUI has not run its background result publication or
    // foreground callback.
    cx.cx.update(|cx| cx.shutdown());

    assert_eq!(
        harness
            .storage
            .document()
            .unwrap()
            .preferences
            .terminal
            .typography
            .base_size,
        21.0
    );
    assert_eq!(harness.storage.writes(), 2);
}

#[gpui::test]
fn resetting_theme_choices_survives_an_appearance_mode_round_trip(cx: &mut TestAppContext) {
    let mut document = SettingsDocument::default();
    document.preferences.terminal.themes.dark =
        crate::appearance::ThemeId::new("custom.previous.terminal").unwrap();
    document.preferences.terminal.themes.dark =
        crate::appearance::ThemeId::new("custom.previous.terminal").unwrap();
    let (window, _, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.editor.reset(
                crate::appearance::ResetTarget::TerminalTheme(Appearance::Dark),
                cx,
            );
            settings.editor.reset(
                crate::appearance::ResetTarget::TerminalTheme(Appearance::Dark),
                cx,
            );
            // Reset persistent slots, then switch modes before the next render.
            settings.set_appearance_mode(AppearanceMode::Light, cx);
            settings.set_appearance_mode(AppearanceMode::Dark, cx);
        })
    });
    let preferences = document_of(&window, cx).preferences;
    let defaults = SettingsDocument::default().preferences;
    assert_eq!(preferences.terminal.themes, defaults.terminal.themes);
    assert_eq!(preferences.terminal.themes, defaults.terminal.themes);
}

#[gpui::test]
fn shared_mode_and_independent_slots_survive_save_reload_and_restart(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Themes, cx);
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            for slot in [Appearance::Light, Appearance::Dark] {
                settings.set_theme(
                    slot,
                    crate::appearance::ThemeId::new(
                        format!("user.saved.{slot:?}").to_ascii_lowercase(),
                    )
                    .unwrap(),
                    cx,
                );
            }
        })
    });
    cx.run_until_parked();
    click("settings-appearance-mode-light", cx);
    click("settings-appearance-mode-auto", cx);
    let expected = document_of(&window, cx).preferences;
    settle(cx);
    assert_eq!(harness.storage.document().unwrap().preferences, expected);
    cx.update(|_, cx| window.update(cx, |settings, cx| settings.editor.reload(cx)));
    cx.run_until_parked();
    assert_eq!(document_of(&window, cx).preferences, expected);
    let restarted = crate::settings::UserSettings::load(harness.storage);
    assert_eq!(restarted.snapshot().candidate.preferences, expected);
}

#[gpui::test]
fn live_chrome_preview_preserves_settings_search_editor_and_focus(cx: &mut TestAppContext) {
    let (window, _, cx) = open_settings(cx);
    set_query(&window, "terminal", cx);
    cx.update(|native, cx| {
        window.update(cx, |settings, cx| {
            settings.search.read(cx).focus_handle().focus(native, cx);
            settings.set_appearance_mode(AppearanceMode::Light, cx);
        })
    });
    cx.run_until_parked();
    window.read_with(cx, |settings, cx| {
        assert_eq!(settings.search.read(cx).value(), "terminal");
        assert!(settings.search.read(cx).is_focused());
    });
}

/// A described row beside a tall control keeps its label and guidance inside the row at the
/// window's minimum width, rather than rising out of the group that clips it.
#[gpui::test]
fn a_described_row_keeps_its_label_inside_the_row_at_minimum_width(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    cx.simulate_resize(gpui::size(
        px(super::WINDOW_WIDTH),
        px(super::WINDOW_HEIGHT),
    ));
    cx.run_until_parked();
    select_section(SettingsSectionId::Font, cx);
    select_section(SettingsSectionId::Themes, cx);

    let row = cx
        .debug_bounds("settings-row-appearance-mode")
        .expect("appearance mode row");
    let label = cx
        .debug_bounds("settings-row-appearance-mode-label")
        .expect("appearance mode label");
    assert!(
        label.top() >= row.top() && label.bottom() <= row.bottom(),
        "label {label:?} must stay inside its row {row:?}"
    );
}

#[gpui::test]
fn appearance_thumbnails_preview_each_terminal_slot(cx: &mut TestAppContext) {
    use crate::appearance::{Color, TerminalColorOverrides};
    let mut document = SettingsDocument::default();
    let light = document.preferences.terminal.themes.light.clone();
    let dark = document.preferences.terminal.themes.dark.clone();
    for (id, background) in [(light, Color::rgb(0xeeeecc)), (dark, Color::rgb(0x102030))] {
        document.preferences.terminal.overrides.insert(
            id,
            TerminalColorOverrides {
                background: Some(background),
                ..Default::default()
            },
        );
    }
    let (window, _, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    let previews = window.read_with(cx, |settings, _| {
        settings.mode_preview_palettes(AppearanceMode::Auto)
    });
    assert_eq!(
        previews[0].root,
        crate::appearance::builtin_chrome_base(Appearance::Light).background
    );
    assert_eq!(
        previews[1].root,
        crate::appearance::builtin_chrome_base(Appearance::Dark).background
    );
    assert_eq!(previews[0].terminal_background, Color::rgb(0xeeeecc));
    assert_eq!(previews[1].terminal_background, Color::rgb(0x102030));
    for (mode, expected) in [
        (AppearanceMode::Light, &previews[0]),
        (AppearanceMode::Dark, &previews[1]),
    ] {
        assert_eq!(
            window.read_with(cx, |settings, _| settings.mode_preview_palettes(mode)),
            vec![expected.clone()]
        );
    }
}

const REGISTRY_LISTING: &str =
    "https://api.zed.dev/extensions?provides=themes&max_schema_version=1";

fn registry_listing(version: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "data": [
            {
                "id": "sample-themes",
                "name": "Sample Themes",
                "version": version,
                "description": "Two sample themes",
                "authors": ["Ada <ada@example.com>"],
                "download_count": 1240,
                "provides": ["themes"],
            },
            {
                "id": "other-themes",
                "name": "Other Themes",
                "version": "1.0.0",
                "authors": [],
                "download_count": 10,
                "provides": ["themes"],
            },
        ]
    }))
    .unwrap()
}

fn registry_archive() -> Vec<u8> {
    extension_archive(&[(
        "sample.json",
        br##"{"name":"Sample","themes":[{"name":"Sample Dark","appearance":"dark","style":{"terminal.background":"#101010"}},{"name":"Sample Light","appearance":"light","style":{}}]}"##,
    )])
}

fn store_listing(
    window: &Entity<SettingsWindow>,
    cx: &mut VisualTestContext,
) -> super::theme_store::Listing {
    window.read_with(cx, |window, cx| {
        window.theme_store.read(cx).listing().clone()
    })
}

fn store_status(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> Option<String> {
    window.read_with(cx, |window, cx| {
        window.theme_store.read(cx).status().map(str::to_owned)
    })
}

fn open_theme_store(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) {
    cx.update(|gpui_window, cx| {
        window.update(cx, |settings, cx| {
            settings.open_theme_store(gpui_window, cx)
        });
    });
    cx.run_until_parked();
}

fn sample_registry() -> Arc<MemoryTransport> {
    Arc::new(
        MemoryTransport::default()
            .route(REGISTRY_LISTING, Ok(registry_listing("1.0.0")))
            .route(
                "https://api.zed.dev/extensions/sample-themes/1.0.0/download",
                Ok(registry_archive()),
            ),
    )
}

/// Opening Get More Themes is what contacts the registry, once; getting an extension adds its
/// themes without selecting any, and its row then reads Installed.
#[gpui::test]
fn get_more_themes_lists_the_registry_and_installs_without_selection(cx: &mut TestAppContext) {
    let transport = sample_registry();
    let (window, _harness, cx) = open_settings_with_registry(cx, transport.clone());
    select_section(SettingsSectionId::Themes, cx);
    assert!(transport.requests().is_empty());

    open_theme_store(&window, cx);
    assert!(matches!(
        store_listing(&window, cx),
        super::theme_store::Listing::Loaded(extensions) if extensions.len() == 2
    ));
    let preferences = document_of(&window, cx).preferences;

    click("settings-zed-extension-action-sample-themes", cx);

    let document = document_of(&window, cx);
    let mut names = document
        .terminal_themes
        .iter()
        .map(|theme| theme.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(names, ["Sample Dark", "Sample Light"]);
    assert_eq!(document.preferences, preferences);
    assert_eq!(
        store_status(&window, cx).as_deref(),
        Some("Installed 2 themes from Sample Themes.")
    );
    assert!(
        cx.debug_bounds("settings-zed-extension-action-sample-themes")
            .is_some(),
        "the listed version can be reinstalled"
    );
    assert!(
        cx.debug_bounds("settings-zed-extension-installed-sample-themes")
            .is_some()
    );

    click("modal-action-settings-theme-store-done", cx);
    open_theme_store(&window, cx);
    assert_eq!(
        transport.requests(),
        [
            REGISTRY_LISTING,
            "https://api.zed.dev/extensions/sample-themes/1.0.0/download"
        ],
        "reopening the sheet reuses the listing"
    );
}

/// A tile's context menu removes every theme its extension installed. A slot that used one of
/// them returns to its built-in theme.
#[gpui::test]
fn removing_an_extension_from_a_tile_removes_all_of_its_themes(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings_with_registry(cx, sample_registry());
    select_section(SettingsSectionId::Themes, cx);
    open_theme_store(&window, cx);
    click("settings-zed-extension-action-sample-themes", cx);
    click("modal-action-settings-theme-store-done", cx);
    let selected = document_of(&window, cx)
        .terminal_themes
        .iter()
        .find(|theme| theme.name == "Sample Dark")
        .map(|theme| theme.id.clone())
        .expect("the dark theme is installed");
    window.update(cx, |settings, cx| {
        settings.set_appearance_mode(AppearanceMode::Dark, cx);
        settings.set_theme(Appearance::Dark, selected.clone(), cx);
    });
    settle(cx);

    let tile = leaked_owned(format!("settings-theme-tile-{}", selected.as_str()));
    right_click(tile, cx);
    click(
        leaked_owned(format!(
            "settings-theme-tile-{}-remove-extension",
            selected.as_str()
        )),
        cx,
    );
    click("modal-action-settings-remove-theme-confirm", cx);

    let document = document_of(&window, cx);
    assert!(document.terminal_themes.is_empty());
    assert_eq!(
        document.preferences.terminal.themes.dark,
        SettingsDocument::default().preferences.terminal.themes.dark
    );
    assert!(cx.debug_bounds(tile).is_none());
}

#[gpui::test]
fn reinstalling_an_extension_restores_a_removed_theme_without_changing_selection(
    cx: &mut TestAppContext,
) {
    let transport = sample_registry();
    let (window, _harness, cx) = open_settings_with_registry(cx, transport.clone());
    select_section(SettingsSectionId::Themes, cx);
    open_theme_store(&window, cx);
    click("settings-zed-extension-action-sample-themes", cx);
    click("modal-action-settings-theme-store-done", cx);
    let themes = document_of(&window, cx).terminal_themes;
    let dark = themes
        .iter()
        .find(|theme| theme.name == "Sample Dark")
        .unwrap()
        .id
        .clone();
    let light = themes
        .iter()
        .find(|theme| theme.name == "Sample Light")
        .unwrap()
        .id
        .clone();
    window.update(cx, |settings, cx| {
        settings.set_theme(Appearance::Dark, dark.clone(), cx);
        settings.set_appearance_mode(AppearanceMode::Light, cx);
    });
    settle(cx);

    right_click(
        leaked_owned(format!("settings-theme-tile-{}", light.as_str())),
        cx,
    );
    click(
        leaked_owned(format!("settings-theme-tile-{}-remove", light.as_str())),
        cx,
    );
    click("modal-action-settings-remove-theme-confirm", cx);
    assert_eq!(installed_count(&window, cx), 1);
    let preferences = document_of(&window, cx).preferences;
    assert_eq!(preferences.terminal.themes.dark, dark);

    open_theme_store(&window, cx);
    click("settings-zed-extension-action-sample-themes", cx);

    let restored = document_of(&window, cx);
    assert_eq!(restored.terminal_themes, themes);
    assert_eq!(restored.preferences, preferences);
    assert_eq!(
        transport.requests(),
        [
            REGISTRY_LISTING,
            "https://api.zed.dev/extensions/sample-themes/1.0.0/download",
            "https://api.zed.dev/extensions/sample-themes/1.0.0/download",
        ]
    );
}

#[gpui::test]
fn a_failed_registry_listing_offers_a_retry(cx: &mut TestAppContext) {
    let transport = Arc::new(MemoryTransport::default());
    let (window, _harness, cx) = open_settings_with_registry(cx, transport.clone());
    select_section(SettingsSectionId::Themes, cx);

    open_theme_store(&window, cx);
    assert!(matches!(
        store_listing(&window, cx),
        super::theme_store::Listing::Failed(crate::theme_registry::RegistryError::Refused)
    ));

    click("settings-theme-store-retry", cx);
    assert_eq!(transport.requests().len(), 2);
}

// Advanced ------------------------------------------------------------------------------------

fn settings_file_text(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> String {
    window.read_with(cx, |window, cx| {
        window.settings_file.area.read(cx).value().to_owned()
    })
}

fn install_settings_file(
    harness: &Harness,
    cx: &mut VisualTestContext,
) -> Rc<crate::platform::settings_file::testing::RecordingSettingsFile> {
    let file = crate::platform::settings_file::testing::RecordingSettingsFile::watchable();
    let access: Rc<dyn crate::platform::settings_file::SettingsFileAccess> = file.clone();
    let settings = harness.settings.clone();
    cx.update(|_, cx| crate::ui::settings_file::SettingsFile::install(settings, access, cx));
    cx.run_until_parked();
    file
}

/// Lets a save made in another program reach the window.
fn follow_outside_save(
    file: &crate::platform::settings_file::testing::RecordingSettingsFile,
    cx: &mut VisualTestContext,
) {
    file.announce_change();
    cx.executor()
        .advance_clock(crate::ui::settings_file::SETTLE_DELAY);
    cx.run_until_parked();
}

#[gpui::test]
fn the_settings_file_shows_the_whole_document_as_stored(cx: &mut TestAppContext) {
    let document = SettingsDocument {
        terminal_themes: crate::appearance::translate_zed_family(IMPORTABLE_FAMILY)
            .expect("fixture Zed family"),
        ..SettingsDocument::default()
    };
    let (window, _harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    select_section(SettingsSectionId::Advanced, cx);
    assert_eq!(
        settings_file_text(&window, cx),
        crate::appearance::export_settings(&document_of(&window, cx)).unwrap()
    );

    cx.update(|_, cx| {
        window.update(cx, |window, cx| {
            window.edit(
                |document| document.preferences.window.density = ChromeDensity::Comfortable,
                cx,
            );
        });
    });
    cx.run_until_parked();

    let text = settings_file_text(&window, cx);
    assert_eq!(
        text,
        crate::appearance::export_settings(&document_of(&window, cx)).unwrap()
    );
    assert!(text.contains("comfortable"), "the text should follow the change");
}

#[gpui::test]
fn the_settings_file_accepts_no_typing(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Advanced, cx);
    let before = settings_file_text(&window, cx);

    click("settings-file-text", cx);
    cx.simulate_input("x");

    assert_eq!(settings_file_text(&window, cx), before);
}

#[gpui::test]
fn edit_json_opens_the_settings_file_and_shows_where_it_lives(cx: &mut TestAppContext) {
    let (_window, harness, cx) = open_settings(cx);
    let file = install_settings_file(&harness, cx);
    select_section(SettingsSectionId::Advanced, cx);
    assert!(cx.debug_bounds("settings-file-location").is_some());
    let writes = harness.storage.writes();

    click("settings-file-edit", cx);

    assert_eq!(file.opened.get(), 1);
    assert_eq!(harness.storage.writes(), writes, "an existing file is opened as it is");
}

#[gpui::test]
fn edit_json_writes_a_missing_settings_file_before_opening_it(cx: &mut TestAppContext) {
    let (_window, harness, cx) = open_settings_with(cx, Arc::new(MemoryStorage::default()));
    let file = install_settings_file(&harness, cx);
    select_section(SettingsSectionId::Advanced, cx);

    click("settings-file-edit", cx);

    assert_eq!(
        harness.storage.document().map(|document| document.preferences),
        Some(SettingsDocument::default().preferences)
    );
    assert_eq!(file.opened.get(), 1);
}

#[gpui::test]
fn edit_json_saves_pending_changes_before_opening_the_file(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    let file = install_settings_file(&harness, cx);
    select_section(SettingsSectionId::Advanced, cx);
    cx.update(|_, cx| {
        window.update(cx, |window, cx| {
            window.edit(
                |document| document.preferences.window.density = ChromeDensity::Comfortable,
                cx,
            );
        });
    });
    cx.run_until_parked();

    click("settings-file-edit", cx);

    assert_eq!(
        harness.storage.document().unwrap().preferences.window.density,
        ChromeDensity::Comfortable,
        "the editor opens what the window shows"
    );
    assert_eq!(file.opened.get(), 1);
}

#[gpui::test]
fn edit_json_is_unavailable_without_a_settings_file(cx: &mut TestAppContext) {
    let (_window, harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Advanced, cx);
    let writes = harness.storage.writes();

    click("settings-file-edit", cx);

    assert_eq!(harness.storage.writes(), writes);
}

#[gpui::test]
fn a_save_in_another_editor_reaches_the_window(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    let file = install_settings_file(&harness, cx);
    select_section(SettingsSectionId::Advanced, cx);
    let mut saved = SettingsDocument::default();
    saved.preferences.window.density = ChromeDensity::Comfortable;

    harness.storage.save_elsewhere(&saved);
    follow_outside_save(&file, cx);

    assert_eq!(
        document_of(&window, cx).preferences.window.density,
        ChromeDensity::Comfortable
    );
    assert!(settings_file_text(&window, cx).contains("comfortable"));
    assert_eq!(status(&window, cx), SaveStatus::Saved);
}

#[gpui::test]
fn a_malformed_save_pauses_editing_until_a_valid_save(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    let file = install_settings_file(&harness, cx);
    let before = document_of(&window, cx);

    harness.storage.save_bytes_elsewhere(b"{ half typed".to_vec());
    follow_outside_save(&file, cx);

    assert_eq!(document_of(&window, cx), before, "the last valid settings stay in effect");
    assert!(matches!(status(&window, cx), SaveStatus::Unavailable(_)));
    assert!(!window.read_with(cx, |window, _| window.editor.editable()));

    let mut fixed = SettingsDocument::default();
    fixed.preferences.window.density = ChromeDensity::Comfortable;
    harness.storage.save_elsewhere(&fixed);
    follow_outside_save(&file, cx);

    assert_eq!(status(&window, cx), SaveStatus::Saved);
    assert_eq!(
        document_of(&window, cx).preferences.window.density,
        ChromeDensity::Comfortable
    );
}

fn exported_document_with_a_theme() -> Vec<u8> {
    let mut document = SettingsDocument::default();
    document.preferences.window.density = ChromeDensity::Comfortable;
    document.terminal_themes = crate::appearance::translate_zed_family(IMPORTABLE_FAMILY)
        .expect("fixture Zed family");
    crate::appearance::export_settings(&document)
        .unwrap()
        .into_bytes()
}

fn finish_import(
    window: &Entity<SettingsWindow>,
    read: Result<Vec<u8>, super::import::ImportError>,
    cx: &mut VisualTestContext,
) {
    cx.update(|native, cx| {
        window.update(cx, |settings, cx| {
            settings.finish_settings_import(read, native, cx);
        });
    });
    cx.run_until_parked();
}

#[gpui::test]
fn importing_settings_replaces_everything_once_confirmed(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Advanced, cx);

    finish_import(&window, Ok(exported_document_with_a_theme()), cx);
    assert!(
        cx.update(|native, cx| spaceterm_ui::window_modal_is_open(native, cx)),
        "the import should ask before replacing anything"
    );
    click("modal-action-settings-import-confirm", cx);
    settle(cx);

    let retained = harness.storage.document().unwrap();
    assert_eq!(retained.preferences.window.density, ChromeDensity::Comfortable);
    assert_eq!(retained.terminal_themes.len(), 1);
}

#[gpui::test]
fn cancelling_an_import_changes_nothing(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Advanced, cx);
    let before = document_of(&window, cx);
    let writes = harness.storage.writes();

    finish_import(&window, Ok(exported_document_with_a_theme()), cx);
    click("modal-action-settings-import-cancel", cx);
    settle(cx);

    assert_eq!(document_of(&window, cx), before);
    assert_eq!(harness.storage.writes(), writes);
}

#[gpui::test]
fn an_unusable_import_file_is_explained_and_changes_nothing(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Advanced, cx);
    let before = document_of(&window, cx);
    let writes = harness.storage.writes();

    for read in [
        Ok(b"{\"preferences\": {}}".to_vec()),
        Ok(IMPORTABLE_FAMILY.to_vec()),
        Err(super::import::ImportError::TooLarge),
        Err(super::import::ImportError::Unreadable),
    ] {
        finish_import(&window, read, cx);
        assert!(cx.debug_bounds("modal-action-settings-import-failed-ok").is_some());
        assert!(cx.debug_bounds("modal-action-settings-import-confirm").is_none());
        click("modal-action-settings-import-failed-ok", cx);
    }
    settle(cx);

    assert_eq!(document_of(&window, cx), before);
    assert_eq!(harness.storage.writes(), writes);
}
