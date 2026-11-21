//! Update Floors and candidate versions shared with the service lane.
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
pub fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}
