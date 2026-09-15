//! The application-owned native backdrop behind one Operating-System Window's content.
//!
//! Asking GPUI for a blurred window background was observed to produce no blur on this macOS: the
//! desktop showed through the window sharp. GPUI's blurred background installs an
//! `NSVisualEffectView` and then reaches into the material's private layer tree, clearing layer
//! backgrounds, hiding a layer matched by private class name and removing a filter matched by
//! description. Which of those mutations defeats the material here has not been established, and
//! establishing it would not help: a technique that depends on private layer names and filter
//! descriptions has no contract to hold across an Operating-System release, so each release can
//! change the result without any change here.
//!
//! SpaceTerm therefore asks GPUI only for a transparent window and owns the effect here, using the
//! material exactly as AppKit publishes it. The view is an ordinary, unmodified
//! `NSVisualEffectView` inserted as the bottom sibling of GPUI's rendering view inside the window's
//! content view, which is where AppKit expects a behind-window material to live. Nothing about
//! GPUI's view hierarchy is reparented and no layer is mutated, so the backdrop keeps whatever
//! appearance the Operating System defines for the material.

use cocoa::appkit::{NSView, NSViewHeightSizable, NSViewWidthSizable, NSWindowOrderingMode};
use cocoa::base::{NO, id, nil};
use cocoa::foundation::{NSInteger, NSRect, NSString};
use objc::runtime::{BOOL, Object};
use objc::{class, msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// `NSVisualEffectMaterialUnderWindowBackground`: the material AppKit draws behind a window's own
/// content. It is chosen because it is semantic rather than decorative and follows the effective
/// Light/Dark appearance. Its exact tint belongs to the Operating System, which may draw it from
/// the desktop behind the window, so SpaceTerm's own neutral identity comes from the Chrome
/// painted over it rather than from an assumption about the material.
const MATERIAL_UNDER_WINDOW_BACKGROUND: NSInteger = 21;
/// `NSVisualEffectBlendingModeBehindWindow`: the material samples the desktop behind the window.
const BLENDING_MODE_BEHIND_WINDOW: NSInteger = 0;
/// `NSVisualEffectStateActive`: an inactive window keeps its frosted backdrop rather than
/// collapsing to a flat fill while the reader looks at another application.
const STATE_ACTIVE: NSInteger = 1;

/// Identifies this view among the content view's subviews, so one window installs one backdrop
/// and can find that same backdrop again to remove it.
const BACKDROP_IDENTIFIER: &str = "dev.spaceterm.window-backdrop";

/// Installs or removes the native blurred backdrop behind one window's content.
///
/// Applying the state a window already has does nothing, so repeated appearance publications cost
/// no native work. Removing is complete: the window keeps no hidden effect view, and a window torn
/// down while blurred releases the view with its own content view.
pub(crate) fn apply(window: &gpui::Window, blurred: bool) {
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    let rendering_view = handle.ns_view.as_ptr().cast::<Object>();

    // SAFETY: GPUI supplies a live NSView for this synchronous AppKit-thread call, and AppKit owns
    // the window and content view reached through it. Every object created here is either released
    // after the content view retains it or released before returning.
    unsafe {
        let native_window: id = msg_send![rendering_view, window];
        if native_window == nil {
            return;
        }
        let content_view: id = msg_send![native_window, contentView];
        if content_view == nil {
            return;
        }
        apply_to_content_view(content_view, blurred);
    }
}

/// SAFETY: called on the AppKit thread with a live content view.
unsafe fn apply_to_content_view(content_view: id, blurred: bool) {
    unsafe {
        let identifier = NSString::alloc(nil).init_str(BACKDROP_IDENTIFIER);
        let installed = installed_backdrop(content_view, identifier);
        if !blurred {
            if installed != nil {
                let _: () = msg_send![installed, removeFromSuperview];
            }
        } else if installed == nil {
            install(content_view, identifier);
        }
        let _: () = msg_send![identifier, release];
    }
}

/// SAFETY: called on the AppKit thread with a live content view and a live identifier string.
unsafe fn installed_backdrop(content_view: id, identifier: id) -> id {
    unsafe {
        let subviews: id = msg_send![content_view, subviews];
        if subviews == nil {
            return nil;
        }
        let count: usize = msg_send![subviews, count];
        for index in 0..count {
            let subview: id = msg_send![subviews, objectAtIndex: index];
            let candidate: id = msg_send![subview, identifier];
            if candidate == nil {
                continue;
            }
            let matches: BOOL = msg_send![candidate, isEqualToString: identifier];
            if matches != NO {
                return subview;
            }
        }
        nil
    }
}

/// SAFETY: called on the AppKit thread with a live content view and a live identifier string.
unsafe fn install(content_view: id, identifier: id) {
    unsafe {
        let bounds: NSRect = NSView::bounds(content_view);
        let backdrop: id = msg_send![class!(NSVisualEffectView), alloc];
        let backdrop: id = msg_send![backdrop, initWithFrame: bounds];
        if backdrop == nil {
            return;
        }
        let _: () = msg_send![backdrop, setMaterial: MATERIAL_UNDER_WINDOW_BACKGROUND];
        let _: () = msg_send![backdrop, setBlendingMode: BLENDING_MODE_BEHIND_WINDOW];
        let _: () = msg_send![backdrop, setState: STATE_ACTIVE];
        let _: () = msg_send![backdrop, setIdentifier: identifier];
        // GPUI's rendering view covers the content view and stays above this one, so the backdrop
        // resizes with the window but never reaches a pointer event.
        backdrop.setAutoresizingMask_(NSViewWidthSizable | NSViewHeightSizable);
        let _: () = msg_send![
            content_view,
            addSubview: backdrop
            positioned: NSWindowOrderingMode::NSWindowBelow
            relativeTo: nil
        ];
        // The content view now holds the only retain this view needs for its whole lifetime.
        let _: () = msg_send![backdrop, release];
    }
}

#[cfg(all(test, feature = "macos-native-tests"))]
mod tests {
    use super::*;
    use cocoa::base::YES;
    use cocoa::foundation::{NSPoint, NSSize};

    /// Owns the content view's initial retain. Its subviews are retained only by AppKit.
    struct ContentView(id);

    impl ContentView {
        unsafe fn new(size: NSSize) -> Self {
            unsafe {
                let frame = NSRect::new(NSPoint::new(0.0, 0.0), size);
                let content = NSView::initWithFrame_(NSView::alloc(nil), frame);
                assert_ne!(content, nil);
                Self(content)
            }
        }

        unsafe fn add_renderer(&self) -> id {
            unsafe {
                let renderer = NSView::initWithFrame_(NSView::alloc(nil), NSView::bounds(self.0));
                assert_ne!(renderer, nil);
                self.0.addSubview_(renderer);
                let _: () = msg_send![renderer, release];
                renderer
            }
        }

        unsafe fn backdrop(&self) -> id {
            unsafe {
                let identifier = NSString::alloc(nil).init_str(BACKDROP_IDENTIFIER);
                let backdrop = installed_backdrop(self.0, identifier);
                let _: () = msg_send![identifier, release];
                backdrop
            }
        }

        unsafe fn subview_count(&self) -> usize {
            unsafe {
                let subviews: id = msg_send![self.0, subviews];
                msg_send![subviews, count]
            }
        }
    }

    impl Drop for ContentView {
        fn drop(&mut self) {
            // SAFETY: this fixture owns the content view's initial retain and is dropped on the
            // AppKit thread after every borrowed subview pointer has gone out of use.
            unsafe {
                let _: () = msg_send![self.0, release];
            }
        }
    }

    #[gpui::test]
    fn backdrop_installation_is_ordered_idempotent_and_reversible(cx: &mut gpui::TestAppContext) {
        cx.update(|_| {
            // SAFETY: GPUI runs this closure on the AppKit thread. The fixture retains the content
            // view, while AppKit retains attached subviews for exactly their attached lifetime.
            unsafe {
                let content = ContentView::new(NSSize::new(320.0, 180.0));
                let renderer = content.add_renderer();

                apply_to_content_view(content.0, true);
                apply_to_content_view(content.0, true);

                let backdrop = content.backdrop();
                assert_ne!(backdrop, nil);
                assert_eq!(content.subview_count(), 2);
                let subviews: id = msg_send![content.0, subviews];
                let first: id = msg_send![subviews, objectAtIndex: 0usize];
                let second: id = msg_send![subviews, objectAtIndex: 1usize];
                assert_eq!(first, backdrop, "the backdrop must stay below the renderer");
                assert_eq!(second, renderer);

                apply_to_content_view(content.0, false);
                assert_eq!(content.subview_count(), 1);
                assert_eq!(content.backdrop(), nil);

                apply_to_content_view(content.0, false);
                assert_eq!(content.subview_count(), 1);

                apply_to_content_view(content.0, true);
                assert_ne!(content.backdrop(), nil);
                assert_eq!(content.subview_count(), 2);
                apply_to_content_view(content.0, false);
                assert_eq!(content.subview_count(), 1);
            }
        });
    }

    #[gpui::test]
    fn backdrop_tracks_content_bounds_through_appkit_autoresizing(cx: &mut gpui::TestAppContext) {
        cx.update(|_| {
            // SAFETY: GPUI runs this closure on the AppKit thread and the fixture owns the live
            // content view for the duration of every AppKit message.
            unsafe {
                let content = ContentView::new(NSSize::new(300.0, 160.0));
                content.add_renderer();
                apply_to_content_view(content.0, true);
                let backdrop = content.backdrop();
                assert_ne!(backdrop, nil);

                let mask: u64 = msg_send![backdrop, autoresizingMask];
                assert_eq!(
                    mask & (NSViewWidthSizable | NSViewHeightSizable),
                    NSViewWidthSizable | NSViewHeightSizable
                );
                let autoresizes_subviews: BOOL = msg_send![content.0, autoresizesSubviews];
                assert_eq!(autoresizes_subviews, YES);

                content.0.setFrameSize(NSSize::new(640.0, 360.0));
                let content_bounds = NSView::bounds(content.0);
                let backdrop_frame = NSView::frame(backdrop);
                assert_eq!(backdrop_frame.origin.x, content_bounds.origin.x);
                assert_eq!(backdrop_frame.origin.y, content_bounds.origin.y);
                assert_eq!(backdrop_frame.size.width, content_bounds.size.width);
                assert_eq!(backdrop_frame.size.height, content_bounds.size.height);

                apply_to_content_view(content.0, false);
            }
        });
    }
}
