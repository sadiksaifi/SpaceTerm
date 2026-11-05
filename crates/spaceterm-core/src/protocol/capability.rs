//! Capabilities: actions whose availability depends on the Core's protocol version.
//!
//! Capabilities travel as no separate message. Admission's selected version reports them, and
//! both sides read the one table in [`Capability::since`], so they can't disagree.
use super::Lane;
use super::admission::{CoreVersions, Negotiated, Refused};
use super::version::{Incompatible, ProtocolVersion};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Work: the UpdateFacts message. Service: Facts and WaitFacts.
    UpdateFacts,
    /// Service: StageFetch, and StageUpload with UploadChunk for a desktop push.
    Install,
    /// Service: Preview and Execute Restart, into the Pending Version when one waits.
    Restart,
    /// Service: Preview and Execute Uninstall. It uninstalls SpaceTerm Core on that Machine and
    /// revokes every pairing (Remove Machine).
    Uninstall,
    /// Work: swap two Panes.
    SwapPanes,
    /// Work: move a Pane to another Tab.
    MovePaneToTab,
    /// Work: merge a Tab back into another Tab as a Pane.
    MergeTab,
}

impl Capability {
    /// Every capability, in declaration order.
    pub const ALL: [Self; 7] = [
        Self::UpdateFacts,
        Self::Install,
        Self::Restart,
        Self::Uninstall,
        Self::SwapPanes,
        Self::MovePaneToTab,
        Self::MergeTab,
    ];

    /// The lane that carries it and the first version that has it.
    pub const fn since(self) -> (Lane, ProtocolVersion) {
        const V1_0: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };
        match self {
            Self::UpdateFacts | Self::SwapPanes | Self::MovePaneToTab | Self::MergeTab => {
                (Lane::Work, V1_0)
            }
            Self::Install | Self::Restart | Self::Uninstall => (Lane::Service, V1_0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// The Core's selected version lacks it, or no major is shared and the Core is older.
    CoreTooOld,
    /// No major is shared and the Core is newer.
    ClientTooOld,
    /// No connection on that lane: stopped, unreachable or not yet admitted.
    NotConnected,
}

impl From<Incompatible> for Unavailable {
    /// The client is the caller, so a listener too old is the Core.
    fn from(older: Incompatible) -> Self {
        match older {
            Incompatible::ListenerTooOld => Self::CoreTooOld,
            Incompatible::CallerTooOld => Self::ClientTooOld,
        }
    }
}

/// What one Machine's connections can do, from their negotiated versions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    work: Result<ProtocolVersion, Unavailable>,
    service: Result<ProtocolVersion, Unavailable>,
    /// Capabilities a fake withholds as `CoreTooOld` whatever the versions.
    #[cfg(any(test, feature = "developer-tools"))]
    withheld: Vec<Capability>,
}

impl Capabilities {
    pub fn new(
        work: Result<ProtocolVersion, Unavailable>,
        service: Result<ProtocolVersion, Unavailable>,
    ) -> Self {
        Self {
            work,
            service,
            #[cfg(any(test, feature = "developer-tools"))]
            withheld: Vec::new(),
        }
    }

    /// The selected work version, or why there is none.
    pub fn work(&self) -> Result<ProtocolVersion, Unavailable> {
        self.work
    }

    /// The selected service lane version, or why there is none.
    pub fn service(&self) -> Result<ProtocolVersion, Unavailable> {
        self.service
    }

    /// Ok when the capability's lane is admitted and its selected version is at least
    /// [`Capability::since`].
    pub fn check(&self, capability: Capability) -> Result<(), Unavailable> {
        #[cfg(any(test, feature = "developer-tools"))]
        if self.withheld.contains(&capability) {
            return Err(Unavailable::CoreTooOld);
        }
        let (lane, since) = capability.since();
        let selected = match lane {
            Lane::Work => self.work,
            Lane::Service => self.service,
        }?;
        if selected >= since {
            Ok(())
        } else {
            Err(Unavailable::CoreTooOld)
        }
    }

    /// A set with exactly these capabilities, for fakes and tests. Every other capability is
    /// `CoreTooOld`.
    #[cfg(any(test, feature = "developer-tools"))]
    pub fn fake(available: &[Capability]) -> Self {
        Self {
            work: Ok(super::version::WORK),
            service: Ok(super::version::SERVICE),
            withheld: Capability::ALL
                .into_iter()
                .filter(|capability| !available.contains(capability))
                .collect(),
        }
    }
}

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

    const fn v(major: u16, minor: u16) -> ProtocolVersion {
        ProtocolVersion { major, minor }
    }

    #[test]
    fn a_capability_needs_its_lane_admitted_at_its_version() {
        let capabilities = Capabilities::new(Ok(v(1, 0)), Err(Unavailable::ClientTooOld));
        assert_eq!(capabilities.check(Capability::SwapPanes), Ok(()));
        assert_eq!(capabilities.check(Capability::UpdateFacts), Ok(()));
        assert_eq!(
            capabilities.check(Capability::Uninstall),
            Err(Unavailable::ClientTooOld)
        );
        let capabilities = Capabilities::new(Err(Unavailable::NotConnected), Ok(v(1, 0)));
        assert_eq!(capabilities.check(Capability::Restart), Ok(()));
        assert_eq!(
            capabilities.check(Capability::MergeTab),
            Err(Unavailable::NotConnected)
        );
        let capabilities = Capabilities::new(Ok(v(0, 9)), Ok(v(0, 9)));
        assert_eq!(
            capabilities.check(Capability::Install),
            Err(Unavailable::CoreTooOld)
        );
    }

    fn core(work: ProtocolVersion) -> CoreVersions {
        CoreVersions {
            work,
            ..CoreVersions::for_tests()
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

    #[test]
    fn a_fake_has_exactly_the_capabilities_it_names() {
        let capabilities = Capabilities::fake(&[Capability::Restart, Capability::MergeTab]);
        assert_eq!(capabilities.check(Capability::Restart), Ok(()));
        assert_eq!(capabilities.check(Capability::MergeTab), Ok(()));
        assert_eq!(
            capabilities.check(Capability::SwapPanes),
            Err(Unavailable::CoreTooOld)
        );
        assert_eq!(
            capabilities.check(Capability::Uninstall),
            Err(Unavailable::CoreTooOld)
        );
    }
}
