use std::fmt;

#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct SshHostAlias(String);

impl fmt::Debug for SshHostAlias {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SshHostAlias(<redacted>)")
    }
}

impl SshHostAlias {
    pub(crate) fn new(value: String) -> Result<Self, SshHostAliasError> {
        if is_positive_literal(&value) {
            Ok(Self(value))
        } else {
            Err(SshHostAliasError::Invalid)
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SshHostAliasError {
    Invalid,
}

fn is_positive_literal(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && !value.starts_with('!')
        && !value.contains(['*', '?'])
        && !value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alias(value: &str) -> SshHostAlias {
        SshHostAlias::new(value.to_owned()).unwrap()
    }

    #[test]
    fn debug_should_redact_aliases() {
        assert_eq!(
            format!("{:?}", alias("sensitive-host")),
            "SshHostAlias(<redacted>)"
        );
    }

    #[test]
    fn alias_should_accept_positive_literal_tokens() {
        for value in [
            "work",
            "work.example",
            "build_host",
            "fedora@orb",
            "host[0]",
            "host:22",
            "host/path",
        ] {
            assert_eq!(alias(value).as_str(), value);
        }
    }

    #[test]
    fn alias_should_reject_options_controls_patterns_and_malformed_tokens() {
        for value in [
            "",
            "-v",
            "two words",
            "line\nbreak",
            "*.example",
            "host?",
            "!host",
        ] {
            assert_eq!(
                SshHostAlias::new(value.to_owned()),
                Err(SshHostAliasError::Invalid),
                "unexpectedly accepted {value:?}"
            );
        }
    }
}
