//! What SpaceTerm's relaunch by itself reads from AppKit: every keyboard, input-method and pointer
//! event SpaceTerm receives, and the native alerts, sheets and menus it shows.
//!
//! The input count comes from an NSEvent local monitor. AppKit calls it with each event posted to
//! SpaceTerm before the event reaches a window, so neither a GPUI view that stops propagation nor
//! an input method composing text can hide a keystroke, and a scroll or a click without movement
//! counts as much as a move.

use std::cell::Cell;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventMask, NSEventTrackingRunLoopMode, NSModalPanelRunLoopMode,
};
use objc2_foundation::NSRunLoop;

/// Every event a person makes with a keyboard, an input method, a pointer, a trackpad or a tablet.
fn input_mask() -> NSEventMask {
    NSEventMask::KeyDown
        | NSEventMask::KeyUp
        | NSEventMask::FlagsChanged
        | NSEventMask::LeftMouseDown
        | NSEventMask::LeftMouseUp
        | NSEventMask::RightMouseDown
        | NSEventMask::RightMouseUp
        | NSEventMask::OtherMouseDown
        | NSEventMask::OtherMouseUp
        | NSEventMask::MouseMoved
        | NSEventMask::LeftMouseDragged
        | NSEventMask::RightMouseDragged
        | NSEventMask::OtherMouseDragged
        | NSEventMask::ScrollWheel
        | NSEventMask::Gesture
        | NSEventMask::Magnify
        | NSEventMask::Swipe
        | NSEventMask::Rotate
        | NSEventMask::BeginGesture
        | NSEventMask::EndGesture
        | NSEventMask::SmartMagnify
        | NSEventMask::Pressure
        | NSEventMask::DirectTouch
        | NSEventMask::TabletPoint
        | NSEventMask::TabletProximity
}

/// [`crate::updates::Presence`] from AppKit and SpaceTerm's portable alerts and menus.
pub(crate) struct MacosPresence {
    events: Rc<Cell<u64>>,
    monitor: Retained<AnyObject>,
    mtm: MainThreadMarker,
}

impl MacosPresence {
    /// Starts counting input. `None` off the main thread or when AppKit refuses the monitor, and
    /// SpaceTerm then never counts as unused.
    pub(crate) fn install() -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let events = Rc::new(Cell::new(0u64));
        let counted = Rc::clone(&events);
        let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            counted.set(counted.get().wrapping_add(1));
            event.as_ptr()
        });
        // SAFETY: The mask is a valid event mask and the handler returns the event it was given,
        // so AppKit dispatches every event unchanged. The handler runs on the main thread, where
        // the count lives.
        let monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(input_mask(), &handler)
        }?;
        Some(Self {
            events,
            monitor,
            mtm,
        })
    }

    /// How many input events SpaceTerm has received since [`Self::install`].
    pub(crate) fn events(&self) -> u64 {
        self.events.get()
    }

    /// Whether AppKit shows a native alert, sheet or menu: a modal alert such as the windowless
    /// quit warning, a sheet on any window such as the quit-warning sheet, or menu tracking in
    /// the application, status or context menus. The main thread runs SpaceTerm's code in the
    /// tracking or modal-panel mode only while such a loop runs.
    pub(crate) fn presenting_natively(&self) -> bool {
        let application = NSApplication::sharedApplication(self.mtm);
        if application.modalWindow().is_some() {
            return true;
        }
        if application
            .windows()
            .iter()
            .any(|window| window.attachedSheet().is_some())
        {
            return true;
        }
        let mode = NSRunLoop::currentRunLoop().currentMode();
        // SAFETY: AppKit's run-loop mode constants are immutable NSStrings.
        let (tracking, modal) = unsafe { (NSEventTrackingRunLoopMode, NSModalPanelRunLoopMode) };
        mode.is_some_and(|mode| &*mode == tracking || &*mode == modal)
    }
}

impl Drop for MacosPresence {
    fn drop(&mut self) {
        // SAFETY: The monitor came from addLocalMonitorForEventsMatchingMask and is removed once.
        unsafe { NSEvent::removeMonitor(&self.monitor) };
    }
}

impl crate::updates::Presence for MacosPresence {
    fn presenting(&self, cx: &mut gpui::App) -> bool {
        self.presenting_natively() || crate::updates::portable_presenting(cx)
    }

    fn input(&self, _: &mut gpui::App) -> Option<u64> {
        Some(self.events())
    }
}

#[cfg(all(test, feature = "native-tests"))]
#[allow(dead_code)]
pub(in crate::platform) mod tests {
    use objc2::ClassType;
    use objc2::rc::Retained;
    use objc2::{MainThreadOnly, msg_send, sel};
    use objc2_app_kit::{
        NSAlert, NSApplication, NSBackingStoreType, NSEvent, NSEventModifierFlags, NSEventType,
        NSMenu, NSWindow, NSWindowStyleMask,
    };
    use objc2_foundation::{
        NSArray, NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString,
    };

    use super::*;

    /// CoreGraphics' opaque event, named so objc2 can check the message's argument type.
    #[repr(C)]
    struct CgEvent {
        _private: [u8; 0],
    }

    // SAFETY: CGEventRef is a pointer to the opaque `__CGEvent` struct.
    unsafe impl objc2::encode::RefEncode for CgEvent {
        const ENCODING_REF: objc2::encode::Encoding =
            objc2::encode::Encoding::Pointer(&objc2::encode::Encoding::Struct("__CGEvent", &[]));
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreateScrollWheelEvent2(
            source: *const std::ffi::c_void,
            units: u32,
            wheel_count: u32,
            wheel1: i32,
            wheel2: i32,
            wheel3: i32,
        ) -> *mut CgEvent;
        fn CFRelease(object: *const std::ffi::c_void);
    }

    /// Posts `event` to SpaceTerm and lets AppKit take it from the queue and dispatch it, as it
    /// does a person's. Events queued before it are dispatched first. AppKit hands back a copy,
    /// so the event is found by its type and its timestamp, which each fixture event makes
    /// unique.
    fn deliver(event: &NSEvent, mtm: MainThreadMarker) {
        let application = NSApplication::sharedApplication(mtm);
        application.postEvent_atStart(event, false);
        // SAFETY: The mode is AppKit's constant.
        let mode = unsafe { NSDefaultRunLoopMode };
        for _ in 0..64 {
            let Some(taken) = application.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&NSDate::dateWithTimeIntervalSinceNow(1.0)),
                mode,
                true,
            ) else {
                break;
            };
            let ours = taken.r#type() == event.r#type() && taken.timestamp() == event.timestamp();
            application.sendEvent(&taken);
            if ours {
                return;
            }
        }
        panic!("AppKit never dispatched the posted event");
    }

    /// A timestamp no other fixture event has.
    fn moment() -> f64 {
        thread_local!(static NEXT: Cell<f64> = const { Cell::new(1_000.0) });
        NEXT.with(|next| {
            next.set(next.get() + 1.0);
            next.get()
        })
    }

    fn mouse(kind: NSEventType, at: NSPoint, clicks: isize) -> Retained<NSEvent> {
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind, at, NSEventModifierFlags::empty(), moment(), 0, None, 0, clicks, 1.0,
        )
        .unwrap()
    }

    fn key(kind: NSEventType, text: &str) -> Retained<NSEvent> {
        let text = NSString::from_str(text);
        NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            kind, NSPoint::new(0.0, 0.0), NSEventModifierFlags::empty(), moment(), 0, None, &text,
            &text, false, 0,
        )
        .unwrap()
    }

    fn scroll() -> Retained<NSEvent> {
        // SAFETY: CoreGraphics returns a +1 CGEvent, or null. NSEvent copies what it needs and
        // the CGEvent is released after.
        unsafe {
            let event = CGEventCreateScrollWheelEvent2(std::ptr::null(), 0, 1, -3, 0, 0);
            assert!(!event.is_null());
            let wrapped: Option<Retained<NSEvent>> =
                msg_send![NSEvent::class(), eventWithCGEvent: event];
            CFRelease(event.cast());
            wrapped.unwrap()
        }
    }

    /// Each kind of input the relaunch must see raises the count: a key, a click without
    /// movement, a scroll, and a move away and back to the same point.
    pub(in crate::platform) fn every_key_click_scroll_and_move_counts_as_input() {
        let mtm = MainThreadMarker::new().unwrap();
        let presence = MacosPresence::install().unwrap();
        let counted = |event: Retained<NSEvent>, what: &str| {
            let before = presence.events();
            deliver(&event, mtm);
            assert!(presence.events() > before, "{what} counts as input");
        };
        counted(key(NSEventType::KeyDown, "a"), "a key press");
        counted(key(NSEventType::KeyUp, "a"), "a key release");
        let here = NSPoint::new(40.0, 40.0);
        counted(
            mouse(NSEventType::LeftMouseDown, here, 1),
            "a press without movement",
        );
        counted(
            mouse(NSEventType::LeftMouseUp, here, 1),
            "a release without movement",
        );
        counted(scroll(), "a scroll");
        let away = NSPoint::new(400.0, 300.0);
        let before = presence.events();
        deliver(&mouse(NSEventType::MouseMoved, away, 0), mtm);
        deliver(&mouse(NSEventType::MouseMoved, here, 0), mtm);
        assert!(
            presence.events() >= before + 2,
            "a move away and back counts twice, though the pointer ends where it was"
        );
    }

    /// A sheet on any window and a modal alert keep SpaceTerm presenting until they end.
    pub(in crate::platform) fn a_native_sheet_or_modal_alert_counts_as_presenting() {
        let mtm = MainThreadMarker::new().unwrap();
        let presence = MacosPresence::install().unwrap();
        assert!(!presence.presenting_natively());

        // SAFETY: A plain titled window, released when it closes.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(320.0, 200.0)),
                NSWindowStyleMask::Titled,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: The test owns the window until it closes it below.
        unsafe { window.setReleasedWhenClosed(false) };
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str("Quit SpaceTerm?"));
        alert.beginSheetModalForWindow_completionHandler(&window, None);
        assert!(presence.presenting_natively(), "a sheet is open");
        window.endSheet(&alert.window());
        assert!(!presence.presenting_natively(), "the sheet ended");

        let application = NSApplication::sharedApplication(mtm);
        let observed = Rc::new(Cell::new(None));
        let inside = {
            let observed = Rc::clone(&observed);
            let presence = Rc::new(presence);
            let check = Rc::clone(&presence);
            let stop = RcBlock::new(move || {
                observed.set(Some(check.presenting_natively()));
                let mtm = MainThreadMarker::new().unwrap();
                NSApplication::sharedApplication(mtm).stopModal();
            });
            // SAFETY: The block runs on the main thread inside the modal loop below.
            unsafe {
                NSRunLoop::currentRunLoop()
                    .performInModes_block(&NSArray::from_slice(&[NSModalPanelRunLoopMode]), &stop);
            }
            presence
        };
        let panel = alert.window();
        application.runModalForWindow(&panel);
        assert_eq!(observed.get(), Some(true), "a modal alert is open");
        assert!(!inside.presenting_natively(), "the modal alert ended");
        window.close();
    }

    /// While a native menu tracks, SpaceTerm is presenting.
    pub(in crate::platform) fn native_menu_tracking_counts_as_presenting() {
        let mtm = MainThreadMarker::new().unwrap();
        let presence = Rc::new(MacosPresence::install().unwrap());
        let menu = NSMenu::new(mtm);
        // SAFETY: An item with no action, which the tracking loop shows and never performs.
        unsafe {
            menu.addItemWithTitle_action_keyEquivalent(
                &NSString::from_str("Item"),
                Some(sel!(description)),
                &NSString::from_str(""),
            );
        }
        let observed = Rc::new(Cell::new(None));
        let check = {
            let observed = Rc::clone(&observed);
            let presence = Rc::clone(&presence);
            let menu = menu.clone();
            RcBlock::new(move || {
                observed.set(Some(presence.presenting_natively()));
                menu.cancelTrackingWithoutAnimation();
            })
        };
        // SAFETY: The block runs on the main thread inside the menu's tracking loop below.
        unsafe {
            NSRunLoop::currentRunLoop()
                .performInModes_block(&NSArray::from_slice(&[NSEventTrackingRunLoopMode]), &check);
        }
        menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(100.0, 100.0), None);
        assert_eq!(observed.get(), Some(true), "a menu tracks");
        assert!(!presence.presenting_natively(), "the menu closed");
    }
}
