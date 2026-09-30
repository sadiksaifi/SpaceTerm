#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ApplicationIdentity {
    display_name: &'static str,
    directory_name: &'static str,
}

impl ApplicationIdentity {
    pub(crate) const fn current() -> Self {
        Self::for_build(
            cfg!(feature = "appearance-exerciser"),
            cfg!(feature = "development-app"),
            cfg!(spaceterm_release),
        )
    }

    /// Only a validated release tag selects the production identity, so an untagged build can
    /// never replace or share state with a release installation.
    const fn for_build(appearance_exerciser: bool, development_app: bool, release: bool) -> Self {
        if appearance_exerciser {
            Self::appearance()
        } else if development_app {
            Self::development()
        } else if release {
            Self::production()
        } else {
            Self::preflight()
        }
    }

    pub(crate) const fn display_name(self) -> &'static str {
        self.display_name
    }

    pub(crate) const fn directory_name(self) -> &'static str {
        self.directory_name
    }

    const fn production() -> Self {
        Self {
            display_name: "SpaceTerm",
            directory_name: "spaceterm",
        }
    }

    const fn development() -> Self {
        Self {
            display_name: "SpaceTerm Dev",
            directory_name: "spaceterm-dev",
        }
    }

    const fn preflight() -> Self {
        Self {
            display_name: "SpaceTerm Preflight",
            directory_name: "spaceterm-preflight",
        }
    }

    const fn appearance() -> Self {
        Self {
            display_name: "SpaceTerm Appearance",
            directory_name: "spaceterm-appearance-exerciser",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRODUCTION_INFO_PLIST: &str = include_str!("../packaging/macos/Info.plist");
    const PREFLIGHT_INFO_PLIST: &str = include_str!("../packaging/macos/Preflight-Info.plist");
    const IDENTITY_KEYS: [&str; 4] = [
        "CFBundleDisplayName",
        "CFBundleExecutable",
        "CFBundleIdentifier",
        "CFBundleName",
    ];

    /// Returns each top-level key with its raw value markup, in document order.
    fn plist_entries(plist: &str) -> Vec<(&str, &str)> {
        let body = plist
            .split_once("<dict>")
            .and_then(|(_, rest)| rest.rsplit_once("</dict>"))
            .map(|(body, _)| body)
            .expect("Info.plist should contain one dictionary");
        body.split("<key>")
            .skip(1)
            .map(|entry| {
                let (key, value) = entry
                    .split_once("</key>")
                    .expect("every key should be closed");
                (key, value.trim())
            })
            .collect()
    }

    fn plist_string<'a>(plist: &'a str, key: &str) -> &'a str {
        plist_entries(plist)
            .into_iter()
            .find(|(candidate, _)| *candidate == key)
            .and_then(|(_, value)| value.strip_prefix("<string>")?.strip_suffix("</string>"))
            .unwrap_or_else(|| panic!("Info.plist should contain the string {key}"))
    }

    #[test]
    fn application_identities_should_have_distinct_namespaces() {
        let identities = [
            ApplicationIdentity::production(),
            ApplicationIdentity::preflight(),
            ApplicationIdentity::development(),
            ApplicationIdentity::appearance(),
        ];

        assert_eq!(
            identities.map(|identity| (identity.display_name(), identity.directory_name())),
            [
                ("SpaceTerm", "spaceterm"),
                ("SpaceTerm Preflight", "spaceterm-preflight"),
                ("SpaceTerm Dev", "spaceterm-dev"),
                ("SpaceTerm Appearance", "spaceterm-appearance-exerciser")
            ]
        );
    }

    #[test]
    fn build_inputs_should_select_one_identity() {
        assert_eq!(
            [
                ApplicationIdentity::for_build(false, false, true),
                ApplicationIdentity::for_build(false, false, false),
                ApplicationIdentity::for_build(false, true, false),
                ApplicationIdentity::for_build(true, false, false),
                ApplicationIdentity::for_build(true, true, false),
            ],
            [
                ApplicationIdentity::production(),
                ApplicationIdentity::preflight(),
                ApplicationIdentity::development(),
                ApplicationIdentity::appearance(),
                ApplicationIdentity::appearance(),
            ]
        );
    }

    #[test]
    fn bundle_templates_should_name_their_identity() {
        let templates = [
            (
                ApplicationIdentity::production(),
                PRODUCTION_INFO_PLIST,
                "io.github.sadiksaifi.spaceterm",
            ),
            (
                ApplicationIdentity::preflight(),
                PREFLIGHT_INFO_PLIST,
                "io.github.sadiksaifi.spaceterm-preflight",
            ),
            (
                ApplicationIdentity::development(),
                include_str!("../packaging/macos/Development-Info.plist"),
                "io.github.sadiksaifi.spaceterm-dev",
            ),
            (
                ApplicationIdentity::appearance(),
                include_str!("../packaging/macos/AppearanceExerciser-Info.plist"),
                "io.github.sadiksaifi.spaceterm.appearance-exerciser",
            ),
        ];

        for (identity, plist, bundle_identifier) in templates {
            // The menu bar title comes from the bundle; its menu items come from the identity.
            for key in ["CFBundleName", "CFBundleDisplayName", "CFBundleExecutable"] {
                assert_eq!(plist_string(plist, key), identity.display_name(), "{key}");
            }
            assert_eq!(plist_string(plist, "CFBundleIdentifier"), bundle_identifier);
        }
    }

    #[test]
    fn preflight_bundle_template_should_differ_from_production_only_by_identity() {
        let without_identity = |plist| {
            plist_entries(plist)
                .into_iter()
                .filter(|(key, _)| !IDENTITY_KEYS.contains(key))
                .collect::<Vec<_>>()
        };

        assert_eq!(
            without_identity(PREFLIGHT_INFO_PLIST),
            without_identity(PRODUCTION_INFO_PLIST)
        );
    }
}
