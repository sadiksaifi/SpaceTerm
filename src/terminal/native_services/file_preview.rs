use std::path::Path;

use super::FilePreviewTarget;

#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only a native preview Adapter observes thread affinity"
    )
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FilePreviewError {
    StaleTarget,
    OffMainThread,
    PlatformUnavailable,
}

/// How an accepted preview request continues.
pub(crate) enum FilePreviewSubmission {
    Presented,
    /// A deferred adapter sends at most one failure. The channel closes without one once the
    /// request was presented, superseded, or dismissed.
    #[allow(
        dead_code,
        reason = "only a deferred preview Adapter answers asynchronously"
    )]
    Pending(async_channel::Receiver<FilePreviewError>),
}

/// Only presentation mechanics cross this capability boundary.
pub(crate) trait FilePreviewPanel {
    fn preview_file(&mut self, path: &Path) -> Result<(), FilePreviewError>;
    /// Deferred adapters retain this authority and revalidate at their final native handoff.
    fn preview_file_in_window(
        &mut self,
        target: FilePreviewTarget,
        _: &gpui::Window,
        _: &mut gpui::App,
    ) -> Result<FilePreviewSubmission, FilePreviewError> {
        let path = target
            .revalidated_path()
            .ok_or(FilePreviewError::StaleTarget)?;
        self.preview_file(&path)
            .map(|()| FilePreviewSubmission::Presented)
    }
    fn dismiss(&mut self);
}

/// The deferred part of one presenter request.
#[must_use = "a deferred preview reports its failure only when awaited"]
pub(crate) struct FilePreviewCompletion {
    request: u64,
    failure: async_channel::Receiver<FilePreviewError>,
}

/// A deferred failure that the presenter settles against its current request.
pub(crate) struct FilePreviewFailure {
    request: u64,
    error: FilePreviewError,
}

impl FilePreviewCompletion {
    /// `None` when the request was presented, superseded, or dismissed.
    pub(crate) async fn failure(self) -> Option<FilePreviewFailure> {
        let error = self.failure.recv().await.ok()?;
        Some(FilePreviewFailure {
            request: self.request,
            error,
        })
    }
}

pub(crate) trait FilePreviewFactory {
    fn is_available(&self) -> bool {
        true
    }
    fn create(&self) -> Box<dyn FilePreviewPanel>;
}

/// Owns revalidation, failure cleanup, replacement and Pane teardown policy.
pub(crate) struct FilePreviewPresenter<P: FilePreviewPanel> {
    pub(super) panel: P,
    /// Each request and dismissal supersedes deferred failures of earlier requests.
    request: u64,
}

impl<P: FilePreviewPanel> FilePreviewPresenter<P> {
    pub(crate) const fn new(panel: P) -> Self {
        Self { panel, request: 0 }
    }

    #[cfg(test)]
    pub(crate) fn preview(&mut self, target: &FilePreviewTarget) -> Result<(), FilePreviewError> {
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
    /// A deferred adapter returns a completion that the owner awaits and then settles.
    pub(crate) fn preview_in_window(
        &mut self,
        target: &FilePreviewTarget,
        window: &gpui::Window,
        cx: &mut gpui::App,
    ) -> Result<Option<FilePreviewCompletion>, FilePreviewError> {
        self.request += 1;
        if target.revalidated_path().is_none() {
            self.panel.dismiss();
            return Err(FilePreviewError::StaleTarget);
        }
        match self
            .panel
            .preview_file_in_window(target.clone(), window, cx)
        {
            Ok(FilePreviewSubmission::Presented) => Ok(None),
            Ok(FilePreviewSubmission::Pending(failure)) => Ok(Some(FilePreviewCompletion {
                request: self.request,
                failure,
            })),
            Err(error) => {
                self.panel.dismiss();
                Err(error)
            }
        }
    }
    /// Applies the failure policy to a deferred failure of the current request. A failure of a
    /// superseded or dismissed request returns `None` and changes nothing.
    pub(crate) fn settle(&mut self, failure: FilePreviewFailure) -> Option<FilePreviewError> {
        if failure.request != self.request {
            return None;
        }
        self.dismiss();
        Some(failure.error)
    }
    pub(crate) fn dismiss(&mut self) {
        self.request += 1;
        self.panel.dismiss();
    }
}

impl FilePreviewPanel for Box<dyn FilePreviewPanel> {
    fn preview_file(&mut self, path: &Path) -> Result<(), FilePreviewError> {
        (**self).preview_file(path)
    }
    fn preview_file_in_window(
        &mut self,
        target: FilePreviewTarget,
        window: &gpui::Window,
        cx: &mut gpui::App,
    ) -> Result<FilePreviewSubmission, FilePreviewError> {
        (**self).preview_file_in_window(target, window, cx)
    }
    fn dismiss(&mut self) {
        (**self).dismiss();
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

    #[cfg(all(test, target_os = "macos", feature = "native-tests"))]
    mod macos_adapter_tests {
        include!("../../platform/macos_adapter_tests/quick_look.rs");
    }
}
