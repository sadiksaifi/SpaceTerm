#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the next stacked context-action layer owns this injectable platform seam"
    )
)]

use std::marker::PhantomData;
use std::path::Path;
use std::rc::Rc;

use cocoa::base::{NO, YES, id, nil};
use cocoa::foundation::{NSAutoreleasePool, NSPoint, NSRect, NSSize, NSString};
use objc::{class, msg_send, sel, sel_impl};

use crate::terminal::QuickLookTarget;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QuickLookError {
    StaleTarget,
    OffMainThread,
    PlatformUnavailable,
}

pub(crate) trait QuickLookPlatform {
    fn preview(&mut self, target: &QuickLookTarget) -> Result<(), QuickLookError>;
    fn dismiss(&mut self);
}

trait QuickLookPanel {
    fn preview_file(&mut self, path: &Path) -> Result<(), QuickLookError>;
    fn dismiss(&mut self);
}

struct QuickLookPresenter<P> {
    panel: P,
}

impl<P> QuickLookPresenter<P> {
    const fn new(panel: P) -> Self {
        Self { panel }
    }
}

impl<P: QuickLookPanel> QuickLookPlatform for QuickLookPresenter<P> {
    fn preview(&mut self, target: &QuickLookTarget) -> Result<(), QuickLookError> {
        let Some(path) = target.revalidated_path() else {
            self.panel.dismiss();
            return Err(QuickLookError::StaleTarget);
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

pub(crate) struct MacosQuickLook {
    presenter: QuickLookPresenter<NativeQuickLookPanel>,
}

impl Default for MacosQuickLook {
    fn default() -> Self {
        Self {
            presenter: QuickLookPresenter::new(NativeQuickLookPanel::default()),
        }
    }
}

impl QuickLookPlatform for MacosQuickLook {
    fn preview(&mut self, target: &QuickLookTarget) -> Result<(), QuickLookError> {
        self.presenter.preview(target)
    }

    fn dismiss(&mut self) {
        self.presenter.dismiss();
    }
}

#[derive(Default)]
struct NativeQuickLookPanel {
    window: Option<OwnedQuickLookWindow>,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl QuickLookPanel for NativeQuickLookPanel {
    fn preview_file(&mut self, path: &Path) -> Result<(), QuickLookError> {
        if !main_thread() {
            return Err(QuickLookError::OffMainThread);
        }
        let path = path.to_str().ok_or(QuickLookError::StaleTarget)?;

        // SAFETY: Callers must invoke this adapter from GPUI's AppKit thread. The explicit
        // assertion above enforces that boundary, and the URL is retained by the owned
        // QLPreviewView before the autorelease pool drains.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let result = (|| {
                drop(self.window.take());
                let string = NSString::alloc(nil).init_str(path).autorelease();
                let url: id = msg_send![class!(NSURL), fileURLWithPath: string isDirectory: NO];
                if url == nil {
                    return Err(QuickLookError::PlatformUnavailable);
                }
                let window = unsafe { OwnedQuickLookWindow::new()? };
                let _: () = msg_send![window.preview, setPreviewItem: url];
                let _: () = msg_send![window.preview, refreshPreviewItem];

                // `orderFront:` presents a nonmodal Quick Look surface without making it key.
                // Terminal Input Focus therefore stays with the Pane that invoked the action.
                let _: () = msg_send![window.panel, orderFront: nil];
                self.window = Some(window);
                Ok(())
            })();
            pool.drain();
            result
        }
    }

    fn dismiss(&mut self) {
        let Some(window) = &self.window else {
            return;
        };
        if !main_thread() {
            return;
        }

        // SAFETY: Both objects are retained by `OwnedQuickLookWindow` and confined to AppKit's
        // main thread. Clearing the preview releases asynchronous Quick Look ownership of the URL.
        unsafe {
            let _: () = msg_send![window.preview, setPreviewItem: nil];
            let _: () = msg_send![window.panel, orderOut: nil];
        }
    }
}

impl Drop for NativeQuickLookPanel {
    fn drop(&mut self) {
        self.dismiss();
    }
}

struct OwnedQuickLookWindow {
    panel: id,
    preview: id,
}

impl OwnedQuickLookWindow {
    unsafe fn new() -> Result<Self, QuickLookError> {
        const NS_WINDOW_STYLE_MASK_TITLED: usize = 1 << 0;
        const NS_WINDOW_STYLE_MASK_CLOSABLE: usize = 1 << 1;
        const NS_WINDOW_STYLE_MASK_RESIZABLE: usize = 1 << 3;
        const NS_BACKING_STORE_BUFFERED: usize = 2;
        const PREVIEW_WIDTH: f64 = 720.0;
        const PREVIEW_HEIGHT: f64 = 540.0;

        let frame = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(PREVIEW_WIDTH, PREVIEW_HEIGHT),
        );
        // SAFETY: Signatures and integer constants match AppKit's 64-bit NSPanel/NSWindow ABI.
        let panel: id = unsafe {
            let allocated: id = msg_send![class!(NSPanel), alloc];
            msg_send![
                allocated,
                initWithContentRect: frame
                styleMask: NS_WINDOW_STYLE_MASK_TITLED
                    | NS_WINDOW_STYLE_MASK_CLOSABLE
                    | NS_WINDOW_STYLE_MASK_RESIZABLE
                backing: NS_BACKING_STORE_BUFFERED
                defer: NO
            ]
        };
        if panel == nil {
            return Err(QuickLookError::PlatformUnavailable);
        }

        // SAFETY: QLPreviewView's designated initializer takes NSRect and NSUInteger on macOS.
        let preview: id = unsafe {
            let allocated: id = msg_send![class!(QLPreviewView), alloc];
            msg_send![allocated, initWithFrame: frame style: 0_usize]
        };
        if preview == nil {
            // SAFETY: `panel` is retained from alloc/init and not published.
            unsafe {
                let _: () = msg_send![panel, release];
            }
            return Err(QuickLookError::PlatformUnavailable);
        }

        // SAFETY: The panel retains its content view. The view closes and releases its asynchronous
        // preview item when the user or this owner closes the panel.
        unsafe {
            let title = NSString::alloc(nil).init_str("Quick Look").autorelease();
            let _: () = msg_send![panel, setTitle: title];
            let _: () = msg_send![panel, setContentView: preview];
            let _: () = msg_send![panel, setReleasedWhenClosed: NO];
            let _: () = msg_send![panel, setHidesOnDeactivate: YES];
            let _: () = msg_send![panel, setBecomesKeyOnlyIfNeeded: YES];
            let _: () = msg_send![panel, center];
            let _: () = msg_send![preview, setShouldCloseWithWindow: YES];
            let _: () = msg_send![preview, setAutostarts: NO];
        }
        Ok(Self { panel, preview })
    }
}

impl Drop for OwnedQuickLookWindow {
    fn drop(&mut self) {
        // SAFETY: Both retained objects are AppKit-main-thread confined by the non-Send owner.
        // Closing the panel closes QLPreviewView and releases its asynchronous preview item.
        unsafe {
            let _: () = msg_send![self.panel, close];
            let _: () = msg_send![self.panel, setContentView: nil];
            let _: () = msg_send![self.preview, release];
            let _: () = msg_send![self.panel, release];
        }
    }
}

fn main_thread() -> bool {
    // SAFETY: `NSThread.isMainThread` is a process query with no object lifetime transfer.
    unsafe {
        let is_main: cocoa::base::BOOL = msg_send![class!(NSThread), isMainThread];
        is_main == YES
    }
}

#[link(name = "QuickLookUI", kind = "framework")]
unsafe extern "C" {}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::terminal::HyperlinkTarget;

    #[derive(Default)]
    struct RecordingPanel {
        previews: Vec<PathBuf>,
        dismissals: usize,
    }

    impl QuickLookPanel for RecordingPanel {
        fn preview_file(&mut self, path: &Path) -> Result<(), QuickLookError> {
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
            "spaceterm-quick-look-platform-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let file = directory.join("preview.txt");
        fs::write(&file, b"preview").unwrap();
        let link = HyperlinkTarget::osc8("file:preview.txt", &directory, None).unwrap();
        let target = QuickLookTarget::from_link(&link).unwrap();
        let mut presenter = QuickLookPresenter::new(RecordingPanel::default());

        let result = presenter.preview(&target);

        assert_eq!(result, Ok(()));
        assert_eq!(presenter.panel.previews, vec![file.canonicalize().unwrap()]);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn presenter_rejects_a_replaced_file_before_calling_the_platform() {
        let directory = std::env::temp_dir().join(format!(
            "spaceterm-quick-look-platform-replaced-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let file = directory.join("preview.txt");
        let replacement = directory.join("replacement.txt");
        fs::write(&file, b"preview").unwrap();
        let link = HyperlinkTarget::osc8("file:preview.txt", &directory, None).unwrap();
        let target = QuickLookTarget::from_link(&link).unwrap();
        fs::write(&replacement, b"replacement").unwrap();
        fs::rename(replacement, &file).unwrap();
        let mut presenter = QuickLookPresenter::new(RecordingPanel::default());

        let result = presenter.preview(&target);

        assert_eq!(result, Err(QuickLookError::StaleTarget));
        assert_eq!(
            (presenter.panel.previews.len(), presenter.panel.dismissals),
            (0, 1)
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn quick_look_target_rejects_web_links_before_the_platform_boundary() {
        let link = HyperlinkTarget::url("https://example.test/file.txt").unwrap();

        let target = QuickLookTarget::from_link(&link);

        assert_eq!(target, None);
    }

    #[test]
    fn quick_look_target_rejects_a_missing_file_before_the_platform_boundary() {
        let directory = std::env::temp_dir().join(format!(
            "spaceterm-quick-look-platform-missing-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let file = directory.join("preview.txt");
        fs::write(&file, b"preview").unwrap();
        let link = HyperlinkTarget::osc8("file:preview.txt", &directory, None).unwrap();
        fs::remove_file(file).unwrap();

        let target = QuickLookTarget::from_link(&link);

        assert_eq!(target, None);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn quick_look_target_rejects_a_directory_before_the_platform_boundary() {
        let directory = std::env::temp_dir().join(format!(
            "spaceterm-quick-look-platform-directory-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();

        let target = HyperlinkTarget::osc8("file:.", &directory, None)
            .and_then(|link| QuickLookTarget::from_link(&link));

        assert_eq!(target, None);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn platform_error_identifiers_carry_no_target_content() {
        assert_eq!(
            [
                QuickLookError::StaleTarget,
                QuickLookError::OffMainThread,
                QuickLookError::PlatformUnavailable,
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
        let mut presenter = QuickLookPresenter::new(RecordingPanel::default());

        presenter.dismiss();

        assert_eq!(presenter.panel.dismissals, 1);
    }
}
