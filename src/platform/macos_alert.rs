//! Native alerts that always show an icon, and the modal loop that runs them by themselves.
use block2::RcBlock;
use dispatch2::MainThreadBound;
use objc2::{MainThreadMarker, MainThreadOnly, Message, rc::Retained};
use objc2_app_kit::{
    NSAlert, NSAlertStyle, NSApplication, NSModalPanelRunLoopMode, NSModalResponse,
};
use objc2_foundation::{NSArray, NSRunLoop};

/// What an alert asks of the person, which decides its icon.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AlertSeverity {
    /// Information, a problem report, or a choice that loses nothing. Shows the application
    /// icon.
    Standard,
    /// A choice that can lose work people may not expect to lose, such as programs running in
    /// Terminal Sessions. Shows the caution symbol badged with the application icon. Apple's
    /// Human Interface Guidelines reserve it for these choices, so a choice that only removes
    /// what the person asked to remove stays Standard.
    Caution,
}

/// Creates an alert whose icon matches `severity`. macOS 26 omits the application icon from a
/// sheet alert that keeps its default image, and it badges a sheet's caution symbol only with an
/// explicit one. App-modal alerts show the icon either way, so every alert sets it and both
/// presentations match.
pub(super) fn application_alert(
    severity: AlertSeverity,
    mtm: MainThreadMarker,
) -> Retained<NSAlert> {
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(match severity {
        AlertSeverity::Standard => NSAlertStyle::Informational,
        AlertSeverity::Caution => NSAlertStyle::Critical,
    });
    if let Some(icon) = NSApplication::sharedApplication(mtm).applicationIconImage() {
        // SAFETY: The icon is a live image, so the setter never receives nil.
        unsafe { alert.setIcon(Some(&icon)) };
    }
    alert
}

/// How the client takes the keyboard before it shows an alert by itself.
#[derive(Clone, Copy)]
pub(super) struct AlertActivation(fn(&NSApplication));

impl AlertActivation {
    /// Activates the client even while another app is frontmost, as GPUI's `cx.activate(true)`
    /// does. The frontmost app can decline the cooperative `activate()`, and the window server
    /// then sends every key to that app, so the alert never becomes key. The activation policy
    /// does not change.
    pub(super) const FORCEFUL: Self = Self(|application| {
        #[allow(deprecated)]
        application.activateIgnoringOtherApps(true);
    });
}

/// Activates the client through `activation`, then runs `alert` app-modally until it ends and
/// returns its response. Starting the modal loop lays the alert out and clears the Return its
/// buttons were given. The window also takes its initial first responder from the main queue,
/// which serves nothing else while the caller is itself a main-queue block. So once the loop has
/// started, a block served in the loop's own run loop mode focuses the initial first responder
/// and calls `bind_keys`.
pub(super) fn run_modal_alert(
    alert: &NSAlert,
    activation: AlertActivation,
    bind_keys: impl Fn(&NSAlert) + Send + 'static,
) -> NSModalResponse {
    let running = MainThreadBound::new(alert.retain(), alert.mtm());
    let started = RcBlock::new(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let alert = running.get(mtm);
        let window = alert.window();
        if let Some(focused) = window.initialFirstResponder() {
            window.makeFirstResponder(Some(&focused));
        }
        bind_keys(alert);
    });
    // SAFETY: The block may be sent: it reaches the alert only through MainThreadBound, and only on
    // the main thread. The mode is AppKit's constant.
    unsafe {
        NSRunLoop::currentRunLoop()
            .performInModes_block(&NSArray::from_slice(&[NSModalPanelRunLoopMode]), &started);
    }
    (activation.0)(&NSApplication::sharedApplication(alert.mtm()));
    alert.runModal()
}

/// Helpers for native fixtures that answer an alert from inside its modal loop.
#[cfg(all(test, feature = "native-tests"))]
pub(super) mod testing {
    use std::cell::{Cell, RefCell};
    use std::ffi::c_void;
    use std::rc::Rc;

    use core_foundation::date::CFDate;
    use core_foundation::runloop::{
        CFRunLoop, CFRunLoopMode, CFRunLoopTimer, CFRunLoopTimerContext, CFRunLoopTimerRef,
        kCFRunLoopCommonModes,
    };
    use dispatch2::DispatchQueue;
    use objc2_app_kit::{
        NSApplicationActivationPolicy, NSEvent, NSEventModifierFlags, NSEventType,
        NSModalResponseStop, NSView, NSWindow,
    };
    use objc2_foundation::{NSDate, NSPoint, NSString};

    use super::*;

    /// How a test answers one presented alert.
    #[derive(Clone, Copy, Debug)]
    pub(crate) enum Answering {
        Click(usize),
        /// A key press sent to the alert's window, by its characters and virtual key code.
        Key(&'static str, u16),
        /// A key press posted to the application, so it passes the event monitors first.
        PostedKey(&'static str, u16),
        /// The modal loop stops without a button.
        Stop,
    }

    thread_local! {
        /// Each activation recorded since the last alert, and whether no modal loop had started.
        static ACTIVATIONS: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
    }

    /// Records each activation request instead of activating, so the test process never takes
    /// the keyboard from the app in front.
    pub(crate) const RECORDED_ACTIVATION: AlertActivation = AlertActivation(|application| {
        let before_modal = application.modalWindow().is_none();
        ACTIVATIONS.with(|activations| activations.borrow_mut().push(before_modal));
    });

    /// What the test saw inside one alert's modal loop.
    pub(crate) struct Asking {
        pub(crate) modal: bool,
        pub(crate) active: bool,
        pub(crate) alone: bool,
        pub(crate) policy: NSApplicationActivationPolicy,
        pub(crate) laid_out: (Vec<(String, String)>, Option<usize>),
        /// The answer left the alert open, so the test stopped its modal loop.
        pub(crate) left_open: bool,
        /// Each activation [`RECORDED_ACTIVATION`] recorded for the alert, and whether it came
        /// before the alert's modal loop started.
        pub(crate) activations: Vec<bool>,
    }

    /// Answers `alert` from inside the modal loop that `present` queues on the main queue. The
    /// test drives the run loop until that block returns, as the client's own run loop does.
    /// While the alert's modal loop runs inside the block, the main queue serves nothing else,
    /// so the test acts from run loop timers. An answer that leaves the alert open is followed
    /// by stopping its loop, so every modal loop the test starts ends.
    pub(crate) fn answer_inside_modal_loop(
        alert: &NSAlert,
        answering: Answering,
        present: impl FnOnce(),
    ) -> Asking {
        let mtm = alert.mtm();
        let asking = Rc::new(RefCell::new(None));
        let (inside, seen) = (alert.retain(), Rc::clone(&asking));
        // Fires only inside a modal loop, after the keys the alert binds as its loop starts.
        after(0.1, modal_panel_mode(), move || {
            let application = NSApplication::sharedApplication(mtm);
            let window = inside.window();
            let alone = application
                .windows()
                .iter()
                .all(|other| !other.isVisible() || std::ptr::eq(&*other, &*window));
            *seen.borrow_mut() = Some(Asking {
                modal: is_modal(&inside),
                active: application.isActive(),
                alone,
                policy: application.activationPolicy(),
                laid_out: laid_out(&inside),
                left_open: false,
                activations: Vec::new(),
            });
            match answering {
                // SAFETY: The button is live on the main thread, and a nil sender is allowed.
                Answering::Click(index) => unsafe {
                    inside.buttons().objectAtIndex(index).performClick(None)
                },
                Answering::Key(characters, code) => press(&window, characters, code),
                Answering::PostedKey(characters, code) => post_key(&window, characters, code),
                Answering::Stop => application.stopModalWithCode(NSModalResponseStop),
            }
            wake_modal_loop(&application);
            // SAFETY: Core Foundation's constant is initialized before any run loop runs.
            after(0.2, unsafe { kCFRunLoopCommonModes }, move || {
                if !is_modal(&inside) {
                    return;
                }
                if let Some(asking) = seen.borrow_mut().as_mut() {
                    asking.left_open = true;
                }
                application.stopModalWithCode(NSModalResponseStop);
                wake_modal_loop(&application);
            });
        });
        ACTIVATIONS.with(|activations| activations.borrow_mut().clear());
        present();
        let returned = Rc::new(Cell::new(false));
        let returning = MainThreadBound::new(Rc::clone(&returned), mtm);
        // The main queue runs its blocks in order, so this one follows the presented alert's.
        DispatchQueue::main().exec_async(move || {
            if let Some(mtm) = MainThreadMarker::new() {
                returning.get(mtm).set(true);
            }
        });
        while !returned.get() {
            NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.05));
        }
        let mut asking = asking.borrow_mut().take().expect("the alert asks");
        asking.activations = ACTIVATIONS.with(|activations| activations.take());
        asking
    }

    /// Lets a later check find its alert already ended.
    pub(crate) fn settle_pending_checks() {
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.4));
    }

    fn is_modal(alert: &NSAlert) -> bool {
        let window = alert.window();
        NSApplication::sharedApplication(alert.mtm())
            .modalWindow()
            .is_some_and(|modal| std::ptr::eq(&*modal, &*window))
    }

    /// The mode an app-modal loop runs in. NSString is toll-free bridged to CFString.
    fn modal_panel_mode() -> CFRunLoopMode {
        // SAFETY: AppKit's constant is initialized before any run loop runs.
        let mode: &NSString = unsafe { NSModalPanelRunLoopMode };
        (mode as *const NSString).cast()
    }

    /// Runs `f` once from the current run loop after `seconds`, while the loop runs in `mode`.
    fn after(seconds: f64, mode: CFRunLoopMode, f: impl FnOnce() + 'static) {
        extern "C" fn fire(_: CFRunLoopTimerRef, info: *mut c_void) {
            // SAFETY: `info` is the box `after` leaked for this timer, which fires once.
            let f = unsafe { Box::from_raw(info.cast::<Box<dyn FnOnce()>>()) };
            f();
        }
        let f: Box<Box<dyn FnOnce()>> = Box::new(Box::new(f));
        let mut context = CFRunLoopTimerContext {
            version: 0,
            info: Box::into_raw(f).cast(),
            retain: None,
            release: None,
            copyDescription: None,
        };
        let timer = CFRunLoopTimer::new(
            CFDate::now().abs_time() + seconds,
            0.0,
            0,
            0,
            fire,
            &mut context,
        );
        CFRunLoop::get_current().add_timer(&timer, mode);
    }

    fn key_events(window: &NSWindow, characters: &str, code: u16) -> [Retained<NSEvent>; 2] {
        let characters = NSString::from_str(characters);
        [NSEventType::KeyDown, NSEventType::KeyUp].map(|kind| {
            NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                kind,
                NSPoint::new(0.0, 0.0),
                NSEventModifierFlags::empty(),
                0.0,
                window.windowNumber(),
                None,
                &characters,
                &characters,
                false,
                code,
            )
            .expect("AppKit builds a key event")
        })
    }

    pub(crate) fn press(window: &NSWindow, characters: &str, code: u16) {
        for event in key_events(window, characters, code) {
            window.sendEvent(&event);
        }
    }

    fn post_key(window: &NSWindow, characters: &str, code: u16) {
        let application = NSApplication::sharedApplication(window.mtm());
        for event in key_events(window, characters, code) {
            application.postEvent_atStart(&event, false);
        }
    }

    /// The titles and key equivalents of `alert`'s buttons, and the index of the focused one.
    pub(crate) fn laid_out(alert: &NSAlert) -> (Vec<(String, String)>, Option<usize>) {
        let buttons = alert.buttons();
        let focus = alert.window().firstResponder();
        let focused = buttons.iter().position(|button| {
            let button: &NSView = &button;
            focus.as_ref().is_some_and(|focus| {
                std::ptr::eq(
                    Retained::as_ptr(focus).cast::<()>(),
                    (button as *const NSView).cast::<()>(),
                )
            })
        });
        let keys = buttons
            .iter()
            .map(|button| {
                (
                    button.title().to_string(),
                    button.keyEquivalent().to_string(),
                )
            })
            .collect();
        (keys, focused)
    }

    /// A modal loop checks for its end after each event, and a button pressed outside AppKit's
    /// event handling delivers none, so an empty event follows.
    fn wake_modal_loop(application: &NSApplication) {
        let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
            NSEventType::ApplicationDefined,
            NSPoint::new(0.0, 0.0),
            NSEventModifierFlags::empty(),
            0.0,
            0,
            None,
            0,
            0,
            0,
        )
        .expect("AppKit builds an application event");
        application.postEvent_atStart(&event, false);
    }

    pub(crate) fn expected_keys(keys: &[(&str, &str)]) -> Vec<(String, String)> {
        keys.iter()
            .map(|(title, key)| ((*title).to_owned(), (*key).to_owned()))
            .collect()
    }
}
