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
        )
    }

    const fn for_build(appearance_exerciser: bool, development_app: bool) -> Self {
        if appearance_exerciser {
            Self::appearance()
        } else if development_app {
            Self::development()
        } else {
            Self::production()
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

    #[test]
    fn application_identities_should_have_distinct_namespaces() {
        let identities = [
            ApplicationIdentity::production(),
            ApplicationIdentity::development(),
            ApplicationIdentity::appearance(),
        ];

        assert_eq!(
            identities.map(|identity| (identity.display_name(), identity.directory_name())),
            [
                ("SpaceTerm", "spaceterm"),
                ("SpaceTerm Dev", "spaceterm-dev"),
                ("SpaceTerm Appearance", "spaceterm-appearance-exerciser")
            ]
        );
    }

    #[test]
    fn build_features_should_select_one_identity() {
        assert_eq!(
            [
                ApplicationIdentity::for_build(false, false),
                ApplicationIdentity::for_build(false, true),
                ApplicationIdentity::for_build(true, false),
                ApplicationIdentity::for_build(true, true),
            ],
            [
                ApplicationIdentity::production(),
                ApplicationIdentity::development(),
                ApplicationIdentity::appearance(),
                ApplicationIdentity::appearance(),
            ]
        );
    }
}
