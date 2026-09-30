//! Linux file preview. The desktop previewer arrives with a later desktop wave; until then every
//! preview reports that the platform cannot present one.
use std::path::Path;

use crate::terminal::native_services::file_preview::{
    FilePreviewError, FilePreviewFactory, FilePreviewPanel,
};

pub(super) struct LinuxFilePreviewFactory;

impl FilePreviewFactory for LinuxFilePreviewFactory {
    fn create(&self) -> Box<dyn FilePreviewPanel> {
        Box::new(LinuxFilePreviewPanel)
    }
}

struct LinuxFilePreviewPanel;

impl FilePreviewPanel for LinuxFilePreviewPanel {
    fn preview_file(&mut self, _: &Path) -> Result<(), FilePreviewError> {
        Err(FilePreviewError::PlatformUnavailable)
    }

    fn dismiss(&mut self) {}
}
