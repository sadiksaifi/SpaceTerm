//! Version ordering: which versions a Core installs, from any source. Its feed, a desktop push
//! and a local archive all pass through [`admit`], so no source installs a version that is not
//! newer than the newest one the Core has, the skipped one, or one whose Update Floor is above
//! the running version.
//!
//! Release versions order by `MAJOR.MINOR.PATCH`. Preflight and Development versions name a
//! commit and have no order, so among them only equality refuses, and floors never apply to
//! them. A Release version and a development version never order against each other.
use super::facts::{InvalidVersion, Version};
use serde::{Deserialize, Serialize};
use std::fmt;

/// An Update Floor: the oldest Release version allowed to update directly to a release. JSON:
/// `"MAJOR.MINOR.PATCH"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Floor([u64; 3]);

impl Floor {
    /// Parses `MAJOR.MINOR.PATCH`. Any other text, a development version included, is refused.
    pub fn parse(text: &str) -> Result<Self, InvalidVersion> {
        Version::parse(text)?
            .release()
            .map(Self)
            .ok_or(InvalidVersion)
    }

    /// Whether `running` may update directly to a release with this floor. A development
    /// version has no order, so every floor admits it.
    pub fn admits(self, running: &Version) -> bool {
        running.release().is_none_or(|running| running >= self.0)
    }

    /// The release this floor names, whose own feed an updater fetches next.
    pub fn version(self) -> Version {
        Version::parse(&self.to_string()).expect("a floor is a release version")
    }
}

impl fmt::Display for Floor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [major, minor, patch] = self.0;
        write!(formatter, "{major}.{minor}.{patch}")
    }
}

impl TryFrom<String> for Floor {
    type Error = InvalidVersion;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}

impl From<Floor> for String {
    fn from(floor: Floor) -> Self {
        floor.to_string()
    }
}

/// A version a source offers and the Update Floor it declares. A Release version's floor is
/// never newer than the version itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    version: Version,
    floor: Floor,
}

impl Candidate {
    /// Refuses a Release `version` older than its `floor`.
    pub fn new(version: Version, floor: Floor) -> Result<Self, InvalidVersion> {
        if version.release().is_some_and(|release| release < floor.0) {
            return Err(InvalidVersion);
        }
        Ok(Self { version, floor })
    }

    pub fn version(&self) -> &Version {
        &self.version
    }

    pub fn floor(&self) -> Floor {
        self.floor
    }
}

/// Why the Core refuses a version from every source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// The Core runs this version or a newer one, or has one of them as its Pending Version.
    #[error("SpaceTerm Core has this version or a newer one")]
    NotNewer,
    /// The version failed to start and is skipped until a newer one appears.
    #[error("this SpaceTerm Core version failed to start and is skipped")]
    Skipped,
}

/// Decides `candidate` against `installed`, the versions the Core runs and has as its Pending
/// Version, and `skipped`, the version it refuses until a newer one appears.
pub fn admit(
    candidate: &Version,
    installed: &[Version],
    skipped: Option<&Version>,
) -> Result<(), Refusal> {
    if skipped == Some(candidate) {
        return Err(Refusal::Skipped);
    }
    if installed.contains(candidate) {
        return Err(Refusal::NotNewer);
    }
    let Some(release) = candidate.release() else {
        return Ok(());
    };
    if installed
        .iter()
        .filter_map(Version::release)
        .any(|have| have > release)
    {
        return Err(Refusal::NotNewer);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn release_versions_install_only_when_newer_than_every_installed_one() {
        let installed = [version("1.4.0"), version("1.5.2")];
        assert_eq!(admit(&version("1.5.3"), &installed, None), Ok(()));
        assert_eq!(admit(&version("2.0.0"), &installed, None), Ok(()));
        assert_eq!(
            admit(&version("1.10.0"), &installed, None),
            Ok(()),
            "numbers compare as numbers, not text"
        );
        for refused in ["1.5.2", "1.5.1", "1.4.0", "1.4.9", "0.9.9"] {
            assert_eq!(
                admit(&version(refused), &installed, None),
                Err(Refusal::NotNewer),
                "{refused}"
            );
        }
        assert_eq!(admit(&version("1.0.0"), &[], None), Ok(()));
    }

    #[test]
    fn a_skipped_version_is_refused_until_a_newer_one_appears() {
        let skipped = version("1.5.0");
        assert_eq!(
            admit(&skipped, &[version("1.4.0")], Some(&skipped)),
            Err(Refusal::Skipped)
        );
        assert_eq!(
            admit(
                &skipped,
                &[version("1.4.0"), skipped.clone()],
                Some(&skipped)
            ),
            Err(Refusal::Skipped)
        );
        assert_eq!(
            admit(&version("1.5.1"), &[version("1.4.0")], Some(&skipped)),
            Ok(())
        );
    }

    #[test]
    fn development_versions_refuse_only_an_installed_one() {
        let running = version("dev.0123456789ab");
        assert_eq!(
            admit(
                &version("dev.ba9876543210"),
                std::slice::from_ref(&running),
                None
            ),
            Ok(())
        );
        assert_eq!(
            admit(
                &version("dev.0123456789ab.dirty"),
                std::slice::from_ref(&running),
                None
            ),
            Ok(())
        );
        assert_eq!(
            admit(&running, std::slice::from_ref(&running), None),
            Err(Refusal::NotNewer)
        );
        assert_eq!(
            admit(&running, &[version("dev.ba9876543210")], Some(&running)),
            Err(Refusal::Skipped)
        );
        assert_eq!(
            admit(&version("dev.ba9876543210"), &[version("9.9.9")], None),
            Ok(()),
            "a development version never orders against a release one"
        );
        assert_eq!(admit(&version("1.0.0"), &[running], None), Ok(()));
    }
}
