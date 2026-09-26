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

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, msg_send};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindowOrderingMode,
};
use objc2_foundation::NSString;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::appearance::ChromeTone;
use crate::platform::appearance::WindowBackdrop;

const BACKDROP_IDENTIFIER: &str = "dev.spaceterm.window-backdrop";

fn material(tone: ChromeTone) -> NSVisualEffectMaterial {
    match tone {
        ChromeTone::Bright => NSVisualEffectMaterial::Sidebar,
        ChromeTone::Dark => NSVisualEffectMaterial::UnderWindowBackground,
    }
}

/// Installs, updates, or removes the material behind one window's content.
pub(crate) fn apply(window: &gpui::Window, backdrop: WindowBackdrop) {
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    // SAFETY: GPUI owns this live NSView for the synchronous appearance call.
    let rendering_view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
    let Some(content_view) = rendering_view
        .window()
        .and_then(|window| window.contentView())
    else {
        return;
    };
    apply_to_content_view(&content_view, requested_material(backdrop), mtm);
}

fn requested_material(backdrop: WindowBackdrop) -> Option<NSVisualEffectMaterial> {
    match backdrop {
        WindowBackdrop::Absent => None,
        WindowBackdrop::Frosted(appearance) => Some(material(appearance)),
    }
}

fn apply_to_content_view(
    content_view: &NSView,
    material: Option<NSVisualEffectMaterial>,
    mtm: MainThreadMarker,
) {
    let installed = installed_backdrop(content_view);
    match (material, installed) {
        (None, Some(installed)) => installed.removeFromSuperview(),
        (None, None) => {}
        (Some(material), Some(installed)) => installed.setMaterial(material),
        (Some(material), None) => install(content_view, material, mtm),
    }
}

fn installed_backdrop(content_view: &NSView) -> Option<Retained<NSVisualEffectView>> {
    let subviews = content_view.subviews();
    for index in 0..subviews.count() {
        let subview = subviews.objectAtIndex(index);
        // SAFETY: NSView implements identifier and objc2 retains its autoreleased return.
        let identifier: Option<Retained<NSString>> = unsafe { msg_send![&*subview, identifier] };
        if identifier
            .as_deref()
            .is_some_and(|identifier| identifier.to_string() == BACKDROP_IDENTIFIER)
        {
            return subview.downcast().ok();
        }
    }
    None
}

fn install(content_view: &NSView, material: NSVisualEffectMaterial, mtm: MainThreadMarker) {
    let backdrop =
        NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), content_view.bounds());
    backdrop.setMaterial(material);
    backdrop.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    backdrop.setState(NSVisualEffectState::Active);
    let identifier = NSString::from_str(BACKDROP_IDENTIFIER);
    // SAFETY: NSView implements setIdentifier: and copies this live string.
    let _: () = unsafe { msg_send![&*backdrop, setIdentifier: &*identifier] };
    backdrop.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    content_view.addSubview_positioned_relativeTo(&backdrop, NSWindowOrderingMode::Below, None);
}

#[cfg(all(test, feature = "macos-native-tests"))]
#[allow(dead_code)]
pub(in crate::platform) mod tests {
    use super::*;
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    fn apply_to_content_view(content_view: &NSView, material: Option<NSVisualEffectMaterial>) {
        super::apply_to_content_view(
            content_view,
            material,
            objc2::MainThreadMarker::new().expect("native test must run on the main thread"),
        );
    }

    struct ContentView(Retained<NSView>);

    impl ContentView {
        fn new(size: NSSize) -> Self {
            let mtm =
                objc2::MainThreadMarker::new().expect("native test must run on the main thread");
            let frame = NSRect::new(NSPoint::new(0.0, 0.0), size);
            Self(NSView::initWithFrame(NSView::alloc(mtm), frame))
        }

        fn add_renderer(&self) -> Retained<NSView> {
            let mtm =
                objc2::MainThreadMarker::new().expect("native test must run on the main thread");
            let renderer = NSView::initWithFrame(NSView::alloc(mtm), self.0.bounds());
            self.0.addSubview(&renderer);
            renderer
        }

        fn backdrop(&self) -> Option<Retained<NSVisualEffectView>> {
            installed_backdrop(&self.0)
        }

        fn backdrop_material(&self) -> NSVisualEffectMaterial {
            self.backdrop().unwrap().material()
        }

        fn subview_count(&self) -> usize {
            self.0.subviews().count()
        }
    }

    pub(in crate::platform) fn backdrop_installation_is_ordered_idempotent_and_reversible(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|_| {
            let content = ContentView::new(NSSize::new(320.0, 180.0));
            let renderer = content.add_renderer();
            let dark = Some(material(ChromeTone::Dark));
            apply_to_content_view(&content.0, dark);
            apply_to_content_view(&content.0, dark);

            let backdrop = content.backdrop().unwrap();
            assert_eq!(content.subview_count(), 2);
            let subviews = content.0.subviews();
            assert!(std::ptr::eq(&*subviews.objectAtIndex(0), &**backdrop));
            assert!(std::ptr::eq(&*subviews.objectAtIndex(1), &*renderer));

            apply_to_content_view(&content.0, None);
            assert_eq!(content.subview_count(), 1);
            assert!(content.backdrop().is_none());
            apply_to_content_view(&content.0, None);
            assert_eq!(content.subview_count(), 1);
            apply_to_content_view(&content.0, dark);
            assert!(content.backdrop().is_some());
            assert_eq!(content.subview_count(), 2);
            apply_to_content_view(&content.0, None);
            assert_eq!(content.subview_count(), 1);
        });
    }

    pub(in crate::platform) fn backdrop_tracks_content_bounds_through_appkit_autoresizing(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|_| {
            let content = ContentView::new(NSSize::new(300.0, 160.0));
            content.add_renderer();
            apply_to_content_view(&content.0, Some(material(ChromeTone::Dark)));
            let backdrop = content.backdrop().unwrap();
            assert_eq!(
                backdrop.autoresizingMask()
                    & (NSAutoresizingMaskOptions::ViewWidthSizable
                        | NSAutoresizingMaskOptions::ViewHeightSizable),
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewHeightSizable,
            );
            assert!(content.0.autoresizesSubviews());
            content.0.setFrameSize(NSSize::new(640.0, 360.0));
            let content_bounds = content.0.bounds();
            let backdrop_frame = backdrop.frame();
            assert_eq!(backdrop_frame.origin.x, content_bounds.origin.x);
            assert_eq!(backdrop_frame.origin.y, content_bounds.origin.y);
            assert_eq!(backdrop_frame.size.width, content_bounds.size.width);
            assert_eq!(backdrop_frame.size.height, content_bounds.size.height);
            apply_to_content_view(&content.0, None);
        });
    }

    pub(in crate::platform) fn changing_tone_replaces_the_material_in_place(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|_| {
            let content = ContentView::new(NSSize::new(320.0, 180.0));
            content.add_renderer();
            apply_to_content_view(&content.0, Some(material(ChromeTone::Dark)));
            let installed = content.backdrop().unwrap();
            assert_eq!(
                content.backdrop_material(),
                NSVisualEffectMaterial::UnderWindowBackground
            );
            apply_to_content_view(&content.0, Some(material(ChromeTone::Bright)));
            assert!(std::ptr::eq(&*content.backdrop().unwrap(), &*installed));
            assert_eq!(content.subview_count(), 2);
            assert_eq!(content.backdrop_material(), NSVisualEffectMaterial::Sidebar);
            apply_to_content_view(&content.0, None);
        });
    }
}

#[cfg(test)]
mod material_tests {
    use super::*;

    #[test]
    fn bright_chrome_asks_for_the_more_transmissive_material() {
        assert_eq!(
            requested_material(WindowBackdrop::Frosted(ChromeTone::Bright)),
            Some(NSVisualEffectMaterial::Sidebar),
        );
        assert_eq!(
            requested_material(WindowBackdrop::Frosted(ChromeTone::Dark)),
            Some(NSVisualEffectMaterial::UnderWindowBackground),
        );
        assert_eq!(requested_material(WindowBackdrop::Absent), None);
    }
}
