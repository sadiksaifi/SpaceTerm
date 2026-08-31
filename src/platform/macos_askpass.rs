#![expect(
    dead_code,
    reason = "the native AskPass presenter lands before its SSH broker integration"
)]

use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::slice;

use block::ConcreteBlock;
use cocoa::base::{BOOL, YES, id, nil};
use cocoa::foundation::{NSAutoreleasePool, NSInteger, NSPoint, NSRect, NSSize, NSString};
use gpui::Window;
use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use zeroize::Zeroizing;

use super::macos_secure_input::{
    SecureInputSecretLease, SecureInputSecretOwnerId, acquire_secret_input,
};

const MAX_ASKPASS_PROMPT_BYTES: usize = 4 * 1024;
const MAX_ASKPASS_SECRET_BYTES: usize = 16 * 1024;
const NS_ALERT_FIRST_BUTTON_RETURN: NSInteger = 1_000;
const NS_ALERT_SECOND_BUTTON_RETURN: NSInteger = 1_001;
const NS_MODAL_RESPONSE_ABORT: NSInteger = -1_001;
const NS_UTF8_STRING_ENCODING: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AskPassPromptKind {
    Secret,
    Confirmation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum AskPassRequestError {
    #[error("the AskPass prompt is empty")]
    Empty,
    #[error("the AskPass prompt exceeds the application limit")]
    TooLong,
    #[error("the AskPass prompt contains a NUL character")]
    ContainsNul,
}

pub(crate) struct AskPassRequest {
    prompt: String,
    kind: AskPassPromptKind,
}

impl AskPassRequest {
    pub(crate) fn new(
        prompt: String,
        kind: AskPassPromptKind,
    ) -> Result<Self, AskPassRequestError> {
        if prompt.is_empty() {
            return Err(AskPassRequestError::Empty);
        }
        if prompt.len() > MAX_ASKPASS_PROMPT_BYTES {
            return Err(AskPassRequestError::TooLong);
        }
        if prompt.contains('\0') {
            return Err(AskPassRequestError::ContainsNul);
        }
        Ok(Self { prompt, kind })
    }

    pub(crate) fn prompt(&self) -> &str {
        &self.prompt
    }

    pub(crate) const fn kind(&self) -> AskPassPromptKind {
        self.kind
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AskPassResponseError {
    SecretTooLong,
    EncodingUnavailable,
}

pub(crate) struct AskPassSecret {
    bytes: Zeroizing<Vec<u8>>,
}

impl AskPassSecret {
    fn new(bytes: Vec<u8>) -> Result<Self, AskPassResponseError> {
        if bytes.len() > MAX_ASKPASS_SECRET_BYTES {
            return Err(AskPassResponseError::SecretTooLong);
        }
        Ok(Self {
            bytes: Zeroizing::new(bytes),
        })
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.bytes.as_slice()
    }
}

pub(crate) enum AskPassResult {
    Secret(AskPassSecret),
    Confirmation(bool),
    Cancelled,
    Failed(AskPassResponseError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum AskPassPresentationError {
    #[error("an AskPass sheet is already active")]
    Busy,
    #[error("AskPass presentation requires the AppKit main thread")]
    OffMainThread,
    #[error("the GPUI window did not expose an AppKit view")]
    NativeViewUnavailable,
    #[error("the GPUI window did not expose an AppKit window")]
    NativeWindowUnavailable,
    #[error("AppKit could not allocate the AskPass sheet")]
    AllocationFailed,
}

pub(crate) type AskPassCompletion = Box<dyn FnOnce(AskPassResult)>;

pub(crate) trait AskPassPresenter {
    fn present(
        &mut self,
        request: AskPassRequest,
        completion: AskPassCompletion,
    ) -> Result<(), AskPassPresentationError>;

    fn cancel_active(&mut self);
}

#[derive(Clone)]
struct AskPassCompletionOnce {
    callback: Rc<RefCell<Option<AskPassCompletion>>>,
}

impl AskPassCompletionOnce {
    fn new(callback: AskPassCompletion) -> Self {
        Self {
            callback: Rc::new(RefCell::new(Some(callback))),
        }
    }

    fn complete(&self, result: AskPassResult) -> bool {
        let callback = self.callback.borrow_mut().take();
        let Some(callback) = callback else {
            return false;
        };
        callback(result);
        true
    }
}

struct PendingAskPassCompletion {
    once: AskPassCompletionOnce,
}

impl PendingAskPassCompletion {
    fn new(callback: AskPassCompletion) -> Self {
        Self {
            once: AskPassCompletionOnce::new(callback),
        }
    }

    fn once(&self) -> AskPassCompletionOnce {
        self.once.clone()
    }

    fn complete(&mut self, result: AskPassResult) -> bool {
        self.once.complete(result)
    }
}

impl Drop for PendingAskPassCompletion {
    fn drop(&mut self) {
        self.once.complete(AskPassResult::Cancelled);
    }
}

#[derive(Clone, Default)]
struct AskPassPresentationLifecycle {
    state: Rc<RefCell<AskPassPresentationState>>,
}

#[derive(Default)]
struct AskPassPresentationState {
    next_generation: u64,
    active: Option<ActiveAskPassPresentation>,
}

#[derive(Clone, Copy)]
struct ActiveAskPassPresentation {
    generation: u64,
    parent_window: id,
    sheet_window: id,
}

impl AskPassPresentationLifecycle {
    fn begin(&self) -> Result<AskPassPresentationActivity, AskPassPresentationError> {
        let mut state = self.state.borrow_mut();
        if state.active.is_some() {
            return Err(AskPassPresentationError::Busy);
        }
        state.next_generation = state.next_generation.wrapping_add(1);
        let generation = state.next_generation;
        state.active = Some(ActiveAskPassPresentation {
            generation,
            parent_window: nil,
            sheet_window: nil,
        });
        Ok(AskPassPresentationActivity {
            lifecycle: self.clone(),
            generation,
            finished: false,
        })
    }

    fn bind_windows(&self, generation: u64, parent_window: id, sheet_window: id) {
        let mut state = self.state.borrow_mut();
        let Some(active) = state.active.as_mut() else {
            return;
        };
        if active.generation == generation {
            active.parent_window = parent_window;
            active.sheet_window = sheet_window;
        }
    }

    fn finish(&self, generation: u64) {
        let mut state = self.state.borrow_mut();
        if state
            .active
            .is_some_and(|active| active.generation == generation)
        {
            state.active = None;
        }
    }

    fn cancellation_target(&self) -> Option<(id, id)> {
        let active = self.state.borrow().active?;
        (active.parent_window != nil && active.sheet_window != nil)
            .then_some((active.parent_window, active.sheet_window))
    }

    #[cfg(test)]
    fn is_active(&self) -> bool {
        self.state.borrow().active.is_some()
    }
}

struct AskPassPresentationActivity {
    lifecycle: AskPassPresentationLifecycle,
    generation: u64,
    finished: bool,
}

impl AskPassPresentationActivity {
    fn bind_windows(&self, parent_window: id, sheet_window: id) {
        self.lifecycle
            .bind_windows(self.generation, parent_window, sheet_window);
    }

    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.lifecycle.finish(self.generation);
    }
}

impl Drop for AskPassPresentationActivity {
    fn drop(&mut self) {
        self.finish();
    }
}

pub(crate) struct MacosAskPassPresenter {
    parent_window: RetainedAppKitWindow,
    lifecycle: AskPassPresentationLifecycle,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl MacosAskPassPresenter {
    pub(crate) fn new(window: &Window) -> Result<Self, AskPassPresentationError> {
        Ok(Self {
            parent_window: RetainedAppKitWindow::new(window)?,
            lifecycle: AskPassPresentationLifecycle::default(),
            _not_send_or_sync: PhantomData,
        })
    }

    fn cancel_native_sheet(&self) {
        if !main_thread() {
            return;
        }
        let Some((parent_window, sheet_window)) = self.lifecycle.cancellation_target() else {
            return;
        };
        // SAFETY: The lifecycle holds these pointers only while `NativeAskPassSheet` retains the
        // alert and the presenter retains its parent. Both are confined to AppKit's main thread.
        unsafe {
            let _: () = msg_send![
                parent_window,
                endSheet: sheet_window
                returnCode: NS_MODAL_RESPONSE_ABORT
            ];
        }
    }
}

impl AskPassPresenter for MacosAskPassPresenter {
    fn present(
        &mut self,
        request: AskPassRequest,
        completion: AskPassCompletion,
    ) -> Result<(), AskPassPresentationError> {
        if !main_thread() {
            return Err(AskPassPresentationError::OffMainThread);
        }
        let activity = self.lifecycle.begin()?;

        // SAFETY: The main-thread check confines every object and callback to AppKit's thread.
        // `NativeAskPassSheet` owns explicit retains until the completion block runs or is dropped.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let result = (|| {
                let mut sheet = NativeAskPassSheet::new(request, activity, completion)?;
                let alert = sheet.alert;
                let sheet_window = sheet.alert_window();
                sheet
                    .activity
                    .bind_windows(self.parent_window.0, sheet_window);
                sheet.acquire_secure_input();
                let presentation = Rc::new(RefCell::new(Some(sheet)));
                let callback_presentation = presentation.clone();
                let block = ConcreteBlock::new(move |response: NSInteger| {
                    let Some(mut sheet) = callback_presentation.borrow_mut().take() else {
                        return;
                    };
                    let pool = NSAutoreleasePool::new(nil);
                    sheet.finish(response);
                    pool.drain();
                });
                let block = block.copy();
                let _: () = msg_send![
                    alert,
                    beginSheetModalForWindow: self.parent_window.0
                    completionHandler: block
                ];
                if let Some(sheet) = presentation.borrow().as_ref() {
                    sheet.focus_secret_field();
                }
                Ok(())
            })();
            pool.drain();
            result
        }
    }

    fn cancel_active(&mut self) {
        self.cancel_native_sheet();
    }
}

impl Drop for MacosAskPassPresenter {
    fn drop(&mut self) {
        self.cancel_native_sheet();
    }
}

struct RetainedAppKitWindow(id);

impl RetainedAppKitWindow {
    fn new(window: &Window) -> Result<Self, AskPassPresentationError> {
        if !main_thread() {
            return Err(AskPassPresentationError::OffMainThread);
        }
        let native_handle = HasWindowHandle::window_handle(window)
            .map_err(|_| AskPassPresentationError::NativeViewUnavailable)?;
        let RawWindowHandle::AppKit(native_handle) = native_handle.as_raw() else {
            return Err(AskPassPresentationError::NativeViewUnavailable);
        };
        let native_view = native_handle.ns_view.as_ptr().cast::<Object>();

        // SAFETY: GPUI supplies a live NSView on AppKit's main thread. The explicit retain keeps
        // the application-owned NSWindow valid for this presenter's lifetime.
        unsafe {
            let parent_window: id = msg_send![native_view, window];
            if parent_window == nil {
                return Err(AskPassPresentationError::NativeWindowUnavailable);
            }
            let parent_window: id = msg_send![parent_window, retain];
            Ok(Self(parent_window))
        }
    }
}

impl Drop for RetainedAppKitWindow {
    fn drop(&mut self) {
        // SAFETY: `new` owns one retain and the enclosing presenter is main-thread confined.
        unsafe {
            let _: () = msg_send![self.0, release];
        }
    }
}

struct NativeAskPassSheet {
    kind: AskPassPromptKind,
    alert: id,
    alert_window: id,
    secret_field: Option<id>,
    secure_input: Option<SecureInputSecretLease>,
    activity: AskPassPresentationActivity,
    completion: PendingAskPassCompletion,
    released: bool,
}

impl NativeAskPassSheet {
    unsafe fn new(
        request: AskPassRequest,
        activity: AskPassPresentationActivity,
        completion: AskPassCompletion,
    ) -> Result<Self, AskPassPresentationError> {
        // SAFETY: Caller establishes AppKit main-thread confinement and owns the returned retains.
        let alert: id = unsafe {
            let allocated: id = msg_send![class!(NSAlert), alloc];
            msg_send![allocated, init]
        };
        if alert == nil {
            return Err(AskPassPresentationError::AllocationFailed);
        }

        // SAFETY: NSAlert copies the strings and retains the accessory view configured below.
        unsafe {
            let title = NSString::alloc(nil)
                .init_str("SpaceTerm SSH Authentication")
                .autorelease();
            let prompt = NSString::alloc(nil)
                .init_str(request.prompt())
                .autorelease();
            let _: () = msg_send![alert, setAlertStyle: 1_usize];
            let _: () = msg_send![alert, setMessageText: title];
            let _: () = msg_send![alert, setInformativeText: prompt];
        }

        let secret_field = match request.kind() {
            AskPassPromptKind::Secret => {
                // SAFETY: NSSecureTextField's designated frame initializer returns an owned view.
                let field = unsafe { new_secure_text_field(alert)? };
                Some(field)
            }
            AskPassPromptKind::Confirmation => None,
        };

        // SAFETY: Button order defines the documented NSAlert return codes. AppKit assigns Return
        // to the first button; Escape is explicitly bound to the second safe/cancel response.
        unsafe {
            let (affirmative, negative) = match request.kind() {
                AskPassPromptKind::Secret => ("Continue", "Cancel"),
                AskPassPromptKind::Confirmation => ("Yes", "No"),
            };
            let affirmative = NSString::alloc(nil).init_str(affirmative).autorelease();
            let negative = NSString::alloc(nil).init_str(negative).autorelease();
            let _: id = msg_send![alert, addButtonWithTitle: affirmative];
            let negative_button: id = msg_send![alert, addButtonWithTitle: negative];
            let escape = NSString::alloc(nil).init_str("\u{1b}").autorelease();
            let _: () = msg_send![negative_button, setKeyEquivalent: escape];
        }

        // SAFETY: The retained alert owns its NSWindow for the alert lifetime.
        let alert_window: id = unsafe { msg_send![alert, window] };
        if alert_window == nil {
            // SAFETY: Neither owned object has been published. Removing the accessory balances
            // NSAlert's retain before the explicit allocation retains are released.
            unsafe {
                if let Some(field) = secret_field {
                    let _: () = msg_send![alert, setAccessoryView: nil];
                    let _: () = msg_send![field, release];
                }
                let _: () = msg_send![alert, release];
            }
            return Err(AskPassPresentationError::AllocationFailed);
        }

        Ok(Self {
            kind: request.kind(),
            alert,
            alert_window,
            secret_field,
            secure_input: None,
            activity,
            completion: PendingAskPassCompletion::new(completion),
            released: false,
        })
    }

    fn alert_window(&self) -> id {
        self.alert_window
    }

    fn acquire_secure_input(&mut self) {
        if self.secret_field.is_some() {
            self.secure_input = Some(acquire_secret_input(SecureInputSecretOwnerId::new()));
        }
    }

    fn focus_secret_field(&self) {
        let Some(field) = self.secret_field else {
            return;
        };
        let window = self.alert_window();
        if window == nil {
            return;
        }
        // SAFETY: Both objects remain retained by this active sheet on AppKit's main thread.
        unsafe {
            let _: () = msg_send![window, setInitialFirstResponder: field];
            let _: BOOL = msg_send![window, makeFirstResponder: field];
        }
    }

    fn finish(&mut self, response: NSInteger) {
        let result = self.result(response);
        self.clear_secret_field();
        self.secure_input.take();
        self.activity.finish();
        self.release_native_objects();
        self.completion.complete(result);
    }

    fn result(&self, response: NSInteger) -> AskPassResult {
        match (self.kind, response) {
            (AskPassPromptKind::Secret, NS_ALERT_FIRST_BUTTON_RETURN) => self
                .read_secret()
                .map_or_else(AskPassResult::Failed, AskPassResult::Secret),
            (AskPassPromptKind::Confirmation, NS_ALERT_FIRST_BUTTON_RETURN) => {
                AskPassResult::Confirmation(true)
            }
            (AskPassPromptKind::Confirmation, NS_ALERT_SECOND_BUTTON_RETURN) => {
                AskPassResult::Confirmation(false)
            }
            _ => AskPassResult::Cancelled,
        }
    }

    fn read_secret(&self) -> Result<AskPassSecret, AskPassResponseError> {
        let field = self
            .secret_field
            .ok_or(AskPassResponseError::EncodingUnavailable)?;
        // SAFETY: `field` is a retained NSSecureTextField and the returned NSString is live for
        // this synchronous copy. No Rust String or debug-formattable value is created.
        unsafe {
            let value: id = msg_send![field, stringValue];
            if value == nil {
                return Err(AskPassResponseError::EncodingUnavailable);
            }
            let length: usize =
                msg_send![value, lengthOfBytesUsingEncoding: NS_UTF8_STRING_ENCODING];
            if length > MAX_ASKPASS_SECRET_BYTES {
                return Err(AskPassResponseError::SecretTooLong);
            }
            if length == 0 {
                return AskPassSecret::new(Vec::new());
            }
            let bytes: *const u8 = msg_send![value, UTF8String];
            if bytes.is_null() {
                return Err(AskPassResponseError::EncodingUnavailable);
            }
            AskPassSecret::new(slice::from_raw_parts(bytes, length).to_vec())
        }
    }

    fn clear_secret_field(&self) {
        let Some(field) = self.secret_field else {
            return;
        };
        // SAFETY: The field is retained and main-thread confined. The explicit NSString ownership
        // avoids depending on an ambient autorelease pool during cancellation or block teardown.
        unsafe {
            let empty = NSString::alloc(nil).init_str("");
            let _: () = msg_send![field, setStringValue: empty];
            let _: () = msg_send![empty, release];
        }
    }

    fn release_native_objects(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        // SAFETY: This owner holds one retain for each object. Removing the accessory first drops
        // NSAlert's retain, then these explicit releases balance allocation ownership.
        unsafe {
            if let Some(field) = self.secret_field.take() {
                let _: () = msg_send![self.alert, setAccessoryView: nil];
                let _: () = msg_send![field, release];
            }
            let _: () = msg_send![self.alert, release];
        }
    }
}

impl Drop for NativeAskPassSheet {
    fn drop(&mut self) {
        self.clear_secret_field();
        self.secure_input.take();
        self.activity.finish();
        self.release_native_objects();
    }
}

unsafe fn new_secure_text_field(alert: id) -> Result<id, AskPassPresentationError> {
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(360.0, 24.0));
    // SAFETY: NSSecureTextField supports NSView's frame initializer and returns an owned object.
    let field: id = unsafe {
        let allocated: id = msg_send![class!(NSSecureTextField), alloc];
        msg_send![allocated, initWithFrame: frame]
    };
    if field == nil {
        // SAFETY: The caller owns `alert` and this failure path has not published it.
        unsafe {
            let _: () = msg_send![alert, release];
        }
        return Err(AskPassPresentationError::AllocationFailed);
    }

    // SAFETY: NSSecureTextField implements these NSTextField and NSAccessibility setters. The
    // protected-content flag prevents assistive APIs from exposing the entered secret value.
    unsafe {
        let accessibility_label = NSString::alloc(nil)
            .init_str("Secure SSH authentication response")
            .autorelease();
        let _: () = msg_send![field, setAccessibilityLabel: accessibility_label];
        let _: () = msg_send![field, setAccessibilityProtectedContent: YES];
        let _: () = msg_send![field, setEditable: YES];
        let _: () = msg_send![field, setSelectable: YES];
        let _: () = msg_send![alert, setAccessoryView: field];
        let window: id = msg_send![alert, window];
        if window != nil {
            let _: () = msg_send![window, setInitialFirstResponder: field];
        }
    }
    Ok(field)
}

fn main_thread() -> bool {
    // SAFETY: `NSThread.isMainThread` is a process query with no object lifetime transfer.
    unsafe {
        let is_main: BOOL = msg_send![class!(NSThread), isMainThread];
        is_main == YES
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::*;

    #[derive(Debug, Eq, PartialEq)]
    enum ObservedResult {
        Secret(Vec<u8>),
        Confirmation(bool),
        Cancelled,
        Failed(AskPassResponseError),
    }

    fn observe(result: AskPassResult) -> ObservedResult {
        match result {
            AskPassResult::Secret(secret) => ObservedResult::Secret(secret.as_bytes().to_vec()),
            AskPassResult::Confirmation(confirmed) => ObservedResult::Confirmation(confirmed),
            AskPassResult::Cancelled => ObservedResult::Cancelled,
            AskPassResult::Failed(error) => ObservedResult::Failed(error),
        }
    }

    struct FakePresentation {
        activity: AskPassPresentationActivity,
        completion: PendingAskPassCompletion,
    }

    #[derive(Default)]
    struct FakeAskPassPresenter {
        lifecycle: AskPassPresentationLifecycle,
        presentations: VecDeque<FakePresentation>,
    }

    impl FakeAskPassPresenter {
        fn respond(&mut self, result: AskPassResult) -> AskPassCompletionOnce {
            let mut presentation = self.presentations.pop_front().unwrap();
            let stale = presentation.completion.once();
            presentation.completion.complete(result);
            drop(presentation.activity);
            stale
        }
    }

    impl AskPassPresenter for FakeAskPassPresenter {
        fn present(
            &mut self,
            _request: AskPassRequest,
            completion: AskPassCompletion,
        ) -> Result<(), AskPassPresentationError> {
            let activity = self.lifecycle.begin()?;
            self.presentations.push_back(FakePresentation {
                activity,
                completion: PendingAskPassCompletion::new(completion),
            });
            Ok(())
        }

        fn cancel_active(&mut self) {
            self.presentations.pop_front();
        }
    }

    fn request(kind: AskPassPromptKind) -> AskPassRequest {
        AskPassRequest::new("Authentication required".to_owned(), kind).unwrap()
    }

    #[test]
    fn request_accepts_the_maximum_bounded_prompt() {
        let prompt = "a".repeat(MAX_ASKPASS_PROMPT_BYTES);

        let request = AskPassRequest::new(prompt, AskPassPromptKind::Secret).unwrap();

        assert_eq!(request.prompt().len(), MAX_ASKPASS_PROMPT_BYTES);
        assert_eq!(request.kind(), AskPassPromptKind::Secret);
    }

    #[test]
    fn request_rejects_empty_nul_and_oversized_prompts() {
        let empty = AskPassRequest::new(String::new(), AskPassPromptKind::Secret).err();
        let nul = AskPassRequest::new("bad\0prompt".to_owned(), AskPassPromptKind::Secret).err();
        let oversized = AskPassRequest::new(
            "a".repeat(MAX_ASKPASS_PROMPT_BYTES + 1),
            AskPassPromptKind::Confirmation,
        )
        .err();

        assert_eq!(empty, Some(AskPassRequestError::Empty));
        assert_eq!(nul, Some(AskPassRequestError::ContainsNul));
        assert_eq!(oversized, Some(AskPassRequestError::TooLong));
    }

    #[test]
    fn secret_result_is_bounded_and_exposes_bytes_without_debug_formatting() {
        let secret = AskPassSecret::new(b"correct horse".to_vec()).unwrap();
        let oversized = AskPassSecret::new(vec![b'x'; MAX_ASKPASS_SECRET_BYTES + 1]).err();

        assert_eq!(secret.as_bytes(), b"correct horse");
        assert_eq!(oversized, Some(AskPassResponseError::SecretTooLong));
    }

    #[test]
    fn fake_presenter_completes_each_request_exactly_once() {
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = observed.clone();
        let mut presenter = FakeAskPassPresenter::default();
        presenter
            .present(
                request(AskPassPromptKind::Secret),
                Box::new(move |result| callback_observed.borrow_mut().push(observe(result))),
            )
            .unwrap();

        let stale = presenter.respond(AskPassResult::Secret(
            AskPassSecret::new(b"one".to_vec()).unwrap(),
        ));
        assert!(!stale.complete(AskPassResult::Cancelled));

        assert_eq!(
            observed.borrow().as_slice(),
            [ObservedResult::Secret(b"one".to_vec())]
        );
    }

    #[test]
    fn fake_presenter_rejects_overlap_without_completing_the_rejected_request() {
        let rejected_completed = Rc::new(RefCell::new(false));
        let callback_completed = rejected_completed.clone();
        let mut presenter = FakeAskPassPresenter::default();
        presenter
            .present(request(AskPassPromptKind::Secret), Box::new(|_| {}))
            .unwrap();

        let result = presenter.present(
            request(AskPassPromptKind::Confirmation),
            Box::new(move |_| *callback_completed.borrow_mut() = true),
        );

        assert_eq!(result, Err(AskPassPresentationError::Busy));
        assert!(!*rejected_completed.borrow());
    }

    #[test]
    fn cancellation_completes_once_and_releases_the_active_lifecycle() {
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = observed.clone();
        let mut presenter = FakeAskPassPresenter::default();
        presenter
            .present(
                request(AskPassPromptKind::Secret),
                Box::new(move |result| callback_observed.borrow_mut().push(observe(result))),
            )
            .unwrap();

        presenter.cancel_active();

        assert_eq!(observed.borrow().as_slice(), [ObservedResult::Cancelled]);
        assert!(!presenter.lifecycle.is_active());
    }

    #[test]
    fn stale_completion_cannot_finish_a_sequential_request() {
        let observed = Rc::new(RefCell::new(Vec::new()));
        let mut presenter = FakeAskPassPresenter::default();
        let first_observed = observed.clone();
        presenter
            .present(
                request(AskPassPromptKind::Confirmation),
                Box::new(move |result| first_observed.borrow_mut().push(observe(result))),
            )
            .unwrap();
        let stale = presenter.respond(AskPassResult::Confirmation(true));
        let second_observed = observed.clone();
        presenter
            .present(
                request(AskPassPromptKind::Confirmation),
                Box::new(move |result| second_observed.borrow_mut().push(observe(result))),
            )
            .unwrap();

        assert!(!stale.complete(AskPassResult::Cancelled));
        assert!(presenter.lifecycle.is_active());
        presenter.respond(AskPassResult::Confirmation(false));

        assert_eq!(
            observed.borrow().as_slice(),
            [
                ObservedResult::Confirmation(true),
                ObservedResult::Confirmation(false)
            ]
        );
    }

    #[test]
    fn sequential_secret_requests_release_before_the_next_presentation() {
        let mut presenter = FakeAskPassPresenter::default();
        presenter
            .present(request(AskPassPromptKind::Secret), Box::new(|_| {}))
            .unwrap();
        presenter.respond(AskPassResult::Secret(
            AskPassSecret::new(b"first".to_vec()).unwrap(),
        ));

        let second = presenter.present(request(AskPassPromptKind::Secret), Box::new(|_| {}));

        assert_eq!(second, Ok(()));
    }
}
