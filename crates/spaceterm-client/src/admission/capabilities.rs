use super::Refused;
use crate::protocol::capability::{Capabilities, Unavailable};
use crate::protocol::version::ProtocolVersion;
use crate::protocol::{CoreVersions, Lane, Negotiated};

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
    pub fn record(&mut self, lane: Lane, result: &Result<Negotiated, Refused>) {
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
