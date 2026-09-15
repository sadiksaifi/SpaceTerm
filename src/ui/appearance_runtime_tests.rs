use super::*;
use crate::appearance::{Appearance, AppearanceDocument, AppearanceMode, SchemeId};
use crate::appearance::{ResolvedAppearance, SurfaceRole};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::secure_filesystem::{PrivateFileSnapshot, SecureEntryIdentity};
use crate::settings::storage::{SettingsStorage, StorageCommit, StorageError};
use gpui::TestAppContext;

fn shell(resolved: &ResolvedAppearance) -> u8 {
    resolved
        .chrome
        .composition
        .materials
        .alpha(SurfaceRole::Base)
}

fn floating(resolved: &ResolvedAppearance) -> u8 {
    resolved
        .chrome
        .composition
        .materials
        .alpha(SurfaceRole::Floating)
}

#[derive(Default)]
struct PreviewStorage(Option<Vec<u8>>);
impl SettingsStorage for PreviewStorage {
    fn read(&self) -> Result<Option<PrivateFileSnapshot>, StorageError> {
        Ok(self.0.as_ref().map(|bytes| PrivateFileSnapshot {
            bytes: bytes.clone(),
            identity: SecureEntryIdentity::from_opaque(1_u64),
        }))
    }
    fn write(
        &self,
        _: &[u8],
        _: Option<&SecureEntryIdentity>,
    ) -> Result<StorageCommit, StorageError> {
        panic!("preview must not write settings")
    }
}

fn start(cx: &mut TestAppContext) -> (UserSettings, RecordingAppearancePlatform) {
    let (settings, changed) = UserSettings::load(Arc::new(PreviewStorage::default()));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        install(settings.clone(), changed, Rc::new(platform.clone()), cx).unwrap();
        crate::ui::initialize_controls(cx).unwrap();
    });
    (settings, platform)
}

/// A blurred Workspace asks the framework only for a transparent window: SpaceTerm installs the
/// native backdrop itself, and takes it away again when the composition resolves opaque.
#[gpui::test]
fn window_owner_installs_the_application_backdrop_and_removes_it_with_the_effect(
    cx: &mut TestAppContext,
) {
    let (_settings, platform) = start(cx);
    platform.set_transparency_supported(true);
    cx.run_until_parked();
    let test_window = cx.add_window(|_, _| gpui::EmptyView);
    let mut owner = WindowAppearanceOwner::default();
    test_window
        .update(cx, |_, window, cx| {
            owner.apply(window, cx);
            assert_eq!(
                window_background(cx),
                gpui::WindowBackgroundAppearance::Transparent
            );
        })
        .unwrap();
    assert_eq!(platform.backdrops.borrow().as_slice(), &[true]);

    platform.set_transparency_supported(false);
    cx.run_until_parked();
    test_window
        .update(cx, |_, window, cx| {
            owner.apply(window, cx);
            assert_eq!(
                window_background(cx),
                gpui::WindowBackgroundAppearance::Opaque
            );
        })
        .unwrap();
    assert_eq!(platform.backdrops.borrow().as_slice(), &[true, false]);

    // Re-applying an unchanged composition costs no native work.
    test_window
        .update(cx, |_, window, cx| owner.apply(window, cx))
        .unwrap();
    assert_eq!(platform.backdrops.borrow().len(), 2);
}

#[gpui::test]
fn transparency_updates_surfaces_and_accessibility_fallback_without_terminal_protocol_changes(
    cx: &mut TestAppContext,
) {
    let (settings, platform) = start(cx);
    platform.set_transparency_supported(true);
    cx.run_until_parked();
    let before = cx.update(|cx| current(cx));
    assert!(shell(&before) > 0 && shell(&before) < 255);
    assert!(floating(&before) > shell(&before));
    assert_eq!(
        before.chrome.composition.effective,
        crate::appearance::WindowBackgroundAppearance::Blurred
    );
    let token = settings.begin_preview(0).unwrap();
    let mut document = AppearanceDocument::default();
    document.preferences.background.transparency = 0.5;
    document.preferences.background.blur = false;
    settings.update_preview(&token, document).unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let after = current(cx);
        assert!(shell(&after) > 0 && shell(&after) < shell(&before));
        assert!(floating(&after) < floating(&before));
        // Floating surfaces covering content stay denser than the base between the endpoints.
        assert!(floating(&after) > shell(&after));
        assert_eq!(
            after.chrome.composition.effective,
            crate::appearance::WindowBackgroundAppearance::Transparent
        );
        let chrome = crate::ui::appearance::chrome(cx);
        assert_eq!(
            chrome
                .surface(SurfaceRole::Sheet, chrome.colors.background)
                .a,
            after.chrome.composition.materials.alpha(SurfaceRole::Sheet)
        );
        assert!(
            chrome
                .surface(SurfaceRole::Sheet, chrome.colors.background)
                .a
                < shell(&after)
        );
        assert_eq!(chrome.colors.text.a, 255);
        assert_eq!(after.terminal, before.terminal);
        assert!(!AppearanceChangeSet::between(&before, &after).terminal_protocol_colors);
    });
    let preview_shell = cx.update(|cx| shell(&current(cx)));
    platform.set_transparency_supported(false);
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(current(cx).chrome.composition.materials.is_opaque());
        assert_eq!(
            window_background(cx),
            gpui::WindowBackgroundAppearance::Opaque
        );
    });
    platform.set_transparency_supported(true);
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(shell(&current(cx)), preview_shell));
    drop(token);
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(shell(&current(cx)), shell(&before)));
}

#[gpui::test]
fn preview_cancel_restores_the_committed_mode_using_current_system_fact(cx: &mut TestAppContext) {
    let (settings, platform) = start(cx);
    let token = settings.begin_preview(0).unwrap();
    let mut candidate = AppearanceDocument::default();
    candidate.preferences.mode = AppearanceMode::Auto;
    settings.update_preview(&token, candidate).unwrap();
    platform.set_system_appearance(Some(Appearance::Light));
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(current(cx).chrome.appearance, Appearance::Light);
        assert_eq!(current(cx).terminal.appearance, Appearance::Light);
    });
    drop(token);
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(current(cx).chrome.appearance, Appearance::Dark));
    assert_eq!(platform.applied.borrow().last(), Some(&Appearance::Dark));
    // The forced application appearance never changes the captured Operating-System fact.
    assert_eq!(platform.system_appearance(), Some(Appearance::Light));
}

#[gpui::test]
fn identical_effective_colors_still_publish_requested_fallback_and_diagnostics(
    cx: &mut TestAppContext,
) {
    let (settings, _) = start(cx);
    let token = settings.begin_preview(0).unwrap();
    let mut candidate = AppearanceDocument::default();
    candidate.preferences.chrome.schemes.dark = SchemeId::new("custom.missing").unwrap();
    settings.update_preview(&token, candidate).unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let resolved = current(cx);
        assert_eq!(resolved.chrome.requested_scheme.as_str(), "custom.missing");
        assert_eq!(
            resolved.chrome.effective_scheme.as_str(),
            "builtin.vague-pro.chrome.dark"
        );
        assert!(!resolved.diagnostics.is_empty());
    });
}

#[gpui::test]
fn terminal_only_preview_does_not_replace_control_catalog_or_force_native_chrome(
    cx: &mut TestAppContext,
) {
    let (settings, platform) = start(cx);
    let (chrome_before, controls_before) = cx.update(|cx| {
        (
            Arc::clone(&cx.global::<InstalledChrome>().0),
            cx.global::<spaceterm_ui::ControlThemeCatalog>()
                .installed_generation(),
        )
    });
    let native_calls = platform.applied.borrow().len();
    let token = settings.begin_preview(0).unwrap();
    let mut candidate = AppearanceDocument::default();
    candidate.preferences.terminal.typography.base_size = 24.0;
    settings.update_preview(&token, candidate).unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(Arc::ptr_eq(
            &chrome_before,
            &cx.global::<InstalledChrome>().0
        ));
        assert_eq!(
            cx.global::<spaceterm_ui::ControlThemeCatalog>()
                .installed_generation(),
            controls_before
        );
        assert_eq!(current(cx).terminal.typography.cell_size, 24.0);
    });
    assert_eq!(platform.applied.borrow().len(), native_calls);
}

#[gpui::test]
fn repeated_system_notifications_without_effective_change_do_not_publish(cx: &mut TestAppContext) {
    let (_, platform) = start(cx);
    let previous = cx.update(|cx| current(cx));
    for _ in 0..32 {
        platform.set_system_appearance(Some(Appearance::Dark));
    }
    cx.run_until_parked();
    cx.update(|cx| assert!(Arc::ptr_eq(&previous, &current(cx))));
}

#[gpui::test]
fn cancelling_fixed_preview_resolves_committed_auto_mode_again(cx: &mut TestAppContext) {
    let mut committed = AppearanceDocument::default();
    committed.preferences.mode = AppearanceMode::Auto;
    let bytes = crate::appearance::export_settings(&committed)
        .unwrap()
        .into_bytes();
    let (settings, changed) = UserSettings::load(Arc::new(PreviewStorage(Some(bytes))));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        install(settings.clone(), changed, Rc::new(platform.clone()), cx).unwrap();
        crate::ui::initialize_controls(cx).unwrap();
    });
    let token = settings.begin_preview(0).unwrap();
    settings
        .update_preview(&token, AppearanceDocument::default())
        .unwrap();
    platform.set_system_appearance(Some(Appearance::Light));
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(current(cx).chrome.appearance, Appearance::Dark));
    drop(token);
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(current(cx).chrome.appearance, Appearance::Light));
}

#[gpui::test]
fn chrome_palette_and_typography_preview_preserve_native_classification_and_terminal(
    cx: &mut TestAppContext,
) {
    let (settings, platform) = start(cx);
    let before = cx.update(|cx| current(cx));
    let native_calls = platform.applied.borrow().len();
    let token = settings.begin_preview(0).unwrap();
    let mut candidate = AppearanceDocument::default();
    candidate.preferences.chrome.typography.base_size = 17.0;
    candidate.preferences.chrome.overrides.insert(
        before.chrome.effective_scheme.clone(),
        crate::appearance::ChromeColorOverrides {
            background: Some(crate::appearance::Color::rgb(0x20252a)),
            ..Default::default()
        },
    );
    settings.update_preview(&token, candidate).unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let after = current(cx);
        assert_eq!(after.terminal, before.terminal);
        assert_ne!(after.chrome.colors, before.chrome.colors);
        assert_ne!(after.chrome.typography, before.chrome.typography);
    });
    assert_eq!(platform.applied.borrow().len(), native_calls);
    drop(token);
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(current(cx).chrome, before.chrome));
    assert_eq!(platform.applied.borrow().len(), native_calls);
}
