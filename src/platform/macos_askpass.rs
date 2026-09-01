use std::borrow::Cow;
use std::cell::RefCell;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::rc::Rc;
use std::slice;

use block::ConcreteBlock;
use cocoa::appkit::NSApp;
use cocoa::base::{BOOL, NO, YES, id, nil};
use cocoa::foundation::{NSAutoreleasePool, NSInteger, NSPoint, NSRect, NSSize, NSString};
use gpui::Window;
use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
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
const NS_LAYOUT_ATTRIBUTE_CENTER_X: NSInteger = 9;
const NS_LAYOUT_RELATION_EQUAL: NSInteger = 0;
const NS_USER_INTERFACE_LAYOUT_DIRECTION_RIGHT_TO_LEFT: NSInteger = 1;
const ASKPASS_ACTION_TRAILING_INSET: f64 = 16.0;
const ASKPASS_ACTION_ALIGNMENT_TOLERANCE: f64 = 0.5;
const SECRET_FIELD_OBSERVER_CLASS: &str = "SpaceTermAskPassSecretFieldObserver";
const SECRET_FIELD_OBSERVER_BUTTON_IVAR: &str = "spaceTermAskPassAffirmativeButton";
const SHEET_RESPONSE_OWNER_CLASS: &str = "SpaceTermAskPassSheetResponseOwner";
const SHEET_RESPONSE_OWNER_STATE_IVAR: &str = "spaceTermAskPassSheetResponseState";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// UI semantics for one bounded AskPass prompt.
pub(crate) enum AskPassPromptKind {
    Secret,
    Confirmation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
/// Validation failure for untrusted prompt text received from the helper transport.
pub(crate) enum AskPassRequestError {
    #[error("the AskPass prompt is empty")]
    Empty,
    #[error("the AskPass prompt exceeds the application limit")]
    TooLong,
    #[error("the AskPass prompt contains an unsafe control or format character")]
    ContainsUnsafeCharacter,
}

/// Validated, bounded AskPass prompt and its presentation kind.
///
/// Prompt text is CRLF-normalized, permits LF line breaks, and must never be logged or persisted.
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
        let prompt = prompt.replace("\r\n", "\n");
        if prompt.chars().any(is_unsafe_prompt_character) {
            return Err(AskPassRequestError::ContainsUnsafeCharacter);
        }
        Ok(Self { prompt, kind })
    }

    pub(crate) fn prompt(&self) -> &str {
        &self.prompt
    }

    pub(crate) const fn kind(&self) -> AskPassPromptKind {
        self.kind
    }

    /// Reports whether this prompt must use the protected native secret-entry surface.
    pub(crate) fn requires_secure_input(&self) -> bool {
        self.presentation().kind.uses_secret_field()
    }

    /// Builds application-owned content for a non-secret SSH decision.
    pub(crate) fn confirmation_presentation(&self) -> Option<AskPassConfirmationPresentation> {
        let presentation = self.presentation();
        if presentation.kind.uses_secret_field() {
            return None;
        }
        let first_contact = presentation.kind == AskPassPresentationKind::FirstContact;
        Some(AskPassConfirmationPresentation {
            first_contact,
            title: presentation.title,
            message: if first_contact {
                "Verify the host key fingerprint before connecting."
            } else {
                "SSH requires confirmation before it can continue."
            },
            detail: presentation.informative_text.into_owned(),
            affirmative: presentation.affirmative,
            negative: presentation.negative,
        })
    }

    fn classification(&self) -> AskPassPromptClassification<'_> {
        classify_prompt(&self.prompt, self.kind)
    }

    fn presentation(&self) -> AskPassPresentation<'_> {
        match self.classification() {
            AskPassPromptClassification::FirstContact(first_contact) => {
                AskPassPresentation::first_contact(first_contact)
            }
            AskPassPromptClassification::Confirmation => AskPassPresentation::confirmation(self),
            AskPassPromptClassification::Password(password) => {
                AskPassPresentation::password(self, password)
            }
            AskPassPromptClassification::KeyPassphrase(key_passphrase) => {
                AskPassPresentation::key_passphrase(self, key_passphrase)
            }
            AskPassPromptClassification::Secret => AskPassPresentation::secret(self),
        }
    }
}

/// Owned, non-secret content rendered by SpaceTerm's shared GPUI Alert surface.
pub(crate) struct AskPassConfirmationPresentation {
    first_contact: bool,
    title: &'static str,
    message: &'static str,
    detail: String,
    affirmative: &'static str,
    negative: &'static str,
}

impl AskPassConfirmationPresentation {
    pub(crate) const fn is_first_contact(&self) -> bool {
        self.first_contact
    }

    pub(crate) const fn title(&self) -> &'static str {
        self.title
    }

    pub(crate) const fn message(&self) -> &'static str {
        self.message
    }

    pub(crate) fn detail(&self) -> &str {
        &self.detail
    }

    pub(crate) const fn affirmative(&self) -> &'static str {
        self.affirmative
    }

    pub(crate) const fn negative(&self) -> &'static str {
        self.negative
    }
}

fn is_unsafe_prompt_character(character: char) -> bool {
    (character != '\n' && character.is_control())
        || matches!(
            character,
            '\u{00ad}'
                | '\u{034f}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061c}'
                | '\u{06dd}'
                | '\u{070f}'
                | '\u{0890}'..='\u{0891}'
                | '\u{08e2}'
                | '\u{115f}'..='\u{1160}'
                | '\u{17b4}'..='\u{17b5}'
                | '\u{180b}'..='\u{180f}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{3164}'
                | '\u{fe00}'..='\u{fe0f}'
                | '\u{feff}'
                | '\u{ffa0}'
                | '\u{e0000}'..='\u{e007f}'
                | '\u{e0100}'..='\u{e01ef}'
        )
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum AskPassPresentationKind {
    FirstContact,
    Confirmation,
    Password,
    KeyPassphrase,
    Secret,
}

struct AskPassPresentation<'a> {
    kind: AskPassPresentationKind,
    title: &'static str,
    informative_text: Cow<'a, str>,
    affirmative: &'static str,
    negative: &'static str,
    field_label: Option<&'static str>,
}

impl<'a> AskPassPresentation<'a> {
    fn first_contact(prompt: FirstContactPrompt<'a>) -> Self {
        let host = match prompt.address {
            Some(address) => format!(
                "Host: {}\nAddress: {address}\n\n{} fingerprint:\n{}",
                prompt.host, prompt.key_type, prompt.fingerprint
            ),
            None => format!(
                "Host: {}\n\n{} fingerprint:\n{}",
                prompt.host, prompt.key_type, prompt.fingerprint
            ),
        };
        let mut informative_text = concat!(
            "SSH does not recognize this host key for this address. Verify the fingerprint with ",
            "the host owner.\n\n"
        )
        .to_owned();
        informative_text.push_str(&host);
        if !prompt.additional_details.is_empty() {
            informative_text.push_str("\n\n");
            informative_text.push_str(&prompt.additional_details.join("\n"));
        }
        informative_text.push_str(
            "\n\nIf you continue, SSH will attempt to remember this key for future connections.",
        );
        Self {
            kind: AskPassPresentationKind::FirstContact,
            title: "Verify SSH Host",
            informative_text: Cow::Owned(informative_text),
            affirmative: "Trust & Connect",
            negative: "Cancel",
            field_label: None,
        }
    }

    fn confirmation(request: &'a AskPassRequest) -> Self {
        Self {
            kind: AskPassPresentationKind::Confirmation,
            title: "SpaceTerm SSH Authentication",
            informative_text: Cow::Borrowed(request.prompt()),
            affirmative: "Yes",
            negative: "No",
            field_label: None,
        }
    }

    fn password(request: &'a AskPassRequest, prompt: PasswordPrompt<'a>) -> Self {
        let mut informative_text = format!("SSH requested:\n{}\n\n", request.prompt());
        if let (Some(account), Some(host)) = (prompt.account, prompt.host) {
            informative_text.push_str(&format!("Host: {host}\nAccount: {account}\n"));
        }
        informative_text.push_str(concat!(
            "Method: Password\n\n",
            "SpaceTerm sends this response to SSH and does not store it."
        ));
        Self {
            kind: AskPassPresentationKind::Password,
            title: "Sign In to Remote Host",
            informative_text: Cow::Owned(informative_text),
            affirmative: "Sign In",
            negative: "Cancel",
            field_label: Some("Password"),
        }
    }

    fn key_passphrase(request: &'a AskPassRequest, prompt: KeyPassphrasePrompt<'a>) -> Self {
        let mut informative_text = format!(
            "SSH requested:\n{}\n\nKey: {}",
            request.prompt(),
            prompt.filename
        );
        if let Some(location) = prompt.location {
            informative_text.push_str(&format!("\nLocation: {location}"));
        }
        informative_text
            .push_str("\n\nSpaceTerm sends this response to SSH and does not store it.");
        Self {
            kind: AskPassPresentationKind::KeyPassphrase,
            title: "SSH Key Passphrase",
            informative_text: Cow::Owned(informative_text),
            affirmative: "Submit & Connect",
            negative: "Cancel",
            field_label: Some("Key passphrase"),
        }
    }

    fn secret(request: &'a AskPassRequest) -> Self {
        Self {
            kind: AskPassPresentationKind::Secret,
            title: "SpaceTerm SSH Authentication",
            informative_text: Cow::Borrowed(request.prompt()),
            affirmative: "Continue",
            negative: "Cancel",
            field_label: Some("Secure SSH authentication response"),
        }
    }
}

impl AskPassPresentationKind {
    const fn uses_secret_field(self) -> bool {
        matches!(self, Self::Password | Self::KeyPassphrase | Self::Secret)
    }

    const fn requires_nonempty_secret(self) -> bool {
        matches!(self, Self::Password | Self::KeyPassphrase)
    }

    #[cfg(test)]
    const fn secret_submission_enabled(self, length: usize) -> bool {
        !self.requires_nonempty_secret() || length > 0
    }

    fn button_result(self, response: NSInteger) -> Option<AskPassResult> {
        match (self, response) {
            (Self::FirstContact, NS_ALERT_FIRST_BUTTON_RETURN) => Some(AskPassResult::Cancelled),
            (Self::FirstContact, NS_ALERT_SECOND_BUTTON_RETURN) => {
                Some(AskPassResult::Confirmation(true))
            }
            (Self::Confirmation, NS_ALERT_FIRST_BUTTON_RETURN) => {
                Some(AskPassResult::Confirmation(false))
            }
            (Self::Confirmation, NS_ALERT_SECOND_BUTTON_RETURN) => {
                Some(AskPassResult::Confirmation(true))
            }
            (
                Self::Password | Self::KeyPassphrase | Self::Secret,
                NS_ALERT_SECOND_BUTTON_RETURN,
            ) => Some(AskPassResult::Cancelled),
            (_, NS_MODAL_RESPONSE_ABORT) => Some(AskPassResult::Cancelled),
            _ => None,
        }
    }
}

enum AskPassPromptClassification<'a> {
    FirstContact(FirstContactPrompt<'a>),
    Confirmation,
    Password(PasswordPrompt<'a>),
    KeyPassphrase(KeyPassphrasePrompt<'a>),
    Secret,
}

struct FirstContactPrompt<'a> {
    host: &'a str,
    address: Option<&'a str>,
    key_type: &'a str,
    fingerprint: &'a str,
    additional_details: Vec<&'a str>,
}

struct PasswordPrompt<'a> {
    account: Option<&'a str>,
    host: Option<&'a str>,
}

struct KeyPassphrasePrompt<'a> {
    filename: &'a str,
    location: Option<&'a str>,
}

/// Recognizes Apple OpenSSH's locally generated first-contact grammar even though `sshconnect.c`
/// requests it through `RP_ECHO` and therefore supplies no confirmation hint to AskPass. The
/// anchored first line, one valid fingerprint, and terminal confirmation question are all required.
/// A keyboard-interactive server that exactly mimics this grammar can receive only the fixed `yes`
/// response; this path never reads a secret and only OpenSSH's local host-key path can update its
/// known-hosts files.
fn classify_prompt(prompt: &str, hint: AskPassPromptKind) -> AskPassPromptClassification<'_> {
    if let Some(first_contact) = parse_first_contact_prompt(prompt) {
        return AskPassPromptClassification::FirstContact(first_contact);
    }
    if looks_like_first_contact_confirmation(prompt) {
        return AskPassPromptClassification::Confirmation;
    }
    if hint == AskPassPromptKind::Secret {
        if let Some(key_passphrase) = parse_key_passphrase_prompt(prompt) {
            return AskPassPromptClassification::KeyPassphrase(key_passphrase);
        }
        if let Some(password) = parse_password_prompt(prompt) {
            return AskPassPromptClassification::Password(password);
        }
    }
    match hint {
        AskPassPromptKind::Confirmation => AskPassPromptClassification::Confirmation,
        AskPassPromptKind::Secret => AskPassPromptClassification::Secret,
    }
}

fn looks_like_first_contact_confirmation(prompt: &str) -> bool {
    prompt
        .lines()
        .next()
        .is_some_and(|line| line.starts_with("The authenticity of host '"))
        && prompt.lines().any(is_continue_connecting_question)
}

fn parse_first_contact_prompt(prompt: &str) -> Option<FirstContactPrompt<'_>> {
    let mut lines = prompt.lines();
    let first_line = lines.next()?;
    let host_and_address = first_line
        .strip_prefix("The authenticity of host '")?
        .strip_suffix("' can't be established.")?;
    let (host, address) = parse_host_and_address(host_and_address)?;

    let mut key_type = None;
    let mut fingerprint = None;
    let mut additional_details = Vec::new();
    let mut found_question = false;
    while let Some(line) = lines.next() {
        let line = line.trim_end();
        if is_continue_connecting_question(line) {
            if lines.any(|trailing| !trailing.trim().is_empty()) {
                return None;
            }
            found_question = true;
            break;
        }
        if let Some((parsed_key_type, parsed_fingerprint)) = parse_fingerprint_line(line) {
            if key_type.is_some() {
                return None;
            }
            key_type = Some(parsed_key_type);
            fingerprint = Some(parsed_fingerprint);
        } else if is_host_security_warning(line) {
            return None;
        } else if !line.is_empty() {
            additional_details.push(line);
        }
    }
    if !found_question {
        return None;
    }
    Some(FirstContactPrompt {
        host,
        address,
        key_type: key_type?,
        fingerprint: fingerprint?,
        additional_details,
    })
}

fn parse_host_and_address(value: &str) -> Option<(&str, Option<&str>)> {
    if let Some((host, address)) = value.rsplit_once(" (") {
        let address = address.strip_suffix(')')?;
        if !safe_first_contact_label(host) || !safe_first_contact_label(address) {
            return None;
        }
        return Some((host, Some(address)));
    }
    safe_first_contact_label(value).then_some((value, None))
}

fn safe_first_contact_label(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(is_unsafe_prompt_character)
}

fn is_host_security_warning(line: &str) -> bool {
    [
        "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!",
        "WARNING: REVOKED HOST KEY DETECTED!",
        "WARNING: POSSIBLE DNS SPOOFING DETECTED!",
    ]
    .iter()
    .any(|warning| line.contains(warning))
}

fn parse_fingerprint_line(line: &str) -> Option<(&str, &str)> {
    let (key_type, fingerprint) = line.split_once(" key fingerprint is")?;
    if key_type.is_empty()
        || !key_type
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@'))
    {
        return None;
    }
    let fingerprint = fingerprint
        .strip_prefix(": ")
        .or_else(|| fingerprint.strip_prefix(' '))?
        .trim_end_matches('.');
    let digest = fingerprint
        .strip_prefix("SHA256:")
        .or_else(|| fingerprint.strip_prefix("MD5:"));
    if digest.is_none_or(str::is_empty)
        || !fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'+' | b'/' | b'='))
    {
        return None;
    }
    Some((key_type, fingerprint))
}

fn is_continue_connecting_question(line: &str) -> bool {
    matches!(
        line.trim_end(),
        "Are you sure you want to continue connecting (yes/no/[fingerprint])?"
            | "Are you sure you want to continue connecting (yes/no)?"
    )
}

fn parse_password_prompt(prompt: &str) -> Option<PasswordPrompt<'_>> {
    if prompt.contains('\n') {
        return None;
    }
    let prompt = prompt.strip_suffix(' ').unwrap_or(prompt);
    if matches!(prompt, "Password:" | "password:") {
        return Some(PasswordPrompt {
            account: None,
            host: None,
        });
    }
    let identity = prompt.strip_suffix("'s password:")?;
    let (account, host) = identity.rsplit_once('@')?;
    if !safe_first_contact_label(account) || !safe_first_contact_label(host) {
        return None;
    }
    Some(PasswordPrompt {
        account: Some(account),
        host: Some(host),
    })
}

fn parse_key_passphrase_prompt(prompt: &str) -> Option<KeyPassphrasePrompt<'_>> {
    if prompt.contains('\n') {
        return None;
    }
    let key_path = prompt
        .strip_suffix(' ')
        .unwrap_or(prompt)
        .strip_prefix("Enter passphrase for key '")?
        .strip_suffix("':")?;
    if key_path.is_empty() {
        return None;
    }
    let path = Path::new(key_path);
    let filename = path.file_name()?.to_str()?;
    let location = path
        .parent()
        .and_then(Path::to_str)
        .filter(|location| !location.is_empty());
    Some(KeyPassphrasePrompt { filename, location })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Failure to capture a bounded response from the native control.
pub(crate) enum AskPassResponseError {
    SecretTooLong,
    EncodingUnavailable,
}

/// Non-clone, non-Debug owner of zeroized AskPass response bytes.
///
/// The bytes are borrowed only long enough to frame the broker response and are cleared on drop.
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

/// Exactly-once result of one native AskPass presentation.
pub(crate) enum AskPassResult {
    Secret(AskPassSecret),
    Confirmation(bool),
    Cancelled,
    Failed(AskPassResponseError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
/// Safe presentation failure with prompt and response content excluded.
pub(crate) enum AskPassPresentationError {
    #[error("an AskPass sheet is already active")]
    Busy,
    #[error("AskPass presentation requires the AppKit main thread")]
    OffMainThread,
    #[error("non-secret AskPass decisions must use the SpaceTerm confirmation surface")]
    NativeSecureInputOnly,
    #[error("SpaceTerm could not present the AskPass confirmation")]
    ApplicationConfirmationUnavailable,
    #[error("the GPUI window did not expose an AppKit view")]
    NativeViewUnavailable,
    #[error("the GPUI window did not expose an AppKit window")]
    NativeWindowUnavailable,
    #[error("AppKit could not allocate the AskPass sheet")]
    AllocationFailed,
}

/// One-shot completion that consumes the response owner.
pub(crate) type AskPassCompletion = Box<dyn FnOnce(AskPassResult)>;

/// Main-thread presenter for at most one active AskPass sheet.
///
/// Implementations must invoke completion exactly once. Cancellation or presenter teardown must
/// close the active sheet, release secure input, and complete with `Cancelled`.
pub(crate) trait AskPassPresenter {
    /// Presents a validated request without transferring secret ownership into retained UI state.
    fn present(
        &mut self,
        request: AskPassRequest,
        completion: AskPassCompletion,
    ) -> Result<(), AskPassPresentationError>;

    /// Cancels the active request, if any, without affecting a later presentation generation.
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

    #[cfg(test)]
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
    response_owner: id,
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
            response_owner: nil,
        });
        Ok(AskPassPresentationActivity {
            lifecycle: self.clone(),
            generation,
            finished: false,
        })
    }

    fn bind_response_owner(&self, generation: u64, response_owner: id) {
        let mut state = self.state.borrow_mut();
        let Some(active) = state.active.as_mut() else {
            return;
        };
        if active.generation == generation {
            active.response_owner = response_owner;
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

    fn take_cancellation_owner(&self) -> Option<id> {
        let mut state = self.state.borrow_mut();
        let active = state.active.as_mut()?;
        if active.response_owner == nil {
            return None;
        }
        let response_owner = active.response_owner;
        active.response_owner = nil;
        Some(response_owner)
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
    fn bind_response_owner(&self, response_owner: id) {
        self.lifecycle
            .bind_response_owner(self.generation, response_owner);
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

/// AppKit sheet presenter confined to the main thread.
///
/// The presenter retains its parent window, explicitly owns native sheet objects through the
/// completion callback, and holds secure input only while a secret field is active. It is neither
/// `Send` nor `Sync`, and all unsafe Objective-C messages remain inside that ownership boundary.
pub(crate) struct MacosAskPassPresenter {
    parent_window: RetainedAppKitWindow,
    lifecycle: AskPassPresentationLifecycle,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl MacosAskPassPresenter {
    /// Captures and retains the AppKit parent for subsequent attempt-local sheets.
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
        let Some(response_owner) = self.lifecycle.take_cancellation_owner() else {
            return;
        };
        // SAFETY: The lifecycle holds this pointer only while `NativeAskPassSheet` retains the
        // response owner. Its abort selector shares the button actions' exactly-once end guard.
        unsafe {
            let response_owner: id = msg_send![response_owner, retain];
            let _: () = msg_send![response_owner, spaceTermAskPassAbort];
            let _: () = msg_send![response_owner, release];
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
        if !request.requires_secure_input() {
            return Err(AskPassPresentationError::NativeSecureInputOnly);
        }
        let activity = self.lifecycle.begin()?;

        // SAFETY: The main-thread check confines every object and callback to AppKit's thread.
        // `NativeAskPassSheet` owns explicit retains until the completion block runs or is dropped.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let result = (|| {
                let mut sheet =
                    NativeAskPassSheet::new(request, activity, completion, self.parent_window.0)?;
                let sheet_window = sheet.alert_window();
                sheet.activity.bind_response_owner(sheet.response_owner());
                sheet.acquire_secure_input();
                let presentation = Rc::new(RefCell::new(Some(sheet)));
                let callback_presentation = presentation.clone();
                let block = ConcreteBlock::new(move |response: NSInteger| {
                    let pool = NSAutoreleasePool::new(nil);
                    contain_appkit_completion(|| {
                        let Some(mut sheet) = callback_presentation.borrow_mut().take() else {
                            return;
                        };
                        sheet.finish(response);
                    });
                    pool.drain();
                });
                let block = block.copy();
                activate_for_sheet(self.parent_window.0);
                let _: () = msg_send![
                    self.parent_window.0,
                    beginSheet: sheet_window
                    completionHandler: block
                ];
                if let Some(sheet) = presentation.borrow().as_ref() {
                    sheet.focus_initial_control();
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

fn contain_appkit_completion(completion: impl FnOnce()) {
    let _ = catch_unwind(AssertUnwindSafe(completion));
}

fn activate_for_sheet(parent_window: id) {
    // SAFETY: Presentation is main-thread confined. `parent_window` is retained by the presenter,
    // and activation/key ordering do not transfer ownership.
    unsafe {
        let application = NSApp();
        if application != nil {
            let _: () = msg_send![application, activateIgnoringOtherApps: YES];
        }
        let _: () = msg_send![parent_window, makeKeyAndOrderFront: nil];
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
    presentation_kind: AskPassPresentationKind,
    alert: id,
    alert_window: id,
    secret_field: Option<id>,
    secret_field_observer: Option<id>,
    first_button: id,
    second_button: id,
    response_owner: Option<id>,
    initial_focus: Option<id>,
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
        parent_window: id,
    ) -> Result<Self, AskPassPresentationError> {
        let presentation = request.presentation();
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
                .init_str(presentation.title)
                .autorelease();
            let prompt = NSString::alloc(nil)
                .init_str(presentation.informative_text.as_ref())
                .autorelease();
            let _: () = msg_send![alert, setAlertStyle: 1_usize];
            let _: () = msg_send![alert, setMessageText: title];
            let _: () = msg_send![alert, setInformativeText: prompt];
        }

        let secret_field = if presentation.kind.uses_secret_field() {
            let Some(field_label) = presentation.field_label else {
                // SAFETY: The unpublished alert is still owned by this failure path.
                unsafe {
                    let _: () = msg_send![alert, release];
                }
                return Err(AskPassPresentationError::AllocationFailed);
            };
            // SAFETY: NSSecureTextField's designated frame initializer returns an owned view.
            Some(unsafe { new_secure_text_field(alert, field_label)? })
        } else {
            None
        };

        // SAFETY: Non-secret decisions place the safe response first so Return cannot trust or
        // approve. Secret prompts retain submit-first ordering and require explicit nonempty values
        // where their strict presentation model requires one.
        let (first_button, second_button, safe_button, affirmative_button): (id, id, id, id) = unsafe {
            let (first, second, safe_is_first) = if presentation.kind.uses_secret_field() {
                (presentation.affirmative, presentation.negative, false)
            } else {
                (presentation.negative, presentation.affirmative, true)
            };
            let first = NSString::alloc(nil).init_str(first).autorelease();
            let second = NSString::alloc(nil).init_str(second).autorelease();
            let first_button: id = msg_send![alert, addButtonWithTitle: first];
            let second_button: id = msg_send![alert, addButtonWithTitle: second];
            let safe_button = if safe_is_first {
                first_button
            } else {
                second_button
            };
            let escape = NSString::alloc(nil).init_str("\u{1b}").autorelease();
            let _: () = msg_send![safe_button, setKeyEquivalent: escape];
            let affirmative_button = if safe_is_first {
                second_button
            } else {
                first_button
            };
            (first_button, second_button, safe_button, affirmative_button)
        };

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
        if !presentation.kind.uses_secret_field() {
            // SAFETY: The safe first button is owned by this alert. Assigning its cell as the
            // default makes Return safe even after Escape is installed as its key equivalent.
            unsafe {
                let safe_cell: id = msg_send![safe_button, cell];
                let _: () = msg_send![alert_window, setDefaultButtonCell: safe_cell];
            }
        }
        // SAFETY: `layout` finalizes NSAlert's public view hierarchy before SpaceTerm replaces the
        // button targets and presents the retained window directly through NSWindow.
        unsafe {
            let _: () = msg_send![alert, layout];
            let _action_group_aligned =
                align_alert_action_group_trailing(alert_window, first_button, second_button);
        }
        let Some(response_owner) = (unsafe {
            new_sheet_response_owner(parent_window, alert_window, first_button, second_button)
        }) else {
            unsafe {
                if let Some(field) = secret_field {
                    let _: () = msg_send![alert, setAccessoryView: nil];
                    let _: () = msg_send![field, release];
                }
                let _: () = msg_send![alert, release];
            }
            return Err(AskPassPresentationError::AllocationFailed);
        };
        let secret_field_observer = if presentation.kind.requires_nonempty_secret() {
            let Some(field) = secret_field else {
                unsafe {
                    let _: () = msg_send![first_button, setTarget: nil];
                    let _: () = msg_send![second_button, setTarget: nil];
                    let _: () = msg_send![response_owner, release];
                    let _: () = msg_send![alert, release];
                }
                return Err(AskPassPresentationError::AllocationFailed);
            };
            let Some(observer) =
                (unsafe { new_nonempty_secret_observer(field, affirmative_button) })
            else {
                // SAFETY: Neither owned object has been published. Removing the accessory balances
                // NSAlert's retain before the explicit allocation retains are released.
                unsafe {
                    let _: () = msg_send![first_button, setTarget: nil];
                    let _: () = msg_send![second_button, setTarget: nil];
                    let _: () = msg_send![response_owner, release];
                    let _: () = msg_send![alert, setAccessoryView: nil];
                    let _: () = msg_send![field, release];
                    let _: () = msg_send![alert, release];
                }
                return Err(AskPassPresentationError::AllocationFailed);
            };
            Some(observer)
        } else {
            None
        };
        let initial_focus = secret_field.or(Some(safe_button));
        if let Some(control) = initial_focus {
            // SAFETY: The alert window retains its control hierarchy. Establishing the initial
            // responder before presentation lets AppKit route Return and Escape from first paint.
            unsafe {
                let _: () = msg_send![alert_window, setInitialFirstResponder: control];
            }
        }

        Ok(Self {
            presentation_kind: presentation.kind,
            alert,
            alert_window,
            secret_field,
            secret_field_observer,
            first_button,
            second_button,
            response_owner: Some(response_owner),
            initial_focus,
            secure_input: None,
            activity,
            completion: PendingAskPassCompletion::new(completion),
            released: false,
        })
    }

    fn alert_window(&self) -> id {
        self.alert_window
    }

    fn response_owner(&self) -> id {
        self.response_owner.unwrap_or(nil)
    }

    fn acquire_secure_input(&mut self) {
        if self.secret_field.is_some() {
            self.secure_input = Some(acquire_secret_input(SecureInputSecretOwnerId::new()));
        }
    }

    fn focus_initial_control(&self) {
        let Some(control) = self.initial_focus else {
            return;
        };
        let window = self.alert_window();
        if window == nil {
            return;
        }
        // SAFETY: Both objects remain retained by this active sheet on AppKit's main thread.
        unsafe {
            let sheet_parent: id = msg_send![window, sheetParent];
            let attached_sheet: id = if sheet_parent == nil {
                nil
            } else {
                msg_send![sheet_parent, attachedSheet]
            };
            if !is_current_attached_sheet(sheet_parent, attached_sheet, window) {
                return;
            }
            let _: () = msg_send![window, setInitialFirstResponder: control];
            let _: BOOL = msg_send![window, makeFirstResponder: control];
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
        if self.presentation_kind.uses_secret_field() && response == NS_ALERT_FIRST_BUTTON_RETURN {
            return self
                .read_secret()
                .map_or_else(AskPassResult::Failed, AskPassResult::Secret);
        }
        self.presentation_kind
            .button_result(response)
            .unwrap_or(AskPassResult::Cancelled)
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
            if let Some(response_owner) = self.response_owner.take() {
                let _: () = msg_send![self.first_button, setTarget: nil];
                let _: () = msg_send![self.second_button, setTarget: nil];
                let _: () = msg_send![response_owner, release];
            }
            if let Some(observer) = self.secret_field_observer.take() {
                if let Some(field) = self.secret_field {
                    let _: () = msg_send![field, setDelegate: nil];
                }
                let _: () = msg_send![observer, release];
            }
            if let Some(field) = self.secret_field.take() {
                let _: () = msg_send![self.alert, setAccessoryView: nil];
                let _: () = msg_send![field, release];
            }
            let _: () = msg_send![self.alert, release];
        }
    }
}

fn is_current_attached_sheet(sheet_parent: id, attached_sheet: id, sheet_window: id) -> bool {
    sheet_parent != nil && attached_sheet == sheet_window
}

impl Drop for NativeAskPassSheet {
    fn drop(&mut self) {
        self.clear_secret_field();
        self.secure_input.take();
        self.activity.finish();
        self.release_native_objects();
    }
}

#[derive(Clone, Copy)]
struct SheetResponseRequest {
    parent_window: id,
    sheet_window: id,
    response: NSInteger,
}

struct SheetResponseState {
    parent_window: id,
    sheet_window: id,
    ended: bool,
}

impl SheetResponseState {
    fn claim(&mut self, response: NSInteger) -> Option<SheetResponseRequest> {
        if self.ended || self.parent_window == nil || self.sheet_window == nil {
            return None;
        }
        self.ended = true;
        Some(SheetResponseRequest {
            parent_window: self.parent_window,
            sheet_window: self.sheet_window,
            response,
        })
    }
}

fn sheet_response_owner_class() -> Option<&'static Class> {
    if let Some(class) = Class::get(SHEET_RESPONSE_OWNER_CLASS) {
        return Some(class);
    }
    let mut declaration = ClassDecl::new(SHEET_RESPONSE_OWNER_CLASS, class!(NSObject))?;
    declaration.add_ivar::<*mut c_void>(SHEET_RESPONSE_OWNER_STATE_IVAR);
    // SAFETY: These selectors are SpaceTerm-owned NSButton actions and NSObject teardown. Their
    // function signatures match the documented Objective-C target/action and dealloc ABIs.
    unsafe {
        declaration.add_method(
            sel!(spaceTermAskPassPrimary:),
            sheet_response_primary as extern "C" fn(&Object, Sel, id),
        );
        declaration.add_method(
            sel!(spaceTermAskPassSafe:),
            sheet_response_safe as extern "C" fn(&Object, Sel, id),
        );
        declaration.add_method(
            sel!(spaceTermAskPassAbort),
            sheet_response_abort as extern "C" fn(&Object, Sel),
        );
        declaration.add_method(
            sel!(dealloc),
            dealloc_sheet_response_owner as extern "C" fn(&Object, Sel),
        );
    }
    Some(declaration.register())
}

unsafe fn new_sheet_response_owner(
    parent_window: id,
    sheet_window: id,
    first_button: id,
    second_button: id,
) -> Option<id> {
    if parent_window == nil || sheet_window == nil || first_button == nil || second_button == nil {
        return None;
    }
    let class = sheet_response_owner_class()?;
    // SAFETY: The registered NSObject subclass owns one boxed response state until dealloc.
    let owner: id = unsafe {
        let allocated: id = msg_send![class, alloc];
        msg_send![allocated, init]
    };
    if owner == nil {
        return None;
    }
    let state = Box::into_raw(Box::new(SheetResponseState {
        parent_window,
        sheet_window,
        ended: false,
    }))
    .cast::<c_void>();
    // SAFETY: NSButton target/action is public AppKit API. The sheet retains `owner` explicitly,
    // because controls do not retain their targets, and clears both targets before releasing it.
    unsafe {
        (*owner).set_ivar(SHEET_RESPONSE_OWNER_STATE_IVAR, state);
        let primary_action = sel!(spaceTermAskPassPrimary:);
        let safe_action = sel!(spaceTermAskPassSafe:);
        let _: () = msg_send![first_button, setTarget: owner];
        let _: () = msg_send![first_button, setAction: primary_action];
        let _: () = msg_send![second_button, setTarget: owner];
        let _: () = msg_send![second_button, setAction: safe_action];
    }
    Some(owner)
}

extern "C" fn sheet_response_primary(this: &Object, _: Sel, _: id) {
    contain_appkit_completion(|| unsafe {
        end_sheet_once(this, NS_ALERT_FIRST_BUTTON_RETURN);
    });
}

extern "C" fn sheet_response_safe(this: &Object, _: Sel, _: id) {
    contain_appkit_completion(|| unsafe {
        end_sheet_once(this, NS_ALERT_SECOND_BUTTON_RETURN);
    });
}

extern "C" fn sheet_response_abort(this: &Object, _: Sel) {
    contain_appkit_completion(|| unsafe {
        end_sheet_once(this, NS_MODAL_RESPONSE_ABORT);
    });
}

unsafe fn end_sheet_once(this: &Object, response: NSInteger) {
    // SAFETY: The owner stores exactly one live state pointer until dealloc. All actions and
    // external cancellation are main-thread confined, so claiming serializes every response.
    let state: *mut c_void = unsafe { *this.get_ivar(SHEET_RESPONSE_OWNER_STATE_IVAR) };
    if state.is_null() {
        return;
    }
    // SAFETY: The ivar was created from `Box<SheetResponseState>` and remains owned by this object.
    let Some(request) = (unsafe { state.cast::<SheetResponseState>().as_mut() })
        .and_then(|state| state.claim(response))
    else {
        return;
    };
    let owner = std::ptr::from_ref(this).cast_mut();
    // SAFETY: `endSheet:returnCode:` may synchronously run completion and release the sheet's
    // ownership. This temporary retain keeps the action receiver alive until the selector returns.
    unsafe {
        let retained_owner: id = msg_send![owner, retain];
        let _: () = msg_send![
            request.parent_window,
            endSheet: request.sheet_window
            returnCode: request.response
        ];
        let _: () = msg_send![retained_owner, release];
    }
}

extern "C" fn dealloc_sheet_response_owner(this: &Object, _: Sel) {
    // SAFETY: Construction stores exactly one boxed state before publishing the owner. NSObject
    // calls dealloc once after the sheet clears button targets and releases its explicit retain.
    unsafe {
        let state: *mut c_void = *this.get_ivar(SHEET_RESPONSE_OWNER_STATE_IVAR);
        if !state.is_null() {
            drop(Box::from_raw(state.cast::<SheetResponseState>()));
        }
        let _: () = msg_send![super(this, class!(NSObject)), dealloc];
    }
}

unsafe fn new_secure_text_field(
    alert: id,
    accessibility_label: &str,
) -> Result<id, AskPassPresentationError> {
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
            .init_str(accessibility_label)
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

fn action_group_is_trailing(
    group_frame: NSRect,
    content_bounds: NSRect,
    right_to_left: bool,
) -> bool {
    let (actual, expected) = if right_to_left {
        (
            group_frame.origin.x,
            content_bounds.origin.x + ASKPASS_ACTION_TRAILING_INSET,
        )
    } else {
        (
            group_frame.origin.x + group_frame.size.width,
            content_bounds.origin.x + content_bounds.size.width - ASKPASS_ACTION_TRAILING_INSET,
        )
    };
    (actual - expected).abs() <= ASKPASS_ACTION_ALIGNMENT_TOLERANCE
}

fn has_exact_center_constraint_properties(
    is_active: bool,
    relation: NSInteger,
    multiplier: f64,
    constant: f64,
) -> bool {
    is_active
        && relation == NS_LAYOUT_RELATION_EQUAL
        && (multiplier - 1.0).abs() <= f64::EPSILON
        && constant.abs() <= f64::EPSILON
}

unsafe fn align_alert_action_group_trailing(
    alert_window: id,
    first_button: id,
    second_button: id,
) -> bool {
    if alert_window == nil || first_button == nil || second_button == nil {
        return false;
    }
    // SAFETY: NSAlert owns both buttons and its content view for the alert lifetime. This uses only
    // public NSView and NSLayoutAnchor selectors after the alert has completed its initial layout.
    unsafe {
        let action_group: id = msg_send![first_button, superview];
        let second_button_group: id = msg_send![second_button, superview];
        let content_view: id = msg_send![alert_window, contentView];
        if action_group == nil || action_group != second_button_group || content_view == nil {
            return false;
        }
        let action_group_parent: id = msg_send![action_group, superview];
        if action_group_parent != content_view {
            return false;
        }
        let layout_direction: NSInteger = msg_send![content_view, userInterfaceLayoutDirection];
        let right_to_left = layout_direction == NS_USER_INTERFACE_LAYOUT_DIRECTION_RIGHT_TO_LEFT;

        let group_frame: NSRect = msg_send![action_group, frame];
        let content_bounds: NSRect = msg_send![content_view, bounds];
        if action_group_is_trailing(group_frame, content_bounds, right_to_left) {
            return true;
        }

        let constraints: id = msg_send![content_view, constraints];
        if constraints == nil {
            return false;
        }
        let count: usize = msg_send![constraints, count];
        let mut center_constraint = None;
        for index in 0..count {
            let constraint: id = msg_send![constraints, objectAtIndex: index];
            let is_active: BOOL = msg_send![constraint, isActive];
            let first_item: id = msg_send![constraint, firstItem];
            let second_item: id = msg_send![constraint, secondItem];
            let first_attribute: NSInteger = msg_send![constraint, firstAttribute];
            let second_attribute: NSInteger = msg_send![constraint, secondAttribute];
            let relation: NSInteger = msg_send![constraint, relation];
            let multiplier: f64 = msg_send![constraint, multiplier];
            let constant: f64 = msg_send![constraint, constant];
            let centers_group = first_item == action_group
                && second_item == content_view
                && first_attribute == NS_LAYOUT_ATTRIBUTE_CENTER_X
                && second_attribute == NS_LAYOUT_ATTRIBUTE_CENTER_X;
            let centers_group_reversed = first_item == content_view
                && second_item == action_group
                && first_attribute == NS_LAYOUT_ATTRIBUTE_CENTER_X
                && second_attribute == NS_LAYOUT_ATTRIBUTE_CENTER_X;
            let exact_active_center = (centers_group || centers_group_reversed)
                && has_exact_center_constraint_properties(
                    is_active == YES,
                    relation,
                    multiplier,
                    constant,
                );
            if exact_active_center {
                if center_constraint.is_some() {
                    return false;
                }
                center_constraint = Some(constraint);
            }
        }
        let Some(center_constraint) = center_constraint else {
            return false;
        };

        let action_trailing: id = msg_send![action_group, trailingAnchor];
        let content_trailing: id = msg_send![content_view, trailingAnchor];
        let trailing_constraint: id = msg_send![
            action_trailing,
            constraintEqualToAnchor: content_trailing
            constant: -ASKPASS_ACTION_TRAILING_INSET
        ];
        if trailing_constraint == nil {
            return false;
        }
        let _: () = msg_send![center_constraint, setActive: NO];
        let _: () = msg_send![trailing_constraint, setActive: YES];
        let _: () = msg_send![content_view, layoutSubtreeIfNeeded];

        let group_frame: NSRect = msg_send![action_group, frame];
        let content_bounds: NSRect = msg_send![content_view, bounds];
        if action_group_is_trailing(group_frame, content_bounds, right_to_left) {
            return true;
        }
        let _: () = msg_send![trailing_constraint, setActive: NO];
        let _: () = msg_send![center_constraint, setActive: YES];
        let _: () = msg_send![content_view, layoutSubtreeIfNeeded];
        false
    }
}

fn secret_field_observer_class() -> Option<&'static Class> {
    if let Some(class) = Class::get(SECRET_FIELD_OBSERVER_CLASS) {
        return Some(class);
    }
    let mut declaration = ClassDecl::new(SECRET_FIELD_OBSERVER_CLASS, class!(NSObject))?;
    declaration.add_ivar::<*mut c_void>(SECRET_FIELD_OBSERVER_BUTTON_IVAR);
    // SAFETY: The selector uses NSControlTextEditingDelegate's documented notification ABI.
    unsafe {
        declaration.add_method(
            sel!(controlTextDidChange:),
            secret_field_did_change as extern "C" fn(&Object, Sel, id),
        );
    }
    Some(declaration.register())
}

unsafe fn new_nonempty_secret_observer(field: id, affirmative_button: id) -> Option<id> {
    let observer_class = secret_field_observer_class()?;
    // SAFETY: The registered class is an NSObject subclass and `init` returns an owned object.
    let observer: id = unsafe {
        let allocated: id = msg_send![observer_class, alloc];
        msg_send![allocated, init]
    };
    if observer == nil {
        return None;
    }
    // SAFETY: The observer is main-thread confined and retained by NativeAskPassSheet. The alert
    // retains its button, and the delegate is cleared before either explicit owner is released.
    unsafe {
        (*observer).set_ivar(
            SECRET_FIELD_OBSERVER_BUTTON_IVAR,
            affirmative_button.cast::<c_void>(),
        );
        let _: () = msg_send![affirmative_button, setEnabled: NO];
        let _: () = msg_send![field, setDelegate: observer];
    }
    Some(observer)
}

extern "C" fn secret_field_did_change(this: &Object, _: Sel, notification: id) {
    // SAFETY: The delegate callback runs synchronously on AppKit's main thread while the sheet
    // retains the field and button. It reads only the response length, never its bytes.
    unsafe {
        let button: *mut c_void = *this.get_ivar::<*mut c_void>(SECRET_FIELD_OBSERVER_BUTTON_IVAR);
        if button.is_null() || notification == nil {
            return;
        }
        let field: id = msg_send![notification, object];
        if field == nil {
            return;
        }
        let value: id = msg_send![field, stringValue];
        let length: usize = if value == nil {
            0
        } else {
            msg_send![value, length]
        };
        let enabled = if length > 0 { YES } else { NO };
        let button = button.cast::<Object>();
        let _: () = msg_send![button, setEnabled: enabled];
    }
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
        let prompt = "a\n".repeat(MAX_ASKPASS_PROMPT_BYTES / 2);

        let request = AskPassRequest::new(prompt, AskPassPromptKind::Secret).unwrap();

        assert_eq!(request.prompt().len(), MAX_ASKPASS_PROMPT_BYTES);
        assert_eq!(request.kind(), AskPassPromptKind::Secret);
    }

    #[test]
    fn request_normalizes_multiline_unknown_host_confirmation() {
        let prompt = concat!(
            "The authenticity of host 'example.test (203.0.113.10)' can't be established.\r\n",
            "ED25519 key fingerprint is SHA256:example.\r\n",
            "This key is not known by any other names.\r\n",
            "Are you sure you want to continue connecting (yes/no/[fingerprint])? "
        );

        let request =
            AskPassRequest::new(prompt.to_owned(), AskPassPromptKind::Confirmation).unwrap();

        assert_eq!(request.prompt(), prompt.replace("\r\n", "\n"));
        assert_eq!(request.kind(), AskPassPromptKind::Confirmation);
    }

    #[test]
    fn classification_recognizes_macos_first_contact_without_confirmation_hint() {
        let prompt = concat!(
            "The authenticity of host 'homelab (100.64.0.10)' can't be established.\n",
            "ED25519 key fingerprint is SHA256:AbCdEf0123456789.\n",
            "This key is not known by any other names.\n",
            "Are you sure you want to continue connecting (yes/no/[fingerprint])? "
        );
        let request = AskPassRequest::new(prompt.to_owned(), AskPassPromptKind::Secret).unwrap();

        let AskPassPromptClassification::FirstContact(classification) = request.classification()
        else {
            panic!("expected a first-contact classification");
        };

        assert_eq!(classification.host, "homelab");
        assert_eq!(classification.address, Some("100.64.0.10"));
        assert_eq!(classification.key_type, "ED25519");
        assert_eq!(classification.fingerprint, "SHA256:AbCdEf0123456789");
        let presentation = request.presentation();
        assert!(matches!(
            presentation.kind,
            AskPassPresentationKind::FirstContact
        ));
        assert_eq!(presentation.title, "Verify SSH Host");
        assert_eq!(
            presentation.informative_text,
            concat!(
                "SSH does not recognize this host key for this address. Verify the fingerprint ",
                "with the host owner.\n\n",
                "Host: homelab\nAddress: 100.64.0.10\n\n",
                "ED25519 fingerprint:\nSHA256:AbCdEf0123456789\n\n",
                "This key is not known by any other names.\n\n",
                "If you continue, SSH will attempt to remember this key for future connections."
            )
        );
        assert_eq!(presentation.affirmative, "Trust & Connect");
        assert_eq!(presentation.negative, "Cancel");
    }

    #[test]
    fn only_secret_prompts_require_the_native_secure_input_surface() {
        let first_contact = AskPassRequest::new(
            concat!(
                "The authenticity of host 'homelab (100.64.0.10)' can't be established.\n",
                "ED25519 key fingerprint is SHA256:AbCdEf0123456789.\n",
                "Are you sure you want to continue connecting (yes/no/[fingerprint])? "
            )
            .to_owned(),
            AskPassPromptKind::Secret,
        )
        .unwrap();
        let confirmation = AskPassRequest::new(
            "Continue with SSH authentication?".to_owned(),
            AskPassPromptKind::Confirmation,
        )
        .unwrap();
        let password =
            AskPassRequest::new("Password:".to_owned(), AskPassPromptKind::Secret).unwrap();
        let passphrase = AskPassRequest::new(
            "Enter passphrase for key '/Users/sdk/.ssh/id_ed25519': ".to_owned(),
            AskPassPromptKind::Secret,
        )
        .unwrap();

        assert!(!first_contact.requires_secure_input());
        assert!(
            first_contact
                .confirmation_presentation()
                .is_some_and(|presentation| presentation.is_first_contact())
        );
        assert!(!confirmation.requires_secure_input());
        assert!(
            confirmation
                .confirmation_presentation()
                .is_some_and(|presentation| !presentation.is_first_contact())
        );
        assert!(password.requires_secure_input());
        assert!(password.confirmation_presentation().is_none());
        assert!(passphrase.requires_secure_input());
        assert!(passphrase.confirmation_presentation().is_none());
    }

    #[test]
    fn action_group_trailing_policy_mirrors_with_layout_direction() {
        let content_bounds = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(392.0, 120.0));
        let left_to_right_group = NSRect::new(NSPoint::new(148.0, 16.0), NSSize::new(228.0, 28.0));
        let right_to_left_group = NSRect::new(NSPoint::new(16.0, 16.0), NSSize::new(228.0, 28.0));

        assert!(action_group_is_trailing(
            left_to_right_group,
            content_bounds,
            false
        ));
        assert!(!action_group_is_trailing(
            left_to_right_group,
            content_bounds,
            true
        ));
        assert!(action_group_is_trailing(
            right_to_left_group,
            content_bounds,
            true
        ));
        assert!(!action_group_is_trailing(
            right_to_left_group,
            content_bounds,
            false
        ));
    }

    #[test]
    fn action_group_replacement_accepts_only_an_exact_active_center_equality() {
        assert!(has_exact_center_constraint_properties(
            true,
            NS_LAYOUT_RELATION_EQUAL,
            1.0,
            0.0
        ));
        assert!(!has_exact_center_constraint_properties(
            false,
            NS_LAYOUT_RELATION_EQUAL,
            1.0,
            0.0
        ));
        assert!(!has_exact_center_constraint_properties(true, 1, 1.0, 0.0));
        assert!(!has_exact_center_constraint_properties(
            true,
            NS_LAYOUT_RELATION_EQUAL,
            2.0,
            0.0
        ));
        assert!(!has_exact_center_constraint_properties(
            true,
            NS_LAYOUT_RELATION_EQUAL,
            1.0,
            1.0
        ));
    }

    #[test]
    fn first_contact_presentation_preserves_every_openssh_context_line() {
        let prompt = concat!(
            "The authenticity of host 'example.test (203.0.113.10)' can't be established.\n",
            "RSA key fingerprint is SHA256:FingerprintValue.\n",
            "No matching host key fingerprint found in DNS.\n",
            "This host key is known by the following other names/addresses:\n",
            "    ~/.ssh/known_hosts:12: old.example.test\n",
            "Are you sure you want to continue connecting (yes/no/[fingerprint])? "
        );
        let request = AskPassRequest::new(prompt.to_owned(), AskPassPromptKind::Secret).unwrap();
        let presentation = request.presentation();

        assert_eq!(
            presentation.informative_text,
            concat!(
                "SSH does not recognize this host key for this address. Verify the fingerprint ",
                "with the host owner.\n\n",
                "Host: example.test\nAddress: 203.0.113.10\n\n",
                "RSA fingerprint:\nSHA256:FingerprintValue\n\n",
                "No matching host key fingerprint found in DNS.\n",
                "This host key is known by the following other names/addresses:\n",
                "    ~/.ssh/known_hosts:12: old.example.test\n\n",
                "If you continue, SSH will attempt to remember this key for future connections."
            )
        );
    }

    #[test]
    fn classification_preserves_ipv6_port_and_accepts_openssh_dns_detail() {
        let prompt = concat!(
            "The authenticity of host '[example.test]:2222 ([2001:db8::10]:2222)' can't be established.\n",
            "ECDSA key fingerprint is: SHA256:FingerprintValue.\n",
            "Matching host key fingerprint found in DNS.\n",
            "Are you sure you want to continue connecting (yes/no/[fingerprint])? "
        );
        let request =
            AskPassRequest::new(prompt.to_owned(), AskPassPromptKind::Confirmation).unwrap();

        let AskPassPromptClassification::FirstContact(classification) = request.classification()
        else {
            panic!("expected a first-contact classification");
        };

        assert_eq!(classification.host, "[example.test]:2222");
        assert_eq!(classification.address, Some("[2001:db8::10]:2222"));
        assert_eq!(classification.key_type, "ECDSA");
        assert_eq!(classification.fingerprint, "SHA256:FingerprintValue");
    }

    #[test]
    fn classification_requires_the_complete_terminal_openssh_grammar() {
        for prompt in [
            concat!(
                "The authenticity of host 'example.test (203.0.113.10)' can't be established.\n",
                "A host key fingerprint could not be parsed.\n",
                "Are you sure you want to continue connecting (yes/no/[fingerprint])? "
            ),
            concat!(
                "The authenticity of host 'example.test (203.0.113.10)' can't be established.\n",
                "ED25519 key fingerprint is SHA256:.\n",
                "Are you sure you want to continue connecting (yes/no/[fingerprint])? "
            ),
            concat!(
                "The authenticity of host 'example.test (203.0.113.10)' can't be established.\n",
                "ED25519 key fingerprint is SHA256:FingerprintValue.\n",
                "Are you sure you want to continue connecting (yes/no/[fingerprint])?\n",
                "unexpected trailing challenge"
            ),
        ] {
            let request =
                AskPassRequest::new(prompt.to_owned(), AskPassPromptKind::Secret).unwrap();
            let presentation = request.presentation();

            assert!(matches!(
                presentation.kind,
                AskPassPresentationKind::Confirmation
            ));
            assert_eq!(presentation.informative_text, request.prompt());
        }
    }

    #[test]
    fn password_prompts_use_the_distinct_nonempty_secure_presentation() {
        for (prompt, expected_context) in [
            (
                "sdk@homelab's password: ",
                concat!(
                    "SSH requested:\nsdk@homelab's password: \n\n",
                    "Host: homelab\nAccount: sdk\nMethod: Password\n\n",
                    "SpaceTerm sends this response to SSH and does not store it."
                ),
            ),
            (
                "Password:",
                concat!(
                    "SSH requested:\nPassword:\n\nMethod: Password\n\n",
                    "SpaceTerm sends this response to SSH and does not store it."
                ),
            ),
        ] {
            let request =
                AskPassRequest::new(prompt.to_owned(), AskPassPromptKind::Secret).unwrap();
            let presentation = request.presentation();

            assert!(matches!(
                presentation.kind,
                AskPassPresentationKind::Password
            ));
            assert_eq!(presentation.title, "Sign In to Remote Host");
            assert_eq!(presentation.informative_text, expected_context);
            assert_eq!(presentation.affirmative, "Sign In");
            assert_eq!(presentation.negative, "Cancel");
            assert_eq!(presentation.field_label, Some("Password"));
            assert!(!presentation.kind.secret_submission_enabled(0));
            assert!(presentation.kind.secret_submission_enabled(1));
        }
    }

    #[test]
    fn key_passphrase_prompts_use_the_distinct_nonempty_secure_presentation() {
        let prompt = "Enter passphrase for key '/Users/sdk/.ssh/id_ed25519': ";
        let request = AskPassRequest::new(prompt.to_owned(), AskPassPromptKind::Secret).unwrap();
        let presentation = request.presentation();

        assert!(matches!(
            presentation.kind,
            AskPassPresentationKind::KeyPassphrase
        ));
        assert_eq!(presentation.title, "SSH Key Passphrase");
        assert_eq!(
            presentation.informative_text,
            concat!(
                "SSH requested:\nEnter passphrase for key '/Users/sdk/.ssh/id_ed25519': \n\n",
                "Key: id_ed25519\nLocation: /Users/sdk/.ssh\n\n",
                "SpaceTerm sends this response to SSH and does not store it."
            )
        );
        assert_eq!(presentation.affirmative, "Submit & Connect");
        assert_eq!(presentation.negative, "Cancel");
        assert_eq!(presentation.field_label, Some("Key passphrase"));
        assert!(!presentation.kind.secret_submission_enabled(0));
        assert!(presentation.kind.secret_submission_enabled(1));
    }

    #[test]
    fn generic_secret_challenges_keep_protocol_compatible_empty_submission() {
        for prompt in ["One-time verification code: ", "PIN: "] {
            let request =
                AskPassRequest::new(prompt.to_owned(), AskPassPromptKind::Secret).unwrap();
            let presentation = request.presentation();

            assert!(matches!(presentation.kind, AskPassPresentationKind::Secret));
            assert_eq!(presentation.title, "SpaceTerm SSH Authentication");
            assert_eq!(presentation.informative_text, prompt);
            assert_eq!(presentation.affirmative, "Continue");
            assert_eq!(presentation.negative, "Cancel");
            assert_eq!(
                presentation.field_label,
                Some("Secure SSH authentication response")
            );
            assert!(presentation.kind.secret_submission_enabled(0));
        }
        assert!(AskPassSecret::new(Vec::new()).is_ok());
    }

    #[test]
    fn strict_secret_parsers_reject_multiline_and_wrong_hint_variants() {
        for prompt in [
            "sdk@homelab's password:\nOne-time code:",
            "One-time code:\nsdk@homelab's password:",
            "Enter passphrase for key '/tmp/id':\nOne-time code:",
            "One-time code:\nEnter passphrase for key '/tmp/id':",
        ] {
            let request =
                AskPassRequest::new(prompt.to_owned(), AskPassPromptKind::Secret).unwrap();
            assert!(matches!(
                request.presentation().kind,
                AskPassPresentationKind::Secret
            ));
        }
        let request = AskPassRequest::new(
            "sdk@homelab's password: ".to_owned(),
            AskPassPromptKind::Confirmation,
        )
        .unwrap();
        assert!(matches!(
            request.presentation().kind,
            AskPassPresentationKind::Confirmation
        ));
    }

    #[test]
    fn host_security_warnings_never_offer_first_contact_trust() {
        for warning in [
            "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!",
            "WARNING: REVOKED HOST KEY DETECTED!",
            "WARNING: POSSIBLE DNS SPOOFING DETECTED!",
        ] {
            let prompt = format!(
                concat!(
                    "The authenticity of host 'example.test (203.0.113.10)' can't be ",
                    "established.\n",
                    "ED25519 key fingerprint is SHA256:FingerprintValue.\n",
                    "{}\n",
                    "Are you sure you want to continue connecting (yes/no/[fingerprint])? "
                ),
                warning
            );
            let request = AskPassRequest::new(prompt, AskPassPromptKind::Secret).unwrap();
            let presentation = request.presentation();

            assert!(!matches!(
                presentation.kind,
                AskPassPresentationKind::FirstContact
            ));
            assert_ne!(presentation.affirmative, "Trust & Connect");
        }
    }

    #[test]
    fn safe_first_button_response_mapping_covers_both_buttons_and_abort() {
        let first_contact = AskPassPresentationKind::FirstContact;
        assert_eq!(
            observe(
                first_contact
                    .button_result(NS_ALERT_FIRST_BUTTON_RETURN)
                    .unwrap()
            ),
            ObservedResult::Cancelled
        );
        assert_eq!(
            observe(
                first_contact
                    .button_result(NS_ALERT_SECOND_BUTTON_RETURN)
                    .unwrap()
            ),
            ObservedResult::Confirmation(true)
        );
        assert_eq!(
            observe(
                first_contact
                    .button_result(NS_MODAL_RESPONSE_ABORT)
                    .unwrap()
            ),
            ObservedResult::Cancelled
        );

        let confirmation = AskPassPresentationKind::Confirmation;
        assert_eq!(
            observe(
                confirmation
                    .button_result(NS_ALERT_FIRST_BUTTON_RETURN)
                    .unwrap()
            ),
            ObservedResult::Confirmation(false)
        );
        assert_eq!(
            observe(
                confirmation
                    .button_result(NS_ALERT_SECOND_BUTTON_RETURN)
                    .unwrap()
            ),
            ObservedResult::Confirmation(true)
        );
        assert_eq!(
            observe(confirmation.button_result(NS_MODAL_RESPONSE_ABORT).unwrap()),
            ObservedResult::Cancelled
        );
    }

    #[test]
    fn generic_confirmation_preserves_yes_no_transport_semantics() {
        let request = AskPassRequest::new(
            "Allow a generic SSH operation?".to_owned(),
            AskPassPromptKind::Confirmation,
        )
        .unwrap();
        let presentation = request.presentation();

        assert!(matches!(
            presentation.kind,
            AskPassPresentationKind::Confirmation
        ));
        assert_eq!(presentation.affirmative, "Yes");
        assert_eq!(presentation.negative, "No");
    }

    #[test]
    fn request_rejects_empty_and_oversized_prompts() {
        let empty = AskPassRequest::new(String::new(), AskPassPromptKind::Secret).err();
        let oversized = AskPassRequest::new(
            "a".repeat(MAX_ASKPASS_PROMPT_BYTES + 1),
            AskPassPromptKind::Confirmation,
        )
        .err();

        assert_eq!(empty, Some(AskPassRequestError::Empty));
        assert_eq!(oversized, Some(AskPassRequestError::TooLong));
    }

    #[test]
    fn request_rejects_every_non_newline_control_after_crlf_normalization() {
        for prompt in [
            "bad\0prompt",
            "bad\tprompt",
            "bad\u{1b}prompt",
            "bad\rprompt",
            "bad\r\r\nprompt",
            "bad\u{7f}prompt",
        ] {
            let error =
                AskPassRequest::new(prompt.to_owned(), AskPassPromptKind::Confirmation).err();

            assert_eq!(error, Some(AskPassRequestError::ContainsUnsafeCharacter));
        }
    }

    #[test]
    fn request_rejects_bidi_and_invisible_format_spoofing() {
        for character in [
            '\u{00ad}', '\u{061c}', '\u{200b}', '\u{202e}', '\u{2066}', '\u{feff}',
        ] {
            let error = AskPassRequest::new(
                format!("trusted{character}host"),
                AskPassPromptKind::Confirmation,
            )
            .err();

            assert_eq!(error, Some(AskPassRequestError::ContainsUnsafeCharacter));
        }
    }

    #[test]
    fn cancellation_owner_is_claimed_exactly_once() {
        let lifecycle = AskPassPresentationLifecycle::default();
        let activity = lifecycle.begin().unwrap();
        let response_owner = 1_usize as id;
        lifecycle.bind_response_owner(activity.generation, response_owner);

        assert_eq!(lifecycle.take_cancellation_owner(), Some(response_owner));
        assert_eq!(lifecycle.take_cancellation_owner(), None);
        drop(activity);
        assert!(!lifecycle.is_active());
    }

    #[test]
    fn stale_generation_cannot_replace_a_later_cancellation_owner() {
        let lifecycle = AskPassPresentationLifecycle::default();
        let first = lifecycle.begin().unwrap();
        let stale_generation = first.generation;
        drop(first);
        let second = lifecycle.begin().unwrap();
        let current_owner = 2_usize as id;
        lifecycle.bind_response_owner(second.generation, current_owner);

        lifecycle.bind_response_owner(stale_generation, 1_usize as id);

        assert_eq!(lifecycle.take_cancellation_owner(), Some(current_owner));
    }

    #[test]
    fn sheet_response_state_claims_only_the_first_response() {
        let mut state = SheetResponseState {
            parent_window: 1_usize as id,
            sheet_window: 2_usize as id,
            ended: false,
        };

        let first = state.claim(NS_ALERT_FIRST_BUTTON_RETURN);
        let second = state.claim(NS_ALERT_SECOND_BUTTON_RETURN);

        assert!(first.is_some());
        assert!(second.is_none());
    }

    #[test]
    fn focus_policy_accepts_only_the_parent_attached_sheet() {
        let parent = 1_usize as id;
        let sheet = 2_usize as id;

        assert!(is_current_attached_sheet(parent, sheet, sheet));
    }

    #[test]
    fn focus_policy_rejects_an_invisible_queued_sheet() {
        let parent = 1_usize as id;
        let queued_sheet = 2_usize as id;
        let visible_sheet = 3_usize as id;

        assert!(!is_current_attached_sheet(
            parent,
            visible_sheet,
            queued_sheet
        ));
    }

    #[test]
    fn appkit_completion_boundary_contains_panics_and_runs_cleanup() {
        struct Cleanup(Rc<RefCell<bool>>);

        impl Drop for Cleanup {
            fn drop(&mut self) {
                *self.0.borrow_mut() = true;
            }
        }

        let cleaned = Rc::new(RefCell::new(false));
        let cleanup = Cleanup(cleaned.clone());
        contain_appkit_completion(move || {
            let _cleanup = cleanup;
            panic!("simulated completion panic");
        });

        assert!(*cleaned.borrow());
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
