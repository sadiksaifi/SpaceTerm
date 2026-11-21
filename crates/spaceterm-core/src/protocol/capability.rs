//! Capabilities derived from the client's latest admissions.
use super::version::ProtocolVersion;
use super::{CoreVersions, Lane, Negotiated, Refused};
pub use spaceterm_protocol::protocol::capability::*;
/// The latest admission on each lane of one Machine, as its client saw them, from which the
/// Machine's [`Capabilities`] follow.
#[derive(Clone, Debug, Default)]
pub struct Admissions {
    work: Option<Result<ProtocolVersion, Unavailable>>,
    service: Option<Result<ProtocolVersion, Unavailable>>,
    core: Option<CoreVersions>,
}

impl Admissions {
    /// Records one admission on `lane`. An Offer, accepted or refused, also records the Core's
    /// versions. A failure before any Offer leaves the lane `NotConnected`.
    #[cfg_attr(
        not(any(test, feature = "client")),
        expect(dead_code, reason = "only the client library records admissions")
    )]
    pub(crate) fn record(&mut self, lane: Lane, result: &Result<Negotiated, Refused>) {
        let (outcome, core) = match result {
            Ok(negotiated) => (Ok(negotiated.selected), Some(&negotiated.core)),
            Err(Refused::Incompatible { older, core }) => (Err((*older).into()), Some(core)),
            Err(Refused::Failed(_)) => (Err(Unavailable::NotConnected), None),
        };
        match lane {
            Lane::Work => self.work = Some(outcome),
            Lane::Service => self.service = Some(outcome),
        }
        if let Some(core) = core {
            self.core = Some(core.clone());
        }
    }

    /// The Machine's capabilities. `work_live` tells whether the work connection that the last
    /// work admission opened is still live; a service-lane admission counts until the next one.
    pub fn capabilities(&self, work_live: bool) -> Capabilities {
        let work = match self.work {
            Some(Ok(_)) if !work_live => Err(Unavailable::NotConnected),
            Some(outcome) => outcome,
            None => Err(Unavailable::NotConnected),
        };
        Capabilities::new(work, self.service.unwrap_or(Err(Unavailable::NotConnected)))
    }

    /// The versions the Core offered at its latest admission on either lane, also when it
    /// refused the client. None before the first Offer.
    pub fn core(&self) -> Option<&CoreVersions> {
        self.core.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::version::Incompatible;

    const fn v(major: u16, minor: u16) -> ProtocolVersion {
        ProtocolVersion { major, minor }
    }

    fn core(work: ProtocolVersion) -> CoreVersions {
        CoreVersions {
            work,
            ..crate::protocol::versions_for_tests()
        }
    }

    #[test]
    fn a_machines_capabilities_follow_its_latest_admission_on_each_lane() {
        let mut admissions = Admissions::default();
        assert_eq!(
            admissions.capabilities(false).check(Capability::Restart),
            Err(Unavailable::NotConnected)
        );
        admissions.record(
            Lane::Work,
            &Err(Refused::Incompatible {
                older: Incompatible::ListenerTooOld,
                core: core(v(1, 0)),
            }),
        );
        admissions.record(
            Lane::Service,
            &Ok(Negotiated {
                selected: v(1, 0),
                core: core(v(1, 0)),
            }),
        );
        let capabilities = admissions.capabilities(false);
        assert_eq!(
            capabilities.check(Capability::SwapPanes),
            Err(Unavailable::CoreTooOld)
        );
        assert_eq!(capabilities.check(Capability::Install), Ok(()));
        assert_eq!(admissions.core(), Some(&core(v(1, 0))));

        admissions.record(
            Lane::Work,
            &Err(Refused::Incompatible {
                older: Incompatible::CallerTooOld,
                core: core(v(3, 0)),
            }),
        );
        assert_eq!(
            admissions.capabilities(false).check(Capability::MergeTab),
            Err(Unavailable::ClientTooOld)
        );
        assert_eq!(admissions.core(), Some(&core(v(3, 0))));

        admissions.record(
            Lane::Work,
            &Ok(Negotiated {
                selected: v(1, 0),
                core: core(v(1, 0)),
            }),
        );
        assert_eq!(
            admissions.capabilities(true).check(Capability::MergeTab),
            Ok(())
        );
        assert_eq!(
            admissions.capabilities(false).check(Capability::MergeTab),
            Err(Unavailable::NotConnected),
            "a work link that ended is not connected"
        );
        admissions.record(
            Lane::Service,
            &Err(Refused::Failed(crate::CoreError::NotRunning)),
        );
        assert_eq!(
            admissions.capabilities(true).check(Capability::Uninstall),
            Err(Unavailable::NotConnected)
        );
        assert_eq!(admissions.core(), Some(&core(v(1, 0))));
    }
}
