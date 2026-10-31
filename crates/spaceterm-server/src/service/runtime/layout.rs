//! Where a server package keeps its executable and resources on each target.
//!
//! A Linux package keeps `bin/spaceterm` beside `share/spaceterm`. A macOS package is an app-like
//! bundle named for its identity, holding `Contents/MacOS/spaceterm` and `Contents/Resources`.
//! macOS attributes the System Permissions of a server's Terminal Sessions to that bundle's
//! identifier and signature, not to an installation's path, so a grant follows the server from one
//! installation to the next. [ADR 0061](../../../../../docs/adr/0061-own-system-permissions-in-the-server-package.md)
//! records the decision.
use std::path::{Path, PathBuf};

const LINUX: &str = "x86_64-unknown-linux-gnu";
const MACOS: &str = "aarch64-apple-darwin";
/// The prefix `packaging/server-resources.json` names every resource under.
const SHARED: &str = "share/spaceterm";
const EXECUTABLE: &str = "spaceterm";

/// One identity's package layout for one target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Layout {
    bundle: Option<&'static str>,
}

impl Layout {
    /// `None` for a target or identity no package exists for.
    pub fn for_target(target: &str, identity: &str) -> Option<Self> {
        match target {
            LINUX => Some(Self { bundle: None }),
            MACOS => bundle_name(identity).map(|bundle| Self {
                bundle: Some(bundle),
            }),
            _ => None,
        }
    }

    /// The layout of packages this machine runs.
    pub fn host(identity: &str) -> Option<Self> {
        Self::for_target(super::TARGET, identity)
    }

    /// The executable, relative to the package root.
    pub fn executable(self) -> PathBuf {
        match self.bundle {
            None => Path::new("bin").join(EXECUTABLE),
            Some(bundle) => Path::new(bundle).join("Contents/MacOS").join(EXECUTABLE),
        }
    }

    /// The resource directory, relative to the package root.
    pub fn resources(self) -> PathBuf {
        match self.bundle {
            None => PathBuf::from(SHARED),
            Some(bundle) => Path::new(bundle).join("Contents/Resources"),
        }
    }

    /// Places a resource the tracked list names under `share/spaceterm` in this layout.
    pub fn resource(self, listed: &Path) -> Option<PathBuf> {
        listed
            .strip_prefix(SHARED)
            .ok()
            .map(|relative| self.resources().join(relative))
    }

    /// The bundle's own files: its Info.plist and the seal codesign writes. Empty without a
    /// bundle.
    pub fn bundle_files(self) -> Vec<PathBuf> {
        self.bundle
            .map(|bundle| {
                vec![
                    Path::new(bundle).join("Contents/Info.plist"),
                    Path::new(bundle).join("Contents/_CodeSignature/CodeResources"),
                ]
            })
            .unwrap_or_default()
    }

    /// What code signing covers: the bundle, or the executable without one.
    pub fn signed(self) -> PathBuf {
        self.bundle.map_or_else(|| self.executable(), PathBuf::from)
    }
}

/// The macOS bundle directory of an identity's server package, named for the identity's product.
/// Privacy prompts and System Settings show a bundle's Launch Services display name, which is its
/// directory name, not `CFBundleName` or `CFBundleDisplayName`. A person therefore sees the
/// product's name: SpaceTerm, SpaceTerm Preflight or SpaceTerm Development. The system hides the
/// `.app` extension unless the person turned on "Show all filename extensions".
pub fn bundle_name(identity: &str) -> Option<&'static str> {
    match identity {
        "spaceterm" => Some("SpaceTerm.app"),
        "spaceterm-preflight" => Some("SpaceTerm Preflight.app"),
        "spaceterm-development" => Some("SpaceTerm Development.app"),
        _ => None,
    }
}

/// The bundle identifier and code signing identifier of an identity's server package.
pub fn bundle_identifier(identity: &str) -> String {
    format!("io.github.sadiksaifi.{identity}.server")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_macos_package_is_a_bundle_named_for_its_identity() {
        let layout = Layout::for_target(MACOS, "spaceterm-preflight").unwrap();
        assert_eq!(
            layout.executable(),
            Path::new("SpaceTerm Preflight.app/Contents/MacOS/spaceterm")
        );
        assert_eq!(
            layout.resource(Path::new("share/spaceterm/shell-integration/VERSION")),
            Some(PathBuf::from(
                "SpaceTerm Preflight.app/Contents/Resources/shell-integration/VERSION"
            ))
        );
        assert_eq!(layout.signed(), Path::new("SpaceTerm Preflight.app"));
        assert_eq!(layout.bundle_files().len(), 2);
    }

    #[test]
    fn a_linux_package_keeps_bin_beside_share() {
        let layout = Layout::for_target(LINUX, "spaceterm").unwrap();
        assert_eq!(layout.executable(), Path::new("bin/spaceterm"));
        assert_eq!(layout.signed(), Path::new("bin/spaceterm"));
        assert_eq!(
            layout.resource(Path::new("share/spaceterm/THIRD-PARTY-NOTICES.txt")),
            Some(PathBuf::from("share/spaceterm/THIRD-PARTY-NOTICES.txt"))
        );
        assert!(layout.bundle_files().is_empty());
    }

    #[test]
    fn an_unknown_target_or_identity_has_no_layout() {
        assert_eq!(
            Layout::for_target("riscv64gc-unknown-linux-gnu", "spaceterm"),
            None
        );
        assert_eq!(Layout::for_target(MACOS, "spaceterm-other"), None);
    }
}
