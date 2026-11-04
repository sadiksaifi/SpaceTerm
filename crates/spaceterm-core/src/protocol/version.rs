//! Protocol versions. The work protocol, the service lane and the Desktop Channel each carry
//! their own `major.minor`, and all three follow one rule: the caller speaks its current major
//! and the previous major, the listener speaks one major, and minors are additive within a major.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

/// This build's work protocol version. Each version is 1.0 at the first release.
pub const WORK: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };
/// This build's service lane version.
pub const SERVICE: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };
/// This build's Desktop Channel version.
pub const DESKTOP_CHANNEL: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };

/// The previous work major at its last minor, which this build still speaks as a caller. None
/// until a major 2 exists.
pub const PREVIOUS_WORK: Option<ProtocolVersion> = None;
/// The previous service major at its last minor.
pub const PREVIOUS_SERVICE: Option<ProtocolVersion> = None;
/// The previous Desktop Channel major at its last minor.
pub const PREVIOUS_DESKTOP_CHANNEL: Option<ProtocolVersion> = None;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Incompatible {
    /// The listener's major is below every major the caller speaks.
    ListenerTooOld,
    /// The listener's major is above every major the caller speaks.
    CallerTooOld,
}

/// The shared major at the lower of the two minors.
///
/// With no shared major, a listener above every offered major is `CallerTooOld` and any other
/// listener is `ListenerTooOld`. An empty offer shares nothing and counts as `CallerTooOld`.
pub fn negotiate(
    offered: &[ProtocolVersion],
    listener: ProtocolVersion,
) -> Result<ProtocolVersion, Incompatible> {
    if let Some(shared) = offered
        .iter()
        .find(|version| version.major == listener.major)
    {
        return Ok(ProtocolVersion {
            major: listener.major,
            minor: shared.minor.min(listener.minor),
        });
    }
    if offered.iter().all(|version| version.major < listener.major) {
        Err(Incompatible::CallerTooOld)
    } else {
        Err(Incompatible::ListenerTooOld)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn v(major: u16, minor: u16) -> ProtocolVersion {
        ProtocolVersion { major, minor }
    }

    #[test]
    fn negotiation_selects_the_shared_major_at_the_lower_minor() {
        assert_eq!(negotiate(&[v(2, 3), v(1, 7)], v(2, 1)), Ok(v(2, 1)));
        assert_eq!(negotiate(&[v(2, 3), v(1, 7)], v(2, 5)), Ok(v(2, 3)));
        assert_eq!(negotiate(&[v(2, 3), v(1, 7)], v(1, 9)), Ok(v(1, 7)));
    }

    #[test]
    fn negotiation_names_the_older_side_when_no_major_is_shared() {
        assert_eq!(
            negotiate(&[v(3, 0), v(2, 4)], v(1, 9)),
            Err(Incompatible::ListenerTooOld)
        );
        assert_eq!(
            negotiate(&[v(3, 0), v(2, 4)], v(4, 0)),
            Err(Incompatible::CallerTooOld)
        );
        assert_eq!(negotiate(&[], v(1, 0)), Err(Incompatible::CallerTooOld));
    }
}
