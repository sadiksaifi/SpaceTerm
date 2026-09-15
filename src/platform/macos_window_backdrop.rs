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
