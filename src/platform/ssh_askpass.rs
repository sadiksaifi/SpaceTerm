use std::path::Path;

use zeroize::Zeroizing;

const MAX_ASKPASS_PROMPT_BYTES: usize = 4 * 1024;
const MAX_ASKPASS_SECRET_BYTES: usize = 16 * 1024;

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

    pub(crate) fn confirmation_presentation(&self) -> Option<AskPassConfirmationPresentation> {
        let presentation = self.presentation();
        if presentation.kind.uses_secret_entry() {
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
            detail: presentation.detail,
            affirmative: presentation.affirmative,
            negative: presentation.negative,
        })
    }

    pub(crate) fn secret_presentation(&self) -> Option<AskPassSecretPresentation> {
        let presentation = self.presentation();
        if !presentation.kind.uses_secret_entry() {
            return None;
        }
        Some(AskPassSecretPresentation {
            title: presentation.title,
            detail: presentation.detail,
            affirmative: presentation.affirmative,
            negative: presentation.negative,
            field_label: presentation.field_label?,
            requires_nonempty: presentation.kind.requires_nonempty_secret(),
        })
    }

    fn classification(&self) -> AskPassPromptClassification<'_> {
        classify_prompt(&self.prompt, self.kind)
    }

    fn presentation(&self) -> AskPassPresentation {
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

/// Owned secret-entry content rendered by SpaceTerm's shared GPUI Dialog surface.
pub(crate) struct AskPassSecretPresentation {
    title: &'static str,
    detail: String,
    affirmative: &'static str,
    negative: &'static str,
    field_label: &'static str,
    requires_nonempty: bool,
}

impl AskPassSecretPresentation {
    pub(crate) const fn title(&self) -> &'static str {
        self.title
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

    pub(crate) const fn field_label(&self) -> &'static str {
        self.field_label
    }

    pub(crate) const fn requires_nonempty(&self) -> bool {
        self.requires_nonempty
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

impl AskPassPresentationKind {
    const fn uses_secret_entry(self) -> bool {
        matches!(self, Self::Password | Self::KeyPassphrase | Self::Secret)
    }

    const fn requires_nonempty_secret(self) -> bool {
        matches!(self, Self::Password | Self::KeyPassphrase)
    }
}

struct AskPassPresentation {
    kind: AskPassPresentationKind,
    title: &'static str,
    detail: String,
    affirmative: &'static str,
    negative: &'static str,
    field_label: Option<&'static str>,
}

impl AskPassPresentation {
    fn first_contact(prompt: FirstContactPrompt<'_>) -> Self {
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
        let mut detail = concat!(
            "SSH does not recognize this host key for this address. Verify the fingerprint with ",
            "the host owner.\n\n"
        )
        .to_owned();
        detail.push_str(&host);
        if !prompt.additional_details.is_empty() {
            detail.push_str("\n\n");
            detail.push_str(&prompt.additional_details.join("\n"));
        }
        detail.push_str(
            "\n\nIf you continue, SSH will attempt to remember this key for future connections.",
        );
        Self {
            kind: AskPassPresentationKind::FirstContact,
            title: "Verify SSH Host",
            detail,
            affirmative: "Trust & Connect",
            negative: "Cancel",
            field_label: None,
        }
    }

    fn confirmation(request: &AskPassRequest) -> Self {
        Self {
            kind: AskPassPresentationKind::Confirmation,
            title: "SpaceTerm SSH Authentication",
            detail: request.prompt().to_owned(),
            affirmative: "Yes",
            negative: "No",
            field_label: None,
        }
    }

    fn password(request: &AskPassRequest, prompt: PasswordPrompt<'_>) -> Self {
        let mut detail = format!("SSH requested:\n{}\n\n", request.prompt());
        if let (Some(account), Some(host)) = (prompt.account, prompt.host) {
            detail.push_str(&format!("Host: {host}\nAccount: {account}\n"));
        }
        detail.push_str(concat!(
            "Method: Password\n\n",
            "SpaceTerm sends this response to SSH and does not store it."
        ));
        Self {
            kind: AskPassPresentationKind::Password,
            title: "Sign In to Remote Host",
            detail,
            affirmative: "Sign In",
            negative: "Cancel",
            field_label: Some("Password"),
        }
    }

    fn key_passphrase(request: &AskPassRequest, prompt: KeyPassphrasePrompt<'_>) -> Self {
        let mut detail = format!(
            "SSH requested:\n{}\n\nKey: {}",
            request.prompt(),
            prompt.filename
        );
        if let Some(location) = prompt.location {
            detail.push_str(&format!("\nLocation: {location}"));
        }
        detail.push_str("\n\nSpaceTerm sends this response to SSH and does not store it.");
        Self {
            kind: AskPassPresentationKind::KeyPassphrase,
            title: "SSH Key Passphrase",
            detail,
            affirmative: "Submit & Connect",
            negative: "Cancel",
            field_label: Some("Key passphrase"),
        }
    }

    fn secret(request: &AskPassRequest) -> Self {
        Self {
            kind: AskPassPresentationKind::Secret,
            title: "SpaceTerm SSH Authentication",
            detail: request.prompt().to_owned(),
            affirmative: "Continue",
            negative: "Cancel",
            field_label: Some("SSH authentication response"),
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
/// requests it through `RP_ECHO` and therefore supplies no confirmation hint to AskPass.
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
        if !safe_label(host) || !safe_label(address) {
            return None;
        }
        return Some((host, Some(address)));
    }
    safe_label(value).then_some((value, None))
}

fn safe_label(value: &str) -> bool {
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
    if !safe_label(account) || !safe_label(host) {
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
/// Failure to capture a bounded response from the in-house control.
pub(crate) enum AskPassResponseError {
    SecretTooLong,
}

/// Non-clone, non-Debug owner of zeroized AskPass response bytes.
pub(crate) struct AskPassSecret {
    bytes: Zeroizing<Vec<u8>>,
}

impl AskPassSecret {
    pub(crate) fn new(bytes: Vec<u8>) -> Result<Self, AskPassResponseError> {
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

/// Exactly-once result of one AskPass presentation.
pub(crate) enum AskPassResult {
    Secret(AskPassSecret),
    Confirmation(bool),
    Cancelled,
    Failed(AskPassResponseError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
/// Safe presentation failure with prompt and response content excluded.
pub(crate) enum AskPassPresentationError {
    #[error("an AskPass prompt is already active")]
    Busy,
    #[error("SpaceTerm could not present the AskPass prompt")]
    ApplicationPresentationUnavailable,
}

/// One-shot completion that consumes the response owner.
pub(crate) type AskPassCompletion = Box<dyn FnOnce(AskPassResult)>;

#[cfg(test)]
mod tests {
    use super::*;

    fn request(prompt: &str, kind: AskPassPromptKind) -> AskPassRequest {
        AskPassRequest::new(prompt.to_owned(), kind).unwrap()
    }

    #[test]
    fn request_should_normalize_crlf_without_accepting_other_controls() {
        let request = request("line one\r\nline two", AskPassPromptKind::Confirmation);

        assert_eq!(request.prompt(), "line one\nline two");
    }

    #[test]
    fn request_should_reject_directional_format_controls() {
        let result = AskPassRequest::new("Password:\u{202e}".to_owned(), AskPassPromptKind::Secret);

        assert_eq!(
            result.err(),
            Some(AskPassRequestError::ContainsUnsafeCharacter)
        );
    }

    #[test]
    fn password_should_require_nonempty_obscured_entry() {
        let presentation = request("root@example.test's password:", AskPassPromptKind::Secret)
            .secret_presentation()
            .unwrap();

        assert!(presentation.requires_nonempty());
    }

    #[test]
    fn password_should_present_account_and_host_without_exposing_response() {
        let presentation = request("root@example.test's password:", AskPassPromptKind::Secret)
            .secret_presentation()
            .unwrap();

        assert!(
            presentation
                .detail()
                .contains("Host: example.test\nAccount: root")
        );
    }

    #[test]
    fn key_passphrase_should_require_nonempty_obscured_entry() {
        let presentation = request(
            "Enter passphrase for key '/Users/dev/.ssh/id_ed25519':",
            AskPassPromptKind::Secret,
        )
        .secret_presentation()
        .unwrap();

        assert!(presentation.requires_nonempty());
    }

    #[test]
    fn generic_secret_should_allow_empty_response() {
        let presentation = request("Verification code:", AskPassPromptKind::Secret)
            .secret_presentation()
            .unwrap();

        assert!(!presentation.requires_nonempty());
    }

    #[test]
    fn generic_confirmation_should_not_build_secret_entry() {
        let request = request("Continue?", AskPassPromptKind::Confirmation);

        assert!(request.secret_presentation().is_none());
    }

    #[test]
    fn first_contact_should_be_presented_as_application_confirmation() {
        let prompt = concat!(
            "The authenticity of host 'example.test (192.0.2.1)' can't be established.\n",
            "ED25519 key fingerprint is SHA256:abcDEF0123+/=.\n",
            "Are you sure you want to continue connecting (yes/no/[fingerprint])?"
        );
        let presentation = request(prompt, AskPassPromptKind::Secret)
            .confirmation_presentation()
            .unwrap();

        assert!(presentation.is_first_contact());
    }

    #[test]
    fn changed_host_warning_should_never_enter_first_contact_path() {
        let prompt = concat!(
            "The authenticity of host 'example.test' can't be established.\n",
            "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\n",
            "ED25519 key fingerprint is SHA256:abcDEF0123+/=.\n",
            "Are you sure you want to continue connecting (yes/no)?"
        );
        let request = request(prompt, AskPassPromptKind::Secret);
        let presentation = request.confirmation_presentation().unwrap();

        assert!(!presentation.is_first_contact());
    }

    #[test]
    fn secret_owner_should_enforce_exact_byte_limit() {
        let accepted = AskPassSecret::new(vec![b'x'; MAX_ASKPASS_SECRET_BYTES]);
        let rejected = AskPassSecret::new(vec![b'x'; MAX_ASKPASS_SECRET_BYTES + 1]);

        assert!(accepted.is_ok());
        assert_eq!(rejected.err(), Some(AskPassResponseError::SecretTooLong));
    }
}
