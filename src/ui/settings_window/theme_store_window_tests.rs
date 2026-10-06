use gpui::{Entity, SharedString, TestAppContext, VisualTestContext};

use crate::appearance::Appearance;
use crate::settings::SettingsDocument;

use super::super::tests::{
    REGISTRY_LISTING, click, document_of, open_settings_with_registry, open_theme_store,
    sample_registry, select_section,
};
use super::super::{SettingsSectionId, SettingsWindow};
use super::Listing;
use crate::theme_registry::testing::MemoryTransport;
use std::sync::Arc;

fn listing(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> Listing {
    window.read_with(cx, |settings, cx| {
        settings.theme_store.read(cx).listing.clone()
    })
}

fn status(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> Option<SharedString> {
    window.read_with(cx, |settings, cx| {
        settings.theme_store.read(cx).status.clone()
    })
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
    assert!(matches!(listing(&window, cx), Listing::Loaded(extensions) if extensions.len() == 2));
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
        status(&window, cx).as_deref(),
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

/// Remove removes every theme the extension installed at once, because the sheet cannot stack a
/// confirmation.
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
        status(&window, cx).as_deref(),
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
    assert!(matches!(listing(&window, cx), Listing::Loading));
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
        listing(&window, cx),
        Listing::Failed(crate::theme_registry::RegistryError::Refused)
    ));

    click("settings-theme-store-retry", cx);
    assert_eq!(transport.requests().len(), 2);
}

#[gpui::test]
fn get_more_themes_publishes_named_extensions_and_reports_the_install(cx: &mut TestAppContext) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    let (window, _harness, cx) = open_settings_with_registry(cx, sample_registry());
    select_section(SettingsSectionId::Themes, cx);
    open_theme_store(&window, cx);
    let tree = A11yTree::read(cx);
    let list = tree.node("Zed theme extensions");
    assert_eq!(list["aria"]["role"], "List");
    let sample = tree.node("Sample Themes");
    assert_eq!(sample["aria"]["role"], "ListItem");
    assert!(
        sample["aria"]["description"]
            .as_str()
            .unwrap()
            .contains("downloads")
    );
    let get = tree
        .children(sample)
        .into_iter()
        .find(|node| node["aria"]["role"] == "Button")
        .expect("the extension offers Get");
    assert_eq!(get["aria"]["label"], "Get");

    perform(cx, get, Action::Click);
    cx.run_until_parked();
    let tree = A11yTree::read(cx);
    let text = tree
        .with_role("Label")
        .into_iter()
        .filter_map(|label| label["aria"]["value"].as_str())
        .collect::<Vec<_>>();
    assert!(
        text.contains(&"Installed 2 themes from Sample Themes."),
        "{text:?}"
    );
    let actions = tree
        .children(tree.node("Sample Themes"))
        .into_iter()
        .map(|node| {
            node["aria"]["label"]
                .as_str()
                .or_else(|| node["aria"]["value"].as_str())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(actions, ["Installed", "Remove"]);
}
