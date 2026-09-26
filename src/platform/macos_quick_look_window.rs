use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBackingStoreType, NSPanel, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_quick_look_ui::{QLPreviewView, QLPreviewViewStyle};

pub(crate) struct OwnedQuickLookWindow {
    pub(crate) panel: Retained<NSPanel>,
    pub(crate) preview: Retained<QLPreviewView>,
}

impl OwnedQuickLookWindow {
    pub(crate) fn new(mtm: MainThreadMarker) -> Option<Self> {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(720.0, 540.0));
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            frame,
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable,
            NSBackingStoreType::Buffered,
            false,
        );
        // SAFETY: QLPreviewView's designated initializer accepts this frame and normal style.
        let preview = unsafe {
            QLPreviewView::initWithFrame_style(
                QLPreviewView::alloc(mtm),
                frame,
                QLPreviewViewStyle::Normal,
            )
        }?;
        panel.setTitle(&NSString::from_str("Quick Look"));
        panel.setContentView(Some(&preview));
        // SAFETY: This owner retains the panel until after close; AppKit must not release it on close.
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setHidesOnDeactivate(true);
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.center();
        // SAFETY: These properties are set while the preview and panel are both live.
        unsafe {
            preview.setShouldCloseWithWindow(true);
            preview.setAutostarts(false);
        }
        Some(Self { panel, preview })
    }
}

impl Drop for OwnedQuickLookWindow {
    fn drop(&mut self) {
        self.panel.close();
        self.panel.setContentView(None);
    }
}
