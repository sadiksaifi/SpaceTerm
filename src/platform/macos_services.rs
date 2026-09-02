use std::cell::Cell;
use std::ffi::{c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

#[cfg(not(test))]
use cocoa::appkit::NSApp;
use cocoa::appkit::NSPasteboardTypeString;
use cocoa::base::{BOOL, NO, YES, id, nil};
use cocoa::foundation::{NSArray, NSAutoreleasePool, NSInteger, NSString, NSUInteger};
use gpui::{AnyWindowHandle, App, AsyncApp, Window};
use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Protocol, Sel};
use objc::{class, msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use thiserror::Error;

use crate::terminal::{
    MAX_PASTE_BYTES, NativeServiceCapabilities, NativeServiceOrigin, NativeServiceStatus,
    SelectionCopy,
};
use crate::ui::WorkspaceManager;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ServiceDataType {
    Absent,
    String,
    Unsupported,
}

impl ServiceDataType {
    const fn is_requested(self) -> bool {
        !matches!(self, Self::Absent)
    }

    const fn is_supported(self) -> bool {
        !matches!(self, Self::Unsupported)
    }
}

#[derive(Clone)]
struct MacosServicesEndpoint {
    app: AsyncApp,
    window: AnyWindowHandle,
}

impl MacosServicesEndpoint {
    fn status(&self) -> NativeServiceStatus {
        self.app
            .update(|cx| {
                self.window.update(cx, |root, window, cx| {
                    let Ok(manager) = root.downcast::<WorkspaceManager>() else {
                        return NativeServiceStatus::default();
                    };
                    manager.update(cx, |manager, cx| manager.native_service_status(window, cx))
                })
            })
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
    }

    fn selection(&self, origin: NativeServiceOrigin) -> Option<SelectionCopy> {
        self.app
            .update(|cx| {
                self.window.update(cx, |root, window, cx| {
                    let Ok(manager) = root.downcast::<WorkspaceManager>() else {
                        return None;
                    };
                    manager.update(cx, |manager, cx| {
                        manager.native_service_selection(origin, window, cx)
                    })
                })
            })
            .ok()
            .and_then(Result::ok)
            .flatten()
    }

    fn insert_text(&self, origin: NativeServiceOrigin, text: String) -> bool {
        self.app
            .update(|cx| {
                self.window.update(cx, |root, window, cx| {
                    let Ok(manager) = root.downcast::<WorkspaceManager>() else {
                        return false;
                    };
                    manager.update(cx, |manager, cx| {
                        manager.insert_native_service_text(origin, text, window, cx)
                    })
                })
            })
            .ok()
            .and_then(Result::ok)
            .unwrap_or(false)
    }
}

struct MacosServicesState {
    // AppKit invokes Services requestor methods on its main thread. The per-validation requestors
    // therefore share this non-Send gate through Rc<Cell<_>>; no instance crosses that thread.
    endpoint: MacosServicesEndpoint,
    next_request_id: Cell<u64>,
    active_return_request: Rc<Cell<Option<u64>>>,
}

impl MacosServicesState {
    fn operation(&self, returns_text: bool) -> Option<MacosServicesOperationState> {
        let request_id = self.next_request_id.get().checked_add(1)?;
        self.next_request_id.set(request_id);
        Some(MacosServicesOperationState {
            endpoint: self.endpoint.clone(),
            identity: ServiceOperationIdentity {
                request_id,
                returns_text,
                active_return_request: Rc::clone(&self.active_return_request),
                write_claimed: Cell::new(false),
                pasteboard: Cell::new(None),
                origin: Cell::new(None),
            },
        })
    }
}

struct MacosServicesOperationState {
    endpoint: MacosServicesEndpoint,
    identity: ServiceOperationIdentity,
}

struct ServiceOperationIdentity {
    request_id: u64,
    returns_text: bool,
    active_return_request: Rc<Cell<Option<u64>>>,
    write_claimed: Cell<bool>,
    pasteboard: Cell<Option<usize>>,
    origin: Cell<Option<NativeServiceOrigin>>,
}

impl ServiceOperationIdentity {
    fn claim_write(&self) -> bool {
        if self.write_claimed.replace(true) || self.active_return_request.get().is_some() {
            return false;
        }
        if self.returns_text {
            self.active_return_request.set(Some(self.request_id));
        }
        true
    }

    fn finish_send(&self, origin: NativeServiceOrigin, pasteboard: id, succeeded: bool) {
        if self.returns_text && succeeded {
            self.origin.set(Some(origin));
            self.pasteboard.set(Some(pasteboard as usize));
        } else {
            self.release_return();
        }
    }

    fn take_return_origin(&self, pasteboard: id) -> Option<NativeServiceOrigin> {
        if self.active_return_request.get() != Some(self.request_id)
            || self.pasteboard.get() != Some(pasteboard as usize)
        {
            return None;
        }
        let origin = self.origin.take();
        self.pasteboard.set(None);
        self.release_return();
        origin
    }

    fn release_return(&self) {
        if self.active_return_request.get() == Some(self.request_id) {
            self.active_return_request.set(None);
        }
    }
}

impl Drop for ServiceOperationIdentity {
    fn drop(&mut self) {
        self.release_return();
    }
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

pub(crate) fn install(window: &Window, cx: &App) -> Result<(), MacosServicesError> {
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

        let state = Box::new(MacosServicesState {
            endpoint: MacosServicesEndpoint {
                app: cx.to_async(),
                window: window.window_handle(),
            },
            next_request_id: Cell::new(0),
            active_return_request: Rc::new(Cell::new(None)),
        });
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

fn accepts_service_request(
    capabilities: NativeServiceCapabilities,
    send_type: ServiceDataType,
    return_type: ServiceDataType,
) -> bool {
    if !send_type.is_supported() || !return_type.is_supported() {
        return false;
    }
    if !send_type.is_requested() && !return_type.is_requested() {
        return false;
    }
    if return_type.is_requested() && !send_type.is_requested() {
        return false;
    }
    (!send_type.is_requested() || capabilities.send_text)
        && (!return_type.is_requested() || capabilities.return_text)
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
            drop(Box::from_raw(state.cast::<MacosServicesState>()));
        }
        let _: () = msg_send![super(this, class!(NSResponder)), dealloc];
    }
}

extern "C" fn dealloc_services_operation_responder(this: &Object, _: Sel) {
    // SAFETY: create_services_operation stores one Box pointer before returning the responder.
    unsafe {
        let state: *mut c_void = *this.get_ivar(SERVICES_OPERATION_STATE_IVAR);
        if !state.is_null() {
            drop(Box::from_raw(state.cast::<MacosServicesOperationState>()));
        }
        let _: () = msg_send![super(this, class!(NSResponder)), dealloc];
    }
}

extern "C" fn valid_requestor(this: &Object, _: Sel, send_type: id, return_type: id) -> id {
    let operation = catch_unwind(AssertUnwindSafe(|| {
        let send_type = unsafe { service_data_type(send_type) };
        let return_type = unsafe { service_data_type(return_type) };
        let state = (unsafe { services_state(this) })?;
        let status = state.endpoint.status();
        let accepted = accepts_service_request(status.capabilities, send_type, return_type);
        if !accepted || status.origin.is_none() {
            return None;
        }
        let operation = state.operation(return_type.is_requested())?;
        unsafe { create_services_operation(operation) }
    }))
    .ok()
    .flatten();
    if let Some(operation) = operation {
        return operation;
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

unsafe fn create_services_operation(state: MacosServicesOperationState) -> Option<id> {
    let responder_class = services_operation_responder_class().ok()?;
    let responder: id = unsafe { msg_send![responder_class, alloc] };
    let responder: id = unsafe { msg_send![responder, init] };
    if responder == nil {
        return None;
    }
    let state = Box::new(state);
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
        if !state.identity.claim_write() {
            return NO;
        }
        let status = state.endpoint.status();
        let Some(origin) = status.origin.filter(|_| status.capabilities.send_text) else {
            state.identity.release_return();
            return NO;
        };
        let Some(selection) = state.endpoint.selection(origin) else {
            state.identity.release_return();
            return NO;
        };
        let wrote = unsafe { write_service_text(pasteboard, &selection.plain_text) };
        state.identity.finish_send(origin, pasteboard, wrote);
        if wrote { YES } else { NO }
    }))
    .unwrap_or(NO)
}

extern "C" fn read_selection_from_pasteboard(this: &Object, _: Sel, pasteboard: id) -> BOOL {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(state) = (unsafe { services_operation_state(this) }) else {
            return NO;
        };
        let Some(origin) = state.identity.take_return_origin(pasteboard) else {
            return NO;
        };
        let Some(text) = (unsafe { read_service_text(pasteboard) }) else {
            return NO;
        };
        if state.endpoint.insert_text(origin, text) {
            YES
        } else {
            NO
        }
    }))
    .unwrap_or(NO)
}

unsafe fn services_state(this: &Object) -> Option<&MacosServicesState> {
    let state: *mut c_void = unsafe { *this.get_ivar(SERVICES_STATE_IVAR) };
    unsafe { state.cast::<MacosServicesState>().as_ref() }
}

unsafe fn services_operation_state(this: &Object) -> Option<&MacosServicesOperationState> {
    let state: *mut c_void = unsafe { *this.get_ivar(SERVICES_OPERATION_STATE_IVAR) };
    unsafe { state.cast::<MacosServicesOperationState>().as_ref() }
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
    let byte_len = bounded_nsstring_byte_len(character_len, byte_len)?;
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

fn bounded_nsstring_byte_len(character_len: NSUInteger, byte_len: NSUInteger) -> Option<usize> {
    let byte_len = usize::try_from(byte_len).ok()?;
    ((byte_len != 0 || character_len == 0) && byte_len <= MAX_PASTE_BYTES).then_some(byte_len)
}

fn decode_service_text_bytes(bytes: &[u8]) -> Option<String> {
    if bytes.len() > MAX_PASTE_BYTES || bytes.contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok().map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use cocoa::appkit::NSPasteboard;

    use super::*;
    use crate::domain::{PaneId, TabId, WorkspaceId};

    fn origin(generation: u64) -> NativeServiceOrigin {
        NativeServiceOrigin::new(
            WorkspaceId::new(1),
            TabId::new(2),
            PaneId::new(3),
            4,
            5,
            generation,
        )
    }

    fn operation_identity(
        request_id: u64,
        returns_text: bool,
        active_return_request: Rc<Cell<Option<u64>>>,
    ) -> ServiceOperationIdentity {
        ServiceOperationIdentity {
            request_id,
            returns_text,
            active_return_request,
            write_claimed: Cell::new(false),
            pasteboard: Cell::new(None),
            origin: Cell::new(None),
        }
    }

    #[test]
    fn service_request_requires_each_requested_capability() {
        assert!(accepts_service_request(
            NativeServiceCapabilities::new(true, false),
            ServiceDataType::String,
            ServiceDataType::Absent,
        ));
        assert!(!accepts_service_request(
            NativeServiceCapabilities::new(true, false),
            ServiceDataType::String,
            ServiceDataType::String,
        ));
    }

    #[test]
    fn string_return_requires_a_bound_send_and_terminal_input_focus_capability() {
        assert!(accepts_service_request(
            NativeServiceCapabilities::new(true, true),
            ServiceDataType::String,
            ServiceDataType::String,
        ));
        assert!(!accepts_service_request(
            NativeServiceCapabilities::new(true, true),
            ServiceDataType::Absent,
            ServiceDataType::String,
        ));
    }

    #[test]
    fn unsupported_service_types_are_never_accepted() {
        assert!(!accepts_service_request(
            NativeServiceCapabilities::new(true, true),
            ServiceDataType::Unsupported,
            ServiceDataType::Absent,
        ));
    }

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
    fn service_operation_keeps_the_validated_origin_through_send_and_return() {
        let operation = operation_identity(1, true, Rc::new(Cell::new(None)));
        let pasteboard = 0x1234usize as id;

        assert!(operation.claim_write());
        operation.finish_send(origin(6), pasteboard, true);

        assert_eq!(operation.take_return_origin(pasteboard), Some(origin(6)));
        assert_eq!(operation.take_return_origin(pasteboard), None);
    }

    #[test]
    fn service_operation_rejects_overlap_and_wrong_pasteboard_return() {
        let active = Rc::new(Cell::new(None));
        let operation = operation_identity(1, true, Rc::clone(&active));
        let overlap = operation_identity(2, true, active);
        let first_pasteboard = 0x1234usize as id;
        let other_pasteboard = 0x5678usize as id;
        assert!(operation.claim_write());
        operation.finish_send(origin(6), first_pasteboard, true);

        assert!(!overlap.claim_write());
        assert_eq!(overlap.take_return_origin(first_pasteboard), None);
        assert_eq!(operation.take_return_origin(other_pasteboard), None);
        assert_eq!(
            operation.take_return_origin(first_pasteboard),
            Some(origin(6))
        );
    }

    #[test]
    fn repeated_validation_produces_distinct_operation_identities() {
        let active = Rc::new(Cell::new(None));
        let transform = operation_identity(1, true, Rc::clone(&active));
        let send_only = operation_identity(2, false, active);

        assert_ne!(transform.request_id, send_only.request_id);
        assert!(transform.returns_text);
        assert!(!send_only.returns_text);
    }

    #[test]
    fn service_operation_write_is_one_shot() {
        let operation = operation_identity(1, false, Rc::new(Cell::new(None)));

        assert!(operation.claim_write());
        assert!(!operation.claim_write());
    }

    #[test]
    fn return_only_service_cannot_create_an_unbound_operation() {
        assert!(!accepts_service_request(
            NativeServiceCapabilities::new(true, true),
            ServiceDataType::Absent,
            ServiceDataType::String,
        ));
    }

    #[test]
    fn service_text_rejects_oversized_and_embedded_nul_payloads() {
        assert!(decode_service_text_bytes(&vec![b'x'; MAX_PASTE_BYTES + 1]).is_none());
        assert!(decode_service_text_bytes(b"before\0after").is_none());
        assert_eq!(
            decode_service_text_bytes("日本語".as_bytes()).as_deref(),
            Some("日本語")
        );
        assert_eq!(bounded_nsstring_byte_len(1, 0), None);
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
