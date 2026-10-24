use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertSecondButtonReturn, NSApplication, NSModalResponse,
    NSView, NSWindow,
};
use objc2_foundation::{NSInteger, NSObject, NSObjectProtocol, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::application_quit::{
    ApplicationQuitAdapter, ApplicationQuitDecision, ApplicationQuitError, ApplicationQuitHandler,
    ApplicationQuitRequest, QuitAnswer, QuitPrompt, present_window_quit,
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

    /// Focus the default Quit button for both Return and Space.
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
        // As with GPUI's own prompts, the sheet begins after this window update, because AppKit
        // reports the key window change back into GPUI while it presents.
        cx.spawn(async move |_| {
            let responses = begin_quit_sheet(&alert, &sheet_window);
            quit_answer(responses.recv().await.ok()?)
        })
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

/// Builds the quit warning: the default Quit button, then Cancel.
fn quit_alert(prompt: &QuitPrompt, mtm: MainThreadMarker) -> Retained<NSAlert> {
    let alert = application_alert(
        if prompt.destructive {
            AlertSeverity::Caution
        } else {
            AlertSeverity::Standard
        },
        mtm,
    );
    alert.setMessageText(&NSString::from_str(&prompt.title));
    alert.setInformativeText(&NSString::from_str(&prompt.detail));
    let quit = alert.addButtonWithTitle(&NSString::from_str("Quit"));
    if prompt.destructive {
        quit.setHasDestructiveAction(true);
    }
    alert.addButtonWithTitle(&NSString::from_str(QuitAnswer::CANCEL_LABEL));
    let quit: &NSView = &quit;
    alert.window().setInitialFirstResponder(Some(quit));
    alert
}

/// Presents `alert` as a sheet on `sheet_window` and returns the channel that receives its one
/// response. Beginning the sheet lays the alert out, and that layout can clear the default
/// button's Return, so both key equivalents are bound once the sheet has begun.
fn begin_quit_sheet(
    alert: &NSAlert,
    sheet_window: &NSWindow,
) -> async_channel::Receiver<NSModalResponse> {
    let (answered, responses) = async_channel::bounded(1);
    let completion = RcBlock::new(move |response: NSModalResponse| {
        let _ = answered.try_send(response);
    });
    alert.beginSheetModalForWindow_completionHandler(sheet_window, Some(&completion));
    let buttons = alert.buttons();
    buttons
        .objectAtIndex(0)
        .setKeyEquivalent(&NSString::from_str("\r"));
    buttons
        .objectAtIndex(1)
        .setKeyEquivalent(&NSString::from_str("\u{1b}"));
    responses
}

/// Reads AppKit's response, which names each button by the order it was added. Ending the sheet
/// any other way answers nothing.
fn quit_answer(response: NSModalResponse) -> Option<QuitAnswer> {
    if response == NSAlertFirstButtonReturn {
        Some(QuitAnswer::Quit)
    } else if response == NSAlertSecondButtonReturn {
        Some(QuitAnswer::Cancel)
    } else {
        None
    }
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

    /// How the test answers one presented sheet.
    #[derive(Clone, Copy, Debug)]
    enum Answering {
        Click(usize),
        /// A key press sent to the sheet, by its characters and virtual key code.
        Key(&'static str, u16),
    }

    /// Presents each sheet on a real window that stays off screen, lets AppKit lay it out and draw
    /// it, then answers it by a click or a key and reads the answer the production mapping
    /// delivers.
    pub(in crate::platform) fn quit_sheet_delivers_each_answer_and_keys_quit_by_default() {
        use objc2_app_kit::{
            NSBackingStoreType, NSEvent, NSEventModifierFlags, NSEventType, NSWindowStyleMask,
        };
        use objc2_foundation::{NSDate, NSPoint, NSRect, NSRunLoop, NSSize};

        let mtm = objc2::MainThreadMarker::new().expect("native test must run on the main thread");
        let prompt = QuitPrompt {
            title: "Quit SpaceTerm?".to_owned(),
            detail: "SpaceTerm stays in the menu bar, and your Terminal Sessions keep running."
                .to_owned(),
            destructive: false,
        };
        let settle =
            || NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.3));
        for (answering, expected) in [
            (Answering::Click(0), QuitAnswer::Quit),
            (Answering::Click(1), QuitAnswer::Cancel),
            (Answering::Key("\r", 36), QuitAnswer::Quit),
            (Answering::Key(" ", 49), QuitAnswer::Quit),
            (Answering::Key("\u{1b}", 53), QuitAnswer::Cancel),
        ] {
            // SAFETY: The window is created and released on the main thread, and AppKit does not
            // release it on close because the test owns it.
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
            let alert = quit_alert(&prompt, mtm);
            let responses = begin_quit_sheet(&alert, &host);
            settle();
            let sheet = alert.window();
            sheet.displayIfNeeded();
            assert!(
                host.attachedSheet().is_some(),
                "the alert begins as a sheet"
            );

            let buttons = alert.buttons();
            let laid_out = buttons
                .iter()
                .map(|button| {
                    (
                        button.title().to_string(),
                        button.keyEquivalent().to_string(),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                laid_out,
                [
                    ("Quit".to_owned(), "\r".to_owned()),
                    ("Cancel".to_owned(), "\u{1b}".to_owned()),
                ]
            );
            // Space presses the focused button, so focus rests on the default answer.
            let focus = sheet.firstResponder().expect("the sheet focuses a button");
            let quit: &NSView = &buttons.objectAtIndex(0);
            assert!(std::ptr::eq(
                Retained::as_ptr(&focus).cast::<()>(),
                (quit as *const NSView).cast::<()>()
            ));

            match answering {
                // SAFETY: The button is live on the main thread, and a nil sender is allowed.
                Answering::Click(index) => unsafe {
                    buttons.objectAtIndex(index).performClick(None)
                },
                Answering::Key(characters, code) => {
                    let characters = NSString::from_str(characters);
                    for kind in [NSEventType::KeyDown, NSEventType::KeyUp] {
                        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                            kind,
                            NSPoint::new(0.0, 0.0),
                            NSEventModifierFlags::empty(),
                            0.0,
                            sheet.windowNumber(),
                            None,
                            &characters,
                            &characters,
                            false,
                            code,
                        )
                        .expect("AppKit builds a key event");
                        sheet.sendEvent(&event);
                    }
                }
            }
            settle();
            let response = responses
                .try_recv()
                .unwrap_or_else(|_| panic!("{answering:?} answers the sheet"));
            assert_eq!(
                quit_answer(response),
                Some(expected),
                "{answering:?}: {response}"
            );
            assert!(host.attachedSheet().is_none(), "the answer ends the sheet");
            assert!(responses.try_recv().is_err(), "the sheet answers once");
            host.close();
        }
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
