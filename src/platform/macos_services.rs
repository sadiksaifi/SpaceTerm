//! AppKit Services responder registration and native pasteboard conversion.
//!
//! GPUI 0.2.2 can install the Services menu but exposes neither its requestor callbacks nor
//! access to a Service's supplied pasteboard. Request policy and lifetime authority live in
//! the portable Services owner; this adapter only connects those operations to AppKit.

use std::ffi::{c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

#[cfg(not(test))]
use cocoa::appkit::NSApp;
use cocoa::appkit::NSPasteboardTypeString;
use cocoa::base::{BOOL, NO, YES, id, nil};
use cocoa::foundation::{NSArray, NSAutoreleasePool, NSInteger, NSString, NSUInteger};
use gpui::Window;
use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Protocol, Sel};
use objc::{class, msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use thiserror::Error;

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

#[derive(Debug, Error)]
pub(crate) enum MacosServicesError {
    #[cfg(not(test))]
    #[error("the AppKit application is unavailable")]
    ApplicationUnavailable,
    #[error("the GPUI window did not expose an AppKit view")]
    NativeViewUnavailable,
    #[error("the SpaceTerm Services responder class could not be registered")]
    ResponderClassUnavailable,
    #[error("the SpaceTerm Services responder could not be allocated")]
    ResponderAllocationFailed,
}

#[cfg(not(test))]
pub(crate) fn register() -> Result<(), MacosServicesError> {
    // SAFETY: SpaceTerm initializes its application on AppKit's main thread. The array is used only
    // for this synchronous registration call, and AppKit retains the registered type strings.
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let application = NSApp();
        if application == nil {
            pool.drain();
            return Err(MacosServicesError::ApplicationUnavailable);
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
) -> Result<(), MacosServicesError> {
    let native_handle = HasWindowHandle::window_handle(window)
        .map_err(|_| MacosServicesError::NativeViewUnavailable)?;
    let RawWindowHandle::AppKit(native_handle) = native_handle.as_raw() else {
        return Err(MacosServicesError::NativeViewUnavailable);
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
            return Err(MacosServicesError::ResponderAllocationFailed);
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

fn services_responder_class() -> Result<&'static Class, MacosServicesError> {
    if let Some(class) = Class::get(SERVICES_RESPONDER_CLASS) {
        return Ok(class);
    }
    let Some(mut declaration) = ClassDecl::new(SERVICES_RESPONDER_CLASS, class!(NSResponder))
    else {
        return Err(MacosServicesError::ResponderClassUnavailable);
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

fn services_operation_responder_class() -> Result<&'static Class, MacosServicesError> {
    if let Some(class) = Class::get(SERVICES_OPERATION_RESPONDER_CLASS) {
        return Ok(class);
    }
    let Some(mut declaration) =
        ClassDecl::new(SERVICES_OPERATION_RESPONDER_CLASS, class!(NSResponder))
    else {
        return Err(MacosServicesError::ResponderClassUnavailable);
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
        if contains_string == NO {
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
    if is_string == YES {
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

#[cfg(test)]
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
            assert_eq!(service_data_type(unsupported), ServiceDataType::Unsupported);

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
}
