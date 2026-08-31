use std::collections::BTreeSet;
use std::num::NonZeroU16;

use thiserror::Error;

use super::destination::SshHostAlias;

const HEADER: &str = "# This file is managed by SpaceTerm.\n\n";
const PRECEDENCE_TAIL: &str = concat!(
    "Host *\n",
    "  Include ~/.ssh/config\n",
    "  Include /etc/ssh/ssh_config\n",
);
const TOKEN_BYTES: usize = 255;
const IDENTITY_FILE_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ManagedSshHostField {
    Alias,
    HostName,
    User,
    IdentityFile,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ManagedSshHostValueError {
    Required,
    TooLong { maximum: usize },
    Pattern,
    Negated,
    Whitespace,
    Control,
    LeadingOption,
    ReservedKeyword,
    Unsafe,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("invalid {field:?}: {kind:?}")]
pub(crate) struct ManagedSshHostValidationError {
    pub(crate) field: ManagedSshHostField,
    pub(crate) kind: ManagedSshHostValueError,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManagedSshHost {
    alias: SshHostAlias,
    host_name: String,
    user: Option<String>,
    port: Option<NonZeroU16>,
    identity_file: Option<String>,
}

impl ManagedSshHost {
    pub(crate) fn new(
        alias: String,
        host_name: String,
        user: Option<String>,
        port: Option<NonZeroU16>,
        identity_file: Option<String>,
    ) -> Result<Self, ManagedSshHostValidationError> {
        validate_alias(&alias)?;
        validate_host_name(&host_name)?;
        if let Some(user) = user.as_deref() {
            validate_user(user)?;
        }
        if let Some(identity_file) = identity_file.as_deref() {
            validate_identity_file(identity_file)?;
        }
        let alias = SshHostAlias::new(alias).map_err(|_| ManagedSshHostValidationError {
            field: ManagedSshHostField::Alias,
            kind: ManagedSshHostValueError::Unsafe,
        })?;
        Ok(Self {
            alias,
            host_name,
            user,
            port,
            identity_file,
        })
    }

    pub(crate) const fn alias(&self) -> &SshHostAlias {
        &self.alias
    }

    pub(crate) fn host_name(&self) -> &str {
        &self.host_name
    }

    pub(crate) fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }

    pub(crate) const fn port(&self) -> Option<NonZeroU16> {
        self.port
    }

    pub(crate) fn identity_file(&self) -> Option<&str> {
        self.identity_file.as_deref()
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum ManagedHostsFormatError {
    #[error("the managed SSH config is not in SpaceTerm's canonical format")]
    NonCanonical,
}

fn validate_alias(value: &str) -> Result<(), ManagedSshHostValidationError> {
    validate_token(ManagedSshHostField::Alias, value, |character| {
        character.is_alphanumeric() || matches!(character, '.' | '_' | '-' | '@' | ':' | '[' | ']')
    })
}

fn validate_host_name(value: &str) -> Result<(), ManagedSshHostValidationError> {
    validate_token(ManagedSshHostField::HostName, value, |character| {
        character.is_alphanumeric() || matches!(character, '.' | '_' | '-' | ':' | '[' | ']')
    })
}

fn validate_user(value: &str) -> Result<(), ManagedSshHostValidationError> {
    validate_token(ManagedSshHostField::User, value, |character| {
        character.is_alphanumeric() || matches!(character, '.' | '_' | '-' | '@' | '+')
    })
}

fn validate_token(
    field: ManagedSshHostField,
    value: &str,
    allowed: impl Fn(char) -> bool,
) -> Result<(), ManagedSshHostValidationError> {
    let kind = if value.is_empty() {
        Some(ManagedSshHostValueError::Required)
    } else if value.len() > TOKEN_BYTES {
        Some(ManagedSshHostValueError::TooLong {
            maximum: TOKEN_BYTES,
        })
    } else if value.chars().any(char::is_control) {
        Some(ManagedSshHostValueError::Control)
    } else if value.chars().any(char::is_whitespace) {
        Some(ManagedSshHostValueError::Whitespace)
    } else if value.starts_with('-') {
        Some(ManagedSshHostValueError::LeadingOption)
    } else if value.starts_with('!') {
        Some(ManagedSshHostValueError::Negated)
    } else if value.contains(['*', '?']) {
        Some(ManagedSshHostValueError::Pattern)
    } else if is_reserved_keyword(value) {
        Some(ManagedSshHostValueError::ReservedKeyword)
    } else if !value.chars().all(allowed) {
        Some(ManagedSshHostValueError::Unsafe)
    } else {
        None
    };
    if let Some(kind) = kind {
        Err(ManagedSshHostValidationError { field, kind })
    } else {
        Ok(())
    }
}

fn validate_identity_file(value: &str) -> Result<(), ManagedSshHostValidationError> {
    let field = ManagedSshHostField::IdentityFile;
    let kind = if value.is_empty() {
        Some(ManagedSshHostValueError::Required)
    } else if value.len() > IDENTITY_FILE_BYTES {
        Some(ManagedSshHostValueError::TooLong {
            maximum: IDENTITY_FILE_BYTES,
        })
    } else if value.chars().any(char::is_control) {
        Some(ManagedSshHostValueError::Control)
    } else if value.starts_with('-') {
        Some(ManagedSshHostValueError::LeadingOption)
    } else if value.starts_with('!') {
        Some(ManagedSshHostValueError::Negated)
    } else if value.contains(['*', '?']) {
        Some(ManagedSshHostValueError::Pattern)
    } else if !concrete_identity_path(value) {
        Some(ManagedSshHostValueError::Unsafe)
    } else {
        None
    };
    if let Some(kind) = kind {
        Err(ManagedSshHostValidationError { field, kind })
    } else {
        Ok(())
    }
}

fn concrete_identity_path(value: &str) -> bool {
    let relative = if let Some(relative) = value.strip_prefix("~/") {
        relative
    } else if let Some(relative) = value.strip_prefix('/') {
        relative
    } else {
        return false;
    };
    !relative.is_empty()
        && !relative.ends_with('/')
        && relative
            .split('/')
            .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
}

fn is_reserved_keyword(value: &str) -> bool {
    [
        "host",
        "hostname",
        "user",
        "port",
        "identityfile",
        "include",
        "match",
    ]
    .iter()
    .any(|keyword| value.eq_ignore_ascii_case(keyword))
}

fn serialize_managed_hosts(hosts: &[ManagedSshHost]) -> String {
    let mut ordered = hosts.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.alias.cmp(&right.alias));
    let mut serialized = String::from(HEADER);
    for host in ordered {
        serialized.push_str("Host ");
        serialized.push_str(host.alias.as_str());
        serialized.push('\n');
        serialized.push_str("  HostName ");
        serialized.push_str(&host.host_name);
        serialized.push('\n');
        if let Some(user) = &host.user {
            serialized.push_str("  User ");
            serialized.push_str(user);
            serialized.push('\n');
        }
        if let Some(port) = host.port {
            serialized.push_str("  Port ");
            serialized.push_str(&port.to_string());
            serialized.push('\n');
        }
        if let Some(identity_file) = &host.identity_file {
            serialized.push_str("  IdentityFile ");
            quote_argument(identity_file, &mut serialized);
            serialized.push('\n');
        }
        serialized.push('\n');
    }
    serialized.push_str(PRECEDENCE_TAIL);
    serialized
}

fn quote_argument(value: &str, output: &mut String) {
    output.push('"');
    for character in value.chars() {
        if matches!(character, '\\' | '"') {
            output.push('\\');
        }
        output.push(character);
    }
    output.push('"');
}

fn parse_managed_hosts(bytes: &[u8]) -> Result<Vec<ManagedSshHost>, ManagedHostsFormatError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ManagedHostsFormatError::NonCanonical)?;
    let body = text
        .strip_prefix(HEADER)
        .and_then(|text| text.strip_suffix(PRECEDENCE_TAIL))
        .ok_or(ManagedHostsFormatError::NonCanonical)?;
    let mut hosts = Vec::new();
    let mut aliases = BTreeSet::new();
    if !body.is_empty() {
        let stanzas = body
            .strip_suffix("\n\n")
            .ok_or(ManagedHostsFormatError::NonCanonical)?;
        for stanza in stanzas.split("\n\n") {
            let host = parse_stanza(stanza)?;
            if !aliases.insert(host.alias.as_str().to_owned()) {
                return Err(ManagedHostsFormatError::NonCanonical);
            }
            hosts.push(host);
        }
    }
    if serialize_managed_hosts(&hosts) != text {
        return Err(ManagedHostsFormatError::NonCanonical);
    }
    Ok(hosts)
}

fn parse_stanza(stanza: &str) -> Result<ManagedSshHost, ManagedHostsFormatError> {
    let mut lines = stanza.lines().peekable();
    let alias = lines
        .next()
        .and_then(|line| line.strip_prefix("Host "))
        .ok_or(ManagedHostsFormatError::NonCanonical)?;
    let host_name = lines
        .next()
        .and_then(|line| line.strip_prefix("  HostName "))
        .ok_or(ManagedHostsFormatError::NonCanonical)?;
    let user = take_prefixed(&mut lines, "  User ").map(str::to_owned);
    let port = take_prefixed(&mut lines, "  Port ")
        .map(|value| value.parse::<NonZeroU16>())
        .transpose()
        .map_err(|_| ManagedHostsFormatError::NonCanonical)?;
    let identity_file = take_prefixed(&mut lines, "  IdentityFile ")
        .map(parse_quoted_argument)
        .transpose()?;
    if lines.next().is_some() {
        return Err(ManagedHostsFormatError::NonCanonical);
    }
    ManagedSshHost::new(
        alias.to_owned(),
        host_name.to_owned(),
        user,
        port,
        identity_file,
    )
    .map_err(|_| ManagedHostsFormatError::NonCanonical)
}

fn take_prefixed<'a, I>(lines: &mut std::iter::Peekable<I>, prefix: &str) -> Option<&'a str>
where
    I: Iterator<Item = &'a str>,
{
    lines.peek().and_then(|line| line.strip_prefix(prefix))?;
    lines.next().and_then(|line| line.strip_prefix(prefix))
}

fn parse_quoted_argument(value: &str) -> Result<String, ManagedHostsFormatError> {
    let inner = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or(ManagedHostsFormatError::NonCanonical)?;
    let mut parsed = String::new();
    let mut characters = inner.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            let escaped = characters
                .next()
                .filter(|escaped| matches!(escaped, '\\' | '"'))
                .ok_or(ManagedHostsFormatError::NonCanonical)?;
            parsed.push(escaped);
        } else if character == '"' {
            return Err(ManagedHostsFormatError::NonCanonical);
        } else {
            parsed.push(character);
        }
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU16;

    use super::*;

    fn host(alias: &str, hostname: &str) -> ManagedSshHost {
        ManagedSshHost::new(alias.to_owned(), hostname.to_owned(), None, None, None).unwrap()
    }

    #[test]
    fn validation_should_report_the_required_field_inline() {
        let error =
            ManagedSshHost::new(String::new(), "server.example".to_owned(), None, None, None)
                .unwrap_err();

        assert_eq!(
            error,
            ManagedSshHostValidationError {
                field: ManagedSshHostField::Alias,
                kind: ManagedSshHostValueError::Required,
            }
        );
    }

    #[test]
    fn validation_should_distinguish_injection_hazards() {
        for (alias, kind) in [
            ("*.example", ManagedSshHostValueError::Pattern),
            ("!blocked", ManagedSshHostValueError::Negated),
            ("two words", ManagedSshHostValueError::Whitespace),
            ("line\nbreak", ManagedSshHostValueError::Control),
            ("-option", ManagedSshHostValueError::LeadingOption),
            ("Host", ManagedSshHostValueError::ReservedKeyword),
            ("bad#alias", ManagedSshHostValueError::Unsafe),
        ] {
            let error = ManagedSshHost::new(
                alias.to_owned(),
                "server.example".to_owned(),
                None,
                None,
                None,
            )
            .unwrap_err();
            assert_eq!(error.kind, kind, "unexpected validation for {alias:?}");
        }
    }

    #[test]
    fn validation_should_accept_a_concrete_identity_path_with_spaces() {
        let managed = ManagedSshHost::new(
            "work".to_owned(),
            "server.example".to_owned(),
            Some("deploy".to_owned()),
            NonZeroU16::new(2222),
            Some("~/Keys/Work Key\"1\"".to_owned()),
        );

        assert!(managed.is_ok(), "unexpected error: {managed:?}");
    }

    #[test]
    fn validation_should_reject_nonconcrete_identity_paths() {
        for identity in ["relative/key", "~/../key", "/keys/*", "/keys/line\nbreak"] {
            let error = ManagedSshHost::new(
                "work".to_owned(),
                "server.example".to_owned(),
                None,
                None,
                Some(identity.to_owned()),
            )
            .unwrap_err();
            assert_eq!(error.field, ManagedSshHostField::IdentityFile);
        }
    }

    #[test]
    fn canonical_format_should_sort_hosts_quote_paths_and_end_with_precedence_tail() {
        let hosts = vec![
            ManagedSshHost::new(
                "zeta".to_owned(),
                "zeta.example".to_owned(),
                None,
                None,
                Some("~/Keys/Zeta Key".to_owned()),
            )
            .unwrap(),
            host("alpha", "alpha.example"),
        ];

        let serialized = serialize_managed_hosts(&hosts);

        assert_eq!(
            serialized,
            concat!(
                "# This file is managed by SpaceTerm.\n\n",
                "Host alpha\n",
                "  HostName alpha.example\n\n",
                "Host zeta\n",
                "  HostName zeta.example\n",
                "  IdentityFile \"~/Keys/Zeta Key\"\n\n",
                "Host *\n",
                "  Include ~/.ssh/config\n",
                "  Include /etc/ssh/ssh_config\n",
            )
        );
    }

    #[test]
    fn canonical_format_should_round_trip_all_five_fields() {
        let expected = ManagedSshHost::new(
            "work".to_owned(),
            "server.example".to_owned(),
            Some("deploy".to_owned()),
            NonZeroU16::new(2222),
            Some("~/Keys/Work Key\"1\"".to_owned()),
        )
        .unwrap();
        let bytes = serialize_managed_hosts(std::slice::from_ref(&expected));

        let parsed = parse_managed_hosts(bytes.as_bytes()).unwrap();

        assert_eq!(parsed, vec![expected]);
    }

    #[test]
    fn canonical_parser_should_reject_unknown_or_noncanonical_text() {
        for bytes in [
            b"Host manual\n  HostName manual.example\n".as_slice(),
            concat!(
                "# This file is managed by SpaceTerm.\n\n",
                "Host work\n",
                "  HostName work.example\n",
                "  ProxyCommand unsafe\n\n",
                "Host *\n",
                "  Include ~/.ssh/config\n",
                "  Include /etc/ssh/ssh_config\n",
            )
            .as_bytes(),
        ] {
            assert_eq!(
                parse_managed_hosts(bytes),
                Err(ManagedHostsFormatError::NonCanonical)
            );
        }
    }
}
