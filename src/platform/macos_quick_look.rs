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

#[cfg(all(test, feature = "native-tests"))]
#[allow(
    dead_code,
    reason = "the main-thread ownership binary invokes this fixture"
)]
pub(crate) mod ownership_tests {
    use super::*;
    use std::cell::RefCell;

    use objc2::rc::{Weak, autoreleasepool};
    use objc2_foundation::{NSDate, NSRunLoop};

    use crate::terminal::native_services::FilePreviewTarget;
    use crate::terminal::native_services::file_preview::FilePreviewPresenter;
    use crate::terminal::{HyperlinkTarget, TerminalLocalFileCapabilities};

    struct ObservedNativePanel(Rc<RefCell<NativeQuickLookPanel>>);

    impl FilePreviewPanel for ObservedNativePanel {
        fn preview_file(&mut self, path: &Path) -> Result<(), FilePreviewError> {
            self.0.borrow_mut().preview_file(path)
        }
        fn dismiss(&mut self) {
            self.0.borrow_mut().dismiss();
        }
    }

    pub(crate) fn preview_replacement_and_teardown_release_native_objects(
        cx: &mut gpui::TestAppContext,
    ) {
        assert!(MainThreadMarker::new().is_some());
        let window = cx.add_window(|_, _| gpui::EmptyView);
        let directory = std::env::temp_dir().join(format!(
            "spaceterm-native-preview-owner-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let filesystem = crate::platform::unix_adapter_tests::local_filesystem();
        let targets = ["first.txt", "second.txt"].map(|name| {
            std::fs::write(directory.join(name), b"preview").unwrap();
            let link = HyperlinkTarget::resolve_osc8(
                &format!("file:{name}"),
                &directory,
                None,
                TerminalLocalFileCapabilities::Enabled,
                &filesystem,
            )
            .unwrap();
            FilePreviewTarget::from_link(&link, TerminalLocalFileCapabilities::Enabled).unwrap()
        });
        let native = Rc::new(RefCell::new(NativeQuickLookPanel::default()));
        let mut presenter = FilePreviewPresenter::new(ObservedNativePanel(native.clone()));
        let (first_panel, first_preview) = autoreleasepool(|_| {
            assert!(
                window
                    .update(cx, |_, window, cx| {
                        presenter
                            .preview_in_window(&targets[0], window, cx)
                            .unwrap()
                            .is_none()
                    })
                    .unwrap()
            );
            let panel = native.borrow();
            let owned = panel.window.as_ref().unwrap();
            assert!(owned.panel.isVisible());
            (
                Weak::from_retained(&owned.panel),
                Weak::from_retained(&owned.preview),
            )
        });
        let (second_panel, second_preview) = autoreleasepool(|_| {
            assert!(
                window
                    .update(cx, |_, window, cx| {
                        presenter
                            .preview_in_window(&targets[1], window, cx)
                            .unwrap()
                            .is_none()
                    })
                    .unwrap()
            );
            let panel = native.borrow();
            let owned = panel.window.as_ref().unwrap();
            assert!(owned.panel.isVisible());
            (
                Weak::from_retained(&owned.panel),
                Weak::from_retained(&owned.preview),
            )
        });
        // Quick Look retains a presented view while its asynchronous handoff finishes.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while autoreleasepool(|_| first_panel.load().is_some() || first_preview.load().is_some())
            && std::time::Instant::now() < deadline
        {
            autoreleasepool(|_| {
                NSRunLoop::currentRunLoop()
                    .runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.01))
            });
        }
        assert!(first_panel.load().is_none());
        assert!(first_preview.load().is_none());
        assert!(second_panel.load().is_some());
        assert!(second_preview.load().is_some());
        autoreleasepool(|_| {
            drop(presenter);
            let panel = native.borrow();
            let owned = panel.window.as_ref().unwrap();
            assert!(!owned.panel.isVisible());
            // SAFETY: The retained view is live on the main thread.
            assert!(unsafe { owned.preview.previewItem() }.is_none());
        });
        autoreleasepool(|_| drop(native));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while autoreleasepool(|_| second_panel.load().is_some() || second_preview.load().is_some())
            && std::time::Instant::now() < deadline
        {
            autoreleasepool(|_| {
                NSRunLoop::currentRunLoop()
                    .runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.01))
            });
        }
        assert!(second_panel.load().is_none());
        assert!(second_preview.load().is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
