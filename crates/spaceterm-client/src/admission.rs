//! The I/O half of work and service-lane admission.
use crate::CoreError;
use crate::pane::control::ClientName;
use crate::protocol::admission::{Hello, MAX_FRAME, Offer};
use crate::protocol::version::{Incompatible, ProtocolVersion, negotiate};
use crate::protocol::{AgentName, CoreVersions, Lane, Negotiated};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Serialize, de::DeserializeOwned};
use std::time::{Duration, Instant};

pub const CONNECTION_LIMIT: Duration = Duration::from_secs(5);

/// Why a client's admission ended without a selected version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The Core answered and shares no major with this client on the lane. It closes the
    /// connection next.
    Incompatible {
        older: Incompatible,
        core: CoreVersions,
    },
    Failed(CoreError),
}

impl From<CoreError> for Refused {
    fn from(error: CoreError) -> Self {
        Self::Failed(error)
    }
}

impl From<Refused> for CoreError {
    fn from(refused: Refused) -> Self {
        match refused {
            Refused::Incompatible { older, .. } => Self::IncompatibleProtocol(older),
            Refused::Failed(error) => error,
        }
    }
}

pub fn random() -> Result<[u8; 32], CoreError> {
    let mut bytes = [0; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| CoreError::StorageUnavailable)?;
    Ok(bytes)
}

/// The transport of one admission: whole length-prefixed frames, each before a deadline. Account
/// IPC writes them back to back; a paired connection carries the same bytes on its QUIC L0 stream.
/// Each transport implements [`Self::send`] or [`Self::send_frame`], and the other follows.
pub trait Preamble {
    fn send(&mut self, frame: &[u8], deadline: Instant) -> Result<(), CoreError> {
        self.send_frame(frame, deadline)
            .map_err(|unsent| unsent.error)
    }
    /// [`Self::send`] that says, when it fails, whether any byte of the frame left. A transport
    /// that cannot tell says one may have.
    fn send_frame(&mut self, frame: &[u8], deadline: Instant) -> Result<(), Unsent> {
        self.send(frame, deadline).map_err(|error| Unsent {
            error,
            written: true,
        })
    }
    /// One frame of at most `max` bytes, without its length prefix.
    fn receive(&mut self, max: usize, deadline: Instant) -> Result<Vec<u8>, CoreError>;
    /// Admission proved the peer, so the transport may now hold frames as large as the
    /// service lane's. A transport without its own flow control needs nothing.
    fn admitted(&mut self) {}
}

/// A frame [`Preamble::send_frame`] did not send whole.
#[derive(Debug, PartialEq, Eq)]
pub struct Unsent {
    pub error: CoreError,
    /// Whether any byte of the frame may have reached the peer. False is definite: nothing of
    /// the frame was written.
    pub written: bool,
}

/// JSON admission messages under one absolute deadline.
pub struct Wire<'a> {
    stream: &'a mut dyn Preamble,
    deadline: Instant,
}
impl<'a> Wire<'a> {
    pub fn new(stream: &'a mut dyn Preamble) -> Self {
        Self::until(stream, Instant::now() + CONNECTION_LIMIT)
    }
    /// Admission frames that end at `deadline` or after [`CONNECTION_LIMIT`], whichever comes
    /// first. A frame is never sent once `deadline` has passed.
    pub fn until(stream: &'a mut dyn Preamble, deadline: Instant) -> Self {
        Self {
            stream,
            deadline: deadline.min(Instant::now() + CONNECTION_LIMIT),
        }
    }
    pub fn read<T: DeserializeOwned>(&mut self) -> Result<T, CoreError> {
        let bytes = self.stream.receive(MAX_FRAME, self.deadline)?;
        serde_json::from_slice(&bytes).map_err(|_| CoreError::Protocol)
    }
    pub fn write(&mut self, value: &impl Serialize) -> Result<(), CoreError> {
        let bytes = serde_json::to_vec(value).map_err(|_| CoreError::Protocol)?;
        if bytes.len() > MAX_FRAME || Instant::now() >= self.deadline {
            return Err(CoreError::Protocol);
        }
        self.stream.send(&bytes, self.deadline)
    }
}

/// Who a client admits as: its base Client Identity, its Client Name and its optional agent.
#[derive(Clone, Copy)]
pub struct Caller<'a> {
    pub client: &'a [u8; 32],
    pub name: &'a ClientName,
    pub agent: Option<&'a AgentName>,
}

impl<'a> From<(&'a [u8; 32], &'a ClientName)> for Caller<'a> {
    fn from((client, name): (&'a [u8; 32], &'a ClientName)) -> Self {
        Self {
            client,
            name,
            agent: None,
        }
    }
}

impl Caller<'_> {
    /// The Client Identity the server must offer back.
    pub fn effective(&self) -> [u8; 32] {
        self.agent
            .map_or(*self.client, |agent| agent.client(self.client))
    }
}

/// Admits a service-lane connection as `caller` by `deadline`. The lane needs a shared service
/// major, just as work admission needs a shared work major.
pub fn admit_service_lane(
    stream: &mut dyn Preamble,
    caller: Caller<'_>,
    core: &[u8; 32],
    build: &str,
    deadline: Instant,
) -> Result<Negotiated, Refused> {
    admit_lane(
        stream,
        caller,
        core,
        build,
        (Lane::Service, &Lane::Service.offered()),
        deadline,
    )
}

/// Admits a work connection as `caller`, offering this build's work versions.
pub fn admit_transport<'a>(
    stream: &mut dyn Preamble,
    caller: impl Into<Caller<'a>>,
    core: &[u8; 32],
    build: &str,
) -> Result<Negotiated, Refused> {
    admit_transport_by(
        stream,
        caller,
        core,
        build,
        Instant::now() + CONNECTION_LIMIT,
    )
}

/// Admits a work connection as `caller`, offering this build's work versions. No admission frame
/// is sent or waited for after `deadline`.
pub fn admit_transport_by<'a>(
    stream: &mut dyn Preamble,
    caller: impl Into<Caller<'a>>,
    core: &[u8; 32],
    build: &str,
    deadline: Instant,
) -> Result<Negotiated, Refused> {
    admit_lane(
        stream,
        caller.into(),
        core,
        build,
        (Lane::Work, &Lane::Work.offered()),
        deadline,
    )
}

/// The caller's half of admission on `lane`, offering `offered`, by `deadline`. The Offer must
/// select one of the offered majors at no more than its offered minor, or none.
pub fn admit_lane(
    stream: &mut dyn Preamble,
    caller: Caller<'_>,
    core: &[u8; 32],
    build: &str,
    (lane, offered): (Lane, &[ProtocolVersion]),
    deadline: Instant,
) -> Result<Negotiated, Refused> {
    let mut wire = Wire::until(stream, deadline);
    wire.write(&Hello {
        lane,
        versions: offered.to_vec(),
        build: build.to_owned(),
        core: *core,
        client: *caller.client,
        name: caller.name.clone(),
        agent: caller.agent.cloned(),
    })?;
    let offer: Offer = wire.read()?;
    if offer.build != build || offer.core != *core || offer.client != caller.effective() {
        return Err(CoreError::AuthenticationRefused.into());
    }
    let expected = negotiate(offered, offer.versions.of(lane));
    match (offer.selected, expected) {
        (Some(selected), Ok(expected)) if selected == expected => {
            if lane == Lane::Service {
                stream.admitted();
            }
            Ok(Negotiated {
                selected,
                core: offer.versions,
            })
        }
        (None, Err(older)) => Err(Refused::Incompatible {
            older,
            core: offer.versions,
        }),
        _ => Err(CoreError::Protocol.into()),
    }
}

mod capabilities;
pub use capabilities::Admissions;
