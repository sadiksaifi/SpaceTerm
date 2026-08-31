use crate::domain::SshDestination;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct SshHostAlias(String);

impl SshHostAlias {
    pub(crate) fn new(value: String) -> Result<Self, SshHostAliasError> {
        if is_positive_literal(&value) {
            Ok(Self(value))
        } else {
            Err(SshHostAliasError::Invalid)
        }
    }

    pub(crate) fn new_bounded(
        value: String,
        maximum_bytes: usize,
    ) -> Result<Self, SshHostAliasError> {
        if value.len() > maximum_bytes {
            return Err(SshHostAliasError::TooLong {
                actual: value.len(),
                maximum: maximum_bytes,
            });
        }
        Self::new(value)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DestinationQueryResolution {
    Configured {
        destination: SshDestination,
        alias: SshHostAlias,
        explicit_user: Option<String>,
    },
    AddHost {
        destination: SshDestination,
    },
}

impl DestinationQueryResolution {
    pub(crate) const fn destination(&self) -> &SshDestination {
        match self {
            Self::Configured { destination, .. } | Self::AddHost { destination } => destination,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SshHostAliasError {
    Invalid,
    TooLong { actual: usize, maximum: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DestinationQueryError {
    Invalid,
    TooLong { actual: usize, maximum: usize },
}

pub(crate) fn resolve_destination_query(
    query: &str,
    configured_aliases: &[SshHostAlias],
    maximum_token_bytes: usize,
) -> Result<DestinationQueryResolution, DestinationQueryError> {
    if query.len() > maximum_token_bytes {
        return Err(DestinationQueryError::TooLong {
            actual: query.len(),
            maximum: maximum_token_bytes,
        });
    }
    if !is_positive_literal(query) {
        return Err(DestinationQueryError::Invalid);
    }
    let destination =
        SshDestination::new(query.to_owned()).map_err(|_| DestinationQueryError::Invalid)?;
    if let Some(alias) = configured_aliases
        .iter()
        .find(|alias| alias.as_str() == query)
    {
        return Ok(DestinationQueryResolution::Configured {
            destination,
            alias: alias.clone(),
            explicit_user: None,
        });
    }
    let suffix = configured_aliases
        .iter()
        .filter_map(|alias| {
            let prefix = query.strip_suffix(alias.as_str())?.strip_suffix('@')?;
            is_positive_literal(prefix).then_some((alias, prefix))
        })
        .max_by_key(|(alias, _)| alias.as_str().len());
    if let Some((alias, explicit_user)) = suffix {
        return Ok(DestinationQueryResolution::Configured {
            destination,
            alias: alias.clone(),
            explicit_user: Some(explicit_user.to_owned()),
        });
    }
    Ok(DestinationQueryResolution::AddHost { destination })
}

fn is_positive_literal(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value.split('@').all(|component| !component.is_empty())
        && value.chars().all(|character| {
            character.is_alphanumeric() || matches!(character, '.' | '_' | '-' | '@')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alias(value: &str) -> SshHostAlias {
        SshHostAlias::new(value.to_owned()).unwrap()
    }

    #[test]
    fn alias_should_accept_positive_literal_tokens() {
        for value in ["work", "work.example", "build_host", "fedora@orb"] {
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
            "host[0]",
            "@host",
            "host@",
            "host@@orb",
            "host:22",
            "host/path",
        ] {
            assert_eq!(
                SshHostAlias::new(value.to_owned()),
                Err(SshHostAliasError::Invalid),
                "unexpectedly accepted {value:?}"
            );
        }
    }

    #[test]
    fn alias_should_enforce_the_injected_token_bound() {
        assert_eq!(
            SshHostAlias::new_bounded("alias".to_owned(), 4),
            Err(SshHostAliasError::TooLong {
                actual: 5,
                maximum: 4,
            })
        );
    }

    #[test]
    fn destination_query_should_prefer_an_exact_alias() {
        let aliases = [alias("orb"), alias("root@fedora@orb")];

        let resolution = resolve_destination_query("root@fedora@orb", &aliases, 128).unwrap();

        assert_eq!(
            resolution,
            DestinationQueryResolution::Configured {
                destination: SshDestination::new("root@fedora@orb".to_owned()).unwrap(),
                alias: alias("root@fedora@orb"),
                explicit_user: None,
            }
        );
    }

    #[test]
    fn destination_query_should_use_the_longest_configured_alias_suffix() {
        let aliases = [alias("orb"), alias("fedora@orb")];

        let resolution = resolve_destination_query("root@fedora@orb", &aliases, 128).unwrap();

        assert_eq!(
            resolution,
            DestinationQueryResolution::Configured {
                destination: SshDestination::new("root@fedora@orb".to_owned()).unwrap(),
                alias: alias("fedora@orb"),
                explicit_user: Some("root".to_owned()),
            }
        );
    }

    #[test]
    fn destination_query_should_preserve_a_multi_part_explicit_user() {
        let resolution =
            resolve_destination_query("name@realm@work", &[alias("work")], 128).unwrap();

        assert_eq!(
            resolution,
            DestinationQueryResolution::Configured {
                destination: SshDestination::new("name@realm@work".to_owned()).unwrap(),
                alias: alias("work"),
                explicit_user: Some("name@realm".to_owned()),
            }
        );
    }

    #[test]
    fn destination_query_should_return_add_host_for_an_unknown_safe_literal() {
        let resolution = resolve_destination_query("new-host", &[alias("work")], 128).unwrap();

        assert_eq!(
            resolution,
            DestinationQueryResolution::AddHost {
                destination: SshDestination::new("new-host".to_owned()).unwrap(),
            }
        );
    }

    #[test]
    fn destination_query_should_reject_unsafe_or_malformed_tokens() {
        for query in [
            "",
            "-oProxyCommand=x",
            "two words",
            "host*",
            "user@",
            "@host",
            "host:22",
            "host/path",
        ] {
            assert_eq!(
                resolve_destination_query(query, &[alias("host")], 128),
                Err(DestinationQueryError::Invalid),
                "unexpectedly accepted {query:?}"
            );
        }
    }

    #[test]
    fn destination_query_should_preserve_the_exact_query_token() {
        let resolution = resolve_destination_query("User@Work", &[alias("Work")], 128).unwrap();

        assert_eq!(resolution.destination().as_str(), "User@Work");
    }

    #[test]
    fn destination_query_should_enforce_the_injected_token_bound() {
        assert_eq!(
            resolve_destination_query("work", &[alias("work")], 3),
            Err(DestinationQueryError::TooLong {
                actual: 4,
                maximum: 3,
            })
        );
    }
}
