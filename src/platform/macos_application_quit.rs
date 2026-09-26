use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::NSApplication;
use objc2_foundation::{NSInteger, NSObject, NSObjectProtocol};

use super::application_quit::{
    ApplicationQuitAdapter, ApplicationQuitDecision, ApplicationQuitError, ApplicationQuitHandler,
};

const NS_TERMINATE_CANCEL: NSInteger = 0;
const NS_TERMINATE_NOW: NSInteger = 1;
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
        state
            .handler
            .borrow()
            .as_ref()
            .map_or(ApplicationQuitDecision::Proceed, |handler| {
                handler.handle_native()
            })
    }))
    .unwrap_or(ApplicationQuitDecision::Cancel);
    match decision {
        ApplicationQuitDecision::Proceed => NS_TERMINATE_NOW,
        ApplicationQuitDecision::Cancel => NS_TERMINATE_CANCEL,
    }
}

#[cfg(all(test, feature = "macos-native-tests"))]
#[allow(dead_code)]
pub(in crate::platform) mod tests {
    use gpui::TestAppContext;
    use objc2::ClassType;

    use super::*;

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and this test class has no ivars.
        #[unsafe(super(NSObject))]
        #[name = "SpaceTermApplicationQuitTestDelegate"]
        struct TestDelegate;

        unsafe impl NSObjectProtocol for TestDelegate {}
    );

    pub(in crate::platform) fn native_hook_cancels_policy_then_consumes_one_confirmation(
        cx: &mut TestAppContext,
    ) {
        let adapter = MacosApplicationQuitAdapter::new();
        let requests = Rc::new(Cell::new(0));
        let recorded_requests = Rc::clone(&requests);
        let handler = cx.update(|cx| {
            ApplicationQuitHandler::new(
                cx,
                Rc::new(move |_| {
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
