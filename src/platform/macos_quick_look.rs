use std::marker::PhantomData;
use std::path::Path;
use std::rc::Rc;

use objc2::MainThreadMarker;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{NSString, NSURL};
use objc2_quick_look_ui::QLPreviewItem;

use super::macos_quick_look_window::OwnedQuickLookWindow;

use crate::terminal::native_services::file_preview::{
    FilePreviewError, FilePreviewFactory, FilePreviewPanel,
};

pub(crate) struct MacosQuickLookFactory;

impl FilePreviewFactory for MacosQuickLookFactory {
    fn create(&self) -> Box<dyn FilePreviewPanel> {
        Box::<NativeQuickLookPanel>::default()
    }
}

#[derive(Default)]
struct NativeQuickLookPanel {
    window: Option<OwnedQuickLookWindow>,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl FilePreviewPanel for NativeQuickLookPanel {
    fn preview_file(&mut self, path: &Path) -> Result<(), FilePreviewError> {
        let mtm = MainThreadMarker::new().ok_or(FilePreviewError::OffMainThread)?;
        let path = path.to_str().ok_or(FilePreviewError::StaleTarget)?;
        self.window.take();
        let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(path), false);
        let window = OwnedQuickLookWindow::new(mtm).ok_or(FilePreviewError::PlatformUnavailable)?;
        // SAFETY: NSURL implements QLPreviewItem, and Quick Look accepts this live file URL.
        unsafe {
            window
                .preview
                .setPreviewItem(Some(ProtocolObject::<dyn QLPreviewItem>::from_ref(&*url)));
            window.preview.refreshPreviewItem();
        }
        // orderFront presents the nonmodal preview without taking Terminal Input Focus.
        window.panel.orderFront(None);
        self.window = Some(window);
        Ok(())
    }

    fn dismiss(&mut self) {
        let Some(window) = &self.window else {
            return;
        };
        if MainThreadMarker::new().is_none() {
            return;
        }
        // SAFETY: Quick Look accepts nil to clear the preview item before hiding the panel.
        unsafe { window.preview.setPreviewItem(None) };
        window.panel.orderOut(None);
    }
}

impl Drop for NativeQuickLookPanel {
    fn drop(&mut self) {
        self.dismiss();
    }
}
