/// Where an identity receives application updates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UpdateSource {
    /// The signed release feed.
    SignedFeed,
    /// A scripted preview of the update interface that installs nothing.
    Simulation,
    Unavailable,
}

/// Expands one display name into the name and the version label that repeats it, so the two cannot
/// disagree.
macro_rules! named {
    ($display_name:literal) => {
        (
            $display_name,
            concat!($display_name, " ", env!("SPACETERM_VERSION")),
        )
    };
}

/// One identity's Icon Composer document and the layer artwork it names.
///
/// The macOS bundle compiles the document into the application icon. A surface that draws the icon
/// itself renders the same document, so every host shows one design per identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ApplicationIcon {
    /// The document's `icon.json`.
    pub(crate) document: &'static str,
    /// Each layer's SVG artwork, keyed by the `image-name` the document gives it.
    pub(crate) artwork: &'static [(&'static str, &'static str)],
}

/// Embeds one identity's Icon Composer document from its bundle template directory.
macro_rules! icon {
    ($directory:literal, [$($artwork:literal),+ $(,)?]) => {
        ApplicationIcon {
            document: include_str!(concat!("../packaging/macos/", $directory, "/icon.json")),
            artwork: &[$((
                $artwork,
                include_str!(concat!("../packaging/macos/", $directory, "/Assets/", $artwork)),
            )),+],
        }
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ApplicationIdentity {
    display_name: &'static str,
    version_label: &'static str,
    directory_name: &'static str,
    application_id: &'static str,
    icon: ApplicationIcon,
    update_source: UpdateSource,
    microphone_access: bool,
}

impl ApplicationIdentity {
    pub(crate) const fn current() -> Self {
        Self::for_build(cfg!(spaceterm_packaged), cfg!(spaceterm_release))
    }

    /// Only the packaging script leaves SpaceTerm Development, and only a validated release tag
    /// selects SpaceTerm, so no other build can replace or share state with a release installation.
    const fn for_build(packaged: bool, release: bool) -> Self {
        match (packaged, release) {
            (false, _) => Self::development(),
            (true, false) => Self::preflight(),
            (true, true) => Self::production(),
        }
    }

    pub(crate) const fn display_name(self) -> &'static str {
        self.display_name
    }

    /// The display name followed by the build's version, as Settings presents the running build.
    pub(crate) const fn version_label(self) -> &'static str {
        self.version_label
    }

    pub(crate) const fn directory_name(self) -> &'static str {
        self.directory_name
    }

    /// The shared desktop identity used by Wayland, X11, D-Bus, and the desktop entry.
    pub(crate) const fn application_id(self) -> &'static str {
        self.application_id
    }

    pub(crate) const fn icon(self) -> ApplicationIcon {
        self.icon
    }

    /// The bundle identifier the Operating System keys this identity's privacy grants to.
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only the native permission host consumes this identity policy"
        )
    )]
    pub(crate) const fn bundle_identifier(self) -> &'static str {
        self.application_id()
    }

    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only the native updater host consumes this identity policy"
        )
    )]
    pub(crate) const fn update_source(self) -> UpdateSource {
        self.update_source
    }

    /// SpaceTerm Development is re-signed ad hoc on every build, so privacy grants would not
    /// persist.
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only the native microphone host consumes this identity policy"
        )
    )]
    pub(crate) const fn microphone_access(self) -> bool {
        self.microphone_access
    }

    const fn production() -> Self {
        let (display_name, version_label) = named!("SpaceTerm");
        Self {
            display_name,
            version_label,
            directory_name: "spaceterm",
            application_id: "io.github.sadiksaifi.spaceterm",
            icon: icon!("spaceterm/SpaceTerm.icon", ["Glyph.svg"]),
            update_source: UpdateSource::SignedFeed,
            microphone_access: true,
        }
    }

    const fn preflight() -> Self {
        let (display_name, version_label) = named!("SpaceTerm Preflight");
        Self {
            display_name,
            version_label,
            directory_name: "spaceterm-preflight",
            application_id: "io.github.sadiksaifi.spaceterm-preflight",
            icon: icon!(
                "preflight/SpaceTerm Preflight.icon",
                ["Channel Mark.svg", "Glyph.svg"]
            ),
            update_source: UpdateSource::Unavailable,
            microphone_access: true,
        }
    }

    const fn development() -> Self {
        let (display_name, version_label) = named!("SpaceTerm Development");
        Self {
            display_name,
            version_label,
            directory_name: "spaceterm-development",
            application_id: "io.github.sadiksaifi.spaceterm-development",
            icon: icon!(
                "development/SpaceTerm Development.icon",
                ["Channel Mark.svg", "Glyph.svg"]
            ),
            update_source: UpdateSource::Simulation,
            // Each rebuild is signed ad hoc again, which invalidates a microphone grant.
            microphone_access: false,
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::ApplicationIdentity;

    /// Every identity a build can carry: production, preflight, and development.
    pub(crate) const fn all() -> [ApplicationIdentity; 3] {
        [
            ApplicationIdentity::production(),
            ApplicationIdentity::preflight(),
            ApplicationIdentity::development(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRODUCTION_INFO_PLIST: &str = include_str!("../packaging/macos/spaceterm/Info.plist");
    const PREFLIGHT_INFO_PLIST: &str = include_str!("../packaging/macos/preflight/Info.plist");
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
        ];

        assert_eq!(
            identities.map(|identity| (identity.display_name(), identity.directory_name())),
            [
                ("SpaceTerm", "spaceterm"),
                ("SpaceTerm Preflight", "spaceterm-preflight"),
                ("SpaceTerm Development", "spaceterm-development"),
            ]
        );
    }

    #[test]
    fn version_labels_should_name_their_own_identity() {
        let identities = [
            ApplicationIdentity::production(),
            ApplicationIdentity::preflight(),
            ApplicationIdentity::development(),
        ];

        assert_eq!(
            identities.map(ApplicationIdentity::version_label),
            [
                concat!("SpaceTerm ", env!("SPACETERM_VERSION")),
                concat!("SpaceTerm Preflight ", env!("SPACETERM_VERSION")),
                concat!("SpaceTerm Development ", env!("SPACETERM_VERSION")),
            ]
        );
    }

    #[test]
    fn only_packaging_should_leave_the_development_identity() {
        assert_eq!(
            [
                ApplicationIdentity::for_build(false, false),
                ApplicationIdentity::for_build(true, false),
                ApplicationIdentity::for_build(true, true),
            ],
            [
                ApplicationIdentity::development(),
                ApplicationIdentity::preflight(),
                ApplicationIdentity::production(),
            ]
        );
    }

    #[test]
    fn application_identities_should_own_their_distribution_policy() {
        let identities = [
            ApplicationIdentity::production(),
            ApplicationIdentity::preflight(),
            ApplicationIdentity::development(),
        ];

        assert_eq!(
            identities.map(|identity| (identity.update_source(), identity.microphone_access())),
            [
                (UpdateSource::SignedFeed, true),
                (UpdateSource::Unavailable, true),
                (UpdateSource::Simulation, false),
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
                include_str!("../packaging/macos/development/Info.plist"),
                "io.github.sadiksaifi.spaceterm-development",
            ),
        ];

        for (identity, plist, bundle_identifier) in templates {
            // The menu bar title comes from the bundle; its menu items come from the identity.
            for key in ["CFBundleName", "CFBundleDisplayName", "CFBundleExecutable"] {
                assert_eq!(plist_string(plist, key), identity.display_name(), "{key}");
            }
            assert_eq!(plist_string(plist, "CFBundleIdentifier"), bundle_identifier);
            assert_eq!(identity.application_id(), bundle_identifier);
            assert_eq!(identity.bundle_identifier(), bundle_identifier);
        }
    }

    /// All icons share the release glyph and rank by distance from a release with one dot per
    /// step, which survives the Dark, Clear, and Tinted appearances that replace their colors.
    #[test]
    fn identity_icons_should_share_the_glyph_and_rank_by_channel_marks() {
        let icons = [
            ApplicationIdentity::production(),
            ApplicationIdentity::preflight(),
            ApplicationIdentity::development(),
        ]
        .map(ApplicationIdentity::icon);
        let artwork = |icon: ApplicationIcon, name: &str| {
            icon.artwork
                .iter()
                .find_map(|(candidate, artwork)| (*candidate == name).then_some(*artwork))
        };

        let glyphs = icons.map(|icon| artwork(icon, "Glyph.svg").expect("every icon has a glyph"));
        assert!(glyphs.iter().all(|glyph| *glyph == glyphs[0]));
        assert_eq!(
            icons.map(|icon| artwork(icon, "Channel Mark.svg")
                .map_or(0, |mark| mark.matches("<circle").count())),
            [0, 1, 2]
        );
        // The embedded artwork is exactly what each document names.
        for icon in icons {
            let named = icon.document.matches("\"image-name\"").count();
            assert_eq!(named, icon.artwork.len());
            for (name, _) in icon.artwork {
                assert!(
                    icon.document
                        .contains(&format!("\"image-name\" : \"{name}\""))
                );
            }
        }
    }

    #[test]
    fn preflight_bundle_template_should_differ_from_production_only_by_identity_and_updates() {
        // Only release packages use the production template, and only they carry Sparkle.
        let without_identity = |plist| {
            plist_entries(plist)
                .into_iter()
                .filter(|(key, _)| !IDENTITY_KEYS.contains(key) && !key.starts_with("SU"))
                .collect::<Vec<_>>()
        };

        assert_eq!(
            without_identity(PREFLIGHT_INFO_PLIST),
            without_identity(PRODUCTION_INFO_PLIST)
        );
        assert!(
            plist_entries(PREFLIGHT_INFO_PLIST)
                .iter()
                .all(|(key, _)| !key.starts_with("SU"))
        );
    }
}
