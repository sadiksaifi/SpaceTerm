#![allow(deprecated)]
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use cocoa::appkit::NSApp;
use cocoa::base::{id, nil};
use cocoa::foundation::NSInteger;
use objc::declare::ClassDecl;
use objc::runtime::{self, BOOL, Class, Imp, NO, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};

use super::application_quit::{
    ApplicationQuitAdapter, ApplicationQuitDecision, ApplicationQuitError, ApplicationQuitHandler,
};

const STATE_HOLDER_CLASS: &str = "SpaceTermApplicationQuitStateHolder";
const STATE_IVAR: &str = "spaceTermApplicationQuitState";
const NS_TERMINATE_CANCEL: NSInteger = 0;
const NS_TERMINATE_NOW: NSInteger = 1;
static QUIT_STATE_ASSOCIATION: u8 = 0;

unsafe extern "C" {
    fn objc_getAssociatedObject(object: id, key: *const c_void) -> id;
    fn objc_setAssociatedObject(object: id, key: *const c_void, value: id, policy: usize);
}

const OBJC_ASSOCIATION_RETAIN_NONATOMIC: usize = 1;

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

pub(crate) struct MacosApplicationQuitAdapter {
    state: Rc<ApplicationQuitState>,
}

impl MacosApplicationQuitAdapter {
    pub(crate) fn new() -> Self {
        Self {
            state: Rc::new(ApplicationQuitState::new()),
        }
    }

    unsafe fn install_on_delegate(
        &self,
        delegate: id,
        handler: ApplicationQuitHandler,
    ) -> Result<(), ApplicationQuitError> {
        unsafe {
            install_should_terminate_method(delegate)?;
            retain_state(delegate, Rc::clone(&self.state))?;
        }
        *self.state.handler.borrow_mut() = Some(handler);
        Ok(())
    }
}

impl ApplicationQuitAdapter for MacosApplicationQuitAdapter {
    fn install(&self, handler: ApplicationQuitHandler) -> Result<(), ApplicationQuitError> {
        if !is_main_thread() {
            return Err(ApplicationQuitError::OffMainThread);
        }
        if self.state.handler.borrow().is_some() {
            return Err(ApplicationQuitError::AlreadyInstalled);
        }

        // SAFETY: Installation runs on AppKit's main thread after GPUI has assigned its live
        // application delegate. The associated holder retains one Rc clone until delegate
        // teardown, and its dealloc implementation releases exactly that clone.
        unsafe {
            let application = NSApp();
            if application == nil {
                return Err(ApplicationQuitError::DelegateUnavailable);
            }
            let delegate: id = msg_send![application, delegate];
            if delegate == nil {
                return Err(ApplicationQuitError::DelegateUnavailable);
            }
            self.install_on_delegate(delegate, handler)?;
        }
        Ok(())
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

fn is_main_thread() -> bool {
    // SAFETY: NSThread's class property returns a scalar and has no ownership transfer.
    unsafe {
        let main: BOOL = msg_send![class!(NSThread), isMainThread];
        main != NO
    }
}

unsafe fn install_should_terminate_method(delegate: id) -> Result<(), ApplicationQuitError> {
    let class = unsafe { runtime::object_getClass(delegate) };
    if class.is_null() {
        return Err(ApplicationQuitError::DelegateUnavailable);
    }
    let selector = sel!(applicationShouldTerminate:);
    if !unsafe { runtime::class_getInstanceMethod(class, selector) }.is_null() {
        return Err(ApplicationQuitError::DelegateConflict);
    }
    let implementation: extern "C" fn(&Object, Sel, id) -> NSInteger = should_terminate;
    // SAFETY: IMP is an untyped Objective-C function pointer. The registered method encoding
    // describes NSInteger self/_cmd/id exactly on SpaceTerm's supported 64-bit macOS targets.
    let implementation: Imp = unsafe { std::mem::transmute(implementation) };
    let encoding = c"q@:@";
    let added = unsafe {
        runtime::class_addMethod(
            class.cast_mut(),
            selector,
            implementation,
            encoding.as_ptr(),
        )
    };
    if added == NO {
        return Err(ApplicationQuitError::DelegateConflict);
    }
    Ok(())
}

unsafe fn retain_state(
    delegate: id,
    state: Rc<ApplicationQuitState>,
) -> Result<(), ApplicationQuitError> {
    let association_key = (&raw const QUIT_STATE_ASSOCIATION).cast::<c_void>();
    if unsafe { objc_getAssociatedObject(delegate, association_key) } != nil {
        return Err(ApplicationQuitError::AlreadyInstalled);
    }
    let holder_class = state_holder_class()?;
    let holder: id = unsafe { msg_send![holder_class, alloc] };
    let holder: id = unsafe { msg_send![holder, init] };
    if holder == nil {
        return Err(ApplicationQuitError::HandlerUnavailable);
    }
    let state = Box::new(state);
    unsafe {
        (*holder).set_ivar(STATE_IVAR, Box::into_raw(state).cast::<c_void>());
        objc_setAssociatedObject(
            delegate,
            association_key,
            holder,
            OBJC_ASSOCIATION_RETAIN_NONATOMIC,
        );
        let _: () = msg_send![holder, release];
    }
    Ok(())
}

fn state_holder_class() -> Result<&'static Class, ApplicationQuitError> {
    if let Some(class) = Class::get(STATE_HOLDER_CLASS) {
        return Ok(class);
    }
    let Some(mut declaration) = ClassDecl::new(STATE_HOLDER_CLASS, class!(NSObject)) else {
        return Err(ApplicationQuitError::HandlerUnavailable);
    };
    declaration.add_ivar::<*mut c_void>(STATE_IVAR);
    // SAFETY: The holder owns one Box<Rc<ApplicationQuitState>> in its pointer ivar.
    unsafe {
        declaration.add_method(
            sel!(dealloc),
            dealloc_state_holder as extern "C" fn(&mut Object, Sel),
        );
    }
    Ok(declaration.register())
}

extern "C" fn dealloc_state_holder(this: &mut Object, _: Sel) {
    // SAFETY: retain_state initializes this ivar exactly once with Box::into_raw. AppKit calls
    // dealloc once after releasing the associated holder.
    unsafe {
        let state: *mut c_void = *this.get_ivar(STATE_IVAR);
        if !state.is_null() {
            drop(Box::from_raw(state.cast::<Rc<ApplicationQuitState>>()));
            this.set_ivar(STATE_IVAR, std::ptr::null_mut::<c_void>());
        }
        let superclass = class!(NSObject);
        let _: () = msg_send![super(this, superclass), dealloc];
    }
}

extern "C" fn should_terminate(this: &Object, _: Sel, _: id) -> NSInteger {
    // SAFETY: The delegate's associated holder is installed on the AppKit thread and retains a
    // valid Rc state. This method is invoked synchronously by NSApplication on that same thread.
    unsafe {
        let association_key = (&raw const QUIT_STATE_ASSOCIATION).cast::<c_void>();
        let holder = objc_getAssociatedObject(this as *const Object as id, association_key);
        if holder == nil {
            return NS_TERMINATE_NOW;
        }
        let state: *mut c_void = *(*holder).get_ivar(STATE_IVAR);
        let Some(state) = state.cast::<Rc<ApplicationQuitState>>().as_ref() else {
            return NS_TERMINATE_NOW;
        };
        let state = state.as_ref();
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
}

#[cfg(all(test, feature = "macos-native-tests"))]
mod tests {
    use std::sync::OnceLock;

    use gpui::TestAppContext;

    use super::*;

    fn test_delegate_class() -> &'static Class {
        static CLASS: OnceLock<&'static Class> = OnceLock::new();
        CLASS.get_or_init(|| {
            ClassDecl::new("SpaceTermApplicationQuitTestDelegate", class!(NSObject))
                .expect("unique test delegate class")
                .register()
        })
    }

    #[gpui::test]
    fn native_hook_cancels_policy_then_consumes_one_confirmation(cx: &mut TestAppContext) {
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
        // SAFETY: The test owns this NSObject subclass instance and releases it after all
        // synchronous selector calls. install_on_delegate registers the production selector and
        // retains the production associated-state holder for exactly that lifetime.
        let delegate: id = unsafe { msg_send![test_delegate_class(), new] };
        unsafe {
            adapter
                .install_on_delegate(delegate, handler)
                .expect("native quit hook should install");
        }

        // SAFETY: install_on_delegate added applicationShouldTerminate: with this exact signature.
        let cancelled: NSInteger = unsafe { msg_send![delegate, applicationShouldTerminate: nil] };
        assert_eq!(cancelled, NS_TERMINATE_CANCEL);
        assert_eq!(requests.get(), 1);

        cx.update(|cx| adapter.confirm_quit(cx));
        // SAFETY: The delegate and installed selector remain live until the final release below.
        let confirmed: NSInteger = unsafe { msg_send![delegate, applicationShouldTerminate: nil] };
        let cancelled_again: NSInteger =
            unsafe { msg_send![delegate, applicationShouldTerminate: nil] };

        assert_eq!(confirmed, NS_TERMINATE_NOW);
        assert_eq!(cancelled_again, NS_TERMINATE_CANCEL);
        assert_eq!(requests.get(), 2);
        // SAFETY: new returned one owned retain, and no code uses delegate after this release.
        unsafe {
            let _: () = msg_send![delegate, release];
        }
    }
}
