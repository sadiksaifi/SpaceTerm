use std::{rc::Rc, sync::Arc};

use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px};

use crate::appearance::{
    Appearance, AppearanceMode, ChromeDensity, SchemeKind, SettingsDocument,
    builtin_fallback_scheme,
};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::{
    OperatingSystemWindowDragPlatform, RecordingOperatingSystemWindowDragPlatform,
};
use crate::settings::storage::StorageError;
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
    let (settings, changed) = crate::settings::UserSettings::load(storage.clone());
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings.clone(), changed, Rc::new(platform.clone()), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
    });
    let (window, cx) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(window_drag, None, window, cx)
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
        SettingsSectionId::Appearance => "settings-navigation-settings-section-appearance",
        SettingsSectionId::Interface => "settings-navigation-settings-section-interface",
        SettingsSectionId::Terminal => "settings-navigation-settings-section-terminal",
        SettingsSectionId::ColorSchemes => "settings-navigation-settings-section-color-schemes",
        SettingsSectionId::Privacy => "settings-navigation-settings-section-privacy",
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

    click("settings-chrome-density-comfortable", cx);

    // The preview is live before anything is written.
    assert_eq!(harness.storage.writes(), 0);
    assert_eq!(status(&window, cx), SaveStatus::Saving);
    assert_eq!(
        document_of(&window, cx).preferences.chrome.density,
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
            .chrome
            .density,
        ChromeDensity::Comfortable
    );
}

#[gpui::test]
fn several_rapid_changes_produce_one_write_carrying_the_last_value(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Terminal, cx);

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

    click("settings-chrome-density-comfortable", cx);

    // The draft carries the change even though the live preview could not.
    assert_eq!(
        document_of(&window, cx).preferences.chrome.density,
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
            .chrome
            .density,
        ChromeDensity::Comfortable
    );
    assert_eq!(status(&window, cx), SaveStatus::Saved);
}

#[gpui::test]
fn a_failed_write_keeps_the_change_applied_and_retry_writes_it(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    harness.storage.fail_writes(Some(StorageError::Unavailable));

    click("settings-chrome-density-comfortable", cx);
    settle(cx);

    assert!(matches!(status(&window, cx), SaveStatus::Failed(_)));
    // The change is still previewing, so the application still shows it.
    assert_eq!(
        harness
            .settings
            .snapshot()
            .candidate
            .preferences
            .chrome
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
            .chrome
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
    click("settings-chrome-density-comfortable", cx);
    assert_eq!(
        document_of(&window, cx).preferences.chrome.density,
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
fn a_document_published_without_identity_pauses_editing_until_reload(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    harness.storage.drop_identity(true);

    click("settings-chrome-density-comfortable", cx);
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
    click("settings-chrome-density-comfortable", cx);
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
            .chrome
            .density,
        ChromeDensity::Comfortable
    );
}

// Appearance Mode ----------------------------------------------------------------------------

#[gpui::test]
fn appearance_mode_switches_both_surfaces_without_changing_any_scheme_slot(
    cx: &mut TestAppContext,
) {
    let (window, _harness, cx) = open_settings(cx);
    let before = document_of(&window, cx).preferences;
    for (selector, mode) in [
        ("settings-appearance-mode-light", AppearanceMode::Light),
        ("settings-appearance-mode-auto", AppearanceMode::Auto),
        ("settings-appearance-mode-dark", AppearanceMode::Dark),
    ] {
        click(selector, cx);
        let after = document_of(&window, cx).preferences;
        assert_eq!(after.mode, mode);
        assert_eq!(after.chrome, before.chrome);
        assert_eq!(after.terminal, before.terminal);
    }
}

#[gpui::test]
fn fixed_mode_offers_two_scheme_rows_and_auto_offers_four(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    for (selector, automatic) in [
        ("settings-appearance-mode-light", false),
        ("settings-appearance-mode-auto", true),
        ("settings-appearance-mode-dark", false),
    ] {
        click(selector, cx);
        let rows = window.read_with(cx, |settings, _| {
            settings.rows_for(SettingsSectionId::Appearance)
        });
        for row in [SettingsRowId::ChromeScheme, SettingsRowId::TerminalScheme] {
            assert_eq!(rows.contains(&row), !automatic);
        }
        for row in [
            SettingsRowId::ChromeLightScheme,
            SettingsRowId::ChromeDarkScheme,
            SettingsRowId::TerminalLightScheme,
            SettingsRowId::TerminalDarkScheme,
        ] {
            assert_eq!(rows.contains(&row), automatic);
        }
        assert_eq!(
            rows.iter()
                .filter(|row| **row == SettingsRowId::AppearanceMode)
                .count(),
            1
        );
    }
}

#[gpui::test]
fn changing_one_scheme_slot_preserves_the_mode_and_other_three_slots(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    for mode in [
        AppearanceMode::Light,
        AppearanceMode::Dark,
        AppearanceMode::Auto,
    ] {
        cx.update(|_, cx| window.update(cx, |settings, cx| settings.set_appearance_mode(mode, cx)));
        for kind in [SchemeKind::Chrome, SchemeKind::Terminal] {
            for slot in [Appearance::Light, Appearance::Dark] {
                let before = document_of(&window, cx).preferences;
                let id = crate::appearance::SchemeId::new(
                    format!("custom.{kind:?}.{slot:?}").to_ascii_lowercase(),
                )
                .unwrap();
                let mut expected = before.clone();
                match kind {
                    SchemeKind::Chrome => expected.chrome.schemes.set(slot, id.clone()),
                    SchemeKind::Terminal => expected.terminal.schemes.set(slot, id.clone()),
                }
                cx.update(|_, cx| {
                    window.update(cx, |settings, cx| settings.set_scheme(kind, slot, id, cx))
                });
                assert_eq!(document_of(&window, cx).preferences, expected);
            }
        }
    }
}

#[gpui::test]
fn resetting_appearance_mode_preserves_all_scheme_choices(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    click("settings-appearance-mode-light", cx);
    let mut expected = document_of(&window, cx).preferences;
    expected.mode = AppearanceMode::default();
    click("settings-row-appearance-mode-reset", cx);
    assert_eq!(document_of(&window, cx).preferences, expected);
}

/// Exercise the actual pickers and reset buttons, including fixed rows whose slot comes from mode.
#[gpui::test]
fn scheme_pickers_and_resets_edit_only_the_displayed_slot(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    for (mode, kind, slot, row) in [
        (
            AppearanceMode::Light,
            SchemeKind::Chrome,
            Appearance::Light,
            SettingsRowId::ChromeScheme,
        ),
        (
            AppearanceMode::Dark,
            SchemeKind::Terminal,
            Appearance::Dark,
            SettingsRowId::TerminalScheme,
        ),
        (
            AppearanceMode::Auto,
            SchemeKind::Chrome,
            Appearance::Light,
            SettingsRowId::ChromeLightScheme,
        ),
        (
            AppearanceMode::Auto,
            SchemeKind::Chrome,
            Appearance::Dark,
            SettingsRowId::ChromeDarkScheme,
        ),
        (
            AppearanceMode::Auto,
            SchemeKind::Terminal,
            Appearance::Light,
            SettingsRowId::TerminalLightScheme,
        ),
        (
            AppearanceMode::Auto,
            SchemeKind::Terminal,
            Appearance::Dark,
            SettingsRowId::TerminalDarkScheme,
        ),
    ] {
        cx.update(|_, cx| {
            window.update(cx, |settings, cx| {
                settings.set_appearance_mode(mode, cx);
                settings.set_scheme(
                    kind,
                    slot,
                    crate::appearance::SchemeId::new("user.unavailable").unwrap(),
                    cx,
                );
            })
        });
        set_query(&window, row.descriptor().label, cx);
        let before = document_of(&window, cx).preferences;
        let selector = super::control_selector(row);
        click(leaked_owned(selector.clone()), cx);
        let chosen = builtin_fallback_scheme(kind, slot);
        click(leaked_owned(format!("{selector}-{}", chosen.as_str())), cx);
        let mut expected = before;
        match kind {
            SchemeKind::Chrome => expected.chrome.schemes.set(slot, chosen),
            SchemeKind::Terminal => expected.terminal.schemes.set(slot, chosen),
        }
        assert_eq!(document_of(&window, cx).preferences, expected);

        // Use a different unavailable choice so reset exists for both builtin-default and alternate slots.
        cx.update(|_, cx| {
            window.update(cx, |settings, cx| {
                settings.set_scheme(
                    kind,
                    slot,
                    crate::appearance::SchemeId::new("user.reset-me").unwrap(),
                    cx,
                )
            })
        });
        cx.run_until_parked();
        click(
            leaked_owned(format!("{}-reset", row.descriptor().selector)),
            cx,
        );
        expected.reset(row.reset_target(slot).unwrap());
        assert_eq!(document_of(&window, cx).preferences, expected);
    }
}

// Reset --------------------------------------------------------------------------------------

#[gpui::test]
fn a_row_reset_appears_only_once_the_row_differs_and_restores_the_default(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    assert!(!window.read_with(cx, |window, _| {
        window.differs_from_default(SettingsRowId::ChromeDensity)
    }));

    click("settings-chrome-density-comfortable", cx);

    assert!(window.read_with(cx, |window, _| {
        window.differs_from_default(SettingsRowId::ChromeDensity)
    }));

    click("settings-row-chrome-density-reset", cx);

    assert_eq!(
        document_of(&window, cx).preferences.chrome.density,
        ChromeDensity::Compact
    );
    assert!(!window.read_with(cx, |window, _| {
        window.differs_from_default(SettingsRowId::ChromeDensity)
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
    select_section(SettingsSectionId::Terminal, cx);

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
    assert!(!window.read_with(cx, |window, _| {
        window.differs_from_default(SettingsRowId::TerminalItalic)
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
    // The largest chrome type with the roomiest density, where the reset is furthest from its
    // default size.
    document.preferences.chrome.typography.base_size = 24.0;
    document.preferences.chrome.density = ChromeDensity::Comfortable;
    let (window, _harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    select_section(SettingsSectionId::Terminal, cx);

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
    assert!(window.read_with(cx, |window, _| {
        window.differs_from_default(SettingsRowId::TerminalBoldAsBright)
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
/// The two reset together: a selection naming an imported scheme is valid only while that scheme
/// is installed, so clearing one without the other would leave the document contradicting itself.
#[gpui::test]
fn resetting_everything_restores_defaults_and_empties_the_installed_catalog(
    cx: &mut TestAppContext,
) {
    let mut document = SettingsDocument::default();
    document.preferences.chrome.density = ChromeDensity::Comfortable;
    document.custom_schemes = crate::appearance::parse_color_document(IMPORTABLE_PACKAGE)
        .expect("fixture color package")
        .schemes;
    let (window, harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    assert!(
        installed_count(&window, cx) > 0,
        "the fixture should install one scheme"
    );

    click("settings-reset-all", cx);
    click("modal-action-settings-reset-all-confirm", cx);
    settle(cx);

    let after = document_of(&window, cx);
    assert_eq!(after.preferences.chrome.density, ChromeDensity::Compact);
    assert!(
        after.custom_schemes.is_empty(),
        "Reset All should empty the imported catalog, got {} schemes",
        after.custom_schemes.len()
    );
    let retained = harness
        .storage
        .document()
        .expect("the retained document should parse");
    assert!(
        retained.custom_schemes.is_empty(),
        "the emptied catalog should reach storage"
    );
}

/// A scheme the preferences select cannot survive the catalog that defines it, so the reset must
/// return the selection to a built-in scheme in the same edit the catalog is emptied by.
#[gpui::test]
fn resetting_everything_releases_a_selected_imported_scheme(cx: &mut TestAppContext) {
    let mut document = SettingsDocument {
        custom_schemes: crate::appearance::parse_color_document(IMPORTABLE_PACKAGE)
            .expect("fixture color package")
            .schemes,
        ..Default::default()
    };
    let imported = document.custom_schemes[0].id().clone();
    document.preferences.chrome.schemes.light = imported.clone();
    let (window, _harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    assert_eq!(
        document_of(&window, cx).preferences.chrome.schemes.light,
        imported,
        "the fixture should select the imported scheme"
    );

    click("settings-reset-all", cx);
    click("modal-action-settings-reset-all-confirm", cx);
    settle(cx);

    let after = document_of(&window, cx);
    assert_eq!(
        after.preferences.chrome.schemes.light,
        SettingsDocument::default().preferences.chrome.schemes.light
    );
    assert!(after.custom_schemes.is_empty());
    after
        .validate()
        .expect("the reset document should stay valid");
}

// Search and navigation ----------------------------------------------------------------------

#[gpui::test]
fn search_narrows_the_detail_pane_to_matching_rows(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);

    set_query(&window, "line height", cx);

    let rows = window.read_with(cx, |window, _| window.rows_for(SettingsSectionId::Terminal));
    assert_eq!(rows, vec![SettingsRowId::TerminalLineHeight]);
    assert!(
        window
            .read_with(cx, |window, _| window
                .rows_for(SettingsSectionId::Appearance))
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
fn search_reveals_the_first_match_available_in_auto_mode(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.set_appearance_mode(AppearanceMode::Auto, cx)
        });
    });

    set_query(&window, "Interface", cx);

    assert_eq!(
        window.read_with(cx, |window, _| window.revealed),
        Some(SettingsRowId::ChromeLightScheme)
    );
}

#[gpui::test]
fn changing_appearance_mode_resynchronizes_the_revealed_search_result(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    set_query(&window, "Interface", cx);
    assert_eq!(
        window.read_with(cx, |window, _| window.revealed),
        Some(SettingsRowId::ChromeScheme)
    );

    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.set_appearance_mode(AppearanceMode::Auto, cx)
        });
    });

    assert_eq!(
        window.read_with(cx, |window, _| window.revealed),
        Some(SettingsRowId::ChromeLightScheme)
    );
}

#[gpui::test]
fn search_navigation_excludes_sections_with_only_unavailable_matches(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.set_appearance_mode(AppearanceMode::Auto, cx)
        });
    });

    set_query(&window, "scheme", cx);

    assert!(!window.read_with(cx, |window, _| {
        window
            .navigable_sections()
            .contains(&SettingsSectionId::Appearance)
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
                .rows_for(SettingsSectionId::Appearance))
            .is_empty()
    );
}

#[gpui::test]
fn selecting_a_section_makes_it_active(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);

    click("settings-navigation-settings-section-terminal", cx);

    assert_eq!(
        window.read_with(cx, |window, _| window.active_section),
        SettingsSectionId::Terminal
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
        SettingsSectionId::Appearance
    );
    assert!(cx.debug_bounds("settings-section-appearance").is_some());
    assert!(cx.debug_bounds("settings-section-terminal").is_none());
    assert!(cx.debug_bounds("settings-section-color-schemes").is_none());
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
    set_query(&window, "line", cx);
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

/// Reset All is reached by a single click on a quiet strip, and what it destroys cannot be given
/// back, so the click may only ever open the confirmation. Nothing is written until the
/// confirmation is answered, which is what makes a mistaken click harmless.
#[gpui::test]
fn pressing_reset_all_only_opens_the_confirmation(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    click("settings-chrome-density-comfortable", cx);
    settle(cx);
    let writes = harness.storage.writes();

    click("settings-reset-all", cx);
    settle(cx);

    assert!(
        cx.update(|gpui_window, cx| spaceterm_ui::window_modal_is_open(gpui_window, cx)),
        "the reset confirmation should be presented"
    );
    assert_eq!(
        document_of(&window, cx).preferences.chrome.density,
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
    click("settings-chrome-density-comfortable", cx);
    settle(cx);
    let writes = harness.storage.writes();

    click("settings-reset-all", cx);
    click("modal-action-settings-reset-all-cancel", cx);
    settle(cx);

    assert_eq!(
        document_of(&window, cx).preferences.chrome.density,
        ChromeDensity::Comfortable
    );
    assert_eq!(harness.storage.writes(), writes);
}

#[gpui::test]
fn confirming_the_reset_restores_defaults(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    click("settings-chrome-density-comfortable", cx);
    settle(cx);

    click("settings-reset-all", cx);
    click("modal-action-settings-reset-all-confirm", cx);
    settle(cx);

    assert_eq!(
        document_of(&window, cx).preferences.chrome.density,
        ChromeDensity::Compact
    );
    assert_eq!(
        harness
            .storage
            .document()
            .expect("the retained document should parse")
            .preferences
            .chrome
            .density,
        ChromeDensity::Compact
    );
}

// Content --------------------------------------------------------------------------------------

#[test]
fn the_terminal_font_list_offers_only_monospace_families() {
    use crate::appearance::{AvailableFont, AvailableFonts, FontClass};

    let fonts = AvailableFonts {
        installed: vec![
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
        vec![String::from("Menlo")]
    );
}

#[gpui::test]
fn a_stepper_stops_at_the_ends_of_its_validated_range(cx: &mut TestAppContext) {
    let mut document = SettingsDocument::default();
    document.preferences.terminal.typography.base_size = 8.0;
    let (window, _harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    select_section(SettingsSectionId::Terminal, cx);

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

    select_section(SettingsSectionId::Terminal, cx);
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
        guidance(SettingsRowId::BackgroundBlur, cx)
            .unwrap()
            .contains("Floating surfaces still use your blur choice")
    );

    // The fallback must not disable editing the choices that will apply when support returns.
    click("settings-transparency-increase", cx);
    click("settings-background-blur", cx);
    settle(cx);
    let retained = harness.storage.document().unwrap().preferences.background;
    assert_eq!(retained.transparency, 0.4);
    assert!(!retained.blur);

    harness
        .platform
        .set_native_window_transparency_supported(true);
    cx.run_until_parked();
    assert_eq!(document_of(&window, cx).preferences.background, retained);
    assert_eq!(
        guidance(SettingsRowId::BackgroundBlur, cx),
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
    assert_eq!(document_of(&window, cx).preferences.background, retained);
    assert_eq!(
        harness.storage.document().unwrap().preferences.background,
        retained
    );
}

#[gpui::test]
fn backdrop_guidance_only_promises_opacity_for_the_resolved_material_policy(
    cx: &mut TestAppContext,
) {
    let (window, harness, cx) = open_settings(cx);
    let retained = document_of(&window, cx).preferences.background;
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
                .row_description(SettingsRowId::BackgroundBlur, cx)
                .unwrap(),
        )
    });
    assert!(transparency.contains("window and floating surfaces opaque"));
    assert!(blur.contains("disable window and floating-surface blur"));

    harness.platform.set_reduce_transparency(false);
    harness.platform.set_increase_contrast(true);
    cx.run_until_parked();
    for row in [SettingsRowId::Transparency, SettingsRowId::BackgroundBlur] {
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
    for row in [SettingsRowId::Transparency, SettingsRowId::BackgroundBlur] {
        let guidance = window
            .read_with(cx, |settings, cx| settings.row_description(row, cx))
            .unwrap();
        assert!(!guidance.contains("currently keep"), "{row:?}: {guidance}");
        assert!(
            !guidance.contains("currently disable"),
            "{row:?}: {guidance}"
        );
    }
    assert_eq!(document_of(&window, cx).preferences.background, retained);
}

#[gpui::test]
fn backdrop_guidance_identifies_zero_transparency_without_claiming_a_system_override(
    cx: &mut TestAppContext,
) {
    let mut document = SettingsDocument::default();
    document.preferences.background.transparency = 0.0;
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
                    .row_description(SettingsRowId::BackgroundBlur, cx)
                    .unwrap(),
            )
        });
        assert!(transparency.contains("opaque at 0"));
        assert!(blur.contains("Increase Transparency above 0"));
        assert!(!transparency.contains("accessibility"));
        assert!(!blur.contains("accessibility"));
        assert_eq!(
            document_of(&window, cx).preferences.background,
            document.preferences.background
        );
    }
}

#[gpui::test]
fn transparency_stepper_persists_bounds_blur_and_reset(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    assert_eq!(
        document_of(&window, cx).preferences.background.transparency,
        0.35
    );
    for _ in 0..8 {
        click("settings-transparency-decrease", cx);
    }
    assert_eq!(
        document_of(&window, cx).preferences.background.transparency,
        0.0
    );
    for _ in 0..21 {
        click("settings-transparency-increase", cx);
    }
    assert_eq!(
        document_of(&window, cx).preferences.background.transparency,
        1.0
    );
    click("settings-background-blur", cx);
    settle(cx);
    let saved = harness.storage.document().unwrap();
    assert_eq!(saved.preferences.background.transparency, 1.0);
    assert!(!saved.preferences.background.blur);
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
    assert_eq!(saved.preferences.background.transparency, 0.35);
    assert!(!saved.preferences.background.blur);
}

/// Nothing is wrong by default, and a warning that says so is noise rather than information.
#[gpui::test]
fn the_scheme_library_warns_only_when_something_could_not_be_resolved(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::ColorSchemes, cx);

    assert_eq!(cx.debug_bounds("settings-diagnostics-notice"), None);
    assert!(
        cx.debug_bounds("settings-installed-schemes-chrome")
            .is_some(),
        "the library lists what is installed"
    );
}

// Layout ---------------------------------------------------------------------------------------

#[gpui::test]
fn settings_surfaces_keep_complete_paints_with_opposite_authored_materials(
    cx: &mut TestAppContext,
) {
    use crate::appearance::{ChromeColors, Color};
    use gpui::prelude::*;
    use gpui::{DivInspectorState, ScrollDelta, ScrollWheelEvent, TouchPhase, div, point};
    use std::cell::RefCell;

    let (settings, harness, cx) = open_settings(cx);
    harness.storage.fail_writes(Some(StorageError::Unavailable));
    click("settings-chrome-density-comfortable", cx);
    settle(cx);
    assert!(matches!(status(&settings, cx), SaveStatus::Failed(_)));
    let authored = ChromeColors {
        background: Color::rgb(0x777777),
        text: Color::rgb(0x777777),
        text_muted: Color::rgb(0x777777),
        panel_background: Color::rgb(0xf0f0f0),
        elevated_surface_background: Color::rgb(0x202020),
        row_background: Color::rgb(0x111122),
        row_foreground: Color::rgb(0x777777),
        row_selected_background: Color::rgb(0xffffdd),
        row_selected_foreground: Color::rgb(0x777777),
        row_selected_secondary: Color::rgb(0x777777),
        info_background: Color::rgb(0xfefefe),
        error: Color::rgb(0x101010),
        error_background: Color::rgb(0xffffff),
        warning: Color::rgb(0x101010),
        warning_background: Color::rgb(0xffffff),
        ..ChromeColors::default()
    };
    let observed = Rc::new(RefCell::new(Vec::<DivInspectorState>::new()));
    let observed_styles = observed.clone();
    let expected = cx.update(|window, cx| {
        let installed = appearance_runtime::current(cx);
        let mut resolved = installed.chrome.as_ref().clone();
        resolved.colors = authored;
        resolved.composition.capabilities.increase_contrast = true;
        let appearance = crate::ui::appearance::ChromeAppearance::prepare(&resolved);
        let root = appearance
            .host_colors(spaceterm_ui::ControlHost::Window)
            .clone();
        let panel = appearance
            .host_colors(spaceterm_ui::ControlHost::Panel)
            .clone();
        let card = appearance
            .host_colors(spaceterm_ui::ControlHost::Card)
            .clone();
        for (foreground, host) in [
            (root.text, root.background),
            (card.text, card.elevated_surface_background),
        ] {
            assert!(
                foreground.contrast_ratio(host) >= 7.0,
                "Increase Contrast must resolve {foreground:?} on its final host {host:?}"
            );
        }
        let navigation_row_fill = super::navigation_chip_paint(false, true, &panel)
            .raised_on(&appearance, panel.panel_background)
            .fill
            .expect("the authored idle navigation row has a fill");
        assert!(
            panel.row_foreground.contrast_ratio(navigation_row_fill) >= 7.0,
            "Increase Contrast must resolve the navigation foreground on its rendered row fill"
        );
        let expected = [
            appearance.surface(
                crate::appearance::SurfaceRole::Sheet,
                appearance.colors.background,
            ),
            navigation_row_fill,
            appearance.surface(
                crate::appearance::SurfaceRole::Surface,
                appearance.colors.elevated_surface_background,
            ),
            super::controls::highlighted_row_background(&appearance),
            card.row_selected_foreground,
            root.text_muted,
        ];
        cx.set_global(crate::ui::appearance::InstalledChrome::single(Arc::new(
            appearance,
        )));
        cx.register_inspector_element(move |_, state: &DivInspectorState, _, _| {
            observed_styles.borrow_mut().push(state.clone());
            gpui::Empty
        });
        cx.set_inspector_renderer(Box::new(|inspector, window, cx| {
            div()
                .children(inspector.render_inspector_states(window, cx))
                .into_any_element()
        }));
        window.refresh();
        expected
    });
    cx.run_until_parked();
    set_query(&settings, "line height", cx);
    for (selector, expected_background, expected_foreground) in [
        ("settings-window-surface", Some(expected[0]), None),
        (
            "settings-navigation-chip-settings-section-interface",
            Some(expected[1]),
            None,
        ),
        (
            "settings-section-terminal-group-font-card",
            Some(expected[2]),
            None,
        ),
        ("settings-row-terminal-line-height", Some(expected[3]), None),
        (
            "settings-row-terminal-line-height-label",
            None,
            Some(expected[4]),
        ),
        ("settings-save-status", None, Some(expected[5])),
    ] {
        cx.update(|window, cx| window.toggle_inspector(cx));
        cx.run_until_parked();
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} should render"));
        let position = bounds.center();
        observed.borrow_mut().clear();
        cx.simulate_mouse_move(position, None, Modifiers::none());
        cx.run_until_parked();
        let matches = || {
            observed.borrow().iter().any(|state| {
                state.bounds == bounds
                    && expected_background.is_none_or(|color| {
                        state.base_style.background
                            == Some(super::controls::gpui_color(color).into())
                    })
                    && expected_foreground.is_none_or(|color| {
                        state.base_style.text.as_ref().and_then(|text| text.color)
                            == Some(super::controls::gpui_color(color).into())
                    })
            })
        };
        for _ in 0..24 {
            if matches() {
                break;
            }
            cx.simulate_event(ScrollWheelEvent {
                position,
                delta: ScrollDelta::Pixels(point(px(0.0), px(36.0))),
                modifiers: Modifiers::none(),
                touch_phase: TouchPhase::Moved,
            });
            cx.run_until_parked();
        }
        assert!(
            matches(),
            "{selector} must render its complete authored paint pair"
        );
        if selector == "settings-section-terminal-group-font-card" {
            assert!(
                observed
                    .borrow()
                    .iter()
                    .filter(|state| state.bounds == bounds)
                    .any(|state| {
                        let widths = &state.base_style.border_widths;
                        [widths.top, widths.right, widths.bottom, widths.left]
                            .into_iter()
                            .all(|width| {
                                width.is_some_and(|width| width.to_pixels(px(16.0)) == px(1.0))
                            })
                    }),
                "a Settings card must paint its fixed one-point edge"
            );
        }
        cx.update(|window, cx| window.toggle_inspector(cx));
        cx.run_until_parked();
    }
}

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
    select_section(SettingsSectionId::Terminal, cx);

    let pane = cx
        .debug_bounds("settings-section-terminal")
        .expect("the section should render");
    let rows = window.read_with(cx, |window, _| window.rows_for(SettingsSectionId::Terminal));
    let mut cards: Vec<(String, gpui::Bounds<gpui::Pixels>)> = Vec::new();
    for row in &rows {
        let group = super::group_selector(SettingsSectionId::Terminal, row.descriptor().group);
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
        .debug_bounds("settings-section-appearance-group-background-card")
        .expect("the Background group must render its card");
    let top = cx
        .debug_bounds("settings-row-transparency")
        .expect("the first Background row must render");
    let bottom = cx
        .debug_bounds("settings-row-background-blur")
        .expect("the second Background row must render");
    let separator = cx
        .debug_bounds("settings-section-appearance-group-background-card-separator-1")
        .expect("adjacent Background rows must render a separator");

    assert_eq!(bottom.top(), top.bottom());
    assert_eq!(separator.size.height, px(1.0));
    assert_eq!(separator.left() - card.left(), px(12.0));
    assert_eq!(separator.right(), card.right() - px(1.0));
}

fn assert_reset_all_leads_the_content_footer(document: SettingsDocument, cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    // A button holds its label clear of its edge by a padding that follows the density scale and a
    // border that does not, so the expected offset is built from each in its own scale.
    let label_inset = cx.update(|_, cx| {
        crate::ui::appearance::chrome(cx)
            .spacing(crate::ui::button_theme::COMPACT_HORIZONTAL_PADDING)
            + px(crate::ui::button_theme::CONTROL_BORDER_WIDTH)
    });
    for section in SettingsSectionId::ALL {
        select_section(section, cx);
        let navigation = cx.debug_bounds("settings-navigation").unwrap();
        // The heading spans the content gutter on both sides, and the title, group headings and
        // row labels all start on it, so it is the column the strip has to join.
        let heading = cx
            .debug_bounds(leaked_owned(format!("{}-heading", section.selector())))
            .unwrap();
        let reset = cx.debug_bounds("settings-reset-all").unwrap();
        let footer = cx.debug_bounds("settings-footer").unwrap();
        let status = cx.debug_bounds("settings-save-status").unwrap();
        // The button's label, not its edge, carries that alignment, so the edge sits exactly one
        // label inset to the left of the column it continues.
        assert_eq!(
            reset.left() + label_inset,
            heading.left(),
            "Reset All's label should land on the content gutter in {section:?}"
        );
        assert_eq!(
            status.right(),
            heading.right(),
            "the save status should end on the content gutter in {section:?}"
        );
        assert!(
            reset.left() >= navigation.right(),
            "Reset All should leave the navigation column in {section:?}"
        );
        assert!(reset.top() >= footer.top() && reset.bottom() <= footer.bottom());
        assert!(
            status.left() > reset.right(),
            "the save status should stay clear of the action in {section:?}"
        );
    }
}

#[gpui::test]
fn the_footer_strip_meets_the_content_gutter_on_every_settings_page(cx: &mut TestAppContext) {
    assert_reset_all_leads_the_content_footer(SettingsDocument::default(), cx);
}

#[gpui::test]
fn the_footer_strip_tracks_the_content_gutter_with_larger_type_and_comfortable_density(
    cx: &mut TestAppContext,
) {
    let mut document = SettingsDocument::default();
    document.preferences.chrome.typography.base_size = 20.0;
    document.preferences.chrome.density = ChromeDensity::Comfortable;
    assert_reset_all_leads_the_content_footer(document, cx);
}

/// The sidebar's width is the one width in the footer that means anything, and a destructive
/// action stretched to it reads as the heaviest element in the window.
#[gpui::test]
fn reset_all_takes_the_width_of_its_label(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    let sidebar = cx.debug_bounds("settings-sidebar").unwrap();
    let footer = cx.debug_bounds("settings-footer").unwrap();
    let reset = cx.debug_bounds("settings-reset-all").unwrap();
    assert!(
        reset.size.width < sidebar.size.width,
        "Reset All should hug its label, got {:?} against a {:?} sidebar",
        reset.size.width,
        sidebar.size.width
    );
    assert!(
        reset.size.width < footer.size.width / 2.0,
        "Reset All should leave the content footer to the save status, got {:?}",
        reset.size.width
    );
}

/// The strip is a status line, so its one action rests at the status's weight rather than
/// outranking the settings it would undo. Both sit at the same step of the ramp and the same
/// muted foreground, and the action lifts to full text only on approach.
#[gpui::test]
fn reset_all_rests_at_the_weight_of_the_save_status(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    let reset = cx.debug_bounds("settings-reset-all").unwrap();
    let (muted, full, compact_height) = cx.update(|_, cx| {
        let appearance = crate::ui::appearance::chrome(cx);
        let theme = crate::ui::button_theme::theme(&appearance.colors);
        (
            appearance.colors.text_muted,
            appearance.colors.text,
            theme.icon_button_size(spaceterm_ui::ButtonSize::Compact),
        )
    });
    let style = cx.update(|_, cx| {
        crate::ui::button_theme::theme(&crate::ui::appearance::chrome(cx).colors)
            .paints(spaceterm_ui::ButtonVariant::Bare)
    });
    assert_eq!(
        style.normal().foreground(),
        super::controls::gpui_color(muted),
        "the resting action should take the save status's muted foreground"
    );
    assert_eq!(
        style.normal().background().a,
        0.0,
        "the resting action should paint no surface on the strip"
    );
    assert_eq!(
        style.hovered().foreground(),
        super::controls::gpui_color(full),
        "approach should lift the action to full text"
    );
    assert_eq!(
        reset.size.height, compact_height,
        "the action should sit at the compact step, not a form control's height"
    );
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
        .debug_bounds("settings-section-appearance-title")
        .expect("section title");
    let description = cx
        .debug_bounds("settings-section-appearance-description")
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
fn client_chrome_geometry_tracks_larger_type_and_comfortable_density(cx: &mut TestAppContext) {
    let mut document = SettingsDocument::default();
    document.preferences.chrome.typography.base_size = 20.0;
    document.preferences.chrome.density = ChromeDensity::Comfortable;
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

    let label = cx
        .debug_bounds("settings-row-appearance-mode-label")
        .expect("the appearance label should render");
    let control = cx
        .debug_bounds("settings-appearance-mode")
        .expect("the appearance control should render");
    let row = cx
        .debug_bounds("settings-row-appearance-mode")
        .expect("the appearance row should render");
    let density = cx
        .debug_bounds("settings-row-chrome-density")
        .expect("the density row should render");

    // The caption is the only thing in this row under the label, so the label column's own extent
    // is what the assertions below measure.
    assert!(
        label.bottom() < control.bottom(),
        "guidance should sit under the label, got {label:?} against {control:?}"
    );
    assert!(
        row.bottom() <= density.top() + px(0.5),
        "guidance should stay inside its own row, got {row:?} against {density:?}"
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
    select_section(SettingsSectionId::Terminal, cx);

    let rows = window.read_with(cx, |window, _| window.rows_for(SettingsSectionId::Terminal));
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
    select_section(SettingsSectionId::Interface, cx);

    let selector = cx
        .debug_bounds("settings-row-chrome-regular-weight-control")
        .expect("the weight selector should render");
    let stepper = cx
        .debug_bounds("settings-chrome-base-size")
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
    let (_window, _harness, cx) = open_settings(cx);

    let trigger = cx
        .debug_bounds("settings-row-chrome-scheme-control")
        .expect("the interface scheme selector should render");
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

/// Every scheme's colors take one column width, so the names beside them line up.
#[gpui::test]
fn every_scheme_strip_shares_one_width(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::ColorSchemes, cx);

    let ids = window.read_with(cx, |window, _| {
        [SchemeKind::Chrome, SchemeKind::Terminal]
            .into_iter()
            .flat_map(|kind| window.editor.scheme_summaries(kind).unwrap_or_default())
            .map(|summary| summary.id.as_str().to_owned())
            .collect::<Vec<_>>()
    });
    assert!(ids.len() > 1, "the library should list several schemes");

    let edges = ids
        .iter()
        .map(|id| {
            cx.debug_bounds(leaked_owned(format!("settings-scheme-swatches-{id}")))
                .unwrap_or_else(|| panic!("{id} should show its colors"))
                .right()
        })
        .collect::<Vec<_>>();

    // A scheme offering fewer colors shows wider bands rather than a shorter strip, so every name
    // beside them starts at the same place.
    assert!(
        edges.windows(2).all(|pair| pair[0] == pair[1]),
        "every strip should end at one edge, got {edges:?} for {ids:?}"
    );
}

#[gpui::test]
fn one_appearance_mode_is_presented_for_both_surfaces(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    assert!(cx.debug_bounds("settings-appearance-mode").is_some());
    assert!(cx.debug_bounds("settings-chrome-appearance-mode").is_none());
    assert!(
        cx.debug_bounds("settings-terminal-appearance-mode")
            .is_none()
    );
}

/// The library groups schemes by the surface they dress, so the two kinds never run together.
#[gpui::test]
fn the_scheme_library_separates_the_interface_from_the_terminal(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::ColorSchemes, cx);

    let interface = cx
        .debug_bounds("settings-installed-schemes-chrome")
        .expect("the interface schemes should render");
    let terminal = cx
        .debug_bounds("settings-installed-schemes-terminal")
        .expect("the terminal schemes should render");

    assert!(
        interface.bottom() <= terminal.top(),
        "the two kinds should not interleave, got {interface:?} and {terminal:?}"
    );
    // Each kind is titled by its own group rather than by a heading inside a shared list, so the
    // library reads the same way as every other page.
    for group in [
        "settings-section-color-schemes-group-interface-title",
        "settings-section-color-schemes-group-terminal-title",
    ] {
        assert!(
            cx.debug_bounds(leaked(group)).is_some(),
            "{group} is absent"
        );
    }
}

#[gpui::test]
fn a_row_label_is_centered_against_the_control_it_names(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Interface, cx);

    // The interface font row pairs a short label with the tallest control in the form.
    let label = cx
        .debug_bounds("settings-row-chrome-font-family-label")
        .expect("the interface font label should render");
    let control = cx
        .debug_bounds("settings-chrome-font-family")
        .expect("the interface font control should render");

    assert!(
        (label.center().y - control.center().y).abs() < px(1.0),
        "the label should sit on the control's line, got {label:?} and {control:?}"
    );
}

#[gpui::test]
fn a_control_hugs_its_content_rather_than_filling_the_row(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);

    let row = cx
        .debug_bounds("settings-row-chrome-density")
        .expect("the density row should render");
    let control = cx
        .debug_bounds("settings-chrome-density")
        .expect("the density control should render");

    assert!(
        control.size.width * 2.0 < row.size.width,
        "two densities should not span the row, got {control:?} in {row:?}"
    );
}

#[gpui::test]
fn a_switch_row_presents_the_switch_without_repeating_the_label(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Terminal, cx);

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
fn every_installed_scheme_row_keeps_one_line(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::ColorSchemes, cx);
    let ids = window.read_with(cx, |window, _| {
        [SchemeKind::Chrome, SchemeKind::Terminal]
            .into_iter()
            .flat_map(|kind| window.editor.scheme_summaries(kind).unwrap_or_default())
            .map(|summary| summary.id.as_str().to_owned())
            .collect::<Vec<_>>()
    });
    assert!(ids.len() > 1, "the built-in schemes should be installed");

    let heights = ids
        .iter()
        .map(|id| {
            let selector: &'static str =
                Box::leak(format!("settings-scheme-row-{id}").into_boxed_str());
            cx.debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} should render"))
                .size
                .height
        })
        .collect::<Vec<_>>();

    // A name that wrapped would grow its row, so equal heights are what keeps the list a list.
    assert!(
        heights.windows(2).all(|pair| pair[0] == pair[1]),
        "scheme rows should share one height, got {heights:?}"
    );
}

#[gpui::test]
fn every_selector_and_stepper_shares_one_control_height(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);

    // The controls live on two pages now, so each page is visited before its own are measured.
    let mut heights = Vec::new();
    for (section, selectors) in [
        (
            SettingsSectionId::Appearance,
            [
                "settings-row-chrome-scheme-control",
                "settings-chrome-density",
            ]
            .as_slice(),
        ),
        (
            SettingsSectionId::Interface,
            [
                "settings-chrome-font-family",
                "settings-chrome-base-size",
                "settings-row-chrome-regular-weight-control",
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
    select_section(SettingsSectionId::Interface, cx);

    click("settings-row-chrome-regular-weight-control", cx);

    let filter = cx
        .debug_bounds("combo-box-input-row")
        .expect("the filter row should render");
    let first = cx
        .debug_bounds("settings-row-chrome-regular-weight-control-300")
        .expect("the first weight should render");
    let second = cx
        .debug_bounds("settings-row-chrome-regular-weight-control-400")
        .expect("the second weight should render");

    assert_eq!(filter.size.height, first.size.height);
    assert_eq!(first.size.height, second.size.height);
}

#[gpui::test]
fn an_open_selector_shows_its_filter_glyph_and_marks_the_current_value(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Interface, cx);

    click("settings-row-chrome-regular-weight-control", cx);

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

const IMPORTABLE_PACKAGE: &[u8] = br##"{"schema_version":1,"schemes":[{"kind":"chrome","id":"custom.sample","name":"Sample","appearance":"light","colors":{"text":"#112233"}}]}"##;

fn document_of(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> SettingsDocument {
    window.read_with(cx, |window, _| window.editor.document().clone())
}

fn installed_count(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> usize {
    window.read_with(cx, |window, _| {
        window.editor.document().custom_schemes.len()
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
    click("settings-chrome-density-comfortable", cx);
    let blocked = harness.storage.block_next_write();
    let worker = cx.update(|_, cx| {
        window.update(cx, |settings, cx| settings.editor.start_threaded_commit(cx))
    });
    blocked.wait_until_started();
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

    blocked.release();
    worker.join().unwrap();
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
        click("settings-chrome-density-comfortable", cx);
        harness.storage.fail_writes(Some(failure));
        let handle = cx.update(|native, _| native.window_handle());

        request_window_close(&window, cx);
        request_window_close(&window, cx);

        assert!(cx.cx.update(|cx| cx.windows().contains(&handle)));
        assert_eq!(
            document_of(&window, cx).preferences.chrome.density,
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
    click("settings-chrome-density-comfortable", cx);
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
    click("settings-chrome-density-comfortable", cx);
    let blocked = harness.storage.block_next_write();
    let worker = cx.update(|_, cx| {
        window.update(cx, |settings, cx| settings.editor.start_threaded_commit(cx))
    });
    blocked.wait_until_started();
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
    blocked.release();
    worker.join().unwrap();
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
    click("settings-chrome-density-comfortable", cx);
    assert_eq!(harness.storage.writes(), 0);

    cx.cx.update(|cx| cx.shutdown());

    assert_eq!(
        harness
            .storage
            .document()
            .unwrap()
            .preferences
            .chrome
            .density,
        ChromeDensity::Comfortable
    );
    assert_eq!(harness.storage.writes(), 1);
}

#[gpui::test]
fn native_shutdown_drains_background_writes_without_a_foreground_callback(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    click("settings-chrome-density-comfortable", cx);
    let blocked = harness.storage.block_next_write();
    let worker = cx.update(|_, cx| {
        window.update(cx, |settings, cx| settings.editor.start_threaded_commit(cx))
    });
    blocked.wait_until_started();
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.edit(
                |document| document.preferences.terminal.typography.base_size = 21.0,
                cx,
            )
        })
    });
    blocked.release();
    worker.join().unwrap();

    // GPUI's deterministic executor cannot park for an external OS thread. The storage result is
    // ready, but neither GPUI's background result publication nor its foreground callback has run.
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
fn resetting_scheme_choices_survives_an_appearance_mode_round_trip(cx: &mut TestAppContext) {
    let mut document = SettingsDocument::default();
    document.preferences.chrome.schemes.dark =
        crate::appearance::SchemeId::new("custom.previous.chrome").unwrap();
    document.preferences.terminal.schemes.dark =
        crate::appearance::SchemeId::new("custom.previous.terminal").unwrap();
    let (window, _, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.editor.reset(
                crate::appearance::ResetTarget::ChromeScheme(Appearance::Dark),
                cx,
            );
            settings.editor.reset(
                crate::appearance::ResetTarget::TerminalScheme(Appearance::Dark),
                cx,
            );
            // Reset persistent slots, then switch modes before the next render.
            settings.set_appearance_mode(AppearanceMode::Light, cx);
            settings.set_appearance_mode(AppearanceMode::Dark, cx);
        })
    });
    let preferences = document_of(&window, cx).preferences;
    let defaults = SettingsDocument::default().preferences;
    assert_eq!(preferences.chrome.schemes, defaults.chrome.schemes);
    assert_eq!(preferences.terminal.schemes, defaults.terminal.schemes);
}

#[gpui::test]
fn shared_mode_and_independent_slots_survive_save_reload_and_restart(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            for kind in [SchemeKind::Chrome, SchemeKind::Terminal] {
                for slot in [Appearance::Light, Appearance::Dark] {
                    settings.set_scheme(
                        kind,
                        slot,
                        crate::appearance::SchemeId::new(
                            format!("user.saved.{kind:?}.{slot:?}").to_ascii_lowercase(),
                        )
                        .unwrap(),
                        cx,
                    );
                }
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
    let (restarted, _) = crate::settings::UserSettings::load(harness.storage);
    assert_eq!(restarted.snapshot().candidate.preferences, expected);
}

#[gpui::test]
fn live_chrome_preview_preserves_settings_search_editor_and_focus(cx: &mut TestAppContext) {
    let (window, _, cx) = open_settings(cx);
    set_query(&window, "terminal", cx);
    cx.update(|native, cx| {
        window.update(cx, |settings, cx| {
            settings.search.read(cx).focus_handle().focus(native);
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
    select_section(SettingsSectionId::Interface, cx);
    select_section(SettingsSectionId::Appearance, cx);

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
