//! Theme store integration through its owning child fixture and actual Settings controls.

use gpui::TestAppContext;

use crate::appearance::Appearance;
use crate::settings::SettingsDocument;

use super::super::SettingsSectionId;
use super::super::tests::{
    REGISTRY_LISTING, click, document_of, open_settings_with_registry, open_theme_store,
    sample_registry, select_section,
};
use super::Listing;
use crate::theme_registry::testing::MemoryTransport;
use std::sync::Arc;

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
        window.read_with(cx, |settings, cx| settings.theme_store.read(cx).listing.clone()),
        Listing::Loaded(extensions) if extensions.len() == 2
    ));
    let preferences = document_of(&window, cx).appearance;

    click("settings-zed-extension-action-sample-themes", cx);

    let document = document_of(&window, cx);
    let mut names = document
        .terminal_themes
        .iter()
        .map(|theme| theme.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(names, ["Sample Dark", "Sample Light"]);
    assert_eq!(document.appearance, preferences);
    assert_eq!(
        window
            .read_with(cx, |settings, cx| settings
                .theme_store
                .read(cx)
                .status
                .clone())
            .as_deref(),
        Some("Installed 2 themes from Sample Themes.")
    );
    assert!(
        cx.debug_bounds("settings-zed-extension-action-sample-themes")
            .is_none(),
        "the listed version is already installed"
    );
    assert!(
        cx.debug_bounds("settings-zed-extension-installed-sample-themes")
            .is_some()
    );
    assert!(
        cx.debug_bounds("settings-zed-extension-remove-sample-themes")
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

/// Remove in Get More Themes removes every theme the extension installed at once, because the
/// sheet cannot stack a confirmation, and offers Get again. A slot that used one of them returns
/// to its built-in theme.
#[gpui::test]
fn removing_an_extension_from_the_sheet_removes_its_themes(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings_with_registry(cx, sample_registry());
    select_section(SettingsSectionId::Themes, cx);
    open_theme_store(&window, cx);
    click("settings-zed-extension-action-sample-themes", cx);
    let selected = document_of(&window, cx)
        .terminal_themes
        .iter()
        .find(|theme| theme.name == "Sample Dark")
        .map(|theme| theme.id.clone())
        .expect("the dark theme is installed");
    window.update(cx, |settings, cx| {
        settings.set_theme(Appearance::Dark, selected, cx);
    });
    cx.run_until_parked();

    click("settings-zed-extension-remove-sample-themes", cx);

    let document = document_of(&window, cx);
    assert!(document.terminal_themes.is_empty());
    assert_eq!(
        document.appearance.terminal.themes.dark,
        SettingsDocument::default().appearance.terminal.themes.dark
    );
    assert_eq!(
        window
            .read_with(cx, |settings, cx| settings
                .theme_store
                .read(cx)
                .status
                .clone())
            .as_deref(),
        Some("Removed 2 themes from Sample Themes.")
    );
    assert!(
        cx.debug_bounds("settings-zed-extension-action-sample-themes")
            .is_some()
    );
}

/// While the listing loads, the sheet shows an indeterminate bar with its caption beneath it.
#[gpui::test]
fn a_loading_registry_listing_shows_a_bar_above_its_caption(cx: &mut TestAppContext) {
    let (window, _harness, cx) = open_settings_with_registry(cx, sample_registry());
    select_section(SettingsSectionId::Themes, cx);

    // The listing arrives on a background task, so the frame drawn before the executor parks is
    // the loading state.
    cx.update(|gpui_window, cx| {
        window.update(cx, |settings, cx| {
            settings.open_theme_store(gpui_window, cx)
        });
    });
    assert!(matches!(
        window.read_with(cx, |settings, cx| settings
            .theme_store
            .read(cx)
            .listing
            .clone()),
        Listing::Loading
    ));
    let bar = cx
        .debug_bounds("settings-theme-store-loading-track")
        .expect("the loading listing shows a bar");
    assert!(
        cx.debug_bounds("settings-theme-store-loading-activity")
            .is_some()
    );
    let caption = cx
        .debug_bounds("settings-theme-store-loading-caption")
        .expect("the loading listing shows its caption");
    assert!(
        caption.top() >= bar.bottom(),
        "the caption sits below the bar"
    );

    cx.run_until_parked();
    assert!(
        cx.debug_bounds("settings-theme-store-loading-track")
            .is_none()
    );
}

#[gpui::test]
fn a_failed_registry_listing_offers_a_retry(cx: &mut TestAppContext) {
    let transport = Arc::new(MemoryTransport::default());
    let (window, _harness, cx) = open_settings_with_registry(cx, transport.clone());
    select_section(SettingsSectionId::Themes, cx);

    open_theme_store(&window, cx);
    assert!(matches!(
        window.read_with(cx, |settings, cx| settings
            .theme_store
            .read(cx)
            .listing
            .clone()),
        Listing::Failed(crate::theme_registry::RegistryError::Refused)
    ));

    click("settings-theme-store-retry", cx);
    assert_eq!(transport.requests().len(), 2);
}
