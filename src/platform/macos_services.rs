//! AppKit Services responder registration and native pasteboard conversion.
//!
//! GPUI 0.2.2 can install the Services menu but exposes neither its requestor callbacks nor
//! access to a Service's supplied pasteboard. Request policy and lifetime authority live in
//! the portable Services owner; this adapter only connects those operations to AppKit.

use std::ffi::{c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use super::services_registration::{ServicesRegistration, ServicesRegistrationError};
use cocoa::appkit::NSApp;
use cocoa::appkit::{NSPasteboardTypeString, NSStringPboardType};
use cocoa::base::{BOOL, NO, YES, id, nil};
use cocoa::foundation::{NSArray, NSAutoreleasePool, NSInteger, NSString, NSUInteger};
use gpui::Window;
use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Protocol, Sel};
use objc::{class, msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::terminal::native_services::services::{
    ServiceDataType, ServiceEndpoint, ServiceOperation, ServicePasteboardIdentity, ServiceRequests,
    bounded_service_text_byte_len, decode_service_text_bytes,
};

const SERVICES_RESPONDER_CLASS: &str = "SpaceTermServicesResponder";
const SERVICES_STATE_IVAR: &str = "spaceTermServicesState";
const SERVICES_OPERATION_RESPONDER_CLASS: &str = "SpaceTermServicesOperationResponder";
const SERVICES_OPERATION_STATE_IVAR: &str = "spaceTermServicesOperationState";
const OBJC_ASSOCIATION_RETAIN_NONATOMIC: usize = 1;
const NS_UTF8_STRING_ENCODING: NSUInteger = 4;
static SERVICES_RESPONDER_ASSOCIATION: u8 = 0;

unsafe extern "C" {
    fn objc_getAssociatedObject(object: id, key: *const c_void) -> id;
    fn objc_setAssociatedObject(object: id, key: *const c_void, value: id, policy: usize);
}

pub(crate) fn register() -> Result<(), ServicesRegistrationError> {
    // SAFETY: SpaceTerm initializes its application on AppKit's main thread. The array is used only
    // for this synchronous registration call, and AppKit retains the registered type strings.
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let application = NSApp();
        if application == nil {
            pool.drain();
            return Err(ServicesRegistrationError::ApplicationUnavailable);
        }
        let string_types = NSArray::arrayWithObject(nil, NSPasteboardTypeString);
        let _: () = msg_send![application,
            registerServicesMenuSendTypes: string_types
            returnTypes: string_types
        ];
        pool.drain();
    }
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
    let native_view = native_handle.ns_view.as_ptr().cast::<Object>();

    // SAFETY: GPUI's AppKit raw-window handle guarantees a live NSView for the lifetime of this
    // call. Installation runs on AppKit's main thread. The associated object retains the custom
    // responder exactly as long as the view, while its unretained nextResponder link preserves the
    // original responder chain. The responder owns the boxed Rust state and drops it from dealloc.
    unsafe {
        let association_key = (&raw const SERVICES_RESPONDER_ASSOCIATION).cast::<c_void>();
        if objc_getAssociatedObject(native_view, association_key) != nil {
            return Ok(());
        }

        let responder_class = services_responder_class()?;
        let responder: id = msg_send![responder_class, alloc];
        let responder: id = msg_send![responder, init];
        if responder == nil {
            return Err(ServicesRegistrationError::ResponderAllocationFailed);
        }

        let state = Box::new(Rc::new(ServiceRequests::new(endpoint)));
        (*responder).set_ivar(SERVICES_STATE_IVAR, Box::into_raw(state).cast::<c_void>());

        let previous_responder: id = msg_send![native_view, nextResponder];
        let _: () = msg_send![responder, setNextResponder: previous_responder];
        objc_setAssociatedObject(
            native_view,
            association_key,
            responder,
            OBJC_ASSOCIATION_RETAIN_NONATOMIC,
        );
        let _: () = msg_send![native_view, setNextResponder: responder];
        let _: () = msg_send![responder, release];
    }
    Ok(())
}

fn services_responder_class() -> Result<&'static Class, ServicesRegistrationError> {
    if let Some(class) = Class::get(SERVICES_RESPONDER_CLASS) {
        return Ok(class);
    }
    let Some(mut declaration) = ClassDecl::new(SERVICES_RESPONDER_CLASS, class!(NSResponder))
    else {
        return Err(ServicesRegistrationError::ResponderClassUnavailable);
    };
    declaration.add_ivar::<*mut c_void>(SERVICES_STATE_IVAR);
    if let Some(protocol) = Protocol::get("NSServicesMenuRequestor") {
        declaration.add_protocol(protocol);
    }
    // SAFETY: Each selector uses AppKit's documented NSServicesMenuRequestor ABI, and the
    // registered function signatures exactly match those Objective-C method encodings.
    unsafe {
        declaration.add_method(
            sel!(dealloc),
            dealloc_services_responder as extern "C" fn(&Object, Sel),
        );
        declaration.add_method(
            sel!(validRequestorForSendType:returnType:),
            valid_requestor as extern "C" fn(&Object, Sel, id, id) -> id,
        );
    }
    Ok(declaration.register())
}

fn services_operation_responder_class() -> Result<&'static Class, ServicesRegistrationError> {
    if let Some(class) = Class::get(SERVICES_OPERATION_RESPONDER_CLASS) {
        return Ok(class);
    }
    let Some(mut declaration) =
        ClassDecl::new(SERVICES_OPERATION_RESPONDER_CLASS, class!(NSResponder))
    else {
        return Err(ServicesRegistrationError::ResponderClassUnavailable);
    };
    declaration.add_ivar::<*mut c_void>(SERVICES_OPERATION_STATE_IVAR);
    if let Some(protocol) = Protocol::get("NSServicesMenuRequestor") {
        declaration.add_protocol(protocol);
    }
    // SAFETY: These selectors use AppKit's documented NSServicesMenuRequestor ABI. Each operation
    // responder owns exactly one request state and is autoreleased under Cocoa naming rules.
    unsafe {
        declaration.add_method(
            sel!(dealloc),
            dealloc_services_operation_responder as extern "C" fn(&Object, Sel),
        );
        declaration.add_method(
            sel!(writeSelectionToPasteboard:types:),
            write_selection_to_pasteboard as extern "C" fn(&Object, Sel, id, id) -> BOOL,
        );
        declaration.add_method(
            sel!(readSelectionFromPasteboard:),
            read_selection_from_pasteboard as extern "C" fn(&Object, Sel, id) -> BOOL,
        );
    }
    Ok(declaration.register())
}

extern "C" fn dealloc_services_responder(this: &Object, _: Sel) {
    // SAFETY: install stores exactly one Box pointer in this ivar before the responder enters the
    // chain. AppKit calls dealloc once after releasing the view's retained association.
    unsafe {
        let state: *mut c_void = *this.get_ivar(SERVICES_STATE_IVAR);
        if !state.is_null() {
            let state = Box::from_raw(state.cast::<Rc<ServiceRequests>>());
            state.retire();
            drop(state);
        }
        let _: () = msg_send![super(this, class!(NSResponder)), dealloc];
    }
}

extern "C" fn dealloc_services_operation_responder(this: &Object, _: Sel) {
    // SAFETY: create_services_operation stores one Box pointer before returning the responder.
    unsafe {
        let state: *mut c_void = *this.get_ivar(SERVICES_OPERATION_STATE_IVAR);
        if !state.is_null() {
            drop(Box::from_raw(state.cast::<Rc<ServiceOperation>>()));
        }
        let _: () = msg_send![super(this, class!(NSResponder)), dealloc];
    }
}

extern "C" fn valid_requestor(this: &Object, _: Sel, send_type: id, return_type: id) -> id {
    let state = unsafe { services_state(this) };
    let operation = catch_unwind(AssertUnwindSafe(|| {
        let send_type = unsafe { service_data_type(send_type) };
        let return_type = unsafe { service_data_type(return_type) };
        let operation = state.as_ref()?.operation(send_type, return_type)?;
        unsafe { create_services_operation(operation) }
    }))
    .ok()
    .flatten();
    if let Some(operation) = operation {
        return operation;
    }

    if state.is_some_and(|state| state.is_retired()) {
        // A callback destroyed the native responder. Its retained Rust owner remains safe, but
        // the old Objective-C object can no longer continue the responder chain.
        return nil;
    }

    // SAFETY: NSResponder's implementation continues the pre-existing responder chain when
    // SpaceTerm cannot satisfy the requested types or current terminal state.
    unsafe {
        msg_send![super(this, class!(NSResponder)),
            validRequestorForSendType: send_type
            returnType: return_type
        ]
    }
}

unsafe fn create_services_operation(state: ServiceOperation) -> Option<id> {
    let responder_class = services_operation_responder_class().ok()?;
    let responder: id = unsafe { msg_send![responder_class, alloc] };
    let responder: id = unsafe { msg_send![responder, init] };
    if responder == nil {
        return None;
    }
    let state = Box::new(Rc::new(state));
    unsafe {
        (*responder).set_ivar(
            SERVICES_OPERATION_STATE_IVAR,
            Box::into_raw(state).cast::<c_void>(),
        );
    }
    let responder: id = unsafe { msg_send![responder, autorelease] };
    Some(responder)
}

extern "C" fn write_selection_to_pasteboard(
    this: &Object,
    _: Sel,
    pasteboard: id,
    types: id,
) -> BOOL {
    catch_unwind(AssertUnwindSafe(|| {
        if pasteboard == nil || types == nil {
            return NO;
        }
        // SAFETY: AppKit supplies NSPasteboard and NSArray objects for this synchronous callback.
        let contains_string: BOOL =
            unsafe { msg_send![types, containsObject: NSPasteboardTypeString] };
        // AppKit may validate modern text but pass the legacy type in this array (FB11838671).
        // Both names describe the same text representation; publication remains modern UTF-8.
        let contains_legacy_string: BOOL =
            unsafe { msg_send![types, containsObject: NSStringPboardType] };
        if contains_string == NO && contains_legacy_string == NO {
            return NO;
        }
        let Some(state) = (unsafe { services_operation_state(this) }) else {
            return NO;
        };
        let wrote = state.write_selection(
            ServicePasteboardIdentity::new(pasteboard as usize),
            |text| unsafe { write_service_text(pasteboard, text) },
        );
        if wrote { YES } else { NO }
    }))
    .unwrap_or(NO)
}

extern "C" fn read_selection_from_pasteboard(this: &Object, _: Sel, pasteboard: id) -> BOOL {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(state) = (unsafe { services_operation_state(this) }) else {
            return NO;
        };
        let inserted = state.read_selection(
            ServicePasteboardIdentity::new(pasteboard as usize),
            || unsafe { read_service_text(pasteboard) },
        );
        if inserted { YES } else { NO }
    }))
    .unwrap_or(NO)
}

unsafe fn services_state(this: &Object) -> Option<Rc<ServiceRequests>> {
    let state: *mut c_void = unsafe { *this.get_ivar(SERVICES_STATE_IVAR) };
    // Clone before any callback can reentrantly destroy the native responder and its ivar.
    unsafe { state.cast::<Rc<ServiceRequests>>().as_ref() }.cloned()
}

unsafe fn services_operation_state(this: &Object) -> Option<Rc<ServiceOperation>> {
    let state: *mut c_void = unsafe { *this.get_ivar(SERVICES_OPERATION_STATE_IVAR) };
    unsafe { state.cast::<Rc<ServiceOperation>>().as_ref() }.cloned()
}

unsafe fn service_data_type(value: id) -> ServiceDataType {
    if value == nil {
        return ServiceDataType::Absent;
    }
    // SAFETY: AppKit documents both arguments as NSString pasteboard types and may represent an
    // omitted side of the Service contract with an empty string instead of nil.
    let character_len: NSUInteger = unsafe { msg_send![value, length] };
    if character_len == 0 {
        return ServiceDataType::Absent;
    }
    let is_string: BOOL = unsafe { msg_send![value, isEqualToString: NSPasteboardTypeString] };
    let is_legacy_string: BOOL = unsafe { msg_send![value, isEqualToString: NSStringPboardType] };
    if is_string == YES || is_legacy_string == YES {
        ServiceDataType::String
    } else {
        ServiceDataType::Unsupported
    }
}

unsafe fn write_service_text(pasteboard: id, text: &str) -> bool {
    if pasteboard == nil {
        return false;
    }
    let pool = unsafe { NSAutoreleasePool::new(nil) };
    let types = unsafe { NSArray::arrayWithObject(nil, NSPasteboardTypeString) };
    let _: NSInteger = unsafe { msg_send![pasteboard, declareTypes: types owner: nil] };
    let value = unsafe { NSString::alloc(nil).init_str(text).autorelease() };
    let result: BOOL =
        unsafe { msg_send![pasteboard, setString: value forType: NSPasteboardTypeString] };
    unsafe { pool.drain() };
    result == YES
}

unsafe fn read_service_text(pasteboard: id) -> Option<String> {
    if pasteboard == nil {
        return None;
    }
    let types: id = unsafe { msg_send![pasteboard, types] };
    let contains_string: BOOL = unsafe { msg_send![types, containsObject: NSPasteboardTypeString] };
    if contains_string == NO {
        return None;
    }
    let value: id = unsafe { msg_send![pasteboard, stringForType: NSPasteboardTypeString] };
    unsafe { read_nsstring_text(value) }
}

unsafe fn read_nsstring_text(value: id) -> Option<String> {
    if value == nil {
        return None;
    }
    let character_len: NSUInteger = unsafe { msg_send![value, length] };
    let byte_len: NSUInteger =
        unsafe { msg_send![value, lengthOfBytesUsingEncoding: NS_UTF8_STRING_ENCODING] };
    let byte_len = bounded_service_text_byte_len(
        usize::try_from(character_len).ok()?,
        usize::try_from(byte_len).ok()?,
    )?;
    let utf8: *const c_char = unsafe { msg_send![value, UTF8String] };
    if utf8.is_null() {
        return None;
    }
    // SAFETY: The pointer is consumed immediately, before another Objective-C call or autorelease
    // pool drain can shorten NSString's documented UTF8String lifetime. The byte count comes from
    // the same object and was rejected before this slice can exceed Paste Payload's hard limit.
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
mod tests {
    use cocoa::appkit::NSPasteboard;

    use super::*;
    use crate::terminal::MAX_PASTE_BYTES;

    #[test]
    fn service_type_classifies_nil_and_empty_nsstring_as_absent() {
        // SAFETY: The NSStrings remain live for these synchronous Objective-C comparisons and the
        // local autorelease pool is drained afterward.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let empty = NSString::alloc(nil).init_str("").autorelease();
            let unsupported = NSString::alloc(nil).init_str("public.html").autorelease();

            assert_eq!(service_data_type(nil), ServiceDataType::Absent);
            assert_eq!(service_data_type(empty), ServiceDataType::Absent);
            assert_eq!(
                service_data_type(NSPasteboardTypeString),
                ServiceDataType::String
            );
            assert_eq!(
                service_data_type(NSStringPboardType),
                ServiceDataType::String
            );
            assert_eq!(service_data_type(unsupported), ServiceDataType::Unsupported);
            let generic_plain_text = NSString::alloc(nil)
                .init_str("public.plain-text")
                .autorelease();
            assert_eq!(
                service_data_type(generic_plain_text),
                ServiceDataType::Unsupported
            );

            pool.drain();
        }
    }

    #[test]
    fn nsstring_decode_enforces_the_paste_limit_before_copying() {
        // SAFETY: Each NSString is initialized from a live byte slice, decoded synchronously, and
        // released before the local autorelease pool is drained.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let at_limit = vec![b'x'; MAX_PASTE_BYTES];
            let over_limit = vec![b'x'; MAX_PASTE_BYTES + 1];
            let at_limit_value: id = msg_send![class!(NSString), alloc];
            let at_limit_value: id = msg_send![at_limit_value,
                initWithBytes: at_limit.as_ptr()
                length: at_limit.len()
                encoding: NS_UTF8_STRING_ENCODING
            ];
            let over_limit_value: id = msg_send![class!(NSString), alloc];
            let over_limit_value: id = msg_send![over_limit_value,
                initWithBytes: over_limit.as_ptr()
                length: over_limit.len()
                encoding: NS_UTF8_STRING_ENCODING
            ];

            assert_eq!(
                read_nsstring_text(at_limit_value).map(|text| text.len()),
                Some(MAX_PASTE_BYTES)
            );
            assert_eq!(read_nsstring_text(over_limit_value), None);

            let _: () = msg_send![at_limit_value, release];
            let _: () = msg_send![over_limit_value, release];
            pool.drain();
        }
    }

    #[test]
    fn nsstring_decode_rejects_embedded_nul_without_truncation() {
        // SAFETY: The NSString owns a synchronous copy of these bytes and is released below.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let bytes = b"before\0after";
            let value: id = msg_send![class!(NSString), alloc];
            let value: id = msg_send![value,
                initWithBytes: bytes.as_ptr()
                length: bytes.len()
                encoding: NS_UTF8_STRING_ENCODING
            ];

            assert_eq!(read_nsstring_text(value), None);

            let _: () = msg_send![value, release];
            pool.drain();
        }
    }

    #[test]
    fn nsstring_decode_rejects_nonempty_failed_utf8_conversion() {
        // SAFETY: This intentionally malformed UTF-16 NSString is owned and released entirely by
        // the synchronous test. A lone high surrogate cannot be converted losslessly to UTF-8.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let invalid_utf16 = [0xd800u16];
            let value: id = msg_send![class!(NSString), alloc];
            let value: id = msg_send![value,
                initWithCharacters: invalid_utf16.as_ptr()
                length: invalid_utf16.len()
            ];

            assert_eq!(read_nsstring_text(value), None);

            let _: () = msg_send![value, release];
            pool.drain();
        }
    }

    #[test]
    fn service_pasteboard_round_trip_uses_only_public_utf8_text() {
        // SAFETY: This test owns the unique AppKit pasteboard for the synchronous round trip.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let pasteboard = NSPasteboard::pasteboardWithUniqueName(nil);

            assert!(write_service_text(pasteboard, "service text\n"));
            let text = read_service_text(pasteboard);

            pasteboard.releaseGlobally();
            pool.drain();
            assert_eq!(text.as_deref(), Some("service text\n"));
        }
    }
    // NSResponder objects have no window or application attachment here. Each fixture is confined
    // to its test thread, and this lock serializes Objective-C class registration and callbacks.
    static NATIVE_REQUESTOR_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct NativePool(id);

    impl NativePool {
        fn new() -> Self {
            // SAFETY: The pool is created and drained on the same synchronous test thread.
            Self(unsafe { NSAutoreleasePool::new(nil) })
        }
    }

    impl Drop for NativePool {
        fn drop(&mut self) {
            unsafe { self.0.drain() };
        }
    }

    struct NativeObject(std::cell::Cell<id>);

    impl NativeObject {
        fn requestor(endpoint: Rc<dyn ServiceEndpoint>) -> Rc<Self> {
            // SAFETY: This constructs the registered production responder with the same owned
            // ivar representation as install, without creating an NSView or NSApplication.
            unsafe {
                let class = services_responder_class().unwrap();
                let object: id = msg_send![class, alloc];
                let object: id = msg_send![object, init];
                assert_ne!(object, nil);
                let state = Box::new(Rc::new(ServiceRequests::new(endpoint)));
                (*object).set_ivar(SERVICES_STATE_IVAR, Box::into_raw(state).cast::<c_void>());
                Rc::new(Self(std::cell::Cell::new(object)))
            }
        }

        fn validate(&self) -> Option<Rc<Self>> {
            self.validate_returning(true)
        }

        fn validate_returning(&self, returns_text: bool) -> Option<Rc<Self>> {
            let _pool = NativePool::new();
            let object = self.0.get();
            assert_ne!(object, nil);
            // SAFETY: The receiver is live at message entry. Tests may destroy it reentrantly
            // through the endpoint; the production callback must survive that destruction.
            unsafe {
                let return_type = if returns_text {
                    NSPasteboardTypeString
                } else {
                    nil
                };
                let operation: id = msg_send![object,
                    validRequestorForSendType: NSPasteboardTypeString
                    returnType: return_type
                ];
                if operation == nil {
                    return None;
                }
                let operation: id = msg_send![operation, retain];
                Some(Rc::new(Self(std::cell::Cell::new(operation))))
            }
        }

        fn write(&self, pasteboard: id) -> bool {
            let object = self.0.get();
            assert_ne!(object, nil);
            // SAFETY: These are the registered production selector and an owned NSPasteboard.
            unsafe {
                let types = NSArray::arrayWithObject(nil, NSPasteboardTypeString);
                let result: BOOL =
                    msg_send![object, writeSelectionToPasteboard: pasteboard types: types];
                result == YES
            }
        }

        fn read(&self, pasteboard: id) -> bool {
            let object = self.0.get();
            assert_ne!(object, nil);
            // SAFETY: The receiver is live at entry and the isolated pasteboard remains owned.
            let result: BOOL =
                unsafe { msg_send![object, readSelectionFromPasteboard: pasteboard] };
            result == YES
        }

        fn release(&self) {
            let object = self.0.replace(nil);
            if object != nil {
                // SAFETY: This consumes only the fixture's owned retain under normal Cocoa rules.
                let _: () = unsafe { msg_send![object, release] };
            }
        }

        fn deallocate(&self) {
            let object = self.0.replace(nil);
            assert_ne!(object, nil);
            // SAFETY: The fixture owns the sole retain and is unattached to an NSView. AppKit's
            // final-release scheduling does not run synchronously on the Rust harness thread.
            // Deliberately dispatch the actual production dealloc selector here to probe
            // reentrant callback ownership and superclass teardown. The cleared cell prevents
            // a second release. This is no claim about native window removal or release timing.
            unsafe {
                let count: NSUInteger = msg_send![object, retainCount];
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

    struct IsolatedPasteboard(id);

    impl IsolatedPasteboard {
        fn new() -> Self {
            // SAFETY: The test's outer pool retains the unique pasteboard through every callback.
            Self(unsafe { NSPasteboard::pasteboardWithUniqueName(nil) })
        }

        fn set_text(&self, text: &str) {
            assert!(unsafe { write_service_text(self.0, text) });
        }

        fn text(&self) -> Option<String> {
            unsafe { read_service_text(self.0) }
        }
    }

    impl Drop for IsolatedPasteboard {
        fn drop(&mut self) {
            // SAFETY: This fixture exclusively owns its named server-side pasteboard.
            unsafe { self.0.releaseGlobally() };
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

    #[test]
    fn native_selectors_publish_selection_and_accept_exactly_one_return() {
        let _serial = NATIVE_REQUESTOR_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _pool = NativePool::new();
        let endpoint = NativeEndpoint::new("selected 日本語");
        let requestor = NativeObject::requestor(endpoint.clone());
        let operation = requestor.validate().unwrap();
        let board = IsolatedPasteboard::new();

        assert!(operation.write(board.0));
        assert_eq!(board.text().as_deref(), Some("selected 日本語"));
        assert!(!operation.write(board.0));
        board.set_text("transformed text");
        assert!(operation.read(board.0));
        assert!(!operation.read(board.0));
        assert_eq!(
            *endpoint.inserted.borrow(),
            vec![(native_origin(1), "transformed text".into())]
        );
    }

    #[test]
    fn native_selectors_reject_stale_validation_and_stale_return() {
        let _serial = NATIVE_REQUESTOR_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _pool = NativePool::new();
        let endpoint = NativeEndpoint::new("successor selection");
        let requestor = NativeObject::requestor(endpoint.clone());
        let stale = requestor.validate().unwrap();
        let board = IsolatedPasteboard::new();
        board.set_text("untouched");
        endpoint.origin.set(native_origin(2));
        assert!(!stale.write(board.0));
        assert_eq!(board.text().as_deref(), Some("untouched"));

        let current = requestor.validate().unwrap();
        assert!(current.write(board.0));
        board.set_text("stale return");
        endpoint.origin.set(native_origin(3));
        assert!(!current.read(board.0));
        endpoint.origin.set(native_origin(2));
        assert!(!current.read(board.0));
        assert!(endpoint.inserted.borrow().is_empty());
    }

    #[test]
    fn native_requestors_keep_overlapping_window_equivalent_owners_isolated() {
        let _serial = NATIVE_REQUESTOR_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _pool = NativePool::new();
        let first = NativeEndpoint::new("first selection");
        let second = NativeEndpoint::new("second selection");
        let first_requestor = NativeObject::requestor(first.clone());
        let second_requestor = NativeObject::requestor(second.clone());
        let first_operation = first_requestor.validate().unwrap();
        let overlap = first_requestor.validate().unwrap();
        let second_operation = second_requestor.validate().unwrap();
        let first_board = IsolatedPasteboard::new();
        let second_board = IsolatedPasteboard::new();

        assert!(first_operation.write(first_board.0));
        assert!(!overlap.write(first_board.0));
        assert!(second_operation.write(second_board.0));
        assert!(!first_operation.read(second_board.0));
        assert!(!second_operation.read(first_board.0));
        first_requestor.deallocate();
        assert!(!first_operation.read(first_board.0));
        second_board.set_text("second return");
        assert!(second_operation.read(second_board.0));
        assert!(first.inserted.borrow().is_empty());
        assert_eq!(
            *second.inserted.borrow(),
            vec![(native_origin(1), "second return".into())]
        );
    }

    #[test]
    fn native_validation_survives_requestor_deallocation_inside_status() {
        let _serial = NATIVE_REQUESTOR_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _pool = NativePool::new();
        let endpoint = NativeEndpoint::new("selected");
        let requestor = NativeObject::requestor(endpoint.clone());
        let state = unsafe { services_state(&*requestor.0.get()) }.unwrap();
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
    }

    #[test]
    fn native_selection_survives_operation_and_owner_deallocation_without_publishing() {
        let _serial = NATIVE_REQUESTOR_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _pool = NativePool::new();
        let endpoint = NativeEndpoint::new("selected");
        let requestor = NativeObject::requestor(endpoint.clone());
        let operation = requestor.validate().unwrap();
        let state = unsafe { services_operation_state(&*operation.0.get()) }.unwrap();
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

        assert!(!operation.write(board.0));
        assert!(observed_retention.get());
        assert_eq!(board.text().as_deref(), Some("untouched"));
        assert!(weak_state.upgrade().is_none());
        let successor = NativeObject::requestor(endpoint.clone());
        assert!(successor.validate().unwrap().write(board.0));
        assert!(endpoint.inserted.borrow().is_empty());
    }

    #[test]
    fn native_return_survives_operation_and_owner_deallocation_inside_status() {
        let _serial = NATIVE_REQUESTOR_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _pool = NativePool::new();
        let endpoint = NativeEndpoint::new("selected");
        let requestor = NativeObject::requestor(endpoint.clone());
        let operation = requestor.validate().unwrap();
        let board = IsolatedPasteboard::new();
        assert!(operation.write(board.0));
        board.set_text("returned after retirement");
        let state = unsafe { services_operation_state(&*operation.0.get()) }.unwrap();
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

        assert!(!operation.read(board.0));
        assert!(observed_retention.get());
        assert!(weak_state.upgrade().is_none());
        assert!(endpoint.inserted.borrow().is_empty());
        let successor = NativeObject::requestor(endpoint);
        assert!(successor.validate().unwrap().write(board.0));
    }

    #[test]
    fn native_operation_deallocation_inside_insertion_releases_state_and_gate_after_callback() {
        let _serial = NATIVE_REQUESTOR_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _pool = NativePool::new();
        let endpoint = NativeEndpoint::new("selected");
        let requestor = NativeObject::requestor(endpoint.clone());
        let operation = requestor.validate().unwrap();
        let board = IsolatedPasteboard::new();
        assert!(operation.write(board.0));
        board.set_text("accepted return");
        let state = unsafe { services_operation_state(&*operation.0.get()) }.unwrap();
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

        assert!(operation.read(board.0));
        assert!(observed_retention.get());
        assert!(weak_state.upgrade().is_none());
        assert_eq!(
            *endpoint.inserted.borrow(),
            vec![(native_origin(1), "accepted return".into())]
        );
        assert!(requestor.validate().unwrap().write(board.0));
    }

    #[test]
    fn native_modern_validation_accepts_legacy_only_write_types_once() {
        let _serial = NATIVE_REQUESTOR_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _pool = NativePool::new();
        let endpoint = NativeEndpoint::new("selected 日本語");
        let requestor = NativeObject::requestor(endpoint.clone());
        // Stickies is a send-only service: validation supplies modern text and no return type.
        let operation = requestor.validate_returning(false).unwrap();
        let board = IsolatedPasteboard::new();
        let object = operation.0.get();

        // SAFETY: The real registered operation responder and isolated pasteboard are retained
        // throughout these selector calls. This reproduces AppKit's legacy-only write array.
        unsafe {
            let types = NSArray::arrayWithObject(nil, NSStringPboardType);
            let contains_modern: BOOL = msg_send![types, containsObject: NSPasteboardTypeString];
            assert_eq!(contains_modern, NO);
            let wrote: BOOL = msg_send![object, writeSelectionToPasteboard: board.0 types: types];
            assert_eq!(wrote, YES);
            assert_eq!(board.text().as_deref(), Some("selected 日本語"));
            let legacy_text: id = msg_send![board.0, stringForType: NSStringPboardType];
            assert_eq!(
                read_nsstring_text(legacy_text).as_deref(),
                Some("selected 日本語")
            );
            let repeated: BOOL =
                msg_send![object, writeSelectionToPasteboard: board.0 types: types];
            assert_eq!(repeated, NO);
        }
        assert!(!operation.read(board.0));
        assert!(endpoint.inserted.borrow().is_empty());
    }
}
