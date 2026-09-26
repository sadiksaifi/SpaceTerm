//! AppKit Services responder registration and native pasteboard conversion.
//!
//! Request policy and lifetime authority live in the portable Services owner. This adapter
//! connects those operations to AppKit's responder chain and supplied pasteboard.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use gpui::Window;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSPasteboard, NSPasteboardTypeString, NSResponder, NSServicesMenuRequestor,
    NSView,
};
use objc2_foundation::{NSArray, NSObjectProtocol, NSString, NSUTF8StringEncoding};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::services_registration::{ServicesRegistration, ServicesRegistrationError};
use crate::terminal::native_services::services::{
    ServiceDataType, ServiceEndpoint, ServiceOperation, ServicePasteboardIdentity, ServiceRequests,
    bounded_service_text_byte_len, decode_service_text_bytes,
};

const LEGACY_STRING_PASTEBOARD_TYPE: &str = "NSStringPboardType";
const OBJC_ASSOCIATION_RETAIN_NONATOMIC: usize = 1;
static SERVICES_RESPONDER_ASSOCIATION: u8 = 0;

unsafe extern "C" {
    fn objc_getAssociatedObject(object: *const AnyObject, key: *const c_void) -> *mut AnyObject;
    fn objc_setAssociatedObject(
        object: *const AnyObject,
        key: *const c_void,
        value: *const AnyObject,
        policy: usize,
    );
}

fn association_key() -> *const c_void {
    (&raw const SERVICES_RESPONDER_ASSOCIATION).cast::<c_void>()
}

pub(crate) fn register() -> Result<(), ServicesRegistrationError> {
    let mtm = MainThreadMarker::new().ok_or(ServicesRegistrationError::ApplicationUnavailable)?;
    let application = NSApplication::sharedApplication(mtm);
    // SAFETY: AppKit exports this immutable modern string pasteboard type.
    let string_type = unsafe { NSPasteboardTypeString };
    let string_types = NSArray::from_slice(&[string_type]);
    application.registerServicesMenuSendTypes_returnTypes(&string_types, &string_types);
    Ok(())
}

pub(crate) fn install(
    window: &Window,
    endpoint: Rc<dyn ServiceEndpoint>,
) -> Result<(), ServicesRegistrationError> {
    let native_handle = HasWindowHandle::window_handle(window)
        .map_err(|_| ServicesRegistrationError::NativeViewUnavailable)?;
    let RawWindowHandle::AppKit(native_handle) = native_handle.as_raw() else {
        return Err(ServicesRegistrationError::NativeViewUnavailable);
    };
    let mtm = MainThreadMarker::new().ok_or(ServicesRegistrationError::ApplicationUnavailable)?;
    // SAFETY: GPUI owns this live NSView for the synchronous installation call.
    let native_view = unsafe { &*native_handle.ns_view.as_ptr().cast::<NSView>() };
    // SAFETY: This unique association key belongs to the view and the view is live.
    if !unsafe {
        objc_getAssociatedObject((native_view as *const NSView).cast(), association_key())
    }
    .is_null()
    {
        return Ok(());
    }
    let responder = ServicesResponder::new(mtm, Rc::new(ServiceRequests::new(endpoint)));
    // SAFETY: The previous responder outlives this link as part of AppKit's responder chain.
    let previous = unsafe { native_view.nextResponder() };
    // SAFETY: AppKit's responder chain keeps the previous responder live for the view's lifetime.
    unsafe { responder.setNextResponder(previous.as_deref()) };
    // SAFETY: The runtime retains the responder under the view's unique key. The view's
    // nextResponder link is unretained, so the association owns its lifetime.
    unsafe {
        objc_setAssociatedObject(
            (native_view as *const NSView).cast(),
            association_key(),
            Retained::as_ptr(&responder).cast(),
            OBJC_ASSOCIATION_RETAIN_NONATOMIC,
        );
        native_view.setNextResponder(Some(&responder));
    }
    Ok(())
}

struct ServicesResponderIvars {
    state: Rc<ServiceRequests>,
    mtm: MainThreadMarker,
}

impl Drop for ServicesResponderIvars {
    fn drop(&mut self) {
        self.state.retire();
    }
}

define_class!(
    // SAFETY: NSResponder has no additional subclassing requirements. define_class! drops the
    // ivars, whose Drop retires operations before the last Rc clone can leave a callback.
    #[unsafe(super(NSResponder))]
    #[name = "SpaceTermServicesResponder"]
    #[thread_kind = MainThreadOnly]
    #[ivars = ServicesResponderIvars]
    struct ServicesResponder;

    impl ServicesResponder {
        #[unsafe(method_id(validRequestorForSendType:returnType:))]
        fn valid_requestor(
            &self,
            send_type: Option<&NSString>,
            return_type: Option<&NSString>,
        ) -> Option<Retained<AnyObject>> {
            let state = Rc::clone(&self.ivars().state);
            let operation = catch_unwind(AssertUnwindSafe(|| {
                state.operation(service_data_type(send_type), service_data_type(return_type))
                    .and_then(|operation| create_services_operation(operation, self.ivars().mtm))
            })).ok().flatten();
            if let Some(operation) = operation {
                Some(operation.into_super().into_super().into_super())
            } else if state.is_retired() {
                None
            } else {
                // SAFETY: NSResponder continues the previous responder chain for unsupported types.
                unsafe { msg_send![super(self), validRequestorForSendType: send_type, returnType: return_type] }
            }
        }
    }

    unsafe impl NSObjectProtocol for ServicesResponder {}
    unsafe impl NSServicesMenuRequestor for ServicesResponder {}
);

impl ServicesResponder {
    fn new(mtm: MainThreadMarker, state: Rc<ServiceRequests>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ServicesResponderIvars { state, mtm });
        // SAFETY: NSResponder's init is its designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

struct ServicesOperationResponderIvars {
    state: Rc<ServiceOperation>,
}

define_class!(
    // SAFETY: NSResponder has no additional subclassing requirements, and define_class! drops
    // the operation Rc when the native responder is deallocated.
    #[unsafe(super(NSResponder))]
    #[name = "SpaceTermServicesOperationResponder"]
    #[thread_kind = MainThreadOnly]
    #[ivars = ServicesOperationResponderIvars]
    struct ServicesOperationResponder;

    unsafe impl NSObjectProtocol for ServicesOperationResponder {}

    #[allow(non_snake_case)]
    unsafe impl NSServicesMenuRequestor for ServicesOperationResponder {
        #[unsafe(method(writeSelectionToPasteboard:types:))]
        fn writeSelectionToPasteboard_types(
            &self,
            pasteboard: &NSPasteboard,
            types: &NSArray<NSString>,
        ) -> bool {
            catch_unwind(AssertUnwindSafe(|| {
                // SAFETY: AppKit exports this immutable modern string pasteboard type.
                let modern = unsafe { NSPasteboardTypeString };
                let legacy = NSString::from_str(LEGACY_STRING_PASTEBOARD_TYPE);
                if !types.containsObject(modern) && !types.containsObject(&legacy) {
                    return false;
                }
                let state = Rc::clone(&self.ivars().state);
                state.write_selection(
                    ServicePasteboardIdentity::new(pasteboard as *const _ as usize),
                    |text| write_service_text(pasteboard, text),
                )
            }))
            .unwrap_or(false)
        }

        #[unsafe(method(readSelectionFromPasteboard:))]
        fn readSelectionFromPasteboard(&self, pasteboard: &NSPasteboard) -> bool {
            catch_unwind(AssertUnwindSafe(|| {
                let state = Rc::clone(&self.ivars().state);
                state.read_selection(
                    ServicePasteboardIdentity::new(pasteboard as *const _ as usize),
                    || read_service_text(pasteboard),
                )
            }))
            .unwrap_or(false)
        }
    }
);

impl ServicesOperationResponder {
    fn new(mtm: MainThreadMarker, state: ServiceOperation) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ServicesOperationResponderIvars {
            state: Rc::new(state),
        });
        // SAFETY: NSResponder's init is its designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

fn create_services_operation(
    state: ServiceOperation,
    mtm: MainThreadMarker,
) -> Option<Retained<ServicesOperationResponder>> {
    Some(ServicesOperationResponder::new(mtm, state))
}

fn service_data_type(value: Option<&NSString>) -> ServiceDataType {
    let Some(value) = value.filter(|value| value.length() != 0) else {
        return ServiceDataType::Absent;
    };
    // SAFETY: AppKit exports this immutable modern string pasteboard type.
    let modern = unsafe { NSPasteboardTypeString };
    let legacy = NSString::from_str(LEGACY_STRING_PASTEBOARD_TYPE);
    if value == modern || value == &*legacy {
        ServiceDataType::String
    } else {
        ServiceDataType::Unsupported
    }
}

fn write_service_text(pasteboard: &NSPasteboard, text: &str) -> bool {
    autoreleasepool(|_| {
        // SAFETY: AppKit exports this immutable modern string pasteboard type.
        let modern = unsafe { NSPasteboardTypeString };
        let types = NSArray::from_slice(&[modern]);
        // SAFETY: A nil owner requires no NSPasteboardOwner protocol implementation.
        unsafe { pasteboard.declareTypes_owner(&types, None) };
        pasteboard.setString_forType(&NSString::from_str(text), modern)
    })
}

fn read_service_text(pasteboard: &NSPasteboard) -> Option<String> {
    // SAFETY: AppKit exports this immutable modern string pasteboard type.
    let modern = unsafe { NSPasteboardTypeString };
    pasteboard.types()?.containsObject(modern).then_some(())?;
    let value = pasteboard.stringForType(modern)?;
    read_nsstring_text(&value)
}

fn read_nsstring_text(value: &NSString) -> Option<String> {
    let byte_len = bounded_service_text_byte_len(
        value.length(),
        value.lengthOfBytesUsingEncoding(NSUTF8StringEncoding),
    )?;
    let utf8 = value.UTF8String();
    if utf8.is_null() {
        return None;
    }
    // SAFETY: NSString keeps this UTF8String pointer live through the synchronous copy. Its
    // byte count was bounded against Paste Payload's limit before constructing the slice.
    let bytes = unsafe { std::slice::from_raw_parts(utf8.cast::<u8>(), byte_len) };
    decode_service_text_bytes(bytes)
}

pub(super) struct NativeServicesRegistration;
impl ServicesRegistration for NativeServicesRegistration {
    fn register(&self) -> Result<(), ServicesRegistrationError> {
        register()
    }
    fn install(
        &self,
        window: &Window,
        endpoint: Rc<dyn ServiceEndpoint>,
    ) -> Result<(), ServicesRegistrationError> {
        install(window, endpoint)
    }
}

#[cfg(all(test, feature = "macos-native-tests"))]
#[allow(dead_code)]
pub(in crate::platform) mod tests {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{AnyThread, msg_send};
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::NSString;
    use std::cell::Cell;

    use super::*;
    use crate::terminal::MAX_PASTE_BYTES;

    fn legacy_string_type() -> Retained<NSString> {
        NSString::from_str(LEGACY_STRING_PASTEBOARD_TYPE)
    }

    pub(in crate::platform) fn service_type_classifies_nil_and_empty_nsstring_as_absent(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|_| {
            let empty = NSString::from_str("");
            let unsupported = NSString::from_str("public.html");
            // SAFETY: AppKit exports this immutable modern string pasteboard type.
            let modern = unsafe { NSPasteboardTypeString };
            let legacy = legacy_string_type();
            assert_eq!(service_data_type(None), ServiceDataType::Absent);
            assert_eq!(service_data_type(Some(&empty)), ServiceDataType::Absent);
            assert_eq!(service_data_type(Some(modern)), ServiceDataType::String);
            assert_eq!(service_data_type(Some(&legacy)), ServiceDataType::String);
            assert_eq!(
                service_data_type(Some(&unsupported)),
                ServiceDataType::Unsupported
            );
            let generic_plain_text = NSString::from_str("public.plain-text");
            assert_eq!(
                service_data_type(Some(&generic_plain_text)),
                ServiceDataType::Unsupported
            );
        });
    }

    pub(in crate::platform) fn nsstring_decode_enforces_the_paste_limit_before_copying(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|_| {
            let at_limit = NSString::from_str(&"x".repeat(MAX_PASTE_BYTES));
            let over_limit = NSString::from_str(&"x".repeat(MAX_PASTE_BYTES + 1));
            assert_eq!(
                read_nsstring_text(&at_limit).map(|text| text.len()),
                Some(MAX_PASTE_BYTES)
            );
            assert_eq!(read_nsstring_text(&over_limit), None);
        });
    }

    pub(in crate::platform) fn nsstring_decode_rejects_embedded_nul_without_truncation(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|_| {
            let value = NSString::from_str("before\0after");
            assert_eq!(read_nsstring_text(&value), None);
        });
    }

    pub(in crate::platform) fn nsstring_decode_rejects_nonempty_failed_utf8_conversion(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|_| {
            let invalid_utf16 = [0xd800u16];
            // SAFETY: The pointer names one live UTF-16 code unit for this initializer.
            let value = unsafe {
                NSString::initWithCharacters_length(
                    NSString::alloc(),
                    std::ptr::NonNull::from(&invalid_utf16[0]),
                    invalid_utf16.len(),
                )
            };
            assert_eq!(read_nsstring_text(&value), None);
        });
    }

    pub(in crate::platform) fn service_pasteboard_round_trip_uses_only_public_utf8_text(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|_| {
            let board = IsolatedPasteboard::new();
            assert!(write_service_text(&board.0, "service text\n"));
            assert_eq!(
                read_service_text(&board.0).as_deref(),
                Some("service text\n")
            );
        });
    }

    static NATIVE_REQUESTOR_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct NativeObject(Cell<*mut AnyObject>);

    impl NativeObject {
        fn requestor(endpoint: Rc<dyn ServiceEndpoint>) -> Rc<Self> {
            let mtm =
                objc2::MainThreadMarker::new().expect("native test must run on the main thread");
            let responder = ServicesResponder::new(mtm, Rc::new(ServiceRequests::new(endpoint)));
            Rc::new(Self(Cell::new(Retained::into_raw(responder).cast())))
        }

        fn validate(&self) -> Option<Rc<Self>> {
            self.validate_returning(true)
        }

        fn validate_returning(&self, returns_text: bool) -> Option<Rc<Self>> {
            let object = self.0.get();
            assert!(!object.is_null());
            // SAFETY: AppKit exports this immutable modern string pasteboard type.
            let modern = unsafe { NSPasteboardTypeString };
            let return_type = returns_text.then_some(modern);
            let operation: Option<Retained<AnyObject>> = autoreleasepool(|_| {
                // SAFETY: The live requestor implements this selector and owns its state through entry.
                unsafe {
                    msg_send![object, validRequestorForSendType: modern, returnType: return_type]
                }
            });
            operation.map(|operation| Rc::new(Self(Cell::new(Retained::into_raw(operation)))))
        }

        fn write(&self, pasteboard: &NSPasteboard) -> bool {
            let object = self.0.get();
            assert!(!object.is_null());
            // SAFETY: AppKit exports this immutable modern string pasteboard type.
            let modern = unsafe { NSPasteboardTypeString };
            let types = NSArray::from_slice(&[modern]);
            // SAFETY: The live operation implements this selector, and both arguments remain live.
            unsafe { msg_send![object, writeSelectionToPasteboard: pasteboard, types: &*types] }
        }

        fn read(&self, pasteboard: &NSPasteboard) -> bool {
            let object = self.0.get();
            assert!(!object.is_null());
            // SAFETY: The live operation implements this selector and the pasteboard remains live.
            unsafe { msg_send![object, readSelectionFromPasteboard: pasteboard] }
        }

        fn release(&self) {
            let object = self.0.replace(std::ptr::null_mut());
            if !object.is_null() {
                // SAFETY: This pointer came from Retained::into_raw and this fixture owns it once.
                drop(unsafe { Retained::from_raw(object) });
            }
        }

        fn deallocate(&self) {
            let object = self.0.replace(std::ptr::null_mut());
            assert!(!object.is_null());
            // SAFETY: The unattached fixture owns the sole retain. Direct dealloc exercises
            // reentrant callback retirement on this AppKit thread; the cleared cell prevents reuse.
            unsafe {
                let count: usize = msg_send![object, retainCount];
                assert_eq!(count, 1);
                let _: () = msg_send![object, dealloc];
            }
        }
    }

    impl Drop for NativeObject {
        fn drop(&mut self) {
            self.release();
        }
    }

    fn services_state(object: *mut AnyObject) -> Option<Rc<ServiceRequests>> {
        // SAFETY: The fixture retains this responder until it clears its cell.
        unsafe { object.as_ref() }?
            .downcast_ref::<ServicesResponder>()
            .map(|responder| Rc::clone(&responder.ivars().state))
    }

    fn services_operation_state(object: *mut AnyObject) -> Option<Rc<ServiceOperation>> {
        // SAFETY: The fixture retains this responder until it clears its cell.
        unsafe { object.as_ref() }?
            .downcast_ref::<ServicesOperationResponder>()
            .map(|responder| Rc::clone(&responder.ivars().state))
    }

    struct IsolatedPasteboard(Retained<NSPasteboard>);

    impl IsolatedPasteboard {
        fn new() -> Self {
            Self(NSPasteboard::pasteboardWithUniqueName())
        }

        fn set_text(&self, text: &str) {
            assert!(write_service_text(&self.0, text));
        }

        fn text(&self) -> Option<String> {
            read_service_text(&self.0)
        }
    }

    impl Drop for IsolatedPasteboard {
        fn drop(&mut self) {
            // SAFETY: This fixture exclusively owns its named server-side pasteboard.
            let _: () = unsafe { msg_send![&*self.0, releaseGlobally] };
        }
    }
    type NativeCallback = std::cell::RefCell<Option<Box<dyn FnOnce()>>>;

    struct NativeEndpoint {
        origin: std::cell::Cell<crate::terminal::NativeServiceOrigin>,
        selection: &'static str,
        inserted: std::cell::RefCell<Vec<(crate::terminal::NativeServiceOrigin, String)>>,
        on_status: NativeCallback,
        on_selection: NativeCallback,
        on_insert: NativeCallback,
    }

    impl NativeEndpoint {
        fn new(selection: &'static str) -> Rc<Self> {
            Rc::new(Self {
                origin: std::cell::Cell::new(native_origin(1)),
                selection,
                inserted: std::cell::RefCell::new(Vec::new()),
                on_status: std::cell::RefCell::new(None),
                on_selection: std::cell::RefCell::new(None),
                on_insert: std::cell::RefCell::new(None),
            })
        }

        fn invoke(callback: &NativeCallback) {
            let callback = callback.borrow_mut().take();
            if let Some(callback) = callback {
                callback();
            }
        }
    }

    impl ServiceEndpoint for NativeEndpoint {
        fn status(&self) -> crate::terminal::NativeServiceStatus {
            Self::invoke(&self.on_status);
            crate::terminal::NativeServiceStatus::new(
                crate::terminal::NativeServiceCapabilities::new(true, true),
                Some(self.origin.get()),
            )
        }

        fn selection(
            &self,
            origin: crate::terminal::NativeServiceOrigin,
        ) -> Option<crate::terminal::SelectionCopy> {
            Self::invoke(&self.on_selection);
            (origin == self.origin.get()).then(|| crate::terminal::SelectionCopy {
                plain_text: self.selection.to_owned(),
                html: None,
            })
        }

        fn insert_text(&self, origin: crate::terminal::NativeServiceOrigin, text: String) -> bool {
            Self::invoke(&self.on_insert);
            if origin != self.origin.get() {
                return false;
            }
            self.inserted.borrow_mut().push((origin, text));
            true
        }
    }

    fn native_origin(generation: u64) -> crate::terminal::NativeServiceOrigin {
        crate::terminal::NativeServiceOrigin::new(
            crate::domain::WorkspaceId::new(1),
            crate::domain::TabId::new(2),
            crate::domain::PaneId::new(3),
            4,
            5,
            generation,
        )
    }

    pub(in crate::platform) fn native_selectors_publish_selection_and_accept_exactly_one_return(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|_| {
            let _serial = NATIVE_REQUESTOR_TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let endpoint = NativeEndpoint::new("selected 日本語");
            let requestor = NativeObject::requestor(endpoint.clone());
            let operation = requestor.validate().unwrap();
            let board = IsolatedPasteboard::new();

            assert!(operation.write(&board.0));
            assert_eq!(board.text().as_deref(), Some("selected 日本語"));
            assert!(!operation.write(&board.0));
            board.set_text("transformed text");
            assert!(operation.read(&board.0));
            assert!(!operation.read(&board.0));
            assert_eq!(
                *endpoint.inserted.borrow(),
                vec![(native_origin(1), "transformed text".into())]
            );
        });
    }

    pub(in crate::platform) fn native_selectors_reject_stale_validation_and_stale_return(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|_| {
            let _serial = NATIVE_REQUESTOR_TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let endpoint = NativeEndpoint::new("successor selection");
            let requestor = NativeObject::requestor(endpoint.clone());
            let stale = requestor.validate().unwrap();
            let board = IsolatedPasteboard::new();
            board.set_text("untouched");
            endpoint.origin.set(native_origin(2));
            assert!(!stale.write(&board.0));
            assert_eq!(board.text().as_deref(), Some("untouched"));

            let current = requestor.validate().unwrap();
            assert!(current.write(&board.0));
            board.set_text("stale return");
            endpoint.origin.set(native_origin(3));
            assert!(!current.read(&board.0));
            endpoint.origin.set(native_origin(2));
            assert!(!current.read(&board.0));
            assert!(endpoint.inserted.borrow().is_empty());
        });
    }

    pub(in crate::platform) fn native_requestors_keep_overlapping_window_equivalent_owners_isolated(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|_| {
            let _serial = NATIVE_REQUESTOR_TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let first = NativeEndpoint::new("first selection");
            let second = NativeEndpoint::new("second selection");
            let first_requestor = NativeObject::requestor(first.clone());
            let second_requestor = NativeObject::requestor(second.clone());
            let first_operation = first_requestor.validate().unwrap();
            let overlap = first_requestor.validate().unwrap();
            let second_operation = second_requestor.validate().unwrap();
            let first_board = IsolatedPasteboard::new();
            let second_board = IsolatedPasteboard::new();

            assert!(first_operation.write(&first_board.0));
            assert!(!overlap.write(&first_board.0));
            assert!(second_operation.write(&second_board.0));
            assert!(!first_operation.read(&second_board.0));
            assert!(!second_operation.read(&first_board.0));
            first_requestor.deallocate();
            assert!(!first_operation.read(&first_board.0));
            second_board.set_text("second return");
            assert!(second_operation.read(&second_board.0));
            assert!(first.inserted.borrow().is_empty());
            assert_eq!(
                *second.inserted.borrow(),
                vec![(native_origin(1), "second return".into())]
            );
        });
    }

    pub(in crate::platform) fn native_validation_survives_requestor_deallocation_inside_status(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|_| {
            let _serial = NATIVE_REQUESTOR_TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let endpoint = NativeEndpoint::new("selected");
            let requestor = NativeObject::requestor(endpoint.clone());
            let state = services_state(requestor.0.get()).unwrap();
            let weak_state = Rc::downgrade(&state);
            drop(state);
            let callback_requestor = Rc::clone(&requestor);
            let observed_state = weak_state.clone();
            let observed_retirement = Rc::new(std::cell::Cell::new(false));
            let callback_observation = Rc::clone(&observed_retirement);
            *endpoint.on_status.borrow_mut() = Some(Box::new(move || {
                callback_requestor.deallocate();
                callback_observation.set(
                    observed_state
                        .upgrade()
                        .is_some_and(|state| state.is_retired()),
                );
            }));

            assert!(requestor.validate().is_none());
            assert!(observed_retirement.get());
            assert!(weak_state.upgrade().is_none());
            let successor = NativeObject::requestor(endpoint);
            assert!(successor.validate().is_some());
        });
    }

    pub(in crate::platform) fn native_selection_survives_operation_and_owner_deallocation_without_publishing(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|_| {
            let _serial = NATIVE_REQUESTOR_TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let endpoint = NativeEndpoint::new("selected");
            let requestor = NativeObject::requestor(endpoint.clone());
            let operation = requestor.validate().unwrap();
            let state = services_operation_state(operation.0.get()).unwrap();
            let weak_state = Rc::downgrade(&state);
            drop(state);
            let callback_requestor = Rc::clone(&requestor);
            let callback_operation = Rc::clone(&operation);
            let observed_state = weak_state.clone();
            let observed_retention = Rc::new(std::cell::Cell::new(false));
            let callback_observation = Rc::clone(&observed_retention);
            *endpoint.on_selection.borrow_mut() = Some(Box::new(move || {
                callback_operation.deallocate();
                callback_requestor.deallocate();
                callback_observation.set(observed_state.upgrade().is_some());
            }));
            let board = IsolatedPasteboard::new();
            board.set_text("untouched");

            assert!(!operation.write(&board.0));
            assert!(observed_retention.get());
            assert_eq!(board.text().as_deref(), Some("untouched"));
            assert!(weak_state.upgrade().is_none());
            let successor = NativeObject::requestor(endpoint.clone());
            assert!(successor.validate().unwrap().write(&board.0));
            assert!(endpoint.inserted.borrow().is_empty());
        });
    }

    pub(in crate::platform) fn native_return_survives_operation_and_owner_deallocation_inside_status(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|_| {
            let _serial = NATIVE_REQUESTOR_TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let endpoint = NativeEndpoint::new("selected");
            let requestor = NativeObject::requestor(endpoint.clone());
            let operation = requestor.validate().unwrap();
            let board = IsolatedPasteboard::new();
            assert!(operation.write(&board.0));
            board.set_text("returned after retirement");
            let state = services_operation_state(operation.0.get()).unwrap();
            let weak_state = Rc::downgrade(&state);
            drop(state);
            let callback_requestor = Rc::clone(&requestor);
            let callback_operation = Rc::clone(&operation);
            let observed_state = weak_state.clone();
            let observed_retention = Rc::new(std::cell::Cell::new(false));
            let callback_observation = Rc::clone(&observed_retention);
            *endpoint.on_status.borrow_mut() = Some(Box::new(move || {
                callback_operation.deallocate();
                callback_requestor.deallocate();
                callback_observation.set(observed_state.upgrade().is_some());
            }));

            assert!(!operation.read(&board.0));
            assert!(observed_retention.get());
            assert!(weak_state.upgrade().is_none());
            assert!(endpoint.inserted.borrow().is_empty());
            let successor = NativeObject::requestor(endpoint);
            assert!(successor.validate().unwrap().write(&board.0));
        });
    }

    pub(in crate::platform) fn native_operation_deallocation_inside_insertion_releases_state_and_gate_after_callback(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|_| {
            let _serial = NATIVE_REQUESTOR_TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let endpoint = NativeEndpoint::new("selected");
            let requestor = NativeObject::requestor(endpoint.clone());
            let operation = requestor.validate().unwrap();
            let board = IsolatedPasteboard::new();
            assert!(operation.write(&board.0));
            board.set_text("accepted return");
            let state = services_operation_state(operation.0.get()).unwrap();
            let weak_state = Rc::downgrade(&state);
            drop(state);
            let callback_operation = Rc::clone(&operation);
            let observed_state = weak_state.clone();
            let observed_retention = Rc::new(std::cell::Cell::new(false));
            let callback_observation = Rc::clone(&observed_retention);
            *endpoint.on_insert.borrow_mut() = Some(Box::new(move || {
                callback_operation.deallocate();
                callback_observation.set(observed_state.upgrade().is_some());
            }));

            assert!(operation.read(&board.0));
            assert!(observed_retention.get());
            assert!(weak_state.upgrade().is_none());
            assert_eq!(
                *endpoint.inserted.borrow(),
                vec![(native_origin(1), "accepted return".into())]
            );
            assert!(requestor.validate().unwrap().write(&board.0));
        });
    }

    pub(in crate::platform) fn native_modern_validation_accepts_legacy_only_write_types_once(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|_| {
            let _serial = NATIVE_REQUESTOR_TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let endpoint = NativeEndpoint::new("selected 日本語");
            let requestor = NativeObject::requestor(endpoint.clone());
            // Stickies is a send-only service: validation supplies modern text and no return type.
            let operation = requestor.validate_returning(false).unwrap();
            let board = IsolatedPasteboard::new();
            let object = operation.0.get();

            // SAFETY: The real registered operation responder and isolated pasteboard are retained
            // throughout these selector calls. This reproduces AppKit's legacy-only write array.
            unsafe {
                let legacy = legacy_string_type();
                let types = NSArray::from_slice(&[&*legacy]);
                let contains_modern = types.containsObject(NSPasteboardTypeString);
                assert!(!contains_modern);
                let wrote: bool =
                    msg_send![object, writeSelectionToPasteboard: &*board.0, types: &*types];
                assert!(wrote);
                assert_eq!(board.text().as_deref(), Some("selected 日本語"));
                let legacy_text = board.0.stringForType(&legacy).unwrap();
                assert_eq!(
                    read_nsstring_text(&legacy_text).as_deref(),
                    Some("selected 日本語")
                );
                let repeated: bool =
                    msg_send![object, writeSelectionToPasteboard: &*board.0, types: &*types];
                assert!(!repeated);
            }
            assert!(!operation.read(&board.0));
            assert!(endpoint.inserted.borrow().is_empty());
        });
    }
}
