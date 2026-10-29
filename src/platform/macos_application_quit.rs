use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use block2::RcBlock;
use dispatch2::{DispatchQueue, MainThreadBound};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSModalResponse, NSView, NSWindow,
};
use objc2_foundation::{NSInteger, NSObject, NSObjectProtocol, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::application_quit::{
    ApplicationQuitAdapter, ApplicationQuitDecision, ApplicationQuitError, ApplicationQuitHandler,
    ApplicationQuitRequest, QuitAnswer, QuitButtons, QuitPrompt, present_window_quit,
};
use super::macos_alert::{AlertActivation, AlertSeverity, application_alert, run_modal_alert};

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
    activation: AlertActivation,
}

impl MacosApplicationQuitAdapter {
    pub(crate) fn new() -> Self {
        Self {
            state: Rc::new(ApplicationQuitState::new()),
            activation: AlertActivation::FORCEFUL,
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
        let mtm = MainThreadMarker::new()?;
        let answers = ask_by_itself(
            quit_alert(prompt, mtm),
            prompt.buttons,
            self.activation,
            mtm,
        );
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

/// Runs `alert` by itself and returns the channel that receives its one answer. The alert runs
/// its own modal loop, so it starts from the main queue rather than inside GPUI's update. It
/// takes the keyboard through `activation` first, because the menu bar item leaves another app
/// in front. Any
/// answer but Quit hides the client, which returns focus to the previous app. A modal loop that
/// ends without a button answers nothing.
fn ask_by_itself(
    alert: Retained<NSAlert>,
    buttons: QuitButtons,
    activation: AlertActivation,
    mtm: MainThreadMarker,
) -> async_channel::Receiver<QuitAnswer> {
    let (answered, answers) = async_channel::bounded(1);
    let alert = MainThreadBound::new(alert, mtm);
    DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let application = NSApplication::sharedApplication(mtm);
        let response = run_modal_alert(alert.get(mtm), activation, move |alert| {
            bind_quit_keys(alert, buttons);
        });
        let answer = quit_answer(response, buttons);
        if answer != Some(QuitAnswer::Quit) {
            application.hide(None);
        }
        if let Some(answer) = answer {
            let _ = answered.try_send(answer);
        }
    });
    answers
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

    use super::super::macos_alert::testing::{
        Answering, RECORDED_ACTIVATION, answer_inside_modal_loop, expected_keys, laid_out, press,
        settle_pending_checks,
    };
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
                    Answering::PostedKey(..) | Answering::Stop => {
                        unreachable!("the answer cases click or press keys")
                    }
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

    /// Asks with each warning through the windowless path a client with no window takes: the
    /// adapter queues the warning on the main queue, and the test drives the run loop until it
    /// returns. The warning asks for the keyboard before its modal loop starts. The test records
    /// that request instead of activating, so the test process never takes the keyboard. Inside
    /// the warning's modal loop the test answers it by a click or a key. The warning stays
    /// transparent, so nothing appears on screen.
    pub(in crate::platform) fn windowless_quit_warning_takes_the_keyboard_and_binds_its_keys() {
        let mtm = objc2::MainThreadMarker::new().expect("native test must run on the main thread");
        let application = NSApplication::sharedApplication(mtm);
        let policy = application.activationPolicy();
        for (buttons, keys, focused, answers) in answer_cases() {
            // A modal loop that ends without a button answers nothing.
            let endings = answers.iter().copied().chain([(Answering::Stop, None)]);
            for (answering, expected) in endings {
                let alert = quit_alert(&quit_prompt_fixture(buttons), mtm);
                alert.window().setAlphaValue(0.0);
                let mut answers = None;
                let asking = answer_inside_modal_loop(&alert, answering, || {
                    answers = Some(ask_by_itself(
                        alert.clone(),
                        buttons,
                        RECORDED_ACTIVATION,
                        mtm,
                    ));
                });
                let answer = answers.and_then(|answers| answers.try_recv().ok());
                assert!(asking.modal, "the warning runs its own modal loop");
                assert_eq!(
                    asking.activations,
                    [true],
                    "the warning asks once for the keyboard, before its modal loop starts"
                );
                assert!(asking.alone, "the warning shows no other window");
                assert!(!asking.active, "asking never activates the test process");
                assert_eq!(asking.policy, policy, "asking keeps the activation policy");
                assert_eq!(
                    asking.laid_out,
                    (expected_keys(keys), Some(focused)),
                    "{buttons:?} binds its keys inside its modal loop"
                );
                assert_eq!(
                    asking.left_open,
                    expected.is_none() && !matches!(answering, Answering::Stop),
                    "{buttons:?} {answering:?}"
                );
                assert_eq!(answer, expected, "{buttons:?} {answering:?}");
                assert!(application.modalWindow().is_none(), "no modal loop remains");
                assert_eq!(application.activationPolicy(), policy);
                if application.isHidden() {
                    application.unhideWithoutActivation();
                }
            }
        }
        settle_pending_checks();
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
