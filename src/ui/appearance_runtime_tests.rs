use super::*;
use crate::appearance::{Appearance, AppearanceDocument, SchemeId, SchemeSelection};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::secure_filesystem::{PrivateFileSnapshot, SecureEntryIdentity};
use crate::settings::storage::{SettingsStorage, StorageCommit, StorageError};
use gpui::TestAppContext;

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

#[gpui::test]
fn preview_cancel_reresolves_committed_policy_using_current_system_fact(cx: &mut TestAppContext) {
    let (settings, platform) = start(cx);
    let token = settings.begin_preview(0).unwrap();
    let mut candidate = AppearanceDocument::default();
    candidate.preferences.chrome.scheme = SchemeSelection::System {
        light: SchemeId::new("builtin.spaceterm.chrome.light").unwrap(),
        dark: SchemeId::new("builtin.vague-pro.chrome.dark").unwrap(),
    };
    settings.update_preview(&token, candidate).unwrap();
    platform.set_system_appearance(Some(Appearance::Light));
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(current(cx).chrome.appearance, Appearance::Light);
        assert_eq!(current(cx).terminal.appearance, Appearance::Dark);
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
    candidate.preferences.chrome.scheme = SchemeSelection::Fixed {
        id: SchemeId::new("custom.missing").unwrap(),
        appearance: Appearance::Dark,
    };
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
fn cancelling_fixed_preview_resolves_committed_system_policy_again(cx: &mut TestAppContext) {
    let mut committed = AppearanceDocument::default();
    committed.preferences.chrome.scheme = SchemeSelection::System {
        light: SchemeId::new("builtin.spaceterm.chrome.light").unwrap(),
        dark: SchemeId::new("builtin.vague-pro.chrome.dark").unwrap(),
    };
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
