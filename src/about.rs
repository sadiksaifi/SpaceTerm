//! What every About surface says about the running build, shared by AppKit's About panel and
//! SpaceTerm's own About window.

use crate::application_identity::ApplicationIdentity;

/// The one sentence that says what SpaceTerm is.
pub(crate) const DESCRIPTION: &str = "A native, keyboard-first desktop terminal multiplexer.";

/// The copyright notice. Each macOS bundle template repeats it as `NSHumanReadableCopyright`,
/// which AppKit's About panel reads from the bundle rather than from the application.
pub(crate) const COPYRIGHT: &str = "Copyright © 2026 Sadik Saifi";

/// The About facts of one build.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct About {
    identity: ApplicationIdentity,
    marketing_version: &'static str,
    build_version: &'static str,
}

impl About {
    pub(crate) const fn current() -> Self {
        Self {
            identity: ApplicationIdentity::current(),
            marketing_version: env!("SPACETERM_BUNDLE_VERSION"),
            build_version: env!("SPACETERM_VERSION"),
        }
    }

    pub(crate) const fn identity(self) -> ApplicationIdentity {
        self.identity
    }

    pub(crate) const fn name(self) -> &'static str {
        self.identity.display_name()
    }

    /// The version in the form AppKit's About panel shows: the marketing version, followed by the
    /// build in parentheses only when the build names something the marketing version does not.
    pub(crate) fn version_line(self) -> String {
        if self.build_version == self.marketing_version {
            format!("Version {}", self.marketing_version)
        } else {
            format!(
                "Version {} ({})",
                self.marketing_version, self.build_version
            )
        }
    }

    pub(crate) const fn description(self) -> &'static str {
        DESCRIPTION
    }

    pub(crate) const fn copyright(self) -> &'static str {
        COPYRIGHT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn about(marketing_version: &'static str, build_version: &'static str) -> About {
        About {
            identity: ApplicationIdentity::current(),
            marketing_version,
            build_version,
        }
    }

    #[test]
    fn a_release_names_its_version_once() {
        assert_eq!(about("1.4.2", "1.4.2").version_line(), "Version 1.4.2");
    }

    #[test]
    fn a_source_build_follows_its_version_with_the_build() {
        assert_eq!(
            about("0.0.0", "dev.571e39794f9d.dirty").version_line(),
            "Version 0.0.0 (dev.571e39794f9d.dirty)"
        );
    }

    #[test]
    fn the_running_build_is_named_by_its_identity_and_build_identity() {
        let about = About::current();
        assert_eq!(about.name(), ApplicationIdentity::current().display_name());
        assert!(about.version_line().contains(env!("SPACETERM_VERSION")));
    }

    #[test]
    fn every_bundle_template_carries_the_shared_copyright() {
        for plist in [
            include_str!("../packaging/macos/spaceterm/Info.plist"),
            include_str!("../packaging/macos/preflight/Info.plist"),
            include_str!("../packaging/macos/development/Info.plist"),
        ] {
            let entry =
                format!("<key>NSHumanReadableCopyright</key>\n\t<string>{COPYRIGHT}</string>");
            assert!(plist.contains(&entry), "{plist}");
        }
    }
}
