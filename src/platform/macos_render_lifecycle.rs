use std::rc::Rc;

use block::ConcreteBlock;
use cocoa::base::{id, nil};
use cocoa::foundation::NSString;
use gpui::Window;
use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::window_visibility::{WindowVisibility, WindowVisibilityFactory, WindowVisibilitySource};

const NS_WINDOW_OCCLUSION_STATE_VISIBLE: u64 = 1 << 1;

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
        let view = handle.ns_view.as_ptr().cast::<Object>();
        // SAFETY: GPUI supplies this live NSView on the AppKit thread. Retaining its exact
        // NSWindow preserves identity across application activation and other window changes.
        unsafe {
            let native_window: id = msg_send![view, window];
            if native_window == nil {
                return None;
            }
            let native_window: id = msg_send![native_window, retain];
            let center: id = msg_send![class!(NSNotificationCenter), defaultCenter];
            let mut source = MacosWindowVisibilitySource {
                window: native_window,
                center,
                observers: Vec::with_capacity(5),
                // Rc also ensures this native owner cannot leave the AppKit thread.
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
                let block = ConcreteBlock::new(move |_: id| {
                    if let Some(callback) = callback.upgrade() {
                        callback();
                    }
                })
                .copy();
                let name = NSString::alloc(nil).init_str(name);
                let observer: id = msg_send![center,
                    addObserverForName: name
                    object: native_window
                    queue: nil
                    usingBlock: &*block
                ];
                let _: () = msg_send![name, release];
                if observer == nil {
                    return None;
                }
                let observer: id = msg_send![observer, retain];
                source.observers.push(observer);
            }
            Some(Box::new(source))
        }
    }
}

struct MacosWindowVisibilitySource {
    window: id,
    center: id,
    observers: Vec<id>,
    callback: Rc<dyn Fn()>,
}

impl WindowVisibilitySource for MacosWindowVisibilitySource {
    fn current(&self) -> WindowVisibility {
        // SAFETY: this source retains the exact NSWindow and is !Send/!Sync. All queries run
        // synchronously on GPUI's AppKit thread, including bounded visibility sampling.
        unsafe {
            let minimized: bool = msg_send![self.window, isMiniaturized];
            let occlusion_state: u64 = msg_send![self.window, occlusionState];
            let live_resize: bool = msg_send![self.window, inLiveResize];
            from_native(minimized, occlusion_state, live_resize)
        }
    }
}

impl Drop for MacosWindowVisibilitySource {
    fn drop(&mut self) {
        // SAFETY: registration retained these tokens and this window. Unregister before releasing
        // the window so callbacks cannot outlive their owner or follow a successor window.
        unsafe {
            for observer in self.observers.drain(..) {
                let _: () = msg_send![self.center, removeObserver: observer];
                let _: () = msg_send![observer, release];
            }
            let _: () = msg_send![self.window, release];
        }
    }
}

fn from_native(minimized: bool, occlusion_state: u64, live_resize: bool) -> WindowVisibility {
    WindowVisibility {
        minimized,
        occluded: occlusion_state & NS_WINDOW_OCCLUSION_STATE_VISIBLE == 0,
        live_resize,
    }
}

#[cfg(all(test, feature = "macos-native-tests"))]
mod tests {
    use super::*;

    #[test]
    fn native_minimize_and_occlusion_bits_are_independent() {
        assert_eq!(
            from_native(false, NS_WINDOW_OCCLUSION_STATE_VISIBLE, false),
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
