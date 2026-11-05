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

/// A version a source offers and the Update Floor it declares. A Release version names a floor
/// never newer than itself. A Preflight or Development version names a commit and has no order,
/// so it names no floor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    version: Version,
    floor: Option<Floor>,
}

impl Candidate {
    /// Refuses a Release `version` without a floor or older than its floor, and a development
    /// `version` with a floor.
    pub fn new(version: Version, floor: Option<Floor>) -> Result<Self, InvalidVersion> {
        match (version.release(), floor) {
            (Some(release), Some(floor)) if floor.0 <= release => {}
            (None, None) => {}
            _ => return Err(InvalidVersion),
        }
        Ok(Self { version, floor })
    }

    pub fn version(&self) -> &Version {
        &self.version
    }

    /// None exactly for a Preflight or Development version.
    pub fn floor(&self) -> Option<Floor> {
        self.floor
    }
}

/// Reads a field whose key must be present and whose value may be null, such as `floor` in a
/// build document. Serde would otherwise read a missing key as null.
pub(crate) fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
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
    /// The running version is older than the version's Update Floor, so the Core updates to
    /// `floor` first.
    #[error("SpaceTerm Core updates to its floor version first")]
    BelowFloor { floor: Floor },
}

/// Decides `candidate` against the version the Core runs, its Pending Version, and `skipped`,
/// the version it refuses until a newer one appears. The floor applies to the running version
/// alone: a restart starts from it, whatever waits as the Pending Version.
pub fn admit(
    candidate: &Candidate,
    running: &Version,
    pending: Option<&Version>,
    skipped: Option<&Version>,
) -> Result<(), Refusal> {
    let version = &candidate.version;
    if skipped == Some(version) {
        return Err(Refusal::Skipped);
    }
    let mut installed = std::iter::once(running).chain(pending);
    if installed.clone().any(|have| have == version) {
        return Err(Refusal::NotNewer);
    }
    let Some(release) = version.release() else {
        return Ok(());
    };
    if installed.any(|have| have.release().is_some_and(|have| have > release)) {
        return Err(Refusal::NotNewer);
    }
    if let Some(floor) = candidate.floor
        && !floor.admits(running)
    {
        return Err(Refusal::BelowFloor { floor });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn floored(text: &str, floor: &str) -> Candidate {
        Candidate::new(version(text), Some(Floor::parse(floor).unwrap())).unwrap()
    }

    /// A candidate whose floor every version meets, or a development one with none.
    fn candidate(text: &str) -> Candidate {
        let version = version(text);
        let floor = version.release().map(|_| Floor::parse("0.0.0").unwrap());
        Candidate::new(version, floor).unwrap()
    }

    #[test]
    fn release_versions_install_only_when_newer_than_every_installed_one() {
        let (running, pending) = (version("1.4.0"), version("1.5.2"));
        let admit = |text: &str| admit(&candidate(text), &running, Some(&pending), None);
        assert_eq!(admit("1.5.3"), Ok(()));
        assert_eq!(admit("2.0.0"), Ok(()));
        assert_eq!(
            admit("1.10.0"),
            Ok(()),
            "numbers compare as numbers, not text"
        );
        for refused in ["1.5.2", "1.5.1", "1.4.0", "1.4.9", "0.9.9"] {
            assert_eq!(admit(refused), Err(Refusal::NotNewer), "{refused}");
        }
    }

    #[test]
    fn a_skipped_version_is_refused_until_a_newer_one_appears() {
        let (running, skipped) = (version("1.4.0"), version("1.5.0"));
        assert_eq!(
            admit(&candidate("1.5.0"), &running, None, Some(&skipped)),
            Err(Refusal::Skipped)
        );
        assert_eq!(
            admit(
                &candidate("1.5.0"),
                &running,
                Some(&skipped),
                Some(&skipped)
            ),
            Err(Refusal::Skipped)
        );
        assert_eq!(
            admit(&candidate("1.5.1"), &running, None, Some(&skipped)),
            Ok(())
        );
    }

    #[test]
    fn a_release_is_refused_while_the_running_version_is_below_its_floor() {
        let floor = Floor::parse("1.1.0").unwrap();
        let below = Err(Refusal::BelowFloor { floor });
        let latest = floored("1.2.2", "1.1.0");
        assert_eq!(admit(&latest, &version("1.0.0"), None, None), below);
        assert_eq!(
            admit(&latest, &version("1.0.0"), Some(&version("1.1.0")), None),
            below,
            "a restart starts from the running version, not the Pending Version"
        );
        for running in ["1.1.0", "1.1.7", "1.2.1"] {
            assert_eq!(
                admit(&latest, &version(running), None, None),
                Ok(()),
                "{running}"
            );
        }
        assert_eq!(
            admit(&latest, &version("1.2.2"), None, None),
            Err(Refusal::NotNewer),
            "a version that is not newer is refused as such"
        );
        assert_eq!(
            admit(&latest, &version("dev.0123456789ab"), None, None),
            Ok(()),
            "a development version has no order, so no floor applies to it"
        );
    }

    #[test]
    fn a_release_names_a_floor_no_newer_than_itself_and_a_development_version_none() {
        let floor = |text: &str| Some(Floor::parse(text).unwrap());
        assert!(Candidate::new(version("1.4.0"), floor("1.4.1")).is_err());
        assert!(Candidate::new(version("1.4.0"), floor("1.4.0")).is_ok());
        assert!(Candidate::new(version("1.4.0"), None).is_err());
        assert!(Candidate::new(version("dev.0123456789ab"), None).is_ok());
        assert!(Candidate::new(version("dev.0123456789ab"), floor("0.0.0")).is_err());
        assert!(Floor::parse("dev.0123456789ab").is_err());
        assert!(Floor::parse("1.4").is_err());
        assert_eq!(Floor::parse("1.10.0").unwrap().to_string(), "1.10.0");
    }

    #[test]
    fn development_versions_refuse_only_an_installed_one() {
        let running = version("dev.0123456789ab");
        assert_eq!(
            admit(&candidate("dev.ba9876543210"), &running, None, None),
            Ok(())
        );
        assert_eq!(
            admit(&candidate("dev.0123456789ab.dirty"), &running, None, None),
            Ok(())
        );
        assert_eq!(
            admit(&candidate("dev.0123456789ab"), &running, None, None),
            Err(Refusal::NotNewer)
        );
        assert_eq!(
            admit(
                &candidate("dev.0123456789ab"),
                &version("dev.ba9876543210"),
                None,
                Some(&running)
            ),
            Err(Refusal::Skipped)
        );
        assert_eq!(
            admit(
                &candidate("dev.ba9876543210"),
                &version("9.9.9"),
                None,
                None
            ),
            Ok(()),
            "a development version never orders against a release one"
        );
        assert_eq!(admit(&candidate("1.0.0"), &running, None, None), Ok(()));
    }
}
