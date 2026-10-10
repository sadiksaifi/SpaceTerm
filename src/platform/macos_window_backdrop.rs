//! The application-owned native backdrop behind one Operating-System Window's content.
//!
//! GPUI's blurred background depends on private layer names, so SpaceTerm installs an unmodified
//! `NSVisualEffectView` as a sibling beneath GPUI's rendering view instead.
//!
//! A Background Image is a layer-backed view at the very bottom. While it is present the material
//! blends within the window, so it softens and tints the image exactly as it would the desktop.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, MainThreadMarker, MainThreadOnly, msg_send};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSImage, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindowOrderingMode,
};
use objc2_foundation::{NSData, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::appearance::ChromeTone;
use crate::platform::appearance::WindowBackdrop;

const BACKDROP_IDENTIFIER: &str = "dev.spaceterm.window-backdrop";
const BACKGROUND_IMAGE_IDENTIFIER: &str = "dev.spaceterm.window-background-image";

fn material(tone: ChromeTone) -> NSVisualEffectMaterial {
    match tone {
        ChromeTone::Bright => NSVisualEffectMaterial::Sidebar,
        ChromeTone::Dark => NSVisualEffectMaterial::UnderWindowBackground,
    }
}

/// Installs, updates, or removes the material behind one window's content.
pub(crate) fn apply(window: &gpui::Window, backdrop: WindowBackdrop) {
    if let Some((content_view, mtm)) = content_view(window) {
        apply_to_content_view(&content_view, requested_material(backdrop), mtm);
    }
}

/// Installs, replaces, or removes the Background Image beneath one window's material. Bytes the
/// system cannot decode present no image.
pub(crate) fn apply_background_image(window: &gpui::Window, image: Option<&[u8]>) {
    if let Some((content_view, mtm)) = content_view(window) {
        apply_image_to_content_view(&content_view, image, mtm);
    }
}

fn content_view(window: &gpui::Window) -> Option<(Retained<NSView>, MainThreadMarker)> {
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return None;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    let mtm = MainThreadMarker::new()?;
    // SAFETY: GPUI owns this live NSView for the synchronous appearance call.
    let rendering_view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
    let content_view = rendering_view.window()?.contentView()?;
    Some((content_view, mtm))
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
    blend_with_background_image(content_view);
}

fn apply_image_to_content_view(content_view: &NSView, image: Option<&[u8]>, mtm: MainThreadMarker) {
    let image =
        image.and_then(|bytes| NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(bytes)));
    let installed = identified_subview(content_view, BACKGROUND_IMAGE_IDENTIFIER);
    match (image, installed) {
        (None, Some(installed)) => installed.removeFromSuperview(),
        (None, None) => {}
        (Some(image), Some(installed)) => present_image(&installed, &image),
        (Some(image), None) => install_image(content_view, &image, mtm),
    }
    blend_with_background_image(content_view);
}

/// The material softens the desktop, or the Background Image when one lies beneath it.
fn blend_with_background_image(content_view: &NSView) {
    let Some(backdrop) = installed_backdrop(content_view) else {
        return;
    };
    backdrop.setBlendingMode(
        if identified_subview(content_view, BACKGROUND_IMAGE_IDENTIFIER).is_some() {
            NSVisualEffectBlendingMode::WithinWindow
        } else {
            NSVisualEffectBlendingMode::BehindWindow
        },
    );
}

fn installed_backdrop(content_view: &NSView) -> Option<Retained<NSVisualEffectView>> {
    identified_subview(content_view, BACKDROP_IDENTIFIER)?
        .downcast()
        .ok()
}

fn identified_subview(content_view: &NSView, wanted: &str) -> Option<Retained<NSView>> {
    let subviews = content_view.subviews();
    for index in 0..subviews.count() {
        let subview = subviews.objectAtIndex(index);
        // SAFETY: NSView implements identifier and objc2 retains its autoreleased return.
        let identifier: Option<Retained<NSString>> = unsafe { msg_send![&*subview, identifier] };
        if identifier
            .as_deref()
            .is_some_and(|identifier| identifier.to_string() == wanted)
        {
            return Some(subview);
        }
    }
    None
}

fn set_identifier(view: &NSView, identifier: &str) {
    let identifier = NSString::from_str(identifier);
    // SAFETY: NSView implements setIdentifier: and copies this live string.
    let _: () = unsafe { msg_send![view, setIdentifier: &*identifier] };
}

fn fill_content_view(view: &NSView) {
    view.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
}

/// The image fills the window and keeps its proportions, cropping what overflows.
fn install_image(content_view: &NSView, image: &NSImage, mtm: MainThreadMarker) {
    let view = NSView::initWithFrame(NSView::alloc(mtm), content_view.bounds());
    view.setWantsLayer(true);
    set_identifier(&view, BACKGROUND_IMAGE_IDENTIFIER);
    fill_content_view(&view);
    present_image(&view, image);
    content_view.addSubview_positioned_relativeTo(&view, NSWindowOrderingMode::Below, None);
}

fn present_image(view: &NSView, image: &NSImage) {
    let gravity = NSString::from_str("resizeAspectFill");
    // SAFETY: the view is layer-backed, so `layer` returns its live CALayer, which accepts an
    // NSImage as contents and a gravity string.
    unsafe {
        let layer: Option<Retained<AnyObject>> = msg_send![view, layer];
        let Some(layer) = layer else {
            return;
        };
        let _: () = msg_send![&*layer, setContents: image];
        let _: () = msg_send![&*layer, setContentsGravity: &*gravity];
    }
}

fn install(content_view: &NSView, material: NSVisualEffectMaterial, mtm: MainThreadMarker) {
    let backdrop =
        NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), content_view.bounds());
    backdrop.setMaterial(material);
    backdrop.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    backdrop.setState(NSVisualEffectState::Active);
    set_identifier(&backdrop, BACKDROP_IDENTIFIER);
    fill_content_view(&backdrop);
    // The material rests directly on a Background Image, or at the bottom without one.
    match identified_subview(content_view, BACKGROUND_IMAGE_IDENTIFIER) {
        Some(image) => content_view.addSubview_positioned_relativeTo(
            &backdrop,
            NSWindowOrderingMode::Above,
            Some(&image),
        ),
        None => content_view.addSubview_positioned_relativeTo(
            &backdrop,
            NSWindowOrderingMode::Below,
            None,
        ),
    }
}

#[cfg(all(test, feature = "native-tests"))]
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

    /// A 1x1 PNG the system decodes.
    const PIXEL: &[u8] = b"\x89\x50\x4e\x47\x0d\x0a\x1a\x0a\x00\x00\x00\x0d\x49\x48\x44\x52\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00\x90\x77\x53\xde\x00\x00\x00\x0c\x49\x44\x41\x54\x78\x9c\x63\xf8\xdf\xc0\x00\x00\x04\x01\x01\x80\xc5\x2a\x18\x5d\x00\x00\x00\x00\x49\x45\x4e\x44\xae\x42\x60\x82";

    fn apply_image(content_view: &NSView, image: Option<&[u8]>) {
        super::apply_image_to_content_view(
            content_view,
            image,
            objc2::MainThreadMarker::new().expect("native test must run on the main thread"),
        );
    }

    pub(in crate::platform) fn background_image_lies_beneath_a_material_that_blends_within_the_window(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|_| {
            let content = ContentView::new(NSSize::new(320.0, 180.0));
            let renderer = content.add_renderer();
            let dark = Some(material(ChromeTone::Dark));
            let order = |content: &ContentView| {
                let subviews = content.0.subviews();
                (0..subviews.count())
                    .map(|index| {
                        let view = subviews.objectAtIndex(index);
                        if std::ptr::eq(&*view, &*renderer) {
                            "renderer"
                        } else if content
                            .backdrop()
                            .is_some_and(|backdrop| std::ptr::eq(&*view, &**backdrop))
                        {
                            "material"
                        } else {
                            "image"
                        }
                    })
                    .collect::<Vec<_>>()
            };
            let blending = |content: &ContentView| content.backdrop().unwrap().blendingMode();

            apply_to_content_view(&content.0, dark);
            apply_image(&content.0, Some(PIXEL));
            apply_image(&content.0, Some(PIXEL));
            assert_eq!(order(&content), ["image", "material", "renderer"]);
            assert_eq!(blending(&content), NSVisualEffectBlendingMode::WithinWindow);

            // Blur off and on again reinstalls the material on the image, not beneath it.
            apply_to_content_view(&content.0, None);
            assert_eq!(order(&content), ["image", "renderer"]);
            apply_to_content_view(&content.0, dark);
            assert_eq!(order(&content), ["image", "material", "renderer"]);
            assert_eq!(blending(&content), NSVisualEffectBlendingMode::WithinWindow);

            apply_image(&content.0, Some(b"not an image"));
            assert_eq!(order(&content), ["material", "renderer"]);
            assert_eq!(blending(&content), NSVisualEffectBlendingMode::BehindWindow);
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
