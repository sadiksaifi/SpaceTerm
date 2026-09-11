use std::{
    rc::Rc,
    sync::{Arc, Mutex},
};

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use crate::appearance::{
    Appearance, AppearanceDocument, ChromeDensity, SchemeSelection, export_settings,
};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::secure_filesystem::{PrivateFileSnapshot, SecureEntryIdentity};
use crate::settings::storage::{Durability, SettingsStorage, StorageCommit, StorageError};
use crate::ui::appearance_runtime;

use super::editor::{COMMIT_DELAY, SaveStatus};
use super::{SettingsRowId, SettingsSectionId, SettingsWindow};

/// In-memory Settings storage that counts writes and can be made to fail on demand.
#[derive(Default)]
struct MemoryStorage(Mutex<MemoryState>);

#[derive(Default)]
struct MemoryState {
    snapshot: Option<(Vec<u8>, u64)>,
    writes: usize,
    write_failure: Option<StorageError>,
    read_failure: Option<StorageError>,
    /// Publishes without a verifiable identity, which forces a reload before the next write.
    drop_identity: bool,
}

impl MemoryStorage {
    fn with_document(document: &AppearanceDocument) -> Arc<Self> {
        let storage = Arc::new(Self::default());
        let bytes = export_settings(document)
            .expect("fixture document")
            .into_bytes();
        storage.0.lock().unwrap().snapshot = Some((bytes, 1));
        storage
    }

    fn writes(&self) -> usize {
        self.0.lock().unwrap().writes
    }

    fn document(&self) -> Option<AppearanceDocument> {
        let state = self.0.lock().unwrap();
        let (bytes, _) = state.snapshot.as_ref()?;
        crate::appearance::parse_settings(bytes).ok()
    }

    fn fail_writes(&self, error: Option<StorageError>) {
        self.0.lock().unwrap().write_failure = error;
    }

    fn corrupt(&self) {
        self.0.lock().unwrap().snapshot = Some((b"{ not settings".to_vec(), 1));
    }

    fn repair(&self) {
        let bytes = export_settings(&AppearanceDocument::default())
            .expect("default document")
            .into_bytes();
        self.0.lock().unwrap().snapshot = Some((bytes, 2));
    }

    fn drop_identity(&self, drop: bool) {
        self.0.lock().unwrap().drop_identity = drop;
    }
}

impl SettingsStorage for MemoryStorage {
    fn read(&self) -> Result<Option<PrivateFileSnapshot>, StorageError> {
        let state = self.0.lock().unwrap();
        if let Some(error) = state.read_failure {
            return Err(error);
        }
        Ok(state
            .snapshot
            .as_ref()
            .map(|(bytes, identity)| PrivateFileSnapshot {
                bytes: bytes.clone(),
                identity: SecureEntryIdentity::from_opaque(*identity),
            }))
    }

    fn write(
        &self,
        bytes: &[u8],
        expected: Option<&SecureEntryIdentity>,
    ) -> Result<StorageCommit, StorageError> {
        let mut state = self.0.lock().unwrap();
        if let Some(error) = state.write_failure {
            return Err(error);
        }
        let expected = expected
            .and_then(|identity| identity.opaque_ref::<u64>())
            .copied();
        if expected != state.snapshot.as_ref().map(|(_, identity)| *identity) {
            return Err(StorageError::Conflict);
        }
        state.writes += 1;
        let identity = expected.unwrap_or_default() + 1;
        state.snapshot = Some((bytes.to_vec(), identity));
        let drop_identity = state.drop_identity;
        Ok(StorageCommit {
            durability: Durability::Synchronized,
            identity: (!drop_identity).then(|| SecureEntryIdentity::from_opaque(identity)),
        })
    }
}

struct Harness {
    storage: Arc<MemoryStorage>,
    settings: crate::settings::UserSettings,
}

fn open_settings(
    cx: &mut TestAppContext,
) -> (Entity<SettingsWindow>, Harness, &mut VisualTestContext) {
    open_settings_with(
        cx,
        MemoryStorage::with_document(&AppearanceDocument::default()),
    )
}

fn open_settings_with(
    cx: &mut TestAppContext,
    storage: Arc<MemoryStorage>,
) -> (Entity<SettingsWindow>, Harness, &mut VisualTestContext) {
    let (settings, changed) = crate::settings::UserSettings::load(storage.clone());
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings.clone(), changed, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
    });
    let (window, cx) = cx.add_window_view(SettingsWindow::new);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (window, Harness { storage, settings }, cx)
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
fn appearance_mode_switches_between_fixed_slots_and_auto(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);

    click("settings-chrome-appearance-mode-light", cx);
    assert!(matches!(
        document_of(&window, cx).preferences.chrome.scheme,
        SchemeSelection::Fixed {
            appearance: Appearance::Light,
            ..
        }
    ));

    click("settings-chrome-appearance-mode-auto", cx);
    assert!(matches!(
        document_of(&window, cx).preferences.chrome.scheme,
        SchemeSelection::System { .. }
    ));

    click("settings-chrome-appearance-mode-dark", cx);
    assert!(matches!(
        document_of(&window, cx).preferences.chrome.scheme,
        SchemeSelection::Fixed {
            appearance: Appearance::Dark,
            ..
        }
    ));
}

#[gpui::test]
fn switching_appearance_mode_preserves_the_other_slots_scheme(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    let dark = match document_of(&window, cx).preferences.chrome.scheme {
        SchemeSelection::Fixed { id, .. } => id,
        SchemeSelection::System { dark, .. } => dark,
    };

    click("settings-chrome-appearance-mode-light", cx);
    click("settings-chrome-appearance-mode-dark", cx);

    // Returning to Dark restores the scheme that slot held, rather than a fallback.
    assert!(matches!(
        document_of(&window, cx).preferences.chrome.scheme,
        SchemeSelection::Fixed { ref id, .. } if *id == dark
    ));
}

#[gpui::test]
fn a_fixed_appearance_offers_one_scheme_row_and_auto_offers_two(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);

    let fixed = window.read_with(cx, |window, _| {
        window.rows_for(SettingsSectionId::Appearance)
    });
    assert!(fixed.contains(&SettingsRowId::ChromeScheme));
    assert!(!fixed.contains(&SettingsRowId::ChromeLightScheme));

    click("settings-chrome-appearance-mode-auto", cx);

    let auto = window.read_with(cx, |window, _| {
        window.rows_for(SettingsSectionId::Appearance)
    });
    assert!(!auto.contains(&SettingsRowId::ChromeScheme));
    assert!(auto.contains(&SettingsRowId::ChromeLightScheme));
    assert!(auto.contains(&SettingsRowId::ChromeDarkScheme));
}

#[gpui::test]
fn the_terminal_keeps_its_own_appearance_independently(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);

    click("settings-chrome-appearance-mode-light", cx);

    let preferences = document_of(&window, cx).preferences;
    assert!(matches!(
        preferences.chrome.scheme,
        SchemeSelection::Fixed {
            appearance: Appearance::Light,
            ..
        }
    ));
    assert!(
        matches!(
            preferences.terminal.scheme,
            SchemeSelection::Fixed {
                appearance: Appearance::Dark,
                ..
            }
        ),
        "changing the application appearance must not move the terminal"
    );
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

#[gpui::test]
fn resetting_everything_restores_defaults_and_keeps_installed_schemes(cx: &mut TestAppContext) {
    let mut document = AppearanceDocument::default();
    document.preferences.chrome.density = ChromeDensity::Comfortable;
    document.custom_schemes = crate::appearance::parse_color_document(IMPORTABLE_PACKAGE)
        .expect("fixture color package")
        .schemes;
    let (window, _harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    let installed = installed_count(&window, cx);
    assert!(installed > 0, "the fixture should install one scheme");

    cx.update(|_, cx| {
        window.update(cx, |window, cx| {
            window
                .editor
                .reset(crate::appearance::ResetTarget::AllAppearance, cx);
        });
    });
    cx.run_until_parked();

    let after = document_of(&window, cx);
    assert_eq!(after.preferences.chrome.density, ChromeDensity::Compact);
    assert_eq!(after.custom_schemes.len(), installed);
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
fn every_section_and_its_rows_render_by_default(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);

    for section in SettingsSectionId::ALL {
        assert!(
            cx.debug_bounds(leaked(section.selector())).is_some(),
            "{section:?} should render"
        );
    }
    let expected = window.read_with(cx, |window, _| {
        SettingsSectionId::ALL
            .iter()
            .flat_map(|section| window.rows_for(*section))
            .collect::<Vec<_>>()
    });
    for row in expected {
        assert!(
            cx.debug_bounds(leaked(row.descriptor().selector)).is_some(),
            "{row:?} should render"
        );
    }
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
fn escape_clears_an_active_search(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    set_query(&window, "line", cx);
    assert!(!window.read_with(cx, |window, _| window.query.is_empty()));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert!(window.read_with(cx, |window, _| window.query.is_empty()));
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
    let mut document = AppearanceDocument::default();
    document.preferences.terminal.typography.base_size = 8.0;
    let (window, _harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));

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
fn the_diagnostics_row_reports_nothing_when_every_choice_is_available(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);

    assert!(cx.debug_bounds("settings-diagnostics-empty").is_some());
}

// Helpers --------------------------------------------------------------------------------------

const IMPORTABLE_PACKAGE: &[u8] = br##"{"schema_version":1,"schemes":[{"kind":"chrome","id":"custom.sample","name":"Sample","appearance":"light","colors":{"text":"#112233"}}]}"##;

fn document_of(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> AppearanceDocument {
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
