//! The Developer Workbench's appearance preview.
//!
//! A preview is an uncommitted Settings Document that every window renders until the developer
//! cancels or commits it. It never writes the settings file on its own. The Settings Document
//! allows one preview at a time, so while this one is open the Settings Window cannot save, and
//! closing the Developer Workbench cancels it.

use crate::appearance::{
    AppearanceMode, ChromeDensity, ResetTarget, SettingsDocument, TerminalFontFamily,
    parse_settings,
};
use crate::settings::{CommitOutcome, PreviewToken, ThemeImport, UserSettings};

/// The terminal typography the alternate-typography switch previews.
const ALTERNATE_BASE_SIZE: f32 = 22.0;
const ALTERNATE_LINE_HEIGHT: f32 = 1.35;

/// Why a preview operation did not apply. Each message names the operation and no document
/// content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PreviewError {
    /// Another preview or a commit holds the Settings Document, or its revision moved on.
    Busy,
    /// The document failed validation.
    Rejected,
    /// The text is not a Settings Document.
    InvalidDocument,
    /// The text is not a Zed theme family, or the catalog moved on.
    InvalidThemeFamily,
    /// There is no preview to act on.
    NoPreview,
    /// Reload waits until the preview is cancelled or committed.
    PreviewOpen,
    /// The settings file could not be read.
    ReloadFailed,
    /// The settings file could not be written.
    SaveFailed,
}

impl PreviewError {
    pub(super) const fn message(self) -> &'static str {
        match self {
            Self::Busy => "Another preview or a save holds the settings. Cancel it first.",
            Self::Rejected => "The preview was rejected.",
            Self::InvalidDocument => "The text is not a valid Settings Document.",
            Self::InvalidThemeFamily => "The text is not a Zed theme family SpaceTerm can install.",
            Self::NoPreview => "There is no preview to commit.",
            Self::PreviewOpen => "Cancel or commit the preview before reloading.",
            Self::ReloadFailed => "Reload failed. The last saved settings remain.",
            Self::SaveFailed => "The save failed. The preview remains open.",
        }
    }
}

pub(super) struct AppearancePreview {
    settings: UserSettings,
    alternate_font_family: String,
    token: Option<PreviewToken>,
}

impl AppearancePreview {
    pub(super) fn new(settings: UserSettings, alternate_font_family: String) -> Self {
        Self {
            settings,
            alternate_font_family,
            token: None,
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.token.is_some()
    }

    /// The document every window renders: the preview while one is open, else the saved one.
    pub(super) fn document(&self) -> SettingsDocument {
        (*self.settings.snapshot().candidate).clone()
    }

    pub(super) fn export(&self) -> Option<String> {
        self.settings.export_document().ok()
    }

    /// Opens the preview on first use. Later edits replace its candidate.
    fn token(&mut self) -> Result<&PreviewToken, PreviewError> {
        if self.token.is_none() {
            let revision = self.settings.snapshot().committed.revision;
            self.token = Some(
                self.settings
                    .begin_preview(revision)
                    .map_err(|_| PreviewError::Busy)?,
            );
        }
        Ok(self.token.as_ref().expect("the preview was opened above"))
    }

    pub(super) fn edit(
        &mut self,
        edit: impl FnOnce(&mut SettingsDocument),
    ) -> Result<(), PreviewError> {
        let mut candidate = self.document();
        edit(&mut candidate);
        let settings = self.settings.clone();
        settings
            .update_preview(self.token()?, candidate)
            .map_err(|_| PreviewError::Rejected)
    }

    pub(super) fn set_mode(&mut self, mode: AppearanceMode) -> Result<(), PreviewError> {
        self.edit(|document| document.preferences.mode = mode)
    }

    /// Light becomes Dark; Dark and Auto become Light.
    pub(super) fn toggle_mode(&mut self) -> Result<AppearanceMode, PreviewError> {
        let mode = match self.document().preferences.mode {
            AppearanceMode::Light => AppearanceMode::Dark,
            AppearanceMode::Dark | AppearanceMode::Auto => AppearanceMode::Light,
        };
        self.set_mode(mode).map(|()| mode)
    }

    pub(super) fn set_density(&mut self, density: ChromeDensity) -> Result<(), PreviewError> {
        self.edit(|document| document.preferences.window.density = density)
    }

    pub(super) fn set_transparency(&mut self, transparency: f32) -> Result<(), PreviewError> {
        self.edit(|document| document.preferences.window.transparency = transparency)
    }

    pub(super) fn set_blur(&mut self, blur: bool) -> Result<(), PreviewError> {
        self.edit(|document| document.preferences.window.blur = blur)
    }

    pub(super) fn set_bold_as_bright(&mut self, enabled: bool) -> Result<(), PreviewError> {
        self.edit(|document| document.preferences.terminal.rendering.bold_as_bright = enabled)
    }

    /// Whether the terminal typography is the alternate one this preview offers.
    pub(super) fn alternate_typography(&self, document: &SettingsDocument) -> bool {
        matches!(
            &document.preferences.terminal.typography.family,
            TerminalFontFamily::Named { family } if family == &self.alternate_font_family
        )
    }

    /// Switches the terminal between its default typography and a larger, looser alternate, so
    /// a capture shows metrics changing without a font install.
    pub(super) fn set_alternate_typography(&mut self, alternate: bool) -> Result<(), PreviewError> {
        let defaults = SettingsDocument::default().preferences.terminal.typography;
        let alternate_font_family = self.alternate_font_family.clone();
        self.edit(|document| {
            let typography = &mut document.preferences.terminal.typography;
            if alternate {
                typography.family = TerminalFontFamily::Named {
                    family: alternate_font_family,
                };
                typography.base_size = ALTERNATE_BASE_SIZE;
                typography.line_height = ALTERNATE_LINE_HEIGHT;
            } else {
                typography.family = defaults.family;
                typography.base_size = defaults.base_size;
                typography.line_height = defaults.line_height;
            }
        })
    }

    pub(super) fn reset(&mut self, target: ResetTarget) -> Result<(), PreviewError> {
        let settings = self.settings.clone();
        settings
            .reset_preview(self.token()?, target)
            .map_err(|_| PreviewError::Rejected)
    }

    pub(super) fn apply_document(&mut self, text: &str) -> Result<(), PreviewError> {
        let document =
            parse_settings(text.as_bytes()).map_err(|_| PreviewError::InvalidDocument)?;
        let settings = self.settings.clone();
        settings
            .update_preview(self.token()?, document)
            .map_err(|_| PreviewError::Rejected)
    }

    /// Installs a Zed theme family into the preview without selecting any of its themes.
    pub(super) fn install_theme_family(&mut self, text: &str) -> Result<usize, PreviewError> {
        let settings = self.settings.clone();
        // Opening the preview retires the catalog revision, so it is read after the preview opens.
        let token = self.token()?;
        let catalog_revision = settings.snapshot().catalog_revision;
        settings
            .import_preview(
                token,
                catalog_revision,
                ThemeImport::ZedFamily(text.as_bytes()),
            )
            .map(|receipt| receipt.installed.len())
            .map_err(|_| PreviewError::InvalidThemeFamily)
    }

    pub(super) fn cancel(&mut self) -> Result<(), PreviewError> {
        let Some(token) = self.token.as_ref() else {
            return Ok(());
        };
        self.settings
            .cancel_preview(token)
            .map_err(|_| PreviewError::Busy)?;
        self.token = None;
        Ok(())
    }

    /// Saves the preview and closes it. A failed save keeps the preview open under the same
    /// token, so the developer can retry or cancel.
    pub(super) fn commit(&mut self) -> Result<CommitOutcome, PreviewError> {
        let token = self.token.as_ref().ok_or(PreviewError::NoPreview)?;
        let job = self
            .settings
            .commit_preview(token)
            .map_err(|_| PreviewError::Busy)?;
        let outcome = job.run().map_err(|_| PreviewError::SaveFailed)?;
        self.token = None;
        Ok(outcome)
    }

    pub(super) fn reload(&self) -> Result<(), PreviewError> {
        if self.token.is_some() {
            return Err(PreviewError::PreviewOpen);
        }
        self.settings
            .reload()
            .map_err(|_| PreviewError::ReloadFailed)
    }
}

impl Drop for AppearancePreview {
    fn drop(&mut self) {
        let _ = self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::settings::storage::StorageError;
    use crate::ui::settings_window::test_support::MemoryStorage;

    fn preview() -> AppearancePreview {
        AppearancePreview::new(
            UserSettings::load(Arc::new(super::super::tests::ReadOnlyStorage)),
            "Fixture Mono".into(),
        )
    }

    #[test]
    fn edits_open_one_preview_and_cancel_restores_the_saved_document() {
        let mut preview = preview();
        let saved = preview.document();
        assert!(!preview.is_open());

        preview.set_mode(AppearanceMode::Light).unwrap();
        preview.set_density(ChromeDensity::Comfortable).unwrap();

        assert!(preview.is_open());
        assert_eq!(preview.document().preferences.mode, AppearanceMode::Light);
        assert_eq!(
            preview.document().preferences.window.density,
            ChromeDensity::Comfortable
        );
        preview.cancel().unwrap();
        assert!(!preview.is_open());
        assert_eq!(preview.document(), saved);
    }

    #[test]
    fn toggling_the_mode_alternates_light_and_dark() {
        let mut preview = preview();
        preview.set_mode(AppearanceMode::Auto).unwrap();

        assert_eq!(preview.toggle_mode(), Ok(AppearanceMode::Light));
        assert_eq!(preview.toggle_mode(), Ok(AppearanceMode::Dark));
        assert_eq!(preview.toggle_mode(), Ok(AppearanceMode::Light));
    }

    #[test]
    fn alternate_typography_round_trips_to_the_defaults() {
        let mut preview = preview();
        let defaults = SettingsDocument::default().preferences.terminal.typography;

        preview.set_alternate_typography(true).unwrap();
        assert!(preview.alternate_typography(&preview.document()));
        assert_eq!(
            preview.document().preferences.terminal.typography.family,
            TerminalFontFamily::Named { family: "Fixture Mono".into() },
        );
        preview.set_alternate_typography(false).unwrap();

        assert!(!preview.alternate_typography(&preview.document()));
        assert_eq!(preview.document().preferences.terminal.typography, defaults);
    }

    #[test]
    fn invalid_text_is_rejected_without_opening_a_preview() {
        let mut preview = preview();

        assert_eq!(
            preview.apply_document("not json"),
            Err(PreviewError::InvalidDocument)
        );
        assert!(!preview.is_open());
        assert_eq!(preview.commit().err(), Some(PreviewError::NoPreview));
    }

    #[test]
    fn commit_saves_the_preview_and_closes_it() {
        let storage = MemoryStorage::with_document(&SettingsDocument::default());
        let mut preview = AppearancePreview::new(UserSettings::load(storage.clone()), "Fixture Mono".into());
        preview.set_mode(AppearanceMode::Light).unwrap();

        assert!(preview.commit().is_ok());

        assert!(!preview.is_open());
        assert_eq!(storage.writes(), 1);
        assert_eq!(
            storage.document().unwrap().preferences.mode,
            AppearanceMode::Light
        );
    }

    #[test]
    fn a_failed_commit_keeps_the_preview_open() {
        let storage = MemoryStorage::with_document(&SettingsDocument::default());
        let mut preview = AppearancePreview::new(UserSettings::load(storage.clone()), "Fixture Mono".into());
        preview.set_mode(AppearanceMode::Light).unwrap();
        storage.fail_writes(Some(StorageError::Unavailable));

        assert_eq!(preview.commit().err(), Some(PreviewError::SaveFailed));

        assert!(preview.is_open());
        assert_eq!(preview.document().preferences.mode, AppearanceMode::Light);
        storage.fail_writes(None);
        assert!(preview.commit().is_ok());
        assert!(!preview.is_open());
    }

    #[test]
    fn a_theme_family_installs_without_an_open_preview() {
        let mut preview = preview();
        let family = r##"{"name":"Sample","themes":[{"name":"Sample","appearance":"light","style":{}}]}"##;

        assert_eq!(preview.install_theme_family(family), Ok(1));

        assert!(preview.is_open());
        assert_eq!(preview.document().terminal_themes.len(), 1);
    }

    #[test]
    fn reload_waits_for_the_preview_to_close() {
        let mut preview = preview();
        preview.set_blur(false).unwrap();

        assert_eq!(preview.reload(), Err(PreviewError::PreviewOpen));
    }

    #[test]
    fn dropping_the_preview_releases_the_settings_document() {
        let settings = UserSettings::load(Arc::new(super::super::tests::ReadOnlyStorage));
        let mut preview = AppearancePreview::new(settings.clone(), "Fixture Mono".into());
        preview.set_blur(false).unwrap();
        drop(preview);

        let revision = settings.snapshot().committed.revision;
        assert!(settings.begin_preview(revision).is_ok());
    }
}
