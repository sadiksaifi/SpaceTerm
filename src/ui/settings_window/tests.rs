use std::{rc::Rc, sync::Arc};

use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px};

use crate::appearance::{Appearance, AppearanceMode, ChromeDensity, builtin_fallback_theme};
use crate::desktop_profile::HostFeature;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::{
    OperatingSystemWindowDragPlatform, RecordingOperatingSystemWindowDragPlatform,
};
use crate::settings::SettingsDocument;
use crate::settings::storage::StorageError;
use crate::theme_registry::ZedThemeRegistry;
use crate::theme_registry::testing::{MemoryTransport, extension_archive};
use crate::ui::appearance_runtime;

use super::editor::{COMMIT_DELAY, SaveStatus};
use super::{SettingsRowId, SettingsSectionId, SettingsWindow};
use crate::ui::sidebar_window::SidebarOwner as _;

use crate::settings::storage::testing::MemoryStorage;

pub(super) struct Harness {
    storage: Arc<MemoryStorage>,
    settings: crate::settings::Settings,
    platform: RecordingAppearancePlatform,
}

/// Retained window effects that differ from the defaults in every respect a row can show.
fn retained_window_effects() -> SettingsDocument {
    let mut document = SettingsDocument::default();
    document.appearance.window.opacity = 0.2;
    document.appearance.window.blur = false;
    document
}

/// Configures the desktop's window-effect capabilities and lets Settings observe them.
fn set_window_effects(harness: &Harness, opacity: bool, blur: bool, cx: &mut VisualTestContext) {
    harness
        .platform
        .set_native_window_opacity_supported(opacity);
    harness.platform.set_native_window_blur_supported(blur);
    cx.run_until_parked();
}

fn window_background_rows(
    window: &Entity<SettingsWindow>,
    cx: &mut VisualTestContext,
) -> crate::appearance::WindowBackgroundChoices {
    window.read_with(cx, |settings, cx| settings.window_background(cx))
}

/// Whether the Blur switch presents itself as on, read from where its thumb rests.
fn blur_switch_shows_on(cx: &mut VisualTestContext) -> bool {
    let track = cx
        .debug_bounds("settings-blur-indicator")
        .expect("the Blur switch should render");
    let thumb = cx
        .debug_bounds("settings-blur-thumb")
        .expect("the Blur switch thumb should render");
    thumb.center().x > track.center().x
}

/// Whether the element at `selector` paints exactly this fill, and this border where one is
/// given.
fn paints_frame(
    selector: &'static str,
    (fill, border): (crate::appearance::Color, Option<crate::appearance::Color>),
    cx: &mut VisualTestContext,
) -> bool {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"));
    cx.update(|window, _| {
        let bounds = bounds.scale(window.scale_factor());
        let fill = gpui::Background::from(crate::ui::appearance::gpui_color(fill));
        let border =
            border.map(|border| gpui::Hsla::from(crate::ui::appearance::gpui_color(border)));
        // A frame paints its fill and its border as separate quads over the same bounds.
        let quads = window.painted_quads();
        let framed = quads.iter().filter(|quad| quad.bounds == bounds);
        framed.clone().any(|quad| quad.background == fill)
            && border.is_none_or(|border| framed.clone().any(|quad| quad.border_color == border))
    })
}

/// The number of distinct keyboard stops Tab visits on the way once around the window.
fn keyboard_stops(cx: &mut VisualTestContext) -> usize {
    cx.update(|window, cx| {
        let mut visited: Vec<gpui::FocusHandle> = Vec::new();
        for _ in 0..512 {
            window.focus_next(cx);
            let Some(focused) = window.focused(cx) else {
                break;
            };
            if visited.contains(&focused) {
                break;
            }
            visited.push(focused);
        }
        visited.len()
    })
}

#[gpui::test]
fn missing_desktop_opacity_shows_disabled_window_effect_defaults(cx: &mut TestAppContext) {
    assert_unavailable_window_effects_show_disabled_defaults(
        false,
        crate::appearance::UnavailableWindowEffect::Opacity,
        [
            "Window opacity adjustment is unavailable on this system, so the window stays opaque. Floating surfaces use the default opacity.",
            "Window opacity adjustment is unavailable on this system, so the window stays opaque. Floating surfaces use the default blur.",
        ],
        cx,
    );
}

#[gpui::test]
fn missing_desktop_blur_shows_disabled_window_effect_defaults(cx: &mut TestAppContext) {
    assert_unavailable_window_effects_show_disabled_defaults(
        true,
        crate::appearance::UnavailableWindowEffect::Blur,
        [
            "Desktop blur is unavailable on this system, so the window stays opaque. Floating surfaces use the default opacity.",
            "Desktop blur is unavailable on this system, so the window stays opaque. Floating surfaces use the default blur.",
        ],
        cx,
    );
}

/// Where the desktop cannot present a window effect, both rows show their disabled defaults and the
/// retained choices return once the desktop can present them.
fn assert_unavailable_window_effects_show_disabled_defaults(
    opacity: bool,
    unavailable: crate::appearance::UnavailableWindowEffect,
    guidance: [&'static str; 2],
    cx: &mut TestAppContext,
) {
    use crate::appearance::{ChromeTone, ResolvedWindowComposition, WindowBackgroundAppearance};

    let blur = false;
    {
        let retained = retained_window_effects();
        let (window, harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&retained));
        set_window_effects(&harness, opacity, blur, cx);
        let case = format!("opacity={opacity} blur={blur}");

        // The rows show the defaults, not the retained choices.
        let defaults = crate::appearance::AppearancePreferences::default().window;
        let rows = window_background_rows(&window, cx);
        assert_eq!(rows.unavailable, Some(unavailable), "{case}");
        assert_eq!(
            (rows.opacity, rows.blur),
            (defaults.opacity, defaults.blur),
            "{case}"
        );
        assert!(blur_switch_shows_on(cx), "{case}: Blur shows its default");
        let description = |row, cx: &mut VisualTestContext| {
            window.read_with(cx, |settings, cx| settings.row_description(row, cx))
        };
        assert_eq!(
            [
                description(SettingsRowId::Opacity, cx),
                description(SettingsRowId::Blur, cx),
            ],
            guidance.map(Some),
            "{case}"
        );

        // What renders is the defaults, floating surfaces included.
        let composition = cx.update(|_, cx| {
            let chrome = &appearance_runtime::current(cx).chrome;
            (
                chrome.composition,
                ResolvedWindowComposition::resolve(
                    &defaults,
                    chrome.composition.capabilities,
                    ChromeTone::of(chrome.colors.background),
                ),
            )
        });
        assert_eq!(composition.0, composition.1, "{case}");
        assert_eq!(composition.0.effective, WindowBackgroundAppearance::Opaque);
        assert!(!composition.0.floating_materials.is_opaque(), "{case}");
        assert!(composition.0.floating_blur, "{case}");

        // Both controls paint the shared disabled state rather than the enabled one.
        let card = cx.update(|_, cx| {
            crate::ui::appearance::settings::shared(cx)
                .chrome
                .host_colors(spaceterm_ui::ControlHost::Card)
                .clone()
        });
        // The switch track's border is a shared control-library treatment in every state, so
        // only its fill tells the states apart.
        for (selector, disabled, enabled) in [
            (
                "settings-blur-indicator",
                (card.toggle_on_disabled_background, None),
                (card.toggle_on_background, None),
            ),
            (
                "settings-opacity-buttons",
                (
                    card.input_disabled_background,
                    Some(card.input_disabled_border),
                ),
                (card.input_background, Some(card.input_border)),
            ),
        ] {
            assert!(
                paints_frame(selector, disabled, cx),
                "{case}: {selector} paints the disabled state"
            );
            assert!(
                !paints_frame(selector, enabled, cx),
                "{case}: {selector} does not paint the enabled state"
            );
        }

        // Neither pointer nor reset reaches the retained choices, and nothing is written.
        assert!(
            !window.read_with(cx, |settings, cx| {
                settings.pending_reset(SettingsRowId::Opacity, cx).is_some()
                    || settings.pending_reset(SettingsRowId::Blur, cx).is_some()
            }),
            "{case}: a row showing its default offers no reset"
        );
        click("settings-opacity-increase", cx);
        click("settings-opacity-decrease", cx);
        click("settings-blur", cx);
        settle(cx);
        assert_eq!(
            document_of(&window, cx).appearance.window,
            retained.appearance.window,
            "{case}"
        );
        assert_eq!(harness.storage.writes(), 0, "{case}");
        assert!(blur_switch_shows_on(cx), "{case}");

        let locked_stops = keyboard_stops(cx);
        set_window_effects(&harness, true, true, cx);
        assert_eq!(keyboard_stops(cx), locked_stops + 5, "{case}");
        assert!(
            window.read_with(cx, |settings, cx| {
                settings.pending_reset(SettingsRowId::Opacity, cx).is_some()
                    && settings.pending_reset(SettingsRowId::Blur, cx).is_some()
            }),
            "{case}"
        );

        // A desktop that presents both effects restores the retained choices, editable again.
        let rows = window_background_rows(&window, cx);
        assert_eq!(rows.unavailable, None, "{case}");
        assert_eq!((rows.opacity, rows.blur), (0.2, false), "{case}");
        assert!(!blur_switch_shows_on(cx), "{case}");
        assert_eq!(
            cx.update(|_, cx| appearance_runtime::current(cx).chrome.composition.effective),
            WindowBackgroundAppearance::Transparent,
            "{case}"
        );
        click("settings-blur", cx);
        settle(cx);
        assert!(harness.storage.document().unwrap().appearance.window.blur);
        assert_eq!(
            harness
                .storage
                .document()
                .unwrap()
                .appearance
                .window
                .opacity,
            0.2
        );
    }
}

/// Require Opaque Surfaces still explains floating surfaces while a missing window effect keeps the
/// rows at their disabled defaults.
#[gpui::test]
fn unavailable_window_effect_guidance_names_accessibility_for_floating_surfaces(
    cx: &mut TestAppContext,
) {
    let (window, harness, cx) =
        open_settings_with(cx, MemoryStorage::with_document(&retained_window_effects()));
    set_window_effects(&harness, true, false, cx);
    harness.platform.set_require_opaque_surfaces(true);
    cx.run_until_parked();
    let description = |row, cx: &mut VisualTestContext| {
        window.read_with(cx, |settings, cx| settings.row_description(row, cx))
    };
    assert_eq!(
        description(SettingsRowId::Opacity, cx),
        Some(
            "Desktop blur is unavailable on this system, so the window stays opaque. Accessibility settings currently keep floating surfaces opaque."
        )
    );
    assert_eq!(
        description(SettingsRowId::Blur, cx),
        Some(
            "Desktop blur is unavailable on this system, so the window stays opaque. Accessibility settings currently disable floating-surface blur."
        )
    );
    harness.platform.set_native_window_opacity_supported(false);
    cx.run_until_parked();
    assert_eq!(
        description(SettingsRowId::Opacity, cx),
        Some(
            "Window opacity adjustment is unavailable on this system, so the window stays opaque. Accessibility settings currently keep floating surfaces opaque."
        )
    );
    assert_eq!(
        description(SettingsRowId::Blur, cx),
        Some(
            "Window opacity adjustment is unavailable on this system, so the window stays opaque. Accessibility settings currently disable floating-surface blur."
        )
    );
    assert!(!window_background_rows(&window, cx).adjustable());
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
    open_settings_with_capabilities(cx, storage, window_drag, None, None)
}

pub(super) fn open_settings_with_registry(
    cx: &mut TestAppContext,
    transport: Arc<MemoryTransport>,
) -> (Entity<SettingsWindow>, Harness, &mut VisualTestContext) {
    open_settings_with_capabilities(
        cx,
        MemoryStorage::with_document(&SettingsDocument::default()),
        Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
        Some(ZedThemeRegistry::new(transport)),
        None,
    )
}

fn open_settings_with_capabilities(
    cx: &mut TestAppContext,
    storage: Arc<MemoryStorage>,
    window_drag: Rc<dyn OperatingSystemWindowDragPlatform>,
    registry: Option<ZedThemeRegistry>,
    background_images: Option<Arc<crate::background_image::BackgroundImageStore>>,
) -> (Entity<SettingsWindow>, Harness, &mut VisualTestContext) {
    let settings = crate::settings::Settings::load(storage.clone());
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings.clone(), Rc::new(platform.clone()), cx)
            .expect("appearance runtime should install");
        if let Some(store) = background_images {
            crate::ui::background_image_runtime::install(settings.clone(), store, cx);
        }
        crate::ui::init(cx).expect("UI initialization should succeed");
    });
    let (window, cx) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(window_drag, Default::default(), registry, window, cx)
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

/// Presses and releases Return, since a focused button activates on release.
fn press_return(cx: &mut VisualTestContext) {
    cx.simulate_keystrokes("enter");
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    cx.run_until_parked();
}

pub(super) fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let position = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

/// Selects one navigation entry, because each section is its own view.
pub(super) fn select_section(section: SettingsSectionId, cx: &mut VisualTestContext) {
    let selector: &'static str = match section {
        SettingsSectionId::Interface => "settings-navigation-settings-section-interface",
        SettingsSectionId::Font => "settings-navigation-settings-section-font",
        SettingsSectionId::Themes => "settings-navigation-settings-section-themes",
        SettingsSectionId::Keybindings => "settings-navigation-settings-section-keybindings",
        SettingsSectionId::Privacy => "settings-navigation-settings-section-privacy",
        SettingsSectionId::Git => "settings-navigation-settings-section-git",
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
        document_of(&window, cx).appearance.window.density,
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
            .appearance
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
            .appearance
            .terminal
            .typography
            .base_size,
        21.0
    );
    assert_eq!(
        harness
            .storage
            .document()
            .unwrap()
            .appearance
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
        document_of(&window, cx).appearance.window.density,
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
            .appearance
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
            .appearance
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
            .appearance
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
        document_of(&window, cx).appearance.window.density,
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
        Some(crate::settings::storage::testing::CORRUPT_DOCUMENT)
    );
    assert!(
        harness.storage.document().is_some(),
        "the reset writes a readable default document"
    );
    assert_eq!(status(&window, cx), SaveStatus::Saved);
    assert!(cx.debug_bounds("settings-banner").is_none());
    click("settings-density-comfortable", cx);
    assert_eq!(
        document_of(&window, cx).appearance.window.density,
        ChromeDensity::Comfortable
    );
}

#[gpui::test]
fn unsafe_settings_do_not_offer_a_reset(cx: &mut TestAppContext) {
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

    harness.storage.drop_identity(false);
    click("settings-banner-reload", cx);

    assert_eq!(status(&window, cx), SaveStatus::Saved);
    assert!(window.read_with(cx, |window, _| window.editor.editable()));
    assert_eq!(
        document_of(&window, cx).appearance.window.density,
        ChromeDensity::Comfortable
    );
}

#[gpui::test]
fn closing_the_window_writes_a_change_that_has_not_settled(cx: &mut TestAppContext) {
    let (_window, harness, cx) = open_settings(cx);
    click("settings-density-comfortable", cx);
    assert_eq!(harness.storage.writes(), 0);
    let handle = cx.update(|native, _| native.window_handle());

    cx.dispatch_action(super::CloseSettingsWindow);
    cx.run_until_parked();

    assert_eq!(harness.storage.writes(), 1);
    assert_eq!(
        harness
            .storage
            .document()
            .expect("the retained document should parse")
            .appearance
            .window
            .density,
        ChromeDensity::Comfortable
    );
    assert!(!cx.cx.update(|cx| cx.windows().contains(&handle)));
}

// Appearance Mode ----------------------------------------------------------------------------

#[gpui::test]
fn appearance_mode_preserves_window_and_terminal_preferences(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Themes, cx);
    let before = document_of(&window, cx).appearance;
    for (selector, mode) in [
        ("settings-appearance-mode-light", AppearanceMode::Light),
        ("settings-appearance-mode-auto", AppearanceMode::Auto),
        ("settings-appearance-mode-dark", AppearanceMode::Dark),
    ] {
        click(selector, cx);
        let after = document_of(&window, cx).appearance;
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
            assert_eq!(
                cx.debug_bounds(slot).is_some(),
                automatic,
                "{mode:?} {slot}"
            );
        }
        assert_eq!(
            cx.debug_bounds("settings-current-theme-name").is_some(),
            !automatic
        );
    }
}

#[gpui::test]
fn auto_theme_slots_publish_a_radio_group_that_selects_on_press(cx: &mut TestAppContext) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    let (window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Themes, cx);
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.set_appearance_mode(AppearanceMode::Auto, cx)
        })
    });
    let tree = A11yTree::read(cx);
    let group = tree.node("Current theme");
    assert_eq!(group["aria"]["role"], "RadioGroup");
    let slots = tree.children(group);
    let labels = slots
        .iter()
        .map(|slot| slot["aria"]["label"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(labels, ["Light", "Dark"]);
    for slot in &slots {
        assert_eq!(slot["aria"]["role"], "RadioButton");
        assert!(!slot["aria"]["description"].as_str().unwrap().is_empty());
    }
    // The system appearance is Dark, so the gallery starts on the Dark slot.
    assert_eq!(slots[0]["aria"]["toggled"], "False");
    assert_eq!(slots[1]["aria"]["toggled"], "True");

    perform(cx, slots[0], Action::Click);
    assert_eq!(
        window.read_with(cx, |settings, cx| settings.theme_slot(cx)),
        Appearance::Light
    );
    let tree = A11yTree::read(cx);
    let slots = tree.children(tree.node("Current theme"));
    assert_eq!(slots[0]["aria"]["toggled"], "True");
    assert_eq!(slots[1]["aria"]["toggled"], "False");
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
                let before = document_of(&window, cx).appearance;
                let id = crate::appearance::ThemeId::new(
                    format!("custom.{slot:?}").to_ascii_lowercase(),
                )
                .unwrap();
                let mut expected = before.clone();
                expected.terminal.themes.set(slot, id.clone());
                cx.update(|_, cx| {
                    window.update(cx, |settings, cx| settings.set_theme(slot, id, cx))
                });
                assert_eq!(document_of(&window, cx).appearance, expected);
            }
        }
    }
}

#[gpui::test]
fn resetting_appearance_mode_preserves_all_theme_choices(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Themes, cx);
    click("settings-appearance-mode-light", cx);
    let mut expected = document_of(&window, cx).appearance;
    expected.mode = AppearanceMode::default();
    click("settings-row-appearance-mode-reset", cx);
    assert_eq!(document_of(&window, cx).appearance, expected);
}

/// A row's Use button applies its theme to the slot the list shows: the fixed mode's slot, or the
/// slot selected under Auto. The other slot and the mode stay as they were.
#[gpui::test]
fn use_applies_a_theme_to_the_displayed_slot(cx: &mut TestAppContext) {
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
        let before = document_of(&window, cx).appearance;
        let chosen = builtin_fallback_theme(slot);
        click(
            leaked_owned(format!("settings-theme-row-{}-use", chosen.as_str())),
            cx,
        );
        assert!(
            cx.debug_bounds(leaked_owned(format!(
                "settings-theme-row-{}-in-use",
                chosen.as_str()
            )))
            .is_some()
        );
        let mut expected = before;
        expected.terminal.themes.set(slot, chosen);
        assert_eq!(document_of(&window, cx).appearance, expected);
    }
}

#[gpui::test]
fn installed_themes_publish_a_list_of_named_themes_with_their_actions(cx: &mut TestAppContext) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    let family = br##"{"themes":[{"name":"Sample Dark","appearance":"dark","style":{}}]}"##;
    let document = SettingsDocument {
        terminal_themes: crate::appearance::translate_zed_family(family).unwrap(),
        ..Default::default()
    };
    let imported = document.terminal_themes[0].id.clone();
    let (window, _, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    select_section(SettingsSectionId::Themes, cx);
    let tree = A11yTree::read(cx);
    // The group heading and the list it titles share a name.
    let list = tree
        .with_role("List")
        .into_iter()
        .find(|list| list["aria"]["label"] == "Dark themes")
        .expect("the installed themes publish a list");
    let items = tree.children(list);
    assert!(items.iter().all(|item| item["aria"]["role"] == "ListItem"));
    let item = |name: &str| {
        *items
            .iter()
            .find(|item| item["aria"]["label"] == name)
            .unwrap_or_else(|| panic!("no theme item is named {name:?}"))
    };
    let in_use = tree.children(item("SpaceTerm Dark"));
    assert!(in_use.iter().any(|node| node["aria"]["value"] == "In Use"));
    let sample = item("Sample Dark");
    assert!(!sample["aria"]["description"].as_str().unwrap().is_empty());
    let actions = tree
        .children(sample)
        .into_iter()
        .filter(|node| node["aria"]["role"] == "Button")
        .map(|node| node["aria"]["label"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(actions, ["Use", "Remove Sample Dark"]);

    let use_sample = tree
        .children(sample)
        .into_iter()
        .find(|node| node["aria"]["label"] == "Use")
        .unwrap();
    perform(cx, use_sample, Action::Click);
    assert_eq!(
        document_of(&window, cx)
            .appearance
            .terminal
            .themes
            .get(Appearance::Dark),
        &imported
    );
}

/// The same slot selection, Use, and removal operations remain reachable without a pointer.
#[gpui::test]
fn keyboard_navigation_selects_slots_applies_themes_and_opens_removal(cx: &mut TestAppContext) {
    let family = br##"{"themes":[{"name":"Sample Dark","appearance":"dark","style":{}}]}"##;
    let mut document = SettingsDocument {
        terminal_themes: crate::appearance::translate_zed_family(family).unwrap(),
        ..Default::default()
    };
    document.appearance.mode = AppearanceMode::Auto;
    let imported = document.terminal_themes[0].id.clone();
    let (settings, _, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    set_query(&settings, "theme", cx);
    cx.update(|window, cx| window.focus(&settings.read(cx).focus_handle.clone(), cx));

    // Search, section navigation, About, then the Light slot.
    cx.simulate_keystrokes("tab tab tab tab enter");
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

    // The list's search and Get More Themes lead its rows. The built-in theme is in use, so the
    // imported theme's Use and removal buttons follow.
    cx.simulate_keystrokes("tab tab tab tab");
    press_return(cx);
    assert!(
        cx.debug_bounds("modal-action-settings-remove-theme-confirm")
            .is_some()
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    cx.simulate_keystrokes("shift-tab");
    press_return(cx);
    assert_eq!(
        document_of(&settings, cx).appearance.terminal.themes.dark,
        imported
    );
    assert_eq!(
        document_of(&settings, cx).appearance.mode,
        AppearanceMode::Auto
    );
    assert_eq!(installed_count(&settings, cx), 1);

    // Use gives way to In Use, so keyboard focus moves on to the same row's removal button.
    press_return(cx);
    assert!(
        cx.debug_bounds("modal-action-settings-remove-theme-confirm")
            .is_some()
    );
}

/// Removing the theme in use asks first, removes every theme its file imported, then returns its
/// slot to the built-in theme.
#[gpui::test]
fn removing_the_theme_in_use_returns_its_slot_to_the_built_in_theme(cx: &mut TestAppContext) {
    let family = br##"{"name":"Sample","themes":[{"name":"Sample Light","appearance":"light","style":{}},{"name":"Sample Dark","appearance":"dark","style":{}}]}"##;
    let mut document = SettingsDocument {
        terminal_themes: crate::appearance::translate_zed_family(family)
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
    document.appearance.mode = AppearanceMode::Light;
    document.appearance.terminal.themes.light = imported.clone();
    let (window, _harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    select_section(SettingsSectionId::Themes, cx);
    let row = leaked_owned(format!("settings-theme-row-{}", imported.as_str()));

    click(
        leaked_owned(format!("settings-theme-row-{}-remove", imported.as_str())),
        cx,
    );
    assert_eq!(
        document_of(&window, cx).appearance.terminal.themes.light,
        imported,
        "removal waits for confirmation"
    );
    click("modal-action-settings-remove-theme-confirm", cx);

    let after = document_of(&window, cx);
    assert!(after.terminal_themes.is_empty());
    assert_eq!(
        after.appearance.terminal.themes.light,
        SettingsDocument::default().appearance.terminal.themes.light
    );
    assert!(cx.debug_bounds(row).is_none());
}

/// The list always offers search and Get More Themes. Only themes SpaceTerm did not ship offer
/// Remove.
#[gpui::test]
fn only_installed_themes_offer_removal(cx: &mut TestAppContext) {
    let mut document = SettingsDocument {
        terminal_themes: crate::appearance::translate_zed_family(IMPORTABLE_FAMILY)
            .expect("fixture Zed family"),
        ..Default::default()
    };
    document.appearance.mode = AppearanceMode::Light;
    let imported = document.terminal_themes[0].id.clone();
    let (_window, _harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    select_section(SettingsSectionId::Themes, cx);

    assert!(
        cx.debug_bounds("settings-theme-gallery-search-frame")
            .is_some()
    );
    assert!(cx.debug_bounds("settings-get-more-themes").is_some());
    let builtin = builtin_fallback_theme(Appearance::Light);
    assert!(
        cx.debug_bounds(leaked_owned(format!(
            "settings-theme-row-{}",
            builtin.as_str()
        )))
        .is_some()
    );
    assert!(
        cx.debug_bounds(leaked_owned(format!(
            "settings-theme-row-{}-remove",
            builtin.as_str()
        )))
        .is_none()
    );
    assert!(
        cx.debug_bounds(leaked_owned(format!(
            "settings-theme-row-{}-remove",
            imported.as_str()
        )))
        .is_some()
    );
}

// Reset --------------------------------------------------------------------------------------

#[gpui::test]
fn a_row_reset_appears_only_once_the_row_differs_and_restores_the_default(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    assert!(!window.read_with(cx, |window, cx| {
        window.pending_reset(SettingsRowId::Density, cx).is_some()
    }));

    click("settings-density-comfortable", cx);

    assert!(window.read_with(cx, |window, cx| {
        window.pending_reset(SettingsRowId::Density, cx).is_some()
    }));

    click("settings-row-density-reset", cx);

    assert_eq!(
        document_of(&window, cx).appearance.window.density,
        ChromeDensity::Compact
    );
    assert!(!window.read_with(cx, |window, cx| {
        window.pending_reset(SettingsRowId::Density, cx).is_some()
    }));
}

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
        window
            .pending_reset(SettingsRowId::TerminalItalic, cx)
            .is_some()
    }));
    assert_eq!(bounds("settings-terminal-italic", cx), control);
}

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

    document.appearance.window.density = ChromeDensity::Comfortable;
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
        window
            .pending_reset(SettingsRowId::TerminalBoldAsBright, cx)
            .is_some()
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

/// Reset All also empties the imported catalog, because a selection naming an imported theme is
/// valid only while that theme is installed.
#[gpui::test]
fn resetting_everything_restores_defaults_and_empties_the_installed_catalog(
    cx: &mut TestAppContext,
) {
    let mut document = SettingsDocument::default();
    document.appearance.window.density = ChromeDensity::Comfortable;
    document.terminal_themes =
        crate::appearance::translate_zed_family(IMPORTABLE_FAMILY).expect("fixture Zed family");
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
    assert_eq!(after.appearance.window.density, ChromeDensity::Compact);
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
    document.appearance.terminal.themes.light = imported.clone();
    let (window, _harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    assert_eq!(
        document_of(&window, cx).appearance.terminal.themes.light,
        imported,
        "the fixture should select the imported theme"
    );

    select_section(SettingsSectionId::Advanced, cx);
    click("settings-reset-all", cx);
    click("modal-action-settings-reset-all-confirm", cx);
    settle(cx);

    let after = document_of(&window, cx);
    assert_eq!(
        after.appearance.terminal.themes.light,
        SettingsDocument::default().appearance.terminal.themes.light
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

    set_query(&window, "opacity", cx);
    assert_eq!(
        window.read_with(cx, |window, _| window
            .rows_for(SettingsSectionId::Interface)),
        vec![SettingsRowId::Opacity]
    );
    assert_eq!(
        window.read_with(cx, |window, _| window.revealed),
        Some(SettingsRowId::Opacity)
    );
    assert!(cx.debug_bounds("settings-row-opacity").is_some());
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
            cx.debug_bounds(section.selector()).is_some(),
            "{section:?} should render"
        );
        for row in window.read_with(cx, |window, _| window.rows_for(section)) {
            assert!(
                cx.debug_bounds(row.descriptor().selector).is_some(),
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
    assert!(cx.update(|gpui_window, cx| {
        window
            .read(cx)
            .navigation
            .list_focus()
            .is_focused(gpui_window)
    }));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert!(cx.update(|gpui_window, cx| {
        window
            .read(cx)
            .navigation
            .list_focus()
            .is_focused(gpui_window)
    }));
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
        (false, (1, 1, 1))
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
        document_of(&window, cx).appearance.window.density,
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
        document_of(&window, cx).appearance.window.density,
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
        document_of(&window, cx).appearance.window.density,
        ChromeDensity::Compact
    );
    assert_eq!(
        harness
            .storage
            .document()
            .expect("the retained document should parse")
            .appearance
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
    for (size, selector) in [
        (8.0, "settings-terminal-base-size-decrease"),
        (32.0, "settings-terminal-base-size-increase"),
    ] {
        let mut document = SettingsDocument::default();
        document.appearance.terminal.typography.base_size = size;
        let (window, harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
        select_section(SettingsSectionId::Font, cx);

        click(selector, cx);
        settle(cx);

        assert_eq!(document_of(&window, cx), document);
        assert_eq!(harness.storage.document().unwrap(), document);
        assert_eq!(harness.storage.writes(), 0);
    }
}

#[gpui::test]
fn steppers_publish_an_adjustable_value_that_stops_at_its_range(cx: &mut TestAppContext) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, perform, supports};

    let mut document = SettingsDocument::default();
    document.appearance.terminal.typography.base_size = 31.0;
    let (window, _harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    select_section(SettingsSectionId::Font, cx);
    let tree = A11yTree::read(cx);
    let size = tree.node("terminal font size");
    assert_eq!(size["aria"]["role"], "SpinButton");
    assert_eq!(size["aria"]["value"], "31 pt");
    assert!(supports(size, Action::Decrement));

    perform(cx, size, Action::Increment);
    let base_size = |cx: &mut VisualTestContext| {
        document_of(&window, cx)
            .appearance
            .terminal
            .typography
            .base_size
    };
    assert_eq!(base_size(cx), 32.0);
    let tree = A11yTree::read(cx);
    let size = tree.node("terminal font size");
    assert_eq!(size["aria"]["value"], "32 pt");
    assert!(!supports(size, Action::Increment));

    perform(cx, size, Action::Decrement);
    assert_eq!(base_size(cx), 31.0);
}

#[gpui::test]
fn line_height_steps_stay_on_the_step_grid(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);

    select_section(SettingsSectionId::Font, cx);
    click("settings-terminal-line-height-increase", cx);

    let height = document_of(&window, cx)
        .appearance
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

    harness.platform.set_native_window_opacity_supported(true);
    cx.run_until_parked();
    assert_eq!(harness.platform.backdrop_presence(), vec![false, true]);

    click("settings-blur", cx);
    assert_eq!(
        harness.platform.backdrop_presence(),
        vec![false, true, false]
    );
    click("settings-blur", cx);
    harness.platform.set_require_opaque_surfaces(true);
    cx.run_until_parked();
    assert_eq!(
        harness.platform.backdrop_presence(),
        vec![false, true, false, true, false]
    );
    harness.platform.set_require_opaque_surfaces(false);
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
    let (window, harness, cx) =
        open_settings_with(cx, MemoryStorage::with_document(&retained_window_effects()));
    let retained = retained_window_effects().appearance.window;
    let guidance = |row, cx: &mut VisualTestContext| {
        window.read_with(cx, |settings, cx| settings.row_description(row, cx))
    };
    assert!(
        guidance(SettingsRowId::Opacity, cx)
            .unwrap()
            .starts_with("Window opacity adjustment is unavailable on this system")
    );
    assert!(
        guidance(SettingsRowId::Blur, cx)
            .unwrap()
            .starts_with("Window opacity adjustment is unavailable on this system")
    );

    // The fallback shows the defaults it renders and leaves the retained choices for a desktop
    // that can present them.
    click("settings-opacity-increase", cx);
    click("settings-blur", cx);
    settle(cx);
    assert_eq!(document_of(&window, cx).appearance.window, retained);
    assert_eq!(harness.storage.writes(), 0);

    harness.platform.set_native_window_opacity_supported(true);
    cx.run_until_parked();
    assert_eq!(document_of(&window, cx).appearance.window, retained);
    assert_eq!(
        guidance(SettingsRowId::Blur, cx),
        Some("Soften the desktop behind the window and content behind floating surfaces.")
    );
    assert!(
        guidance(SettingsRowId::Opacity, cx)
            .unwrap()
            .contains("0 is transparent; 1 is opaque")
    );
    assert_eq!(
        cx.update(|_, cx| appearance_runtime::current(cx).chrome.composition.effective),
        crate::appearance::WindowBackgroundAppearance::Transparent
    );

    harness.platform.set_native_window_opacity_supported(false);
    cx.run_until_parked();
    assert!(
        guidance(SettingsRowId::Opacity, cx)
            .unwrap()
            .starts_with("Window opacity adjustment is unavailable on this system")
    );
    assert_eq!(document_of(&window, cx).appearance.window, retained);
    assert_eq!(
        harness.storage.document().unwrap().appearance.window,
        retained
    );
}

#[gpui::test]
fn backdrop_guidance_only_promises_opacity_for_the_resolved_material_policy(
    cx: &mut TestAppContext,
) {
    let (window, harness, cx) = open_settings(cx);
    let retained = document_of(&window, cx).appearance.window;
    harness.platform.set_native_window_opacity_supported(true);
    harness.platform.set_require_opaque_surfaces(true);
    cx.run_until_parked();
    let (opacity, blur) = window.read_with(cx, |settings, cx| {
        (
            settings
                .row_description(SettingsRowId::Opacity, cx)
                .unwrap(),
            settings.row_description(SettingsRowId::Blur, cx).unwrap(),
        )
    });
    assert!(opacity.contains("window and floating surfaces opaque"));
    assert!(blur.contains("disable window and floating-surface blur"));

    harness.platform.set_require_opaque_surfaces(false);
    harness.platform.set_increase_contrast(true);
    cx.run_until_parked();
    for row in [SettingsRowId::Opacity, SettingsRowId::Blur] {
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
    for row in [SettingsRowId::Opacity, SettingsRowId::Blur] {
        let guidance = window
            .read_with(cx, |settings, cx| settings.row_description(row, cx))
            .unwrap();
        assert!(!guidance.contains("currently keep"), "{row:?}: {guidance}");
        assert!(
            !guidance.contains("currently disable"),
            "{row:?}: {guidance}"
        );
    }
    assert_eq!(document_of(&window, cx).appearance.window, retained);
}

#[gpui::test]
fn backdrop_guidance_identifies_full_opacity_without_claiming_a_system_override(
    cx: &mut TestAppContext,
) {
    let mut document = SettingsDocument::default();
    document.appearance.window.opacity = 1.0;
    let (window, harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    for supported in [false, true] {
        harness
            .platform
            .set_native_window_opacity_supported(supported);
        cx.run_until_parked();
        let (opacity, blur) = window.read_with(cx, |settings, cx| {
            (
                settings
                    .row_description(SettingsRowId::Opacity, cx)
                    .unwrap(),
                settings.row_description(SettingsRowId::Blur, cx).unwrap(),
            )
        });
        if supported {
            assert!(opacity.contains("opaque at 1"));
            assert!(blur.contains("Decrease Opacity below 1"));
        } else {
            // Without window opacity adjustment the rows show the default rather than the retained 1.
            assert!(opacity.starts_with("Window opacity adjustment is unavailable"));
            assert!(blur.starts_with("Window opacity adjustment is unavailable"));
        }
        assert!(!opacity.contains("ccessibility"));
        assert!(!blur.contains("ccessibility"));
        assert_eq!(
            document_of(&window, cx).appearance.window,
            document.appearance.window
        );
    }
}

#[gpui::test]
fn opacity_stepper_persists_bounds_blur_and_reset(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    set_window_effects(&harness, true, true, cx);
    assert_eq!(document_of(&window, cx).appearance.window.opacity, 0.65);
    let initial_alpha = cx.update(|_, cx| {
        appearance_runtime::current(cx)
            .chrome
            .composition
            .materials
            .alpha(crate::appearance::SurfaceRole::Base)
    });
    click("settings-opacity-increase", cx);
    assert_eq!(document_of(&window, cx).appearance.window.opacity, 0.7);
    assert!(cx.update(|_, cx| {
        appearance_runtime::current(cx)
            .chrome
            .composition
            .materials
            .alpha(crate::appearance::SurfaceRole::Base)
            > initial_alpha
    }));
    for _ in 0..15 {
        click("settings-opacity-decrease", cx);
    }
    assert_eq!(document_of(&window, cx).appearance.window.opacity, 0.0);
    assert!(
        window
            .read_with(cx, |settings, cx| settings
                .row_description(SettingsRowId::Opacity, cx))
            .unwrap()
            .contains("0 is transparent; 1 is opaque")
    );
    for _ in 0..21 {
        click("settings-opacity-increase", cx);
    }
    assert_eq!(document_of(&window, cx).appearance.window.opacity, 1.0);
    assert_eq!(
        cx.update(|_, cx| appearance_runtime::current(cx).chrome.composition.effective),
        crate::appearance::WindowBackgroundAppearance::Opaque
    );
    assert!(
        window
            .read_with(cx, |settings, cx| settings
                .row_description(SettingsRowId::Opacity, cx))
            .unwrap()
            .contains("opaque at 1")
    );
    click("settings-blur", cx);
    settle(cx);
    let saved = harness.storage.document().unwrap();
    assert_eq!(saved.appearance.window.opacity, 1.0);
    assert!(!saved.appearance.window.blur);
    window.update(cx, |settings, cx| {
        settings.edit(
            |draft| {
                draft
                    .appearance
                    .reset(crate::appearance::ResetTarget::Opacity)
            },
            cx,
        )
    });
    settle(cx);
    let saved = harness.storage.document().unwrap();
    assert_eq!(saved.appearance.window.opacity, 0.65);
    assert!(!saved.appearance.window.blur);
}

/// Nothing is wrong by default, and a warning that says so is noise rather than information.
#[gpui::test]
fn the_default_themes_page_has_no_warning(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Themes, cx);

    assert_eq!(cx.debug_bounds("settings-diagnostics-notice"), None);
    assert!(
        cx.debug_bounds("settings-current-theme").is_some(),
        "the page leads with the theme in use"
    );
}

// Layout ---------------------------------------------------------------------------------------

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
                .debug_bounds(row.descriptor().selector)
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
                .debug_bounds(pair[0].descriptor().selector)
                .expect("row bounds");
            let bottom = cx
                .debug_bounds(pair[1].descriptor().selector)
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
        .debug_bounds("settings-row-opacity")
        .expect("the Opacity row must render");
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

    document.appearance.window.density = ChromeDensity::Comfortable;
    assert_the_footer_closes_only_the_content_column(document, cx);
}

/// The footer is a status line. Actions on Settings as a whole live in the Advanced section.
#[gpui::test]
fn the_footer_carries_only_the_save_status(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    let footer = cx.debug_bounds("settings-footer").unwrap();

    for selector in [
        "settings-reset-all",
        "settings-document-export",
        "settings-import",
    ] {
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

    document.appearance.window.density = ChromeDensity::Comfortable;
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
fn settings_window_titlebar_preserves_modal_focus_while_moving(cx: &mut TestAppContext) {
    let records = Rc::new(RecordingOperatingSystemWindowDragPlatform::default());
    let (_window, _harness, cx) = open_settings_with_drag(
        cx,
        MemoryStorage::with_document(&SettingsDocument::default()),
        records.clone(),
    );
    cx.simulate_decorations(gpui::Decorations::Client {
        tiling: gpui::Tiling::default(),
    });
    cx.simulate_button_layout(Some(gpui::WindowButtonLayout {
        left: [Some(gpui::WindowButton::Close), None, None],
        right: [None; 3],
    }));
    cx.run_until_parked();
    select_section(SettingsSectionId::Advanced, cx);
    click("settings-reset-all", cx);
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("modal-action-settings-reset-all-confirm-keyboard-focus")
            .is_some()
    );
    let focused = cx.update(|window, cx| window.focused(cx).unwrap());
    for region in [
        "settings-sidebar-drag-region-hitbox",
        "settings-detail-drag-region-hitbox",
    ] {
        let bounds = cx.debug_bounds(region).unwrap();
        let start = point(bounds.right() - px(10.), bounds.top() + px(10.));
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        assert!(
            cx.update(|window, _| focused.is_focused(window)),
            "{region} must preserve modal focus on press"
        );
        cx.simulate_mouse_move(
            point(start.x - px(8.), start.y),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_up(start, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        assert!(
            cx.update(|window, _| focused.is_focused(window)),
            "{region} must preserve modal focus after movement"
        );
    }
    assert_eq!(records.counts(), (2, 2, 2));
    assert!(cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
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

    assert_eq!(records.counts(), (1, 1, 1));

    let search = cx
        .debug_bounds("settings-search-frame")
        .expect("search field")
        .center();
    cx.simulate_click(search, Modifiers::none());
    assert_eq!(
        records.counts(),
        (1, 1, 1),
        "Search interaction must remain outside the titlebar drag owner"
    );
}

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

/// The rendered control's selector, independent from the enclosing form row.
fn rendered_control_selector(row: SettingsRowId) -> Option<String> {
    Some(
        match row {
            SettingsRowId::AppearanceMode => "settings-appearance-mode",
            SettingsRowId::Opacity => "settings-opacity",
            SettingsRowId::Blur => "settings-blur",
            SettingsRowId::BackgroundImage => "settings-background-image-choose",
            SettingsRowId::Density => "settings-density",
            SettingsRowId::TerminalTheme => "settings-current-theme",
            SettingsRowId::TerminalFontFamily => "settings-terminal-font-family",
            SettingsRowId::TerminalBaseSize => "settings-terminal-base-size",
            SettingsRowId::TerminalLineHeight => "settings-terminal-line-height",
            SettingsRowId::TerminalRegularWeight => "settings-row-terminal-regular-weight-control",
            SettingsRowId::TerminalBoldWeight => "settings-row-terminal-bold-weight-control",
            SettingsRowId::TerminalItalic => "settings-terminal-italic",
            SettingsRowId::TerminalBoldAsBright => "settings-terminal-bold-as-bright",
            SettingsRowId::InstalledThemes => "settings-installed-themes",
            SettingsRowId::MicrophoneAccess => "settings-microphone-access-control",
            SettingsRowId::ScreenRecordingAccess => "settings-screen-recording-access-control",
            SettingsRowId::AccessibilityAccess => "settings-accessibility-access-control",
            SettingsRowId::ClipboardWrites => "settings-clipboard-writes",
            SettingsRowId::ClipboardReads => "settings-clipboard-reads",
            SettingsRowId::ShowRepositoryStatus => "settings-show-repository-status",
            SettingsRowId::ShowPullRequests => "settings-show-pull-requests",
            SettingsRowId::GitTool => "settings-row-git-tool-control",
            SettingsRowId::GitHubCli => "settings-row-github-cli-control",
            SettingsRowId::WorktreeLocation => "settings-worktree-location-frame",
            // The fixture installs no update service, so this row presents no action control.
            SettingsRowId::UpdateStatus => return None,
            SettingsRowId::AutomaticUpdateDownloads => "settings-automatic-update-downloads",
            SettingsRowId::UpdateCheckInterval => "settings-update-check-interval",
            SettingsRowId::UpdateReminderInterval => "settings-update-reminder-interval",
            SettingsRowId::SettingsFile => "settings-file-frame",
            SettingsRowId::ExportSettings => "settings-document-export",
            SettingsRowId::ImportSettings => "settings-import",
            SettingsRowId::ResetAllSettings => "settings-reset-all",
            SettingsRowId::Shortcut(_) => {
                return Some(format!("{}-control", row.descriptor().selector));
            }
        }
        .to_owned(),
    )
}

/// Labels share a left edge, rendered controls share a right edge, and row boxes keep one inset.
#[gpui::test]
fn every_row_shares_one_left_edge_for_labels_and_one_right_edge_for_controls(
    cx: &mut TestAppContext,
) {
    let (window, _harness, cx) = open_settings(cx);

    for section in SettingsSectionId::ALL {
        select_section(section, cx);

        let mut left: Option<(SettingsRowId, gpui::Pixels)> = None;
        let mut right: Option<(SettingsRowId, gpui::Pixels)> = None;
        let mut control_right: Option<(SettingsRowId, gpui::Pixels)> = None;
        let mut inset: Option<(SettingsRowId, gpui::Pixels)> = None;
        for row in window.read_with(cx, |window, _| window.rows_for(section)) {
            let bounds = cx
                .debug_bounds(row.descriptor().selector)
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
            if let Some(selector) = rendered_control_selector(row) {
                let control = cx
                    .debug_bounds(leaked_owned(selector))
                    .unwrap_or_else(|| panic!("{row:?} should render its control"));
                if let Some((first, edge)) = control_right {
                    assert_eq!(
                        control.right(),
                        edge,
                        "{row:?} ends its control at a different edge than {first:?}"
                    );
                } else {
                    control_right = Some((row, control.right()));
                }
            } else {
                assert_eq!(
                    window.read_with(cx, |settings, cx| settings.update_status(cx).action),
                    None
                );
                for selector in [
                    "settings-update-check-now",
                    "settings-update-download",
                    "settings-update-restart",
                ] {
                    assert!(cx.debug_bounds(selector).is_none());
                }
            }
        }
        // The library's rows carry no label of their own: their group title names them. Every
        // section still shares the one right edge, which is what the loop above checked.
        assert!(right.is_some(), "{section:?} should present a row");
        assert!(
            control_right.is_some(),
            "{section:?} should present a control"
        );
    }
}

#[gpui::test]
fn guidance_sits_under_its_label_and_stops_before_the_control(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Themes, cx);

    let label = cx
        .debug_bounds("settings-row-appearance-mode-label")
        .expect("the appearance label should render");
    let description = cx
        .debug_bounds("settings-row-appearance-mode-description")
        .expect("the appearance guidance should render");
    let control = cx
        .debug_bounds("settings-appearance-mode")
        .expect("the appearance control should render");
    let row = cx
        .debug_bounds("settings-row-appearance-mode")
        .expect("the appearance row should render");
    let next = cx
        .debug_bounds("settings-row-terminal-theme")
        .expect("the theme in use should render under the appearance row");

    assert!(
        description.top() >= label.bottom(),
        "guidance should start below its label, got {description:?} after {label:?}"
    );
    assert!(
        description.right() <= control.left(),
        "guidance should stop before the control, got {description:?} against {control:?}"
    );
    assert!(
        description.bottom() <= row.bottom(),
        "guidance should stay inside its row, got {description:?} in {row:?}"
    );
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

/// Group cards keep their inset row separators and leave more space between cards than rows.
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
                .debug_bounds(descriptor.selector)
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

/// A selector shares the right edge and height of the stepper beside it.
#[gpui::test]
fn a_selector_aligns_with_the_stepper_beside_it(cx: &mut TestAppContext) {
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

    // Visit Interface and Font before measuring the controls each section contains.
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
                cx.debug_bounds(selector)
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

/// A family with one light and one dark theme, in that order.
const PAIRED_FAMILY: &[u8] = br##"{"name":"Paired","themes":[{"name":"Paired Light","appearance":"light","style":{}},{"name":"Paired Dark","appearance":"dark","style":{}}]}"##;
const IMPORTABLE_FAMILY: &[u8] = br##"{"name":"Sample","themes":[{"name":"Sample","appearance":"light","style":{"terminal.foreground":"#112233"}}]}"##;

pub(super) fn document_of(
    window: &Entity<SettingsWindow>,
    cx: &mut VisualTestContext,
) -> SettingsDocument {
    window.read_with(cx, |window, _| window.editor.document().clone())
}

fn installed_count(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> usize {
    window.read_with(cx, |window, _| {
        window.editor.document().terminal_themes.len()
    })
}

/// Leaks a composed selector for GPUI's static debug_bounds argument.
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

/// Starts the real debounced job and leaves it queued for background execution.
fn start_scheduled_write(
    window: &Entity<SettingsWindow>,
    storage: &MemoryStorage,
    cx: &mut VisualTestContext,
) -> crate::settings::storage::testing::BlockedWrite {
    let blocked = storage.block_next_write();
    // Advance time without draining the scheduled job into the blocked storage write.
    cx.cx
        .dispatcher
        .scheduler()
        .clock()
        .advance(COMMIT_DELAY * 2);
    while !window.read_with(cx, |settings, _| settings.editor.is_writing()) {
        assert!(
            cx.cx.dispatcher.tick(false),
            "the scheduled commit should start"
        );
    }
    assert_eq!(storage.writes(), 0);
    blocked
}

#[gpui::test]
fn closing_during_a_write_retains_the_window_until_the_newer_edit_is_saved(
    cx: &mut TestAppContext,
) {
    let (window, harness, cx) = open_settings(cx);
    cx.simulate_decorations(gpui::Decorations::Client {
        tiling: gpui::Tiling::default(),
    });
    cx.run_until_parked();
    click("settings-density-comfortable", cx);
    let blocked = start_scheduled_write(&window, &harness.storage, cx);
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.edit(
                |document| document.appearance.terminal.typography.base_size = 21.0,
                cx,
            )
        })
    });
    let handle = cx.update(|native, _| native.window_handle());

    cx.update(|native, cx| {
        let focused = native
            .focused(cx)
            .expect("the Settings control should own focus");
        focused.dispatch_action(&super::CloseSettingsWindow, native, cx);
    });
    assert!(cx.cx.update(|cx| cx.windows().contains(&handle)));
    assert!(window.read_with(cx, |settings, _| settings.close_after_save.is_some()));

    let release = std::thread::spawn(move || {
        blocked.wait_until_started();
        blocked.release();
    });
    cx.run_until_parked();
    release.join().unwrap();

    assert_eq!(
        harness
            .storage
            .document()
            .unwrap()
            .appearance
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
            document_of(&window, cx).appearance.window.density,
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
    let blocked = start_scheduled_write(&window, &harness.storage, cx);
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.edit(
                |document| document.appearance.terminal.typography.base_size = 21.0,
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
    assert_eq!(completions.get(), 0);
    let release = std::thread::spawn(move || {
        blocked.wait_until_started();
        blocked.release();
    });
    cx.run_until_parked();
    release.join().unwrap();

    assert_eq!(
        harness
            .storage
            .document()
            .unwrap()
            .appearance
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
            .appearance
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
    let blocked = start_scheduled_write(&window, &harness.storage, cx);
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.edit(
                |document| document.appearance.terminal.typography.base_size = 21.0,
                cx,
            )
        })
    });
    let release = std::thread::spawn(move || {
        blocked.wait_until_started();
        blocked.release();
    });
    // Shutdown must drive the queued job and its result publication before saving the newer edit.
    assert!(window.read_with(cx, |settings, _| settings.editor.is_writing()));
    assert_eq!(harness.storage.writes(), 0);
    cx.cx.update(|cx| cx.shutdown());
    release.join().unwrap();

    assert_eq!(
        harness
            .storage
            .document()
            .unwrap()
            .appearance
            .terminal
            .typography
            .base_size,
        21.0
    );
    assert_eq!(harness.storage.writes(), 2);
}

#[gpui::test]
fn resetting_theme_choices_survives_an_appearance_mode_round_trip(cx: &mut TestAppContext) {
    let mut document = SettingsDocument {
        terminal_themes: crate::appearance::translate_zed_family(PAIRED_FAMILY)
            .expect("fixture Zed family"),
        ..Default::default()
    };
    document.appearance.terminal.themes.light = document.terminal_themes[0].id.clone();
    document.appearance.terminal.themes.dark = document.terminal_themes[1].id.clone();
    let (window, _, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.editor.reset(
                crate::appearance::ResetTarget::TerminalTheme(Appearance::Light),
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
    let preferences = document_of(&window, cx).appearance;
    let defaults = SettingsDocument::default().appearance;
    assert_eq!(preferences.terminal.themes, defaults.terminal.themes);
}

#[gpui::test]
fn shared_mode_and_independent_slots_survive_save_reload_and_restart(cx: &mut TestAppContext) {
    let document = SettingsDocument {
        terminal_themes: crate::appearance::translate_zed_family(PAIRED_FAMILY)
            .expect("fixture Zed family"),
        ..Default::default()
    };
    let themes = [
        document.terminal_themes[0].id.clone(),
        document.terminal_themes[1].id.clone(),
    ];
    let (window, harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&document));
    select_section(SettingsSectionId::Themes, cx);
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.set_theme(Appearance::Light, themes[0].clone(), cx);
            settings.set_theme(Appearance::Dark, themes[1].clone(), cx);
        })
    });
    cx.run_until_parked();
    click("settings-appearance-mode-light", cx);
    click("settings-appearance-mode-auto", cx);
    let expected = document_of(&window, cx).appearance;
    settle(cx);
    assert_eq!(harness.storage.document().unwrap().appearance, expected);
    cx.update(|_, cx| window.update(cx, |settings, cx| settings.editor.reload(cx)));
    cx.run_until_parked();
    assert_eq!(document_of(&window, cx).appearance, expected);
    let restarted = crate::settings::Settings::load(harness.storage);
    assert_eq!(restarted.snapshot().candidate.appearance, expected);
}

#[gpui::test]
fn live_chrome_preview_preserves_settings_search_editor_and_focus(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    set_query(&window, "terminal", cx);
    let mut expected = document_of(&window, cx);
    expected.appearance.terminal.typography.base_size = 21.0;
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.edit(
                |document| document.appearance.terminal.typography.base_size = 21.0,
                cx,
            );
        });
    });
    assert_eq!(document_of(&window, cx), expected);
    expected.appearance.mode = AppearanceMode::Light;
    assert_eq!(harness.storage.writes(), 0);
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
    assert_eq!(document_of(&window, cx), expected);
    assert_eq!(harness.storage.writes(), 0);
    assert_eq!(
        cx.update(|_, cx| appearance_runtime::current(cx).chrome.appearance),
        Appearance::Light
    );
}

#[gpui::test]
fn settings_content_publishes_a_named_scroll_bar_before_it_is_revealed(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (_window, _harness, cx) = open_settings(cx);
    cx.simulate_resize(gpui::size(
        px(super::WINDOW_WIDTH),
        px(super::WINDOW_HEIGHT),
    ));
    select_section(SettingsSectionId::Keybindings, cx);
    let tree = A11yTree::read(cx);
    assert_eq!(tree.node("Settings content")["aria"]["role"], "ScrollBar");
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
    let light = document.appearance.terminal.themes.light.clone();
    let dark = document.appearance.terminal.themes.dark.clone();
    for (id, background) in [(light, Color::rgb(0xeeeecc)), (dark, Color::rgb(0x102030))] {
        document.appearance.terminal.overrides.insert(
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

pub(super) const REGISTRY_LISTING: &str =
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

pub(super) fn open_theme_store(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) {
    cx.update(|gpui_window, cx| {
        window.update(cx, |settings, cx| {
            settings.open_theme_store(gpui_window, cx)
        });
    });
    cx.run_until_parked();
}

pub(super) fn sample_registry() -> Arc<MemoryTransport> {
    Arc::new(
        MemoryTransport::default()
            .route(REGISTRY_LISTING, Ok(registry_listing("1.0.0")))
            .route(
                "https://api.zed.dev/extensions/sample-themes/1.0.0/download",
                Ok(registry_archive()),
            ),
    )
}

/// Removing any theme an extension installed removes every theme it installed, including those
/// of the other appearance. A slot that used one of them returns to its built-in theme.
#[gpui::test]
fn removing_a_theme_removes_every_theme_its_extension_installed(cx: &mut TestAppContext) {
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

    let row = leaked_owned(format!("settings-theme-row-{}", selected.as_str()));
    click(
        leaked_owned(format!("settings-theme-row-{}-remove", selected.as_str())),
        cx,
    );
    click("modal-action-settings-remove-theme-confirm", cx);

    let document = document_of(&window, cx);
    assert!(document.terminal_themes.is_empty());
    assert_eq!(
        document.appearance.terminal.themes.dark,
        SettingsDocument::default().appearance.terminal.themes.dark
    );
    assert!(cx.debug_bounds(row).is_none());
}

#[gpui::test]
fn reinstalling_an_extension_restores_its_removed_themes_without_changing_selection(
    cx: &mut TestAppContext,
) {
    let transport = sample_registry();
    let (window, _harness, cx) = open_settings_with_registry(cx, transport.clone());
    select_section(SettingsSectionId::Themes, cx);
    open_theme_store(&window, cx);
    click("settings-zed-extension-action-sample-themes", cx);
    click("modal-action-settings-theme-store-done", cx);
    let themes = document_of(&window, cx).terminal_themes;
    let light = themes
        .iter()
        .find(|theme| theme.name == "Sample Light")
        .unwrap()
        .id
        .clone();
    window.update(cx, |settings, cx| {
        settings.set_appearance_mode(AppearanceMode::Light, cx);
    });
    settle(cx);

    click(
        leaked_owned(format!("settings-theme-row-{}-remove", light.as_str())),
        cx,
    );
    click("modal-action-settings-remove-theme-confirm", cx);
    assert_eq!(installed_count(&window, cx), 0);
    let preferences = document_of(&window, cx).appearance;

    open_theme_store(&window, cx);
    click("settings-zed-extension-action-sample-themes", cx);

    let restored = document_of(&window, cx);
    assert_eq!(restored.terminal_themes, themes);
    assert_eq!(restored.appearance, preferences);
    assert_eq!(
        transport.requests(),
        [
            REGISTRY_LISTING,
            "https://api.zed.dev/extensions/sample-themes/1.0.0/download",
            "https://api.zed.dev/extensions/sample-themes/1.0.0/download",
        ]
    );
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
        crate::settings::export_settings(&document_of(&window, cx)).unwrap()
    );

    cx.update(|_, cx| {
        window.update(cx, |window, cx| {
            window.edit(
                |document| document.appearance.window.density = ChromeDensity::Comfortable,
                cx,
            );
        });
    });
    cx.run_until_parked();

    let text = settings_file_text(&window, cx);
    assert_eq!(
        text,
        crate::settings::export_settings(&document_of(&window, cx)).unwrap()
    );
    assert!(
        text.contains("comfortable"),
        "the text should follow the change"
    );
}

/// A compact settings file within the storage limit can print past the text view's limit.
#[gpui::test]
fn a_settings_file_too_large_to_show_says_so_instead_of_showing_an_earlier_one(
    cx: &mut TestAppContext,
) {
    let (window, harness, cx) = open_settings(cx);
    let file = install_settings_file(&harness, cx);
    select_section(SettingsSectionId::Advanced, cx);
    assert!(!settings_file_text(&window, cx).is_empty());

    let family = crate::appearance::translate_zed_family(IMPORTABLE_FAMILY).expect("fixture");
    let wide = "\u{1F600}";
    let themes = (0..512)
        .map(|index| {
            let mut theme = family[0].clone();
            theme.id = crate::appearance::ThemeId::new(format!("large.{index}")).unwrap();
            theme.name = format!("{}{index}", wide.repeat(125));
            theme.metadata.author = Some(wide.repeat(256));
            theme.metadata.license = Some(wide.repeat(256));
            theme.metadata.description = Some(wide.repeat(1024));
            theme
        })
        .collect();
    let document = SettingsDocument {
        terminal_themes: themes,
        ..SettingsDocument::default()
    };
    let compact = serde_json::to_vec(&document).unwrap();
    assert!(compact.len() <= 4 * 1024 * 1024);
    assert!(crate::settings::export_settings(&document).unwrap().len() > 4 * 1024 * 1024);
    harness.storage.save_bytes_elsewhere(compact);
    follow_outside_save(&file, cx);

    assert_eq!(document_of(&window, cx).terminal_themes.len(), 512);
    // The empty view shows its placeholder, which says why.
    assert_eq!(settings_file_text(&window, cx), "");
}

/// A full-width row holds a block rather than one centered line, so the block sits the same
/// distance from every edge of its row.
#[gpui::test]
fn the_settings_file_is_inset_equally_on_every_side(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Advanced, cx);
    let row = cx.debug_bounds("settings-row-settings-file").unwrap();
    let frame = cx.debug_bounds("settings-file-frame").unwrap();
    let edit = cx.debug_bounds("settings-file-edit").unwrap();

    let left = frame.left() - row.left();
    assert_eq!(frame.top() - row.top(), left);
    assert_eq!(row.right() - frame.right(), left);
    assert_eq!(row.bottom() - edit.bottom(), left);
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
    assert_eq!(
        harness.storage.writes(),
        writes,
        "an existing file is opened as it is"
    );
}

#[gpui::test]
fn edit_json_writes_a_missing_settings_file_before_opening_it(cx: &mut TestAppContext) {
    let (_window, harness, cx) = open_settings_with(cx, Arc::new(MemoryStorage::default()));
    let file = install_settings_file(&harness, cx);
    select_section(SettingsSectionId::Advanced, cx);

    click("settings-file-edit", cx);

    assert_eq!(
        harness
            .storage
            .document()
            .map(|document| document.appearance),
        Some(SettingsDocument::default().appearance)
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
                |document| document.appearance.window.density = ChromeDensity::Comfortable,
                cx,
            );
        });
    });
    cx.run_until_parked();

    click("settings-file-edit", cx);

    assert_eq!(
        harness
            .storage
            .document()
            .unwrap()
            .appearance
            .window
            .density,
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
    saved.appearance.window.density = ChromeDensity::Comfortable;

    harness.storage.save_elsewhere(&saved);
    follow_outside_save(&file, cx);

    assert_eq!(
        document_of(&window, cx).appearance.window.density,
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

    harness
        .storage
        .save_bytes_elsewhere(b"{ half typed".to_vec());
    follow_outside_save(&file, cx);

    assert_eq!(
        document_of(&window, cx),
        before,
        "the last valid settings stay in effect"
    );
    assert!(matches!(status(&window, cx), SaveStatus::Unavailable(_)));
    assert!(!window.read_with(cx, |window, _| window.editor.editable()));

    let mut fixed = SettingsDocument::default();
    fixed.appearance.window.density = ChromeDensity::Comfortable;
    harness.storage.save_elsewhere(&fixed);
    follow_outside_save(&file, cx);

    assert_eq!(status(&window, cx), SaveStatus::Saved);
    assert_eq!(
        document_of(&window, cx).appearance.window.density,
        ChromeDensity::Comfortable
    );
}

#[gpui::test]
fn reload_reads_a_save_the_watch_did_not_report(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Advanced, cx);
    let mut saved = SettingsDocument::default();
    saved.appearance.window.density = ChromeDensity::Comfortable;
    harness.storage.save_elsewhere(&saved);

    click("settings-file-reload", cx);

    assert_eq!(
        document_of(&window, cx).appearance.window.density,
        ChromeDensity::Comfortable
    );
    assert!(settings_file_text(&window, cx).contains("comfortable"));
    assert_eq!(status(&window, cx), SaveStatus::Saved);
}

#[gpui::test]
fn reload_writes_a_pending_change_before_reading_the_file(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Advanced, cx);
    cx.update(|_, cx| {
        window.update(cx, |window, cx| {
            window.edit(
                |document| document.appearance.window.density = ChromeDensity::Comfortable,
                cx,
            );
        });
    });
    cx.run_until_parked();

    click("settings-file-reload", cx);

    assert_eq!(
        harness
            .storage
            .document()
            .unwrap()
            .appearance
            .window
            .density,
        ChromeDensity::Comfortable
    );
    assert_eq!(
        document_of(&window, cx).appearance.window.density,
        ChromeDensity::Comfortable
    );
    assert_eq!(status(&window, cx), SaveStatus::Saved);
}

#[gpui::test]
fn reload_reports_a_malformed_file_and_recovers_from_a_fixed_one(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Advanced, cx);
    let before = document_of(&window, cx);

    harness
        .storage
        .save_bytes_elsewhere(b"{ half typed".to_vec());
    click("settings-file-reload", cx);

    assert_eq!(document_of(&window, cx), before);
    assert!(matches!(status(&window, cx), SaveStatus::Unavailable(_)));

    let mut fixed = SettingsDocument::default();
    fixed.appearance.window.density = ChromeDensity::Comfortable;
    harness.storage.save_elsewhere(&fixed);
    click("settings-file-reload", cx);

    assert_eq!(status(&window, cx), SaveStatus::Saved);
    assert_eq!(
        document_of(&window, cx).appearance.window.density,
        ChromeDensity::Comfortable
    );
}

fn exported_document_with_a_theme() -> Vec<u8> {
    let mut document = SettingsDocument::default();
    document.appearance.window.density = ChromeDensity::Comfortable;
    document.terminal_themes =
        crate::appearance::translate_zed_family(IMPORTABLE_FAMILY).expect("fixture Zed family");
    crate::settings::export_settings(&document)
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
    let mut before = SettingsDocument {
        revision: 12,
        terminal_themes: crate::appearance::translate_zed_family(PAIRED_FAMILY)
            .expect("fixture Zed family"),
        ..Default::default()
    };
    before.appearance.window.opacity = 0.2;
    before.appearance.window.blur = false;
    before.appearance.terminal.typography.base_size = 27.0;
    before.appearance.terminal.themes.light = before.terminal_themes[0].id.clone();
    before.appearance.terminal.themes.dark = before.terminal_themes[1].id.clone();
    before.updates.automatic_downloads = false;
    before.clipboard.allow_write = false;
    before.clipboard.allow_read = true;
    before.keybindings = serde_json::from_str(r#"{"new_workspace":"cmd-shift-y"}"#).unwrap();
    let imported = exported_document_with_a_theme();
    let mut expected = crate::settings::parse_settings(&imported).unwrap();
    expected.revision = before.revision + 1;
    let (window, harness, cx) = open_settings_with(cx, MemoryStorage::with_document(&before));
    select_section(SettingsSectionId::Advanced, cx);

    finish_import(&window, Ok(imported), cx);
    assert!(
        cx.update(|native, cx| spaceterm_ui::window_modal_is_open(native, cx)),
        "the import should ask before replacing anything"
    );
    assert_eq!(document_of(&window, cx), before);
    assert_eq!(harness.storage.document().as_ref(), Some(&before));
    assert_eq!(harness.storage.writes(), 0);
    click("modal-action-settings-import-confirm", cx);
    settle(cx);

    let retained = harness.storage.document().unwrap();
    assert_eq!(
        retained.appearance.window.density,
        ChromeDensity::Comfortable
    );
    assert_eq!(retained.terminal_themes.len(), 1);
    assert_eq!(retained, expected);
    assert_eq!(harness.storage.writes(), 1);
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
        Ok(b"{\"appearance\": {}}".to_vec()),
        Ok(IMPORTABLE_FAMILY.to_vec()),
        Err(super::import::ImportError::TooLarge),
        Err(super::import::ImportError::Unreadable),
    ] {
        finish_import(&window, read, cx);
        assert!(
            cx.debug_bounds("modal-action-settings-import-failed-ok")
                .is_some()
        );
        assert!(
            cx.debug_bounds("modal-action-settings-import-confirm")
                .is_none()
        );
        click("modal-action-settings-import-failed-ok", cx);
    }
    settle(cx);

    assert_eq!(document_of(&window, cx), before);
    assert_eq!(harness.storage.writes(), writes);
}

#[gpui::test]
fn git_switches_save_reset_and_hiding_repository_status_locks_pull_requests(
    cx: &mut TestAppContext,
) {
    let (window, harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Git, cx);
    assert_eq!(document_of(&window, cx).git, Default::default());

    click("settings-show-repository-status", cx);
    settle(cx);
    assert!(
        !harness
            .storage
            .document()
            .unwrap()
            .git
            .show_repository_status
    );
    click("settings-show-pull-requests", cx);
    settle(cx);
    assert!(
        harness.storage.document().unwrap().git.show_pull_requests,
        "the Pull Requests switch is disabled while Repository Status is hidden"
    );

    click("settings-row-show-repository-status-reset", cx);
    click("settings-show-pull-requests", cx);
    settle(cx);
    let saved = harness.storage.document().unwrap().git;
    assert!(saved.show_repository_status);
    assert!(!saved.show_pull_requests);
    set_query(&window, "pull request", cx);
    assert!(cx.debug_bounds("settings-row-show-pull-requests").is_some());
}

#[gpui::test]
fn the_worktree_location_saves_valid_templates_and_explains_invalid_ones(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Git, cx);
    let saved = |harness: &Harness| {
        harness
            .storage
            .document()
            .map(|document| document.git.worktree_path_template)
    };
    let default = crate::worktrees::path_template::DEFAULT_WORKTREE_PATH_TEMPLATE;
    assert!(
        cx.debug_bounds("settings-row-worktree-location-reset")
            .is_none()
    );

    click("settings-worktree-location", cx);
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("~/wt/{repository}");
    settle(cx);
    let explained = cx
        .debug_bounds("settings-worktree-location-invalid")
        .is_some();
    cx.simulate_keystrokes("enter");
    settle(cx);
    let refused = document_of(&window, cx).git.worktree_path_template;
    cx.simulate_input("/{branch}");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let accepted = saved(&harness);
    click("settings-row-worktree-location-reset", cx);
    settle(cx);

    assert!(
        explained,
        "a template without {{branch}} is explained as it is typed"
    );
    assert_eq!(
        refused, default,
        "Return keeps an invalid template out of Settings"
    );
    assert_eq!(accepted.as_deref(), Some("~/wt/{repository}/{branch}"));
    assert_eq!(saved(&harness).as_deref(), Some(default));
    assert!(
        cx.debug_bounds("settings-worktree-location-invalid")
            .is_none()
    );
}

#[gpui::test]
fn git_tool_rows_follow_the_reported_tool_status(cx: &mut TestAppContext) {
    use crate::repository_status::{GitHubCliStatus, GitToolStatus, ToolVersion};
    use crate::ui::repository_status_store::RepositoryTools;

    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Git, cx);
    assert!(cx.debug_bounds("settings-git-tool-state-unknown").is_some());
    assert!(
        cx.debug_bounds("settings-github-cli-state-unknown")
            .is_some()
    );

    cx.update(|_, cx| {
        cx.set_global(RepositoryTools {
            git: GitToolStatus::Ready(ToolVersion {
                major: 2,
                minor: 47,
                patch: 0,
            }),
            github_cli: GitHubCliStatus::SignedOut,
        });
    });
    cx.run_until_parked();

    assert!(cx.debug_bounds("settings-git-tool-state-ready").is_some());
    assert!(
        cx.debug_bounds("settings-github-cli-state-signed-out")
            .is_some()
    );
}

#[gpui::test]
fn clipboard_privacy_switches_save_reset_and_remain_searchable(cx: &mut TestAppContext) {
    let (window, harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Privacy, cx);
    assert!(document_of(&window, cx).clipboard.allow_write);
    assert!(!document_of(&window, cx).clipboard.allow_read);
    click("settings-clipboard-reads", cx);
    click("settings-clipboard-writes", cx);
    settle(cx);
    let saved = harness.storage.document().unwrap();
    assert!(saved.clipboard.allow_read);
    assert!(!saved.clipboard.allow_write);
    click("settings-row-clipboard-reads-reset", cx);
    click("settings-row-clipboard-writes-reset", cx);
    settle(cx);
    assert_eq!(
        harness.storage.document().unwrap().clipboard,
        Default::default()
    );
    set_query(&window, "osc52", cx);
    assert!(cx.debug_bounds("settings-row-clipboard-reads").is_some());
    assert!(cx.debug_bounds("settings-row-clipboard-writes").is_some());
}

#[gpui::test]
fn a_desktop_without_host_features_omits_their_surfaces_but_keeps_clipboard_privacy(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        appearance_runtime::install(
            crate::settings::Settings::load(MemoryStorage::with_document(
                &SettingsDocument::default(),
            )),
            Rc::new(RecordingAppearancePlatform::default()),
            cx,
        )
        .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        let presentation = crate::desktop_profile::DesktopPresentation::get(cx)
            .clone()
            .without_features(&[
                HostFeature::Updates,
                HostFeature::MicrophoneAccess,
                HostFeature::SystemPermissions,
            ]);
        cx.set_global(presentation);
    });
    let (window, cx) = cx.add_window_view(SettingsWindow::new);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    window.read_with(cx, |settings, _| {
        assert_eq!(
            settings.reset_all_detail(),
            "This cannot be undone. Themes can be installed again from their Zed extension or file."
        );
        assert!(
            settings
                .navigable_sections()
                .contains(&SettingsSectionId::Privacy)
        );
        assert!(
            !settings
                .navigable_sections()
                .contains(&SettingsSectionId::Updates)
        );
    });
    assert!(
        cx.debug_bounds("settings-navigation-settings-section-privacy")
            .is_some()
    );
    assert!(
        cx.debug_bounds("settings-navigation-settings-section-updates")
            .is_none()
    );
    select_section(SettingsSectionId::Privacy, cx);
    for omitted in [
        "settings-row-microphone-access",
        "settings-row-screen-recording-access",
        "settings-row-accessibility-access",
    ] {
        assert!(cx.debug_bounds(omitted).is_none(), "{omitted} rendered");
    }
    assert!(cx.debug_bounds("settings-row-clipboard-reads").is_some());
    assert!(cx.debug_bounds("settings-row-clipboard-writes").is_some());
    for query in [
        "microphone",
        "updates",
        "screen recording",
        "device control",
        "accessibility",
    ] {
        set_query(&window, query, cx);
        window.read_with(cx, |settings, _| {
            assert!(settings.matching_rows().is_empty(), "{query} matched");
            assert!(settings.navigable_sections().is_empty());
            assert_eq!(settings.revealed, None);
        });
    }
}

#[gpui::test]
fn a_desktop_with_every_feature_presents_unavailable_host_features(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);

    window.read_with(cx, |settings, _| {
        let accessibility = settings.permission_access.row(crate::platform::permission_access::SystemPermission::Accessibility).copy().name;
        assert_eq!(settings.reset_all_detail(), format!("This cannot be undone. Themes can be installed again from their Zed extension or file. Microphone, Screen Recording, and {accessibility} access are system permissions and are not affected."));
    });

    assert!(
        cx.debug_bounds("settings-navigation-settings-section-updates")
            .is_some()
    );
    select_section(SettingsSectionId::Privacy, cx);
    for unavailable in [
        "settings-row-microphone-access",
        "settings-row-screen-recording-access",
        "settings-row-accessibility-access",
    ] {
        assert!(
            cx.debug_bounds(unavailable).is_some(),
            "{unavailable} omitted"
        );
    }
    for query in ["microphone", "updates", "screen recording", "accessibility"] {
        set_query(&window, query, cx);
        window.read_with(cx, |settings, _| {
            assert!(
                !settings.matching_rows().is_empty(),
                "{query} matched nothing"
            );
        });
    }
}

struct RecordingMovement;

impl crate::platform::window_movement::WindowMovementFactory for RecordingMovement {
    fn create(&self) -> Rc<dyn OperatingSystemWindowDragPlatform> {
        Rc::new(RecordingOperatingSystemWindowDragPlatform::default())
    }
}

#[gpui::test]
fn opening_settings_from_its_own_window_keeps_one_window(cx: &mut TestAppContext) {
    let settings =
        crate::settings::Settings::load(MemoryStorage::with_document(&SettingsDocument::default()));
    cx.update(|cx| {
        appearance_runtime::install(
            settings,
            Rc::new(RecordingAppearancePlatform::default()),
            cx,
        )
        .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        super::configure_window_chrome(Rc::new(RecordingMovement), Default::default(), None, cx);
        super::open_or_activate(None, cx);
    });
    cx.run_until_parked();
    let settings_windows = |cx: &mut TestAppContext| {
        cx.windows()
            .into_iter()
            .filter_map(|window| window.downcast::<SettingsWindow>())
            .collect::<Vec<_>>()
    };
    let [opened] = settings_windows(cx)[..] else {
        panic!("Settings should open one window");
    };

    // The Settings shortcut dispatches inside Settings once Settings is the main window.
    opened
        .update(cx, |_, _, cx| super::open_or_activate(None, cx))
        .expect("Settings should stay open");
    cx.run_until_parked();

    assert_eq!(settings_windows(cx).len(), 1);
}

#[gpui::test]
fn keyboard_shortcuts_opens_settings_at_keybindings_and_moves_an_open_window_there(
    cx: &mut TestAppContext,
) {
    let settings =
        crate::settings::Settings::load(MemoryStorage::with_document(&SettingsDocument::default()));
    cx.update(|cx| {
        appearance_runtime::install(
            settings,
            Rc::new(RecordingAppearancePlatform::default()),
            cx,
        )
        .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        super::configure_window_chrome(Rc::new(RecordingMovement), Default::default(), None, cx);
        super::init(cx);
    });
    let settings_window = |cx: &mut TestAppContext| {
        let windows = cx
            .windows()
            .into_iter()
            .filter_map(|window| window.downcast::<SettingsWindow>())
            .collect::<Vec<_>>();
        let [window] = windows[..] else {
            panic!(
                "Settings should have exactly one window, found {}",
                windows.len()
            );
        };
        window
    };
    let active_section = |window: gpui::WindowHandle<SettingsWindow>, cx: &mut TestAppContext| {
        window
            .read_with(cx, |settings, _| settings.active_section())
            .expect("Settings should stay open")
    };

    cx.update(|cx| cx.dispatch_action(&super::OpenKeyboardShortcuts));
    cx.run_until_parked();
    let opened = settings_window(cx);
    assert_eq!(active_section(opened, cx), SettingsSectionId::Keybindings);

    opened
        .update(cx, |settings, _, cx| {
            settings.select_section(SettingsSectionId::Font, cx);
        })
        .expect("Settings should stay open");
    cx.update(|cx| cx.dispatch_action(&super::OpenSettings));
    cx.run_until_parked();
    assert_eq!(
        active_section(opened, cx),
        SettingsSectionId::Font,
        "Open Settings keeps the section in view"
    );

    cx.update(|cx| cx.dispatch_action(&super::OpenKeyboardShortcuts));
    cx.run_until_parked();
    assert_eq!(settings_window(cx), opened);
    assert_eq!(active_section(opened, cx), SettingsSectionId::Keybindings);
}

#[gpui::test]
fn client_window_controls_share_the_search_row_and_its_edge_inset(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    cx.simulate_decorations(gpui::Decorations::Client {
        tiling: gpui::Tiling::default(),
    });
    let edge_margin = px(spaceterm_ui::DesktopWindowStyle::Adwaita
        .control_metrics()
        .edge_margin);
    let center_y = |bounds: gpui::Bounds<gpui::Pixels>| bounds.origin.y + bounds.size.height / 2.0;

    // Trailing controls leave no empty strip above Search, and Close keeps the same inset from the
    // top edge as from the trailing edge.
    cx.simulate_button_layout(Some(gpui::WindowButtonLayout {
        left: [None; 3],
        right: [Some(gpui::WindowButton::Close), None, None],
    }));
    cx.run_until_parked();
    let surface = cx.debug_bounds("settings-window-surface").unwrap();
    let close = cx.debug_bounds("window-close").unwrap();
    let search = cx.debug_bounds("settings-search-frame").unwrap();
    assert_eq!(surface.right() - close.right(), edge_margin);
    assert_eq!(close.top() - surface.top(), edge_margin);
    assert_eq!(search.top() - surface.top(), px(10.0));
    assert_eq!(center_y(close), center_y(search));

    // Leading controls take their own row above Search on that same center line.
    cx.simulate_button_layout(Some(gpui::WindowButtonLayout {
        left: [Some(gpui::WindowButton::Close), None, None],
        right: [None; 3],
    }));
    cx.run_until_parked();
    let close = cx.debug_bounds("window-close").unwrap();
    let search = cx.debug_bounds("settings-search-frame").unwrap();
    let titlebar = cx.debug_bounds("settings-sidebar-titlebar").unwrap();
    assert_eq!(close.left() - surface.left(), edge_margin);
    assert_eq!(close.top() - surface.top(), edge_margin);
    assert!(search.top() >= titlebar.bottom());
    assert_eq!(search.top() - titlebar.bottom(), px(10.0));
}

/// Counts About requests, as the application's own handler would receive them.
fn count_about_requests(cx: &mut VisualTestContext) -> Rc<std::cell::Cell<usize>> {
    let requests = Rc::new(std::cell::Cell::new(0));
    let counted = requests.clone();
    cx.update(|_, cx| {
        cx.on_action(move |_: &crate::app::ShowAboutApplication, _| counted.set(counted.get() + 1));
    });
    requests
}

#[gpui::test]
fn about_sits_apart_at_the_foot_of_the_sidebar_and_requests_about(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    let requests = count_about_requests(cx);

    let sidebar = cx.debug_bounds("settings-sidebar").unwrap();
    let navigation = cx.debug_bounds("settings-navigation").unwrap();
    let about = cx
        .debug_bounds("settings-about")
        .expect("Settings offers About");
    let section_row = cx
        .debug_bounds("settings-navigation-settings-section-interface")
        .unwrap();
    assert!(
        about.top() > navigation.bottom(),
        "About follows the sections"
    );
    assert!(about.bottom() <= sidebar.bottom());
    // It rests in the band the content footer draws across the window's bottom edge.
    let footer = cx.debug_bounds("settings-footer").unwrap();
    assert!((about.center().y - footer.center().y).abs() < px(0.5));
    assert_eq!(footer.bottom(), sidebar.bottom());
    assert_eq!(about.left(), section_row.left());
    assert_eq!(about.size, section_row.size);

    cx.simulate_mouse_move(about.center(), None, gpui::Modifiers::none());
    cx.simulate_click(about.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(requests.get(), 1);
    // A click is not keyboard traversal, so it leaves no focus ring behind.
    assert!(cx.debug_bounds("settings-about-keyboard-focus").is_none());
}

#[gpui::test]
fn about_is_a_named_button_for_assistive_technology(cx: &mut TestAppContext) {
    let (_window, _harness, cx) = open_settings(cx);
    let requests = count_about_requests(cx);
    cx.activate_accessibility();
    let tree: serde_json::Value = cx
        .update(|window, _| serde_json::from_str(&window.debug_a11y_tree_json().unwrap()).unwrap());
    let label = crate::keybindings::Command::About.label();
    let (_, node) = tree["nodes"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, node)| node["aria"]["label"] == label)
        .unwrap_or_else(|| panic!("{label} must be exposed to screen readers"));
    assert_eq!(node["aria"]["role"], "Button");

    cx.simulate_accessibility_action(gpui::accesskit::ActionRequest {
        action: gpui::AccessibleAction::Click,
        target_tree: gpui::accesskit::TreeId::ROOT,
        target_node: gpui::accesskit::NodeId(
            node["accesskit_id"].as_str().unwrap().parse().unwrap(),
        ),
        data: None,
    });
    cx.run_until_parked();
    assert_eq!(requests.get(), 1);
}

#[gpui::test]
fn about_follows_the_sections_in_keyboard_order_and_activates_from_the_keyboard(
    cx: &mut TestAppContext,
) {
    let (window, _harness, cx) = open_settings(cx);
    let requests = count_about_requests(cx);
    let list_focused = |cx: &mut VisualTestContext| {
        cx.update(|gpui_window, cx| {
            window
                .read(cx)
                .navigation
                .list_focus()
                .is_focused(gpui_window)
        })
    };

    // Search, then the section list, then About.
    cx.simulate_keystrokes("cmd-f tab");
    cx.run_until_parked();
    assert!(list_focused(cx));
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert!(!list_focused(cx));
    assert!(
        cx.debug_bounds("settings-about-keyboard-focus").is_some(),
        "keyboard focus on About must show"
    );

    cx.simulate_keystrokes("space");
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("space").unwrap(),
    });
    cx.run_until_parked();
    assert_eq!(requests.get(), 1);
    press_return(cx);
    assert_eq!(requests.get(), 2);

    cx.simulate_keystrokes("shift-tab");
    cx.run_until_parked();
    assert!(list_focused(cx));
}

#[gpui::test]
fn settings_sections_publish_a_list_that_selects_on_press(cx: &mut TestAppContext) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    let (window, _harness, cx) = open_settings(cx);
    let tree = A11yTree::read(cx);
    let list = tree.node("Sections");
    assert_eq!(list["aria"]["role"], "ListBox");
    let font = tree.node("Font");
    assert_eq!(font["aria"]["role"], "ListBoxOption");
    assert_eq!(font["aria"]["selected"], false);

    perform(cx, font, Action::Click);
    assert_eq!(
        window.read_with(cx, |window, _| window.active_section),
        SettingsSectionId::Font
    );
    assert_eq!(A11yTree::read(cx).node("Font")["aria"]["selected"], true);
}

#[gpui::test]
fn about_publishes_one_button_that_requests_the_about_window(cx: &mut TestAppContext) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    let (_window, _harness, cx) = open_settings(cx);
    let requests = count_about_requests(cx);
    let tree = A11yTree::read(cx);
    let about = tree
        .with_role("Button")
        .into_iter()
        .filter(|button| button["aria"]["label"] == "About SpaceTerm")
        .collect::<Vec<_>>();
    assert_eq!(about.len(), 1);

    perform(cx, about[0], Action::Click);
    cx.run_until_parked();
    assert_eq!(requests.get(), 1);
}

#[gpui::test]
fn settings_publish_headings_row_titles_and_guidance_in_reading_order(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (_window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Font, cx);
    let tree = A11yTree::read(cx);
    let headings = tree
        .with_role("Heading")
        .into_iter()
        .map(|heading| {
            (
                heading["aria"]["label"].as_str().unwrap(),
                heading["aria"]["level"].as_u64().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    for heading in [
        ("Font", 1),
        ("Typeface", 2),
        ("Weight", 2),
        ("Rendering", 2),
    ] {
        assert!(headings.contains(&heading), "{heading:?} in {headings:?}");
    }

    select_section(SettingsSectionId::Themes, cx);
    let tree = A11yTree::read(cx);
    let reading = tree
        .in_order()
        .into_iter()
        .map(|node| {
            let text = node["aria"]["label"]
                .as_str()
                .or_else(|| node["aria"]["value"].as_str())
                .unwrap_or_default();
            format!("{} {text}", node["aria"]["role"].as_str().unwrap())
        })
        .collect::<Vec<_>>();
    let position = |entry: &str| {
        reading
            .iter()
            .position(|candidate| candidate == entry)
            .unwrap_or_else(|| panic!("{entry:?} is not in {reading:?}"))
    };
    assert!(position("Heading Themes") < position("Label Appearance"));
    // Guidance stacks under its row title, beside the control it explains.
    let guidance = position("Label Auto matches the system light or dark setting.");
    assert!(position("Label Appearance") < guidance);
    assert!(guidance < position("RadioGroup Appearance"));
}

#[gpui::test]
fn a_fixed_appearance_publishes_the_current_theme_and_its_origin(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Themes, cx);
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.set_appearance_mode(AppearanceMode::Light, cx)
        })
    });
    let tree = A11yTree::read(cx);
    let current = tree.node("Current theme");
    assert_eq!(current["aria"]["role"], "Group");
    let text = tree
        .children(current)
        .into_iter()
        .filter_map(|node| node["aria"]["value"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(text, ["SpaceTerm Light", "Built into SpaceTerm"]);

    set_query(&window, "zzzz", cx);
    let tree = A11yTree::read(cx);
    assert!(
        tree.with_role("Label")
            .iter()
            .any(|label| label["aria"]["value"] == "No settings match “zzzz”.")
    );
}

#[gpui::test]
fn save_status_and_the_failure_banner_publish_their_text(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (_window, harness, cx) = open_settings(cx);
    let labels = |cx: &mut VisualTestContext| {
        A11yTree::read(cx)
            .with_role("Label")
            .into_iter()
            .filter_map(|label| label["aria"]["value"].as_str().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    assert!(labels(cx).contains(&"All changes saved".to_owned()));

    harness.storage.fail_writes(Some(StorageError::Unavailable));
    click("settings-density-comfortable", cx);
    settle(cx);
    let text = labels(cx);
    for expected in [
        "Could not save your changes",
        "The change is still applied. Retry to write it to your settings file.",
    ] {
        assert!(
            text.contains(&expected.to_owned()),
            "{expected:?} in {text:?}"
        );
    }
}

#[gpui::test]
fn settings_focus_reaches_assistive_technology(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (_window, _harness, cx) = open_settings(cx);
    let tree = A11yTree::read(cx);
    let focused = tree
        .focused()
        .expect("the focused surface publishes a node");
    assert_eq!(focused["aria"]["role"], "Group");
}

#[gpui::test]
fn a_search_that_orders_a_group_apart_should_still_show_it_as_one_card(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (window, _harness, cx) = open_settings(cx);
    select_section(SettingsSectionId::Git, cx);
    let _ = A11yTree::read(cx);
    // Ordered by match, these rows alternate between the Repository Status and Pull Requests
    // groups. Two cards for one group would publish the same accessibility node twice.
    set_query(&window, "s", cx);
    let rows = cx.update(|_, cx| window.read(cx).rows_for(SettingsSectionId::Git));
    let groups: Vec<_> = rows.iter().map(|row| row.descriptor().group).collect();
    assert_eq!(
        groups,
        [
            "Repository Status",
            "Pull Requests",
            "Pull Requests",
            "Repository Status",
            "Worktrees"
        ]
    );

    let tree = A11yTree::read(cx);
    assert!(tree.find("Show Repository Status").is_some());
    assert!(tree.find("Show Pull Requests").is_some());
}

fn open_settings_presenting_background_images(
    cx: &mut TestAppContext,
) -> (
    Entity<SettingsWindow>,
    Harness,
    Arc<crate::background_image::BackgroundImageStore>,
    &mut VisualTestContext,
) {
    let store = Arc::new(crate::background_image::testing::store().0);
    let (window, harness, cx) = open_settings_with_capabilities(
        cx,
        MemoryStorage::with_document(&SettingsDocument::default()),
        Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
        None,
        Some(Arc::clone(&store)),
    );
    (window, harness, store, cx)
}

/// A desktop whose windows cannot present a Background Image offers no row to choose one.
#[gpui::test]
fn a_desktop_without_background_images_offers_no_row(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings(cx);
    assert!(
        !window
            .read_with(cx, |window, _| window
                .rows_for(SettingsSectionId::Interface))
            .contains(&SettingsRowId::BackgroundImage)
    );
}

#[gpui::test]
fn a_desktop_presenting_background_images_offers_to_choose_one(cx: &mut TestAppContext) {
    let (window, _harness, _store, cx) = open_settings_presenting_background_images(cx);
    assert!(
        window
            .read_with(cx, |window, _| window
                .rows_for(SettingsSectionId::Interface))
            .contains(&SettingsRowId::BackgroundImage)
    );
    assert!(
        cx.debug_bounds("settings-background-image-choose")
            .is_some()
    );
    assert!(
        cx.debug_bounds("settings-row-background-image-reset")
            .is_none(),
        "no image is chosen, so there is nothing to reset"
    );
}

/// A chosen image is named by its copy until the person resets the row, and then the copy goes
/// too.
#[gpui::test]
fn a_chosen_image_is_retained_until_the_row_is_reset(cx: &mut TestAppContext) {
    let (window, harness, store, cx) = open_settings_presenting_background_images(cx);
    let id = store
        .install(crate::background_image::testing::PNG)
        .expect("the fixture image is kept");

    cx.update(|native, cx| {
        window.update(cx, |settings, cx| {
            let choice = settings.begin_background_image_choice();
            settings.finish_background_image(choice, Ok(id), native, cx);
        });
    });
    settle(cx);
    assert_eq!(
        harness
            .storage
            .document()
            .and_then(|document| document.appearance.window.background_image),
        Some(id)
    );

    click("settings-row-background-image-reset", cx);
    settle(cx);
    assert_eq!(
        harness
            .storage
            .document()
            .and_then(|document| document.appearance.window.background_image),
        None
    );
    assert!(
        cx.debug_bounds("settings-row-background-image-reset")
            .is_none()
    );
    assert_eq!(
        store.load(id).err(),
        Some(crate::background_image::BackgroundImageError::Missing),
        "the copy no Settings name is discarded"
    );
}

#[gpui::test]
fn an_unusable_image_is_explained_and_changes_nothing(cx: &mut TestAppContext) {
    let (window, harness, _store, cx) = open_settings_presenting_background_images(cx);
    let before = document_of(&window, cx);

    cx.update(|native, cx| {
        window.update(cx, |settings, cx| {
            let choice = settings.begin_background_image_choice();
            settings.finish_background_image(
                choice,
                Err(super::background_image::ChooseError::Keep(
                    crate::background_image::BackgroundImageError::UnsupportedFormat,
                )),
                native,
                cx,
            );
        });
    });
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("modal-action-settings-background-image-failed-ok")
            .is_some(),
        "the failure should be explained"
    );
    click("modal-action-settings-background-image-failed-ok", cx);
    settle(cx);
    assert_eq!(document_of(&window, cx), before);
    assert_eq!(harness.storage.writes(), 0);
}

/// Resetting the row while a choice is still copying an image keeps the image removed.
#[gpui::test]
fn a_choice_that_completes_after_a_reset_changes_nothing(cx: &mut TestAppContext) {
    let (window, harness, store, cx) = open_settings_presenting_background_images(cx);
    let first = store
        .install(crate::background_image::testing::PNG)
        .expect("the fixture image is kept");
    let second = store
        .install(crate::background_image::testing::OTHER_PNG)
        .expect("the fixture image is kept");
    cx.update(|native, cx| {
        window.update(cx, |settings, cx| {
            let choice = settings.begin_background_image_choice();
            settings.finish_background_image(choice, Ok(first), native, cx);
        });
    });
    settle(cx);

    let slow = window.update(cx, |settings, _| settings.begin_background_image_choice());
    click("settings-row-background-image-reset", cx);
    cx.update(|native, cx| {
        window.update(cx, |settings, cx| {
            settings.finish_background_image(slow, Ok(second), native, cx);
        });
    });
    settle(cx);

    assert_eq!(
        harness
            .storage
            .document()
            .and_then(|document| document.appearance.window.background_image),
        None
    );
}
