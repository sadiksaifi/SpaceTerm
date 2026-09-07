use std::path::Path;

use super::FilePreviewTarget;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FilePreviewError {
    StaleTarget,
    OffMainThread,
    PlatformUnavailable,
}

/// Only presentation mechanics cross this capability boundary.
pub(crate) trait FilePreviewPanel {
    fn preview_file(&mut self, path: &Path) -> Result<(), FilePreviewError>;
    fn dismiss(&mut self);
}

pub(crate) trait FilePreviewFactory {
    fn create(&self) -> Box<dyn FilePreviewPanel>;
}

pub(crate) trait FilePreviewPlatform {
    fn preview(&mut self, target: &FilePreviewTarget) -> Result<(), FilePreviewError>;
    fn dismiss(&mut self);
}

/// Owns revalidation, failure cleanup, replacement and Pane teardown policy.
pub(crate) struct FilePreviewPresenter<P: FilePreviewPanel> {
    pub(super) panel: P,
}

impl<P: FilePreviewPanel> FilePreviewPresenter<P> {
    pub(crate) const fn new(panel: P) -> Self {
        Self { panel }
    }
}

impl FilePreviewPanel for Box<dyn FilePreviewPanel> {
    fn preview_file(&mut self, path: &Path) -> Result<(), FilePreviewError> {
        (**self).preview_file(path)
    }
    fn dismiss(&mut self) {
        (**self).dismiss();
    }
}

impl<P: FilePreviewPanel> FilePreviewPlatform for FilePreviewPresenter<P> {
    fn preview(&mut self, target: &FilePreviewTarget) -> Result<(), FilePreviewError> {
        let Some(path) = target.revalidated_path() else {
            self.panel.dismiss();
            return Err(FilePreviewError::StaleTarget);
        };
        if let Err(error) = self.panel.preview_file(&path) {
            self.panel.dismiss();
            return Err(error);
        }
        Ok(())
    }
    fn dismiss(&mut self) {
        self.panel.dismiss();
    }
}

impl<P: FilePreviewPanel> Drop for FilePreviewPresenter<P> {
    fn drop(&mut self) {
        self.panel.dismiss();
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::terminal::{HyperlinkTarget, TerminalLocalFileCapabilities};

    const LOCAL_FILES: TerminalLocalFileCapabilities = TerminalLocalFileCapabilities::Enabled;

    #[test]
    fn preview_failure_and_owner_teardown_release_presentation() {
        use std::cell::Cell;
        use std::rc::Rc;
        struct Panel(Rc<Cell<usize>>);
        impl FilePreviewPanel for Panel {
            fn preview_file(&mut self, _: &Path) -> Result<(), FilePreviewError> {
                Err(FilePreviewError::PlatformUnavailable)
            }
            fn dismiss(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let directory =
            std::env::temp_dir().join(format!("spaceterm-preview-failure-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let file = directory.join("fixture");
        fs::write(&file, b"fixture").unwrap();
        let link = HyperlinkTarget::osc8("file:fixture", &directory, None, LOCAL_FILES).unwrap();
        let target = FilePreviewTarget::from_link(&link, LOCAL_FILES).unwrap();
        let dismissals = Rc::new(Cell::new(0));
        {
            let mut presenter = FilePreviewPresenter::new(Panel(dismissals.clone()));
            assert_eq!(
                presenter.preview(&target),
                Err(FilePreviewError::PlatformUnavailable)
            );
            assert_eq!(dismissals.get(), 1);
        }
        assert_eq!(dismissals.get(), 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[derive(Default)]
    struct RecordingPanel {
        previews: Vec<PathBuf>,
        dismissals: usize,
    }

    impl FilePreviewPanel for RecordingPanel {
        fn preview_file(&mut self, path: &Path) -> Result<(), FilePreviewError> {
            self.previews.push(path.to_path_buf());
            Ok(())
        }

        fn dismiss(&mut self) {
            self.dismissals += 1;
        }
    }

    #[test]
    fn presenter_submits_exactly_one_revalidated_regular_file() {
        let directory = std::env::temp_dir().join(format!(
            "spaceterm-file-preview-platform-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let file = directory.join("preview.txt");
        fs::write(&file, b"preview").unwrap();
        let link =
            HyperlinkTarget::osc8("file:preview.txt", &directory, None, LOCAL_FILES).unwrap();
        let target = FilePreviewTarget::from_link(&link, LOCAL_FILES).unwrap();
        let mut presenter = FilePreviewPresenter::new(RecordingPanel::default());

        let result = presenter.preview(&target);

        assert_eq!(result, Ok(()));
        assert_eq!(presenter.panel.previews, vec![file.canonicalize().unwrap()]);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn file_preview_target_rejects_web_links_before_the_platform_boundary() {
        let link = HyperlinkTarget::url("https://example.test/file.txt").unwrap();

        let target = FilePreviewTarget::from_link(&link, LOCAL_FILES);

        assert_eq!(target, None);
    }

    #[test]
    fn file_preview_target_rejects_a_missing_file_before_the_platform_boundary() {
        let directory = std::env::temp_dir().join(format!(
            "spaceterm-file-preview-platform-missing-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let file = directory.join("preview.txt");
        fs::write(&file, b"preview").unwrap();
        let link =
            HyperlinkTarget::osc8("file:preview.txt", &directory, None, LOCAL_FILES).unwrap();
        fs::remove_file(file).unwrap();

        let target = FilePreviewTarget::from_link(&link, LOCAL_FILES);

        assert_eq!(target, None);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn file_preview_target_rejects_a_directory_before_the_platform_boundary() {
        let directory = std::env::temp_dir().join(format!(
            "spaceterm-file-preview-platform-directory-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();

        let target = HyperlinkTarget::osc8("file:.", &directory, None, LOCAL_FILES)
            .and_then(|link| FilePreviewTarget::from_link(&link, LOCAL_FILES));

        assert_eq!(target, None);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn platform_error_identifiers_carry_no_target_content() {
        assert_eq!(
            [
                FilePreviewError::StaleTarget,
                FilePreviewError::OffMainThread,
                FilePreviewError::PlatformUnavailable,
            ]
            .map(|error| format!("{error:?}")),
            [
                "StaleTarget".to_owned(),
                "OffMainThread".to_owned(),
                "PlatformUnavailable".to_owned(),
            ]
        );
    }

    #[test]
    fn presenter_dismissal_is_explicit_and_injectable() {
        let mut presenter = FilePreviewPresenter::new(RecordingPanel::default());

        presenter.dismiss();

        assert_eq!(presenter.panel.dismissals, 1);
    }
    #[cfg(all(target_os = "macos", feature = "macos-native-tests"))]
    mod macos_adapter_tests {
        include!("../../platform/macos_adapter_tests/quick_look.rs");
    }
}
