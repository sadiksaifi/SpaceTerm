use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSAlert, NSAlertStyle, NSApplication, NSModalResponse, NSView, NSWindow};
use objc2_foundation::{NSInteger, NSObject, NSObjectProtocol, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::application_quit::{
    ApplicationQuitAdapter, ApplicationQuitDecision, ApplicationQuitError, ApplicationQuitHandler,
    ApplicationQuitRequest, DismissalAnswer, DismissalPrompt, present_window_dismissal,
};

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

    /// GPUI's alert focuses its last button, so Space would quit completely. This sheet focuses
    /// the default answer instead.
    fn present_dismissal(
        &self,
        prompt: &DismissalPrompt,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> gpui::Task<Option<DismissalAnswer>> {
        let (Some(mtm), Some(sheet_window)) = (MainThreadMarker::new(), native_window(window))
        else {
            return present_window_dismissal(prompt, window, cx);
        };
        let alert = dismissal_alert(prompt, mtm);
        let (answered, answer) = async_channel::bounded(1);
        // As with GPUI's own prompts, the sheet begins after this window update, because AppKit
        // reports the key window change back into GPUI while it presents.
        cx.spawn(async move |_| {
            let completion = RcBlock::new(move |response: NSModalResponse| {
                let _ = answered.try_send(response);
            });
            alert.beginSheetModalForWindow_completionHandler(&sheet_window, Some(&completion));
            let response = answer.recv().await.ok()?;
            DismissalAnswer::from_index(usize::try_from(response).ok()?)
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

/// Builds the dismissal alert with its buttons in [`DismissalPrompt`] order. Each button's tag is
/// its position, which the sheet reports as its response.
fn dismissal_alert(prompt: &DismissalPrompt, mtm: MainThreadMarker) -> Retained<NSAlert> {
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Informational);
    alert.setMessageText(&NSString::from_str(&prompt.title));
    alert.setInformativeText(&NSString::from_str(&prompt.detail));
    let labels = [
        prompt.keep.as_str(),
        DismissalAnswer::CANCEL_LABEL,
        prompt.quit_completely.as_str(),
    ];
    let buttons = labels.map(|label| alert.addButtonWithTitle(&NSString::from_str(label)));
    for (tag, button) in buttons.iter().enumerate() {
        button.setTag(tag as NSInteger);
    }
    // The first button already answers Return. Escape cancels, and the complete quit answers
    // no key at all.
    buttons[0].setKeyEquivalent(&NSString::from_str("\r"));
    buttons[1].setKeyEquivalent(&NSString::from_str("\u{1b}"));
    buttons[2].setKeyEquivalent(&NSString::new());
    let keep: &NSView = &buttons[0];
    alert.window().setInitialFirstResponder(Some(keep));
    alert
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
            ApplicationQuitRequest::Exit
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
            assert_eq!(request_from_event(&event), ApplicationQuitRequest::Exit);
        });
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and this test class has no ivars.
        #[unsafe(super(NSObject))]
        #[name = "SpaceTermApplicationQuitTestDelegate"]
        struct TestDelegate;

        unsafe impl NSObjectProtocol for TestDelegate {}
    );

    pub(in crate::platform) fn dismissal_alert_focuses_the_default_and_keys_never_quit_completely()
    {
        let mtm = objc2::MainThreadMarker::new().expect("native test must run on the main thread");
        let prompt = DismissalPrompt {
            title: "Are you sure you want to quit?".to_owned(),
            detail: "SpaceTerm stays in the menu bar, and your Terminal Sessions keep running."
                .to_owned(),
            keep: "Quit SpaceTerm".to_owned(),
            quit_completely: "Quit SpaceTerm Completely".to_owned(),
        };
        let alert = dismissal_alert(&prompt, mtm);
        let buttons = alert.buttons();
        let titles = buttons
            .iter()
            .map(|button| button.title().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            titles,
            ["Quit SpaceTerm", "Cancel", "Quit SpaceTerm Completely"]
        );
        let keys = buttons
            .iter()
            .map(|button| (button.tag(), button.keyEquivalent().to_string()))
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            [
                (0, "\r".to_owned()),
                (1, "\u{1b}".to_owned()),
                (2, String::new())
            ]
        );
        // Space presses the focused button, so focus starts on the default answer.
        let focus = alert
            .window()
            .initialFirstResponder()
            .expect("the alert names its initial focus");
        let keep: &NSView = &buttons.objectAtIndex(0);
        assert!(std::ptr::eq(&*focus, keep));
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
