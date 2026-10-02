//! macOS adapter for the Setup Guide: System Settings' window from the window server's list, and
//! the running application's bundle and icon from AppKit.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{Bounds, DisplayId, ImageFormat, Pixels, point, px, size};
use objc2::AnyThread as _;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSDeviceRGBColorSpace, NSGraphicsContext,
    NSRunningApplication, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSBundle, NSDictionary, NSNumber, NSPoint, NSRect, NSSize, NSString, NSURL,
};

use super::setup_guide_host::{
    ApplicationBundle, SetupGuideHost, SetupGuideHostError, SystemSettingsWindow,
};

const SYSTEM_SETTINGS_BUNDLE_IDENTIFIER: &str = "com.apple.systempreferences";
/// The icon's pixel size, enough for the guide's largest presentation on a Retina display.
const ICON_PIXELS: isize = 128;
/// Windows smaller than this are utility surfaces, such as the window server's own overlays, that
/// never cover an application.
const MINIMUM_WINDOW_SIDE: f64 = 40.0;

const ON_SCREEN_ONLY: u32 = 1 << 0;
const EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;
const NULL_WINDOW: u32 = 0;
const MAXIMUM_DISPLAYS: u32 = 16;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> *mut c_void;
    fn CGGetActiveDisplayList(max_displays: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayBounds(display: u32) -> NSRect;
}

pub(crate) struct MacosSetupGuideHost;

impl SetupGuideHost for MacosSetupGuideHost {
    fn locate_system_settings(&self) -> SystemSettingsWindow {
        autoreleasepool(|_| {
            let owners = system_settings_processes();
            if owners.is_empty() {
                return SystemSettingsWindow::Closed;
            }
            let Some(frame) = frontmost_window(&on_screen_windows(), &owners) else {
                return SystemSettingsWindow::Covered;
            };
            match display_relative(frame, &active_displays()) {
                Some((display, bounds)) => SystemSettingsWindow::Frontmost { display, bounds },
                None => SystemSettingsWindow::Covered,
            }
        })
    }

    fn application_bundle(&self) -> Option<ApplicationBundle> {
        let bundle = NSBundle::mainBundle();
        let path = bundle.bundlePath();
        if !path.to_string().ends_with(".app") {
            return None;
        }
        let png = icon_png(&NSWorkspace::sharedWorkspace().iconForFile(&path))?;
        Some(ApplicationBundle {
            path: PathBuf::from(path.to_string()),
            icon: Arc::new(gpui::Image::from_bytes(ImageFormat::Png, png)),
        })
    }

    fn reveal_application_bundle(&self) -> Result<(), SetupGuideHostError> {
        let url = NSBundle::mainBundle().bundleURL();
        if !url
            .path()
            .is_some_and(|path| path.to_string().ends_with(".app"))
        {
            return Err(SetupGuideHostError::BundleUnavailable);
        }
        NSWorkspace::sharedWorkspace()
            .activateFileViewerSelectingURLs(&NSArray::<NSURL>::from_retained_slice(&[url]));
        Ok(())
    }
}

fn system_settings_processes() -> Vec<i32> {
    let identifier = NSString::from_str(SYSTEM_SETTINGS_BUNDLE_IDENTIFIER);
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&identifier)
        .iter()
        .map(|application| application.processIdentifier())
        .filter(|process| *process > 0)
        .collect()
}

/// One on-screen window as the window server lists it, front to back.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ListedWindow {
    owner: i32,
    layer: i64,
    alpha: f64,
    /// The frame in global coordinates, with the origin at the primary display's top-left corner.
    frame: NSRect,
}

fn on_screen_windows() -> Vec<ListedWindow> {
    // SAFETY: The call takes plain options and returns a +1 CFArray of CFDictionary, or null. Both
    // are toll-free bridged to their Foundation counterparts, so `Retained` takes that reference.
    let list = unsafe {
        CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP_ELEMENTS, NULL_WINDOW)
    };
    // SAFETY: See above; a null result yields `None`.
    let Some(list) = (unsafe {
        Retained::from_raw(list.cast::<NSArray<NSDictionary<NSString, AnyObject>>>())
    }) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|window| {
            let number = |key: &str| {
                window
                    .objectForKey(&NSString::from_str(key))
                    .and_then(|value| value.downcast::<NSNumber>().ok())
            };
            let bounds = window
                .objectForKey(&NSString::from_str("kCGWindowBounds"))
                .and_then(|value| value.downcast::<NSDictionary>().ok())?;
            let coordinate = |key: &str| {
                bounds
                    .objectForKey(&NSString::from_str(key))
                    .and_then(|value| value.downcast::<NSNumber>().ok())
                    .map(|value| value.doubleValue())
            };
            Some(ListedWindow {
                owner: number("kCGWindowOwnerPID")?.intValue(),
                layer: number("kCGWindowLayer")?.longLongValue(),
                alpha: number("kCGWindowAlpha").map_or(1.0, |alpha| alpha.doubleValue()),
                frame: NSRect::new(
                    NSPoint::new(coordinate("X")?, coordinate("Y")?),
                    NSSize::new(coordinate("Width")?, coordinate("Height")?),
                ),
            })
        })
        .collect()
}

/// System Settings' largest application-layer window, when the frontmost application-layer window
/// belongs to System Settings.
fn frontmost_window(windows: &[ListedWindow], owners: &[i32]) -> Option<NSRect> {
    let mut application_windows = windows.iter().filter(|window| {
        window.layer == 0
            && window.alpha > 0.0
            && window.frame.size.width >= MINIMUM_WINDOW_SIDE
            && window.frame.size.height >= MINIMUM_WINDOW_SIDE
    });
    let front = application_windows.next()?;
    if !owners.contains(&front.owner) {
        return None;
    }
    std::iter::once(front)
        .chain(application_windows)
        .filter(|window| owners.contains(&window.owner))
        .map(|window| window.frame)
        .max_by(|first, second| area(*first).total_cmp(&area(*second)))
}

fn active_displays() -> Vec<(u32, NSRect)> {
    let mut displays = [0_u32; MAXIMUM_DISPLAYS as usize];
    let mut count = 0_u32;
    // SAFETY: The buffer holds `MAXIMUM_DISPLAYS` entries and `count` receives how many were set.
    let status =
        unsafe { CGGetActiveDisplayList(MAXIMUM_DISPLAYS, displays.as_mut_ptr(), &mut count) };
    if status != 0 {
        return Vec::new();
    }
    displays[..count.min(MAXIMUM_DISPLAYS) as usize]
        .iter()
        // SAFETY: Each identifier came from the active display list.
        .map(|display| (*display, unsafe { CGDisplayBounds(*display) }))
        .collect()
}

/// The display showing most of `frame`, and `frame` relative to that display's top-left corner.
fn display_relative(frame: NSRect, displays: &[(u32, NSRect)]) -> Option<(DisplayId, Bounds<Pixels>)> {
    let (display, display_frame) = displays
        .iter()
        .map(|(display, bounds)| (*display, *bounds, overlap(frame, *bounds)))
        .filter(|(_, _, overlap)| *overlap > 0.0)
        .max_by(|first, second| first.2.total_cmp(&second.2))
        .map(|(display, bounds, _)| (display, bounds))?;
    Some((
        DisplayId::new(u64::from(display)),
        Bounds::new(
            point(
                px((frame.origin.x - display_frame.origin.x) as f32),
                px((frame.origin.y - display_frame.origin.y) as f32),
            ),
            size(px(frame.size.width as f32), px(frame.size.height as f32)),
        ),
    ))
}

fn area(frame: NSRect) -> f64 {
    frame.size.width * frame.size.height
}

fn overlap(first: NSRect, second: NSRect) -> f64 {
    let width = (first.origin.x + first.size.width).min(second.origin.x + second.size.width)
        - first.origin.x.max(second.origin.x);
    let height = (first.origin.y + first.size.height).min(second.origin.y + second.size.height)
        - first.origin.y.max(second.origin.y);
    width.max(0.0) * height.max(0.0)
}

/// Draws the icon into a fixed-size bitmap and encodes it as PNG.
fn icon_png(icon: &objc2_app_kit::NSImage) -> Option<Vec<u8>> {
    // SAFETY: Null planes ask AppKit to allocate the bitmap, and every size argument describes the
    // same 8-bit RGBA layout.
    let bitmap = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            ICON_PIXELS,
            ICON_PIXELS,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            0,
            0,
        )
    }?;
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap)?;
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&context));
    icon.drawInRect(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(ICON_PIXELS as f64, ICON_PIXELS as f64),
    ));
    context.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();
    // SAFETY: An empty property dictionary asks for the default PNG encoding.
    let png = unsafe {
        bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }?;
    Some(png.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(owner: i32, layer: i64, x: f64, y: f64, width: f64, height: f64) -> ListedWindow {
        ListedWindow {
            owner,
            layer,
            alpha: 1.0,
            frame: NSRect::new(NSPoint::new(x, y), NSSize::new(width, height)),
        }
    }

    const SETTINGS: i32 = 40;
    const OTHER: i32 = 41;

    #[test]
    fn system_settings_counts_as_frontmost_only_above_other_application_windows() {
        let settings = window(SETTINGS, 0, 100.0, 80.0, 700.0, 600.0);
        let other = window(OTHER, 0, 0.0, 0.0, 900.0, 700.0);
        let menu_bar = window(OTHER, 24, 0.0, 0.0, 1440.0, 30.0);

        assert_eq!(
            frontmost_window(&[menu_bar, settings, other], &[SETTINGS]),
            Some(settings.frame)
        );
        assert_eq!(frontmost_window(&[menu_bar, other, settings], &[SETTINGS]), None);
        assert_eq!(frontmost_window(&[menu_bar], &[SETTINGS]), None);
    }

    #[test]
    fn the_largest_system_settings_window_is_the_list_beside_its_alerts() {
        let alert = window(SETTINGS, 0, 300.0, 200.0, 260.0, 180.0);
        let settings = window(SETTINGS, 0, 100.0, 80.0, 700.0, 600.0);

        assert_eq!(
            frontmost_window(&[alert, settings], &[SETTINGS]),
            Some(settings.frame)
        );
    }

    #[test]
    fn invisible_and_tiny_windows_never_cover_system_settings() {
        let settings = window(SETTINGS, 0, 100.0, 80.0, 700.0, 600.0);
        let mut invisible = window(OTHER, 0, 0.0, 0.0, 900.0, 700.0);
        invisible.alpha = 0.0;
        let tiny = window(OTHER, 0, 0.0, 0.0, 10.0, 10.0);

        assert_eq!(
            frontmost_window(&[invisible, tiny, settings], &[SETTINGS]),
            Some(settings.frame)
        );
    }

    #[test]
    fn a_frame_is_placed_on_the_display_showing_most_of_it() {
        let primary = (1, NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1440.0, 900.0)));
        let right = (2, NSRect::new(NSPoint::new(1440.0, -100.0), NSSize::new(1920.0, 1080.0)));
        let frame = NSRect::new(NSPoint::new(1300.0, 50.0), NSSize::new(700.0, 600.0));

        assert_eq!(
            display_relative(frame, &[primary, right]),
            Some((
                DisplayId::new(2),
                Bounds::new(point(px(-140.0), px(150.0)), size(px(700.0), px(600.0)))
            ))
        );
        assert_eq!(
            display_relative(
                NSRect::new(NSPoint::new(5000.0, 0.0), NSSize::new(10.0, 10.0)),
                &[primary]
            ),
            None
        );
    }
}
