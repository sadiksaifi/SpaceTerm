use std::rc::Rc;

use block2::RcBlock;
use gpui::Window;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSView, NSWindow, NSWindowOcclusionState};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::window_visibility::{WindowVisibility, WindowVisibilityFactory, WindowVisibilitySource};

pub(crate) struct MacosWindowVisibilityFactory;

impl WindowVisibilityFactory for MacosWindowVisibilityFactory {
    fn capture(
        &self,
        window: &Window,
        changed: Box<dyn Fn()>,
    ) -> Option<Box<dyn WindowVisibilitySource>> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return None;
        };
        // SAFETY: GPUI owns this live NSView during the synchronous capture call.
        let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
        let native_window = view.window()?;
        let center = NSNotificationCenter::defaultCenter();
        let mut source = MacosWindowVisibilitySource {
            window: native_window,
            center,
            observers: Vec::with_capacity(5),
            callback: Rc::from(changed),
        };
        for name in [
            "NSWindowDidMiniaturizeNotification",
            "NSWindowDidDeminiaturizeNotification",
            "NSWindowDidChangeOcclusionStateNotification",
            "NSWindowWillStartLiveResizeNotification",
            "NSWindowDidEndLiveResizeNotification",
        ] {
            let callback = Rc::downgrade(&source.callback);
            let block = RcBlock::new(move |_: std::ptr::NonNull<NSNotification>| {
                if let Some(callback) = callback.upgrade() {
                    callback();
                }
            });
            let name = NSString::from_str(name);
            // SAFETY: AppKit posts these notifications on the owning window's UI thread. The
            // source removes each observer before dropping its UI-thread-only callback.
            let observer = unsafe {
                source.center.addObserverForName_object_queue_usingBlock(
                    Some(&name),
                    Some(&source.window),
                    None,
                    &block,
                )
            };
            source.observers.push(observer);
        }
        Some(Box::new(source))
    }
}

struct MacosWindowVisibilitySource {
    window: Retained<NSWindow>,
    center: Retained<NSNotificationCenter>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    callback: Rc<dyn Fn()>,
}

impl WindowVisibilitySource for MacosWindowVisibilitySource {
    fn current(&self) -> WindowVisibility {
        from_native(
            self.window.isMiniaturized(),
            self.window.occlusionState().0 as u64,
            self.window.inLiveResize(),
        )
    }
}

impl Drop for MacosWindowVisibilitySource {
    fn drop(&mut self) {
        for observer in self.observers.drain(..) {
            // SAFETY: This is the observer token returned by this center during registration.
            unsafe { self.center.removeObserver((*observer).as_ref()) };
        }
    }
}

fn from_native(minimized: bool, occlusion_state: u64, live_resize: bool) -> WindowVisibility {
    WindowVisibility {
        minimized,
        occluded: occlusion_state & NSWindowOcclusionState::Visible.bits() as u64 == 0,
        live_resize,
    }
}

#[cfg(all(test, feature = "macos-native-tests"))]
mod tests {
    use super::*;

    #[test]
    fn native_minimize_and_occlusion_bits_are_independent() {
        assert_eq!(
            from_native(false, NSWindowOcclusionState::Visible.bits() as u64, false),
            WindowVisibility::default()
        );
        assert_eq!(
            from_native(true, 0, true),
            WindowVisibility {
                minimized: true,
                occluded: true,
                live_resize: true
            }
        );
    }
}
