use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use block2::RcBlock;
use dispatch2::{DispatchQueue, MainThreadBound};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSModalResponse, NSView, NSWindow,
};
use objc2_foundation::{NSInteger, NSObject, NSObjectProtocol, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::application_quit::{
    ApplicationQuitAdapter, ApplicationQuitDecision, ApplicationQuitError, ApplicationQuitHandler,
    ApplicationQuitRequest, QuitAnswer, QuitButtons, QuitPrompt, present_window_quit,
};
use super::macos_alert::{AlertSeverity, application_alert};

const NS_TERMINATE_CANCEL: NSInteger = 0;
const NS_TERMINATE_NOW: NSInteger = 1;
/// `kCoreEventClass`, `kAEQuitApplication`, and `kAEQuitReason` from the Apple Event Manager.
const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
const QUIT_APPLICATION_EVENT: u32 = u32::from_be_bytes(*b"quit");
const QUIT_REASON_KEYWORD: u32 = u32::from_be_bytes(*b"why?");
static QUIT_STATE_ASSOCIATION: u8 = 0;
const OBJC_ASSOCIATION_RETAIN_NONATOMIC: usize = 1;

unsafe extern "C" {
    fn objc_getAssociatedObject(object: *const AnyObject, key: *const c_void) -> *mut AnyObject;
    fn objc_setAssociatedObject(
        object: *const AnyObject,
        key: *const c_void,
        value: *const AnyObject,
        policy: usize,
    );
    fn class_addMethod(
        class: *mut AnyClass,
        selector: Sel,
        implementation: Imp,
        types: *const i8,
    ) -> Bool;
}

struct ApplicationQuitState {
    handler: RefCell<Option<ApplicationQuitHandler>>,
    confirmed: Cell<bool>,
}

impl ApplicationQuitState {
    fn new() -> Self {
        Self {
            handler: RefCell::new(None),
            confirmed: Cell::new(false),
        }
    }
}

struct QuitStateHolderIvars {
    state: Rc<ApplicationQuitState>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and define_class! drops the Rc ivar.
    #[unsafe(super(NSObject))]
    #[name = "SpaceTermApplicationQuitStateHolder"]
    #[thread_kind = MainThreadOnly]
    #[ivars = QuitStateHolderIvars]
    struct QuitStateHolder;

    unsafe impl NSObjectProtocol for QuitStateHolder {}
);

impl QuitStateHolder {
    fn new(mtm: MainThreadMarker, state: Rc<ApplicationQuitState>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(QuitStateHolderIvars { state });
        // SAFETY: NSObject's init is its designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

pub(crate) struct MacosApplicationQuitAdapter {
    state: Rc<ApplicationQuitState>,
}

impl MacosApplicationQuitAdapter {
    pub(crate) fn new() -> Self {
        Self {
            state: Rc::new(ApplicationQuitState::new()),
        }
    }

    fn install_on_delegate(
        &self,
        delegate: &AnyObject,
        handler: ApplicationQuitHandler,
    ) -> Result<(), ApplicationQuitError> {
        let mtm = MainThreadMarker::new().ok_or(ApplicationQuitError::OffMainThread)?;
        self.install_on_delegate_with_marker(delegate, handler, mtm)
    }

    fn install_on_delegate_with_marker(
        &self,
        delegate: &AnyObject,
        handler: ApplicationQuitHandler,
        mtm: MainThreadMarker,
    ) -> Result<(), ApplicationQuitError> {
        install_should_terminate_method(delegate)?;
        retain_state(delegate, mtm, Rc::clone(&self.state))?;
        *self.state.handler.borrow_mut() = Some(handler);
        Ok(())
    }
}

impl ApplicationQuitAdapter for MacosApplicationQuitAdapter {
    fn install(&self, handler: ApplicationQuitHandler) -> Result<(), ApplicationQuitError> {
        let mtm = MainThreadMarker::new().ok_or(ApplicationQuitError::OffMainThread)?;
        if self.state.handler.borrow().is_some() {
            return Err(ApplicationQuitError::AlreadyInstalled);
        }
        let application = NSApplication::sharedApplication(mtm);
        let delegate = application
            .delegate()
            .ok_or(ApplicationQuitError::DelegateUnavailable)?;
        self.install_on_delegate((*delegate).as_ref(), handler)
    }

    /// An installed build focuses the default Quit button for both Return and Space. A
    /// Development run's quit ends its Terminal Sessions, so it focuses the safe Cancel and Return
    /// quits nothing.
    fn present_confirmation(
        &self,
        prompt: &QuitPrompt,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> gpui::Task<Option<QuitAnswer>> {
        let (Some(mtm), Some(sheet_window)) = (MainThreadMarker::new(), native_window(window))
        else {
            return present_window_quit(prompt, window, cx);
        };
        let alert = quit_alert(prompt, mtm);
        let buttons = prompt.buttons;
        // As with GPUI's own prompts, the sheet begins after this window update, because AppKit
        // reports the key window change back into GPUI while it presents.
        cx.spawn(async move |_| {
            let responses = begin_quit_sheet(&alert, &sheet_window, buttons);
            quit_answer(responses.recv().await.ok()?, buttons)
        })
    }

    /// The warning appears by itself in front of the app the person was using. The client keeps
    /// its activation policy, so a client shown only in the menu bar gains no Dock icon or
    /// application menu, and it creates no window of its own.
    fn present_windowless_confirmation(
        &self,
        prompt: &QuitPrompt,
        cx: &mut gpui::App,
    ) -> Option<gpui::Task<Option<QuitAnswer>>> {
        MainThreadMarker::new()?;
        let prompt = prompt.clone();
        let (answered, answers) = async_channel::bounded(1);
        // The alert runs its own modal loop, so it starts from the run loop rather than inside
        // GPUI's update. A modal loop that ends any other way answers nothing.
        DispatchQueue::main().exec_async(move || {
            let Some(mtm) = MainThreadMarker::new() else {
                return;
            };
            let application = NSApplication::sharedApplication(mtm);
            let alert = quit_alert(&prompt, mtm);
            application.activate();
            let answer = run_quit_alert(&alert, prompt.buttons, mtm);
            if answer != Some(QuitAnswer::Quit) {
                application.hide(None);
            }
            if let Some(answer) = answer {
                let _ = answered.try_send(answer);
            }
        });
        Some(cx.spawn(async move |_| answers.recv().await.ok()))
    }

    fn request_quit(&self, cx: &mut gpui::App) {
        if self.state.handler.borrow().is_some() {
            cx.quit();
        }
    }

    fn confirm_quit(&self, cx: &mut gpui::App) {
        if self.state.handler.borrow().is_none() {
            return;
        }
        self.state.confirmed.set(true);
        cx.quit();
    }
}

fn native_window(window: &gpui::Window) -> Option<Retained<NSWindow>> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    // SAFETY: GPUI owns this live NSView during the synchronous call.
    let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
    view.window()
}

/// Builds the quit warning with its buttons in [`QuitButtons::answers`] order. An installed
/// build's Quit is the default.
fn quit_alert(prompt: &QuitPrompt, mtm: MainThreadMarker) -> Retained<NSAlert> {
    let alert = application_alert(
        if prompt.buttons == QuitButtons::DestructiveQuit {
            AlertSeverity::Caution
        } else {
            AlertSeverity::Standard
        },
        mtm,
    );
    alert.setMessageText(&NSString::from_str(&prompt.title));
    alert.setInformativeText(&NSString::from_str(&prompt.detail));
    let buttons = prompt
        .buttons
        .answers()
        .iter()
        .map(|answer| {
            (
                *answer,
                alert.addButtonWithTitle(&NSString::from_str(answer.label())),
            )
        })
        .collect::<Vec<_>>();
    // A quit that ends Terminal Sessions rests on the safe Cancel, as every destructive
    // SpaceTerm alert does.
    let focused = if prompt.buttons == QuitButtons::Quit {
        QuitAnswer::Quit
    } else {
        QuitAnswer::Cancel
    };
    for (answer, button) in &buttons {
        if *answer == QuitAnswer::Quit && prompt.buttons == QuitButtons::DestructiveQuit {
            button.setHasDestructiveAction(true);
        }
        if *answer == focused {
            let focused: &NSView = button;
            alert.window().setInitialFirstResponder(Some(focused));
        }
    }
    alert
}

/// Presents `alert` as a sheet on `sheet_window` and returns the channel that receives its one
/// response. Beginning the sheet lays the alert out, and that layout can clear the default
/// button's Return, so the key equivalents are bound once the sheet has begun.
fn begin_quit_sheet(
    alert: &NSAlert,
    sheet_window: &NSWindow,
    buttons: QuitButtons,
) -> async_channel::Receiver<NSModalResponse> {
    let (answered, responses) = async_channel::bounded(1);
    let completion = RcBlock::new(move |response: NSModalResponse| {
        let _ = answered.try_send(response);
    });
    alert.beginSheetModalForWindow_completionHandler(sheet_window, Some(&completion));
    bind_quit_keys(alert, buttons);
    responses
}

/// Runs `alert` by itself until it ends. Running it lays it out, which can clear the default
/// button's Return as beginning a sheet does, so the key equivalents are bound from inside its
/// modal loop.
fn run_quit_alert(
    alert: &NSAlert,
    buttons: QuitButtons,
    mtm: MainThreadMarker,
) -> Option<QuitAnswer> {
    let running = MainThreadBound::new(alert.retain(), mtm);
    DispatchQueue::main().exec_async(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            bind_quit_keys(running.get(mtm), buttons);
        }
    });
    quit_answer(alert.runModal(), buttons)
}

/// Escape always cancels. Return chooses an installed build's Quit; a destructive Quit has no
/// key, so Return never ends Terminal Sessions.
fn bind_quit_keys(alert: &NSAlert, buttons: QuitButtons) {
    for (button, answer) in alert.buttons().iter().zip(buttons.answers()) {
        button.setKeyEquivalent(&NSString::from_str(match answer {
            QuitAnswer::Quit if buttons == QuitButtons::Quit => "\r",
            QuitAnswer::Quit => "",
            QuitAnswer::Cancel => "\u{1b}",
        }));
    }
}

/// Reads AppKit's response, which names each button by the order it was added. Ending the alert
/// any other way answers nothing.
fn quit_answer(response: NSModalResponse, buttons: QuitButtons) -> Option<QuitAnswer> {
    let index = usize::try_from(response.checked_sub(NSAlertFirstButtonReturn)?).ok()?;
    buttons.answers().get(index).copied()
}

fn install_should_terminate_method(delegate: &AnyObject) -> Result<(), ApplicationQuitError> {
    let class = delegate.class();
    let selector = sel!(applicationShouldTerminate:);
    if class.instance_method(selector).is_some() {
        return Err(ApplicationQuitError::DelegateConflict);
    }
    let implementation: unsafe extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> NSInteger =
        should_terminate;
    // SAFETY: The method encoding describes NSInteger self/_cmd/id on supported 64-bit macOS.
    // AppKit calls this method only while the delegate is alive on its main thread.
    let added = unsafe {
        class_addMethod(
            (class as *const AnyClass).cast_mut(),
            selector,
            std::mem::transmute::<
                unsafe extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> NSInteger,
                Imp,
            >(implementation),
            c"q@:@".as_ptr(),
        )
    };
    if !added.as_bool() {
        return Err(ApplicationQuitError::DelegateConflict);
    }
    Ok(())
}

fn association_key() -> *const c_void {
    (&raw const QUIT_STATE_ASSOCIATION).cast::<c_void>()
}

fn retain_state(
    delegate: &AnyObject,
    mtm: MainThreadMarker,
    state: Rc<ApplicationQuitState>,
) -> Result<(), ApplicationQuitError> {
    // SAFETY: This key is unique to the adapter and the delegate is live for the call.
    if !unsafe { objc_getAssociatedObject(delegate, association_key()) }.is_null() {
        return Err(ApplicationQuitError::AlreadyInstalled);
    }
    let holder = QuitStateHolder::new(mtm, state);
    // SAFETY: The runtime retains the holder under this unique key. Its Rc ivar drops when the
    // delegate releases the associated object.
    unsafe {
        objc_setAssociatedObject(
            delegate,
            association_key(),
            Retained::as_ptr(&holder).cast(),
            OBJC_ASSOCIATION_RETAIN_NONATOMIC,
        );
    }
    Ok(())
}

unsafe extern "C-unwind" fn should_terminate(
    this: &AnyObject,
    _: Sel,
    _: *mut AnyObject,
) -> NSInteger {
    // SAFETY: The delegate retains this holder under the adapter's unique association key.
    let holder = unsafe { objc_getAssociatedObject(this, association_key()).as_ref() };
    let Some(state) = holder
        .and_then(|holder| holder.downcast_ref::<QuitStateHolder>())
        .map(|holder| &holder.ivars().state)
    else {
        return NS_TERMINATE_NOW;
    };
    if state.confirmed.replace(false) {
        return NS_TERMINATE_NOW;
    }
    let decision = catch_unwind(AssertUnwindSafe(|| {
        let request = termination_request();
        state
            .handler
            .borrow()
            .as_ref()
            .map_or(ApplicationQuitDecision::Proceed, |handler| {
                handler.handle_native(request)
            })
    }))
    .unwrap_or(ApplicationQuitDecision::Cancel);
    match decision {
        ApplicationQuitDecision::Proceed => NS_TERMINATE_NOW,
        ApplicationQuitDecision::Cancel => NS_TERMINATE_CANCEL,
    }
}

/// A Quit Apple Event without a reason comes from the person, such as the Dock's Quit. One with a
/// reason means the session is logging out, restarting, or shutting down. A termination outside
/// any Apple Event comes from the client itself, such as an installed update relaunching it.
fn termination_request() -> ApplicationQuitRequest {
    let Some(manager_class) = AnyClass::get(c"NSAppleEventManager") else {
        return ApplicationQuitRequest::Exit;
    };
    // SAFETY: These Foundation selectors take no arguments or one FourCharCode, return
    // autoreleased objects or FourCharCodes, and run on the main thread inside AppKit's callback.
    unsafe {
        let manager: *mut AnyObject = msg_send![manager_class, sharedAppleEventManager];
        let Some(manager) = manager.as_ref() else {
            return ApplicationQuitRequest::Exit;
        };
        let event: *mut AnyObject = msg_send![manager, currentAppleEvent];
        let Some(event) = event.as_ref() else {
            return ApplicationQuitRequest::Exit;
        };
        request_from_event(event)
    }
}

fn request_from_event(event: &AnyObject) -> ApplicationQuitRequest {
    // SAFETY: The caller supplies a live NSAppleEventDescriptor from the event manager.
    unsafe {
        let class: u32 = msg_send![event, eventClass];
        let id: u32 = msg_send![event, eventID];
        if class != CORE_EVENT_CLASS || id != QUIT_APPLICATION_EVENT {
            return ApplicationQuitRequest::Exit;
        }
        let reason: *mut AnyObject =
            msg_send![event, paramDescriptorForKeyword: QUIT_REASON_KEYWORD];
        if reason.is_null() {
            ApplicationQuitRequest::Dismissal
        } else {
            ApplicationQuitRequest::SessionEnd
        }
    }
}

#[cfg(all(test, feature = "native-tests"))]
#[allow(dead_code)]
pub(in crate::platform) mod tests {
    use gpui::TestAppContext;
    use objc2::ClassType;
    use objc2_app_kit::{NSApplicationActivationPolicy, NSModalResponseStop};

    use super::*;

    #[test]
    fn quit_event_reasons_distinguish_personal_dismissal_from_session_exit() {
        objc2::rc::autoreleasepool(|_| {
            let class = AnyClass::get(c"NSAppleEventDescriptor").unwrap();
            // SAFETY: Foundation constructs a local descriptor; this sends no Apple Event.
            let event: Retained<AnyObject> = unsafe {
                msg_send![class,
                    appleEventWithEventClass: CORE_EVENT_CLASS,
                    eventID: QUIT_APPLICATION_EVENT,
                    targetDescriptor: std::ptr::null_mut::<AnyObject>(),
                    returnID: -1_i16,
                    transactionID: 0_i32
                ]
            };
            assert_eq!(
                request_from_event(&event),
                ApplicationQuitRequest::Dismissal
            );
            // The SDK defines kAEQuitReason as a parameter, not an Apple Event attribute.
            // SAFETY: Both selectors use the documented FourCharCode and object signatures.
            unsafe {
                let reason: Retained<AnyObject> =
                    msg_send![class, descriptorWithTypeCode: u32::from_be_bytes(*b"logo")];
                let _: () = msg_send![&*event, setParamDescriptor: &*reason, forKeyword: QUIT_REASON_KEYWORD];
            }
            assert_eq!(
                request_from_event(&event),
                ApplicationQuitRequest::SessionEnd
            );
        });
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and this test class has no ivars.
        #[unsafe(super(NSObject))]
        #[name = "SpaceTermApplicationQuitTestDelegate"]
        struct TestDelegate;

        unsafe impl NSObjectProtocol for TestDelegate {}
    );

    /// How the test answers one presented warning.
    #[derive(Clone, Copy, Debug)]
    enum Answering {
        Click(usize),
        /// A key press sent to the warning, by its characters and virtual key code.
        Key(&'static str, u16),
    }

    fn quit_prompt_fixture(buttons: QuitButtons) -> QuitPrompt {
        QuitPrompt {
            title: "Quit SpaceTerm?".to_owned(),
            detail: match buttons {
                QuitButtons::Quit => "SpaceTerm leaves the menu bar. Your Terminal Sessions keep running, so everything is where you left it.",
                QuitButtons::DestructiveQuit => "This development build runs its own server. Quitting ends its 3 Terminal Sessions and stops their running commands.",
                QuitButtons::CancelOnly => "Reconnect to the development server, then try again.",
            }
            .to_owned(),
            buttons,
        }
    }

    fn press(window: &NSWindow, characters: &str, code: u16) {
        use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
        use objc2_foundation::NSPoint;

        let characters = NSString::from_str(characters);
        for kind in [NSEventType::KeyDown, NSEventType::KeyUp] {
            let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
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
            .expect("AppKit builds a key event");
            window.sendEvent(&event);
        }
    }

    /// The titles and key equivalents of `alert`'s buttons, and the index of the focused one.
    fn laid_out(alert: &NSAlert) -> (Vec<(String, String)>, Option<usize>) {
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

    /// Presents each sheet on a real window that stays off screen, lets AppKit lay it out and draw
    /// it, then answers it by a click or a key and reads the answer the production mapping
    /// delivers.
    pub(in crate::platform) fn quit_sheet_delivers_each_answer_and_keys_quit_by_default() {
        use objc2_app_kit::{NSBackingStoreType, NSWindowStyleMask};
        use objc2_foundation::{NSDate, NSPoint, NSRect, NSRunLoop, NSSize};

        let mtm = objc2::MainThreadMarker::new().expect("native test must run on the main thread");
        let settle =
            || NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.3));
        for (buttons, keys, focused, answers) in answer_cases() {
            for &(answering, expected) in answers {
                // SAFETY: The window is created and released on the main thread, and AppKit does
                // not release it on close because the test owns it.
                let host = unsafe {
                    NSWindow::initWithContentRect_styleMask_backing_defer(
                        NSWindow::alloc(mtm),
                        NSRect::new(
                            NSPoint::new(-20_000.0, -20_000.0),
                            NSSize::new(600.0, 400.0),
                        ),
                        NSWindowStyleMask::Titled,
                        NSBackingStoreType::Buffered,
                        false,
                    )
                };
                unsafe { host.setReleasedWhenClosed(false) };
                let prompt = quit_prompt_fixture(buttons);
                let alert = quit_alert(&prompt, mtm);
                assert_eq!(alert.informativeText().to_string(), prompt.detail);
                let responses = begin_quit_sheet(&alert, &host, buttons);
                settle();
                let sheet = alert.window();
                sheet.displayIfNeeded();
                assert!(
                    host.attachedSheet().is_some(),
                    "the alert begins as a sheet"
                );
                // Space presses the focused button.
                assert_eq!(laid_out(&alert), (expected_keys(keys), Some(focused)));

                match answering {
                    // SAFETY: The button is live on the main thread, and a nil sender is allowed.
                    Answering::Click(index) => unsafe {
                        alert.buttons().objectAtIndex(index).performClick(None)
                    },
                    Answering::Key(characters, code) => press(&sheet, characters, code),
                }
                settle();
                let Some(expected) = expected else {
                    assert!(
                        responses.try_recv().is_err(),
                        "{answering:?} answers nothing on {buttons:?}"
                    );
                    assert!(host.attachedSheet().is_some(), "the sheet stays open");
                    press(&sheet, "\u{1b}", 53);
                    settle();
                    assert_eq!(
                        responses
                            .try_recv()
                            .ok()
                            .and_then(|response| quit_answer(response, buttons)),
                        Some(QuitAnswer::Cancel)
                    );
                    host.close();
                    continue;
                };
                let response = responses
                    .try_recv()
                    .unwrap_or_else(|_| panic!("{answering:?} answers the sheet"));
                assert_eq!(
                    quit_answer(response, buttons),
                    Some(expected),
                    "{answering:?}: {response}"
                );
                assert!(host.attachedSheet().is_none(), "the answer ends the sheet");
                assert!(responses.try_recv().is_err(), "the sheet answers once");
                host.close();
            }
        }
    }

    /// Each warning's button keys and focused button, and how each answer ends it. An installed
    /// build's Quit is the default for Return and the focused button for Space. A Development
    /// run's Quit ends its Terminal Sessions, so focus rests on Cancel and Return answers nothing.
    /// A warning that offers only Cancel answers Escape, Space, or a click.
    type AnswerCase = (
        QuitButtons,
        &'static [(&'static str, &'static str)],
        usize,
        &'static [(Answering, Option<QuitAnswer>)],
    );

    fn answer_cases() -> [AnswerCase; 3] {
        [
            (
                QuitButtons::Quit,
                &[("Quit", "\r"), ("Cancel", "\u{1b}")],
                0,
                &[
                    (Answering::Click(0), Some(QuitAnswer::Quit)),
                    (Answering::Click(1), Some(QuitAnswer::Cancel)),
                    (Answering::Key("\r", 36), Some(QuitAnswer::Quit)),
                    (Answering::Key(" ", 49), Some(QuitAnswer::Quit)),
                    (Answering::Key("\u{1b}", 53), Some(QuitAnswer::Cancel)),
                ],
            ),
            (
                QuitButtons::DestructiveQuit,
                &[("Quit", ""), ("Cancel", "\u{1b}")],
                1,
                &[
                    (Answering::Click(0), Some(QuitAnswer::Quit)),
                    (Answering::Click(1), Some(QuitAnswer::Cancel)),
                    (Answering::Key("\r", 36), None),
                    (Answering::Key(" ", 49), Some(QuitAnswer::Cancel)),
                    (Answering::Key("\u{1b}", 53), Some(QuitAnswer::Cancel)),
                ],
            ),
            (
                QuitButtons::CancelOnly,
                &[("Cancel", "\u{1b}")],
                0,
                &[
                    (Answering::Click(0), Some(QuitAnswer::Cancel)),
                    (Answering::Key(" ", 49), Some(QuitAnswer::Cancel)),
                    (Answering::Key("\u{1b}", 53), Some(QuitAnswer::Cancel)),
                ],
            ),
        ]
    }

    fn expected_keys(keys: &[(&str, &str)]) -> Vec<(String, String)> {
        keys.iter()
            .map(|(title, key)| ((*title).to_owned(), (*key).to_owned()))
            .collect()
    }

    /// What the test saw while one warning asked by itself.
    struct Asking {
        policy: NSApplicationActivationPolicy,
        modal: bool,
        alone: bool,
        laid_out: (Vec<(String, String)>, Option<usize>),
        /// The answer left the warning open, so the test ended it with Escape.
        left_open: bool,
    }

    /// Runs each warning by itself, as a client with no window does, and answers it from inside
    /// its modal loop by a click or a key. The warning stays transparent and the test never
    /// activates the process, so nothing appears on screen or takes focus. Every modal loop the
    /// test starts ends: an answer that leaves the warning open is followed by Escape, then by
    /// stopping the loop.
    pub(in crate::platform) fn windowless_quit_alert_delivers_each_answer_without_a_window_or_policy_change()
     {
        use objc2_foundation::{NSDate, NSRunLoop};

        let mtm = objc2::MainThreadMarker::new().expect("native test must run on the main thread");
        let application = NSApplication::sharedApplication(mtm);
        let policy = application.activationPolicy();
        for (buttons, keys, focused, answers) in answer_cases() {
            let endings = answers
                .iter()
                .map(|&(answering, expected)| (Some(answering), expected))
                // A modal loop that ends without a button answers nothing.
                .chain([(None, None)]);
            for (answering, expected) in endings {
                let alert = quit_alert(&quit_prompt_fixture(buttons), mtm);
                alert.window().setAlphaValue(0.0);
                let asking = Rc::new(RefCell::new(None));
                let inside = MainThreadBound::new((alert.clone(), Rc::clone(&asking)), mtm);
                // Answers once the warning has bound its keys.
                let shortly = dispatch2::DispatchTime::NOW.time(100_000_000);
                let _ = DispatchQueue::main().after(shortly, move || {
                    let mtm =
                        MainThreadMarker::new().expect("the main queue runs on the main thread");
                    let (alert, asking) = inside.get(mtm);
                    let application = NSApplication::sharedApplication(mtm);
                    let window = alert.window();
                    let modal = application
                        .modalWindow()
                        .is_some_and(|modal| std::ptr::eq(&*modal, &*window));
                    let alone = application
                        .windows()
                        .iter()
                        .all(|other| !other.isVisible() || std::ptr::eq(&*other, &*window));
                    *asking.borrow_mut() = Some(Asking {
                        policy: application.activationPolicy(),
                        modal,
                        alone,
                        laid_out: laid_out(alert),
                        left_open: false,
                    });
                    match answering {
                        // SAFETY: The button is live on the main thread, and a nil sender is
                        // allowed.
                        Some(Answering::Click(index)) => unsafe {
                            alert.buttons().objectAtIndex(index).performClick(None)
                        },
                        Some(Answering::Key(characters, code)) => press(&window, characters, code),
                        None => application.stopModalWithCode(NSModalResponseStop),
                    }
                    wake_modal_loop(&application);
                    end_if_open(inside, false);
                });
                let answer = run_quit_alert(&alert, buttons, mtm);
                let asking = asking.borrow_mut().take().expect("the warning asks");
                assert!(asking.modal, "the warning runs its own modal loop");
                assert!(asking.alone, "the warning shows no other window");
                assert_eq!(asking.policy, policy, "asking keeps the activation policy");
                assert_eq!(asking.laid_out, (expected_keys(keys), Some(focused)));
                match expected {
                    Some(expected) => {
                        assert!(!asking.left_open, "{answering:?} ends the warning");
                        assert_eq!(answer, Some(expected), "{buttons:?} {answering:?}");
                    }
                    None if answering.is_some() => {
                        assert!(asking.left_open, "{answering:?} answers nothing");
                        assert_eq!(answer, Some(QuitAnswer::Cancel), "Escape cancels");
                    }
                    None => assert_eq!(answer, None, "stopping the loop answers nothing"),
                }
                assert!(application.modalWindow().is_none(), "no modal loop remains");
                assert_eq!(application.activationPolicy(), policy);
            }
        }
        // Lets each pending check find its warning already ended.
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.6));
    }

    type AskingAlert = MainThreadBound<(Retained<NSAlert>, Rc<RefCell<Option<Asking>>>)>;

    /// Checks shortly afterwards whether `alert` still runs its modal loop. The first check
    /// records it and presses Escape; a later one stops the loop.
    fn end_if_open(inside: AskingAlert, escaped: bool) {
        let later = dispatch2::DispatchTime::NOW.time(200_000_000);
        let _ = DispatchQueue::main().after(later, move || {
            let mtm = MainThreadMarker::new().expect("the main queue runs on the main thread");
            let (alert, asking) = inside.get(mtm);
            let application = NSApplication::sharedApplication(mtm);
            let window = alert.window();
            if !application
                .modalWindow()
                .is_some_and(|modal| std::ptr::eq(&*modal, &*window))
            {
                return;
            }
            if escaped {
                application.stopModalWithCode(NSModalResponseStop);
                wake_modal_loop(&application);
                return;
            }
            if let Some(asking) = asking.borrow_mut().as_mut() {
                asking.left_open = true;
            }
            press(&window, "\u{1b}", 53);
            wake_modal_loop(&application);
            end_if_open(inside, true);
        });
    }

    /// A modal loop checks for its end after each event, and a button pressed outside AppKit's
    /// event handling delivers none, so an empty event follows.
    fn wake_modal_loop(application: &NSApplication) {
        use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
        use objc2_foundation::NSPoint;

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

    pub(in crate::platform) fn native_hook_cancels_policy_then_consumes_one_confirmation(
        cx: &mut TestAppContext,
    ) {
        let adapter = MacosApplicationQuitAdapter::new();
        let requests = Rc::new(Cell::new(0));
        let recorded_requests = Rc::clone(&requests);
        let handler = cx.update(|cx| {
            ApplicationQuitHandler::new(
                cx,
                Rc::new(move |_, request| {
                    // A termination outside any Apple Event is the client's own exit.
                    assert_eq!(request, ApplicationQuitRequest::Exit);
                    recorded_requests.set(recorded_requests.get() + 1);
                    ApplicationQuitDecision::Cancel
                }),
            )
        });
        // SAFETY: NSObject's new selector returns one owned test delegate.
        let delegate: Retained<TestDelegate> = unsafe { msg_send![TestDelegate::class(), new] };
        adapter
            .install_on_delegate_with_marker(
                &delegate,
                handler,
                objc2::MainThreadMarker::new().expect("native test must run on the main thread"),
            )
            .expect("native quit hook should install");
        assert_eq!(Rc::strong_count(&adapter.state), 2);

        // SAFETY: The installed selector has this exact NSInteger self/_cmd/id signature.
        let cancelled: NSInteger = unsafe {
            msg_send![&*delegate, applicationShouldTerminate: std::ptr::null_mut::<AnyObject>()]
        };
        assert_eq!(cancelled, NS_TERMINATE_CANCEL);
        assert_eq!(requests.get(), 1);

        cx.update(|cx| adapter.confirm_quit(cx));
        // SAFETY: The delegate and installed selector remain live for both synchronous calls.
        let confirmed: NSInteger = unsafe {
            msg_send![&*delegate, applicationShouldTerminate: std::ptr::null_mut::<AnyObject>()]
        };
        // SAFETY: The delegate and installed selector remain live for this synchronous call.
        let cancelled_again: NSInteger = unsafe {
            msg_send![&*delegate, applicationShouldTerminate: std::ptr::null_mut::<AnyObject>()]
        };
        assert_eq!(confirmed, NS_TERMINATE_NOW);
        assert_eq!(cancelled_again, NS_TERMINATE_CANCEL);
        assert_eq!(requests.get(), 2);
        drop(delegate);
        assert_eq!(Rc::strong_count(&adapter.state), 1);
    }
}
