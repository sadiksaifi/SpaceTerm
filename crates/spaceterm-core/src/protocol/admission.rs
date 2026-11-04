//! Bounded admission: the Hello/Offer preamble every protocol major reads the same way, as
//! length-prefixed JSON. An incompatible peer therefore always receives a typed incompatibility.
//! After admission both peers switch to the postcard framing in [`super::frame`].
//! Peer-account authority comes from the socket. IDs are nonsecret fences, never credentials.
//!
//! The preamble is frozen: later majors read and write these exact messages, each at most
//! [`MAX_FRAME`] bytes with unknown fields refused. A Hello names the lane it opens and offers
//! the caller's versions of that lane's protocol, and the Core selects one with
//! [`negotiate`] or none. The Offer always carries the Core's own versions, so a refused client
//! knows which side is older and whether the other lane admits it.
use super::version::{self, Incompatible, ProtocolVersion, negotiate};
use crate::pane::control::ClientName;
use crate::update::facts::Version;
use crate::{AuthenticatedStatus, CoreError};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

const MAX_FRAME: usize = 8192;
const CONNECTION_LIMIT: Duration = Duration::from_secs(5);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    lane: Lane,
    /// The caller's versions of `lane`'s protocol: its current version and, when one exists,
    /// its previous major at that major's last minor. 1 or 2 entries, distinct majors, highest
    /// first.
    versions: Vec<ProtocolVersion>,
    build: String,
    core: [u8; 32],
    /// The base Client Identity.
    client: [u8; 32],
    /// Composed by the client and rechecked by the server.
    name: ClientName,
    agent: Option<AgentName>,
}

/// What an admitted connection carries. Each lane negotiates its own protocol version: the work
/// protocol or the service lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lane {
    Work,
    Service,
}

impl Lane {
    /// The versions this build offers as a caller of this lane: the current one, then the
    /// previous major when one exists.
    pub fn offered(self) -> Vec<ProtocolVersion> {
        let (current, previous) = match self {
            Self::Work => (version::WORK, version::PREVIOUS_WORK),
            Self::Service => (version::SERVICE, version::PREVIOUS_SERVICE),
        };
        std::iter::once(current).chain(previous).collect()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    /// `negotiate(hello.versions, the Core's version of hello.lane)`. None when no major is
    /// shared; the Core then closes the connection.
    selected: Option<ProtocolVersion>,
    /// The Core's own versions.
    versions: CoreVersions,
    build: String,
    core: [u8; 32],
    /// The effective Client Identity, derived from the agent name when the client sent one.
    client: [u8; 32],
}

/// A SpaceTerm Core's version and the protocol versions it listens with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreVersions {
    pub version: Version,
    pub work: ProtocolVersion,
    pub service: ProtocolVersion,
}

impl CoreVersions {
    /// A Core of this build running `version`.
    pub fn current(version: Version) -> Self {
        Self {
            version,
            work: version::WORK,
            service: version::SERVICE,
        }
    }

    /// A Core of this build running [`crate::UNVERSIONED`].
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self::current(Version::parse(crate::UNVERSIONED).unwrap_or_else(|_| unreachable!()))
    }

    /// The version the Core listens with on `lane`.
    pub fn of(&self, lane: Lane) -> ProtocolVersion {
        match lane {
            Lane::Work => self.work,
            Lane::Service => self.service,
        }
    }
}

/// What a client's admission negotiated on one lane, and the Core's own versions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Negotiated {
    pub selected: ProtocolVersion,
    pub core: CoreVersions,
}

/// Why a client's admission ended without a selected version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
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

/// An agent name, `[a-z0-9][a-z0-9-]{0,31}`. It names an identity, not a secret.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentName(String);

impl AgentName {
    pub const MAX_LEN: usize = 32;

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The Client Identity of this agent under the base Client Identity `base`:
    /// `SHA-256("spaceterm.agent.v1" || base || name)`. The server derives it at admission, so a
    /// paired client can only name agents of its own pairing.
    pub fn client(&self, base: &[u8; 32]) -> [u8; 32] {
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        digest.update(b"spaceterm.agent.v1");
        digest.update(base);
        digest.update(self.0.as_bytes());
        let mut client = [0; 32];
        client.copy_from_slice(digest.finish().as_ref());
        client
    }

    /// The suffix every Client Name of this agent ends with.
    pub(crate) fn name_suffix(&self) -> String {
        format!(" CLI {}", self.0)
    }
}

/// The agent name did not match `[a-z0-9][a-z0-9-]{0,31}`. It carries no value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "Agent names use 1 to 32 lowercase letters, digits or hyphens, starting with a letter or digit."
)]
pub struct InvalidAgentName;

impl TryFrom<String> for AgentName {
    type Error = InvalidAgentName;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        let bytes = name.as_bytes();
        let valid = !bytes.is_empty()
            && bytes.len() <= Self::MAX_LEN
            && bytes[0].is_ascii_lowercase() | bytes[0].is_ascii_digit()
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-');
        if valid {
            Ok(Self(name))
        } else {
            Err(InvalidAgentName)
        }
    }
}

impl std::str::FromStr for AgentName {
    type Err = InvalidAgentName;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        name.to_owned().try_into()
    }
}

impl From<AgentName> for String {
    fn from(name: AgentName) -> Self {
        name.0
    }
}

pub(crate) fn random() -> Result<[u8; 32], CoreError> {
    let mut bytes = [0; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| CoreError::StorageUnavailable)?;
    Ok(bytes)
}

/// The transport of one admission: whole length-prefixed frames, each before a deadline. Account
/// IPC writes them back to back; a paired connection carries the same bytes on its QUIC L0 stream.
pub(crate) trait Preamble {
    fn send(&mut self, frame: &[u8], deadline: Instant) -> Result<(), CoreError>;
    /// One frame of at most `max` bytes, without its length prefix.
    fn receive(&mut self, max: usize, deadline: Instant) -> Result<Vec<u8>, CoreError>;
    /// Admission proved the peer, so the transport may now hold frames as large as the
    /// service lane's. A transport without its own flow control needs nothing.
    fn admitted(&mut self) {}
}

/// Prefixes `bytes` with their length, the admission framing on every transport.
pub(crate) fn prefixed(bytes: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(4 + bytes.len());
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend_from_slice(bytes);
    frame
}

impl Preamble for UnixStream {
    fn send(&mut self, frame: &[u8], deadline: Instant) -> Result<(), CoreError> {
        self.set_nonblocking(true)
            .map_err(|_| CoreError::Protocol)?;
        let frame = prefixed(frame);
        let mut rest = frame.as_slice();
        while !rest.is_empty() {
            match self.write(rest) {
                Ok(0) => return Err(CoreError::Protocol),
                Ok(count) => rest = &rest[count..],
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if !crate::connection::io::wait_fd(self.as_raw_fd(), true, deadline) {
                        return Err(CoreError::Protocol);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(CoreError::Protocol),
            }
        }
        Ok(())
    }

    fn receive(&mut self, max: usize, deadline: Instant) -> Result<Vec<u8>, CoreError> {
        self.set_nonblocking(true)
            .map_err(|_| CoreError::Protocol)?;
        let mut size = [0; 4];
        read_exact(self, &mut size, deadline)?;
        let size = u32::from_be_bytes(size) as usize;
        if size > max {
            return Err(CoreError::Protocol);
        }
        let mut bytes = vec![0; size];
        read_exact(self, &mut bytes, deadline)?;
        Ok(bytes)
    }
}

fn read_exact(
    stream: &mut UnixStream,
    mut bytes: &mut [u8],
    deadline: Instant,
) -> Result<(), CoreError> {
    while !bytes.is_empty() {
        match stream.read(bytes) {
            Ok(0) => return Err(CoreError::Protocol),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if !crate::connection::io::wait_fd(stream.as_raw_fd(), false, deadline) {
                    return Err(CoreError::Protocol);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(CoreError::Protocol),
        }
    }
    Ok(())
}

/// JSON admission messages under one absolute deadline.
pub(crate) struct Wire<'a> {
    stream: &'a mut dyn Preamble,
    deadline: Instant,
}
impl<'a> Wire<'a> {
    pub(crate) fn new(stream: &'a mut dyn Preamble) -> Self {
        Self {
            stream,
            deadline: Instant::now() + CONNECTION_LIMIT,
        }
    }
    pub(crate) fn read<T: DeserializeOwned>(&mut self) -> Result<T, CoreError> {
        let bytes = self.stream.receive(MAX_FRAME, self.deadline)?;
        serde_json::from_slice(&bytes).map_err(|_| CoreError::Protocol)
    }
    pub(crate) fn write(&mut self, value: &impl Serialize) -> Result<(), CoreError> {
        let bytes = serde_json::to_vec(value).map_err(|_| CoreError::Protocol)?;
        if bytes.len() > MAX_FRAME {
            return Err(CoreError::Protocol);
        }
        self.stream.send(&bytes, self.deadline)
    }
}

/// A connection the server admitted: its effective Client Identity, which an agent name derives
/// from the base one, the name it presents and the work version it selected.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Admitted {
    pub status: AuthenticatedStatus,
    pub client: [u8; 32],
    pub agent: Option<AgentName>,
    pub name: ClientName,
    /// Every version the client's Hello offered, highest first.
    pub offered: Vec<ProtocolVersion>,
}

/// A service-lane connection the server admitted.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AdmittedLane {
    pub actor: crate::service::operations::Actor,
    pub selected: ProtocolVersion,
}

#[cfg(test)]
impl From<crate::service::operations::Actor> for AdmittedLane {
    /// A lane admitted at this build's service version.
    fn from(actor: crate::service::operations::Actor) -> Self {
        Self {
            actor,
            selected: version::SERVICE,
        }
    }
}

/// What a client's hello selected.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    /// A work connection, which the caller serves with the protocol.
    Work(Admitted),
    /// A service-lane connection. The caller serves the lane, or closes the connection when it
    /// has none.
    Service(AdmittedLane),
}

#[cfg(test)]
impl Admission {
    /// The admitted work connection. Panics on a service-lane hello.
    pub(crate) fn work(self) -> Admitted {
        match self {
            Self::Work(admitted) => admitted,
            Self::Service(_) => panic!("expected a work connection"),
        }
    }
}

/// Both peers verify the owning account before accepting any protocol values. The client's
/// identity and name are nonsecret and grant nothing beyond the connection's authority.
pub(crate) fn admit_core(
    stream: &mut UnixStream,
    core: &[u8; 32],
    build: &str,
    versions: &CoreVersions,
) -> Result<Admission, CoreError> {
    crate::host::check_peer(stream)?;
    admit_core_transport(stream, core, build, versions, None)
}

/// Only the paired host calls this after pinned-server and enrolled-device proof validation.
/// Every agent it admits derives from the pairing's own Client Identity.
pub(crate) fn admit_direct_machine(
    stream: &mut dyn Preamble,
    paired: &crate::direct::PairedDevice,
    versions: &CoreVersions,
) -> Result<Admission, CoreError> {
    admit_core_transport(
        stream,
        &paired.pin.core,
        &paired.pin.build,
        versions,
        Some((paired.client, paired.pairing)),
    )
}

/// 1 or 2 versions with distinct majors, highest first.
fn well_formed(versions: &[ProtocolVersion]) -> bool {
    match versions {
        [_] => true,
        [first, second] => first.major > second.major,
        _ => false,
    }
}

fn admit_core_transport(
    stream: &mut dyn Preamble,
    core: &[u8; 32],
    build: &str,
    versions: &CoreVersions,
    paired: Option<([u8; 32], crate::direct::PairingId)>,
) -> Result<Admission, CoreError> {
    let expected_client = paired.map(|(client, _)| client);
    let mut wire = Wire::new(stream);
    let hello: Hello = wire.read()?;
    if hello.build != build
        || hello.core != *core
        || hello.client == [0; 32]
        || expected_client.is_some_and(|client| hello.client != client)
    {
        return Err(CoreError::AuthenticationRefused);
    }
    if !well_formed(&hello.versions) {
        return Err(CoreError::Protocol);
    }
    // An agent's Client Name keeps " CLI <agent>" whole, so every client can tell agents apart.
    if let Some(agent) = &hello.agent
        && !hello.name.as_str().ends_with(&agent.name_suffix())
    {
        return Err(CoreError::Protocol);
    }
    let client = hello
        .agent
        .as_ref()
        .map_or(hello.client, |agent| agent.client(&hello.client));
    let negotiated = negotiate(&hello.versions, versions.of(hello.lane));
    wire.write(&Offer {
        selected: negotiated.ok(),
        versions: versions.clone(),
        build: build.to_owned(),
        core: *core,
        client,
    })?;
    let selected = negotiated.map_err(CoreError::IncompatibleProtocol)?;
    if hello.lane == Lane::Service {
        stream.admitted();
        return Ok(Admission::Service(AdmittedLane {
            actor: crate::service::operations::Actor {
                pairing: paired.map(|(_, pairing)| pairing),
                client,
            },
            selected,
        }));
    }
    Ok(Admission::Work(Admitted {
        status: AuthenticatedStatus { work: selected },
        client,
        agent: hello.agent,
        name: hello.name,
        offered: hello.versions,
    }))
}

/// Who a client admits as: its base Client Identity, its Client Name and its optional agent.
#[derive(Clone, Copy)]
pub(crate) struct Caller<'a> {
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
    pub(crate) fn effective(&self) -> [u8; 32] {
        self.agent
            .map_or(*self.client, |agent| agent.client(self.client))
    }
}

/// Admits a service-lane connection as `caller`. The lane needs a shared service major, just as
/// work admission needs a shared work major.
#[cfg_attr(
    not(feature = "client"),
    allow(dead_code, reason = "the CLI's server commands open the service lane")
)]
pub(crate) fn admit_service_lane(
    stream: &mut dyn Preamble,
    caller: Caller<'_>,
    core: &[u8; 32],
    build: &str,
) -> Result<Negotiated, Refused> {
    admit_lane(
        stream,
        caller,
        core,
        build,
        Lane::Service,
        &Lane::Service.offered(),
    )
}

#[cfg_attr(
    not(feature = "client"),
    allow(dead_code, reason = "client side used by desktop and admission tests")
)]
pub(crate) fn admit_client<'a>(
    stream: &mut UnixStream,
    caller: impl Into<Caller<'a>>,
    core: &[u8; 32],
    build: &str,
) -> Result<Negotiated, Refused> {
    crate::host::check_peer(stream)?;
    admit_transport(stream, caller, core, build)
}

/// Admits a work connection as `caller`, offering this build's work versions.
pub(crate) fn admit_transport<'a>(
    stream: &mut dyn Preamble,
    caller: impl Into<Caller<'a>>,
    core: &[u8; 32],
    build: &str,
) -> Result<Negotiated, Refused> {
    admit_lane(
        stream,
        caller.into(),
        core,
        build,
        Lane::Work,
        &Lane::Work.offered(),
    )
}

/// The caller's half of admission on `lane`, offering `offered`. The Offer must select one of
/// the offered majors at no more than its offered minor, or none.
fn admit_lane(
    stream: &mut dyn Preamble,
    caller: Caller<'_>,
    core: &[u8; 32],
    build: &str,
    lane: Lane,
    offered: &[ProtocolVersion],
) -> Result<Negotiated, Refused> {
    let mut wire = Wire::new(stream);
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

#[cfg(test)]
mod tests {
    use super::*;

    const fn v(major: u16, minor: u16) -> ProtocolVersion {
        ProtocolVersion { major, minor }
    }

    fn versions() -> CoreVersions {
        CoreVersions::current(Version::parse("1.4.0").unwrap())
    }

    /// Serves one admission as a Core listening with `core`.
    fn serve(
        core: CoreVersions,
    ) -> (
        UnixStream,
        std::thread::JoinHandle<Result<Admission, CoreError>>,
    ) {
        let (client, mut stream) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || {
            admit_core(&mut stream, &[7; 32], "spaceterm-development", &core)
        });
        (client, thread)
    }

    /// Admits `lane` offering `offered` against a Core listening with `core`.
    fn admit(
        lane: Lane,
        offered: &[ProtocolVersion],
        core: CoreVersions,
    ) -> (Result<Negotiated, Refused>, Result<Admission, CoreError>) {
        let (mut client, thread) = serve(core);
        let result = admit_lane(
            &mut client,
            (&[9; 32], &"Build Mac".into()).into(),
            &[7; 32],
            "spaceterm-development",
            lane,
            offered,
        );
        (result, thread.join().unwrap())
    }

    #[test]
    fn account_admission_preserves_nonsecret_client_identity() {
        let (mut client, thread) = serve(versions());
        let result = admit_client(
            &mut client,
            (&[9; 32], &"Build Mac".into()),
            &[7; 32],
            "spaceterm-development",
        );
        assert_eq!(
            result,
            Ok(Negotiated {
                selected: version::WORK,
                core: versions()
            })
        );
        let admitted = thread.join().unwrap().unwrap().work();
        assert_eq!(
            (admitted.client, admitted.name.as_str()),
            ([9; 32], "Build Mac")
        );
        assert_eq!(admitted.status.work, version::WORK);
        assert_eq!(admitted.offered, Lane::Work.offered());
    }

    /// A client one major ahead still speaks the Core's major, at the lower of the two minors.
    #[test]
    fn a_client_at_the_next_major_still_talks_to_a_core_at_the_previous_one() {
        let core = CoreVersions {
            work: v(1, 3),
            ..versions()
        };
        let (client, admitted) = admit(Lane::Work, &[v(2, 0), v(1, 2)], core.clone());
        assert_eq!(
            client,
            Ok(Negotiated {
                selected: v(1, 2),
                core
            })
        );
        let admitted = admitted.unwrap().work();
        assert_eq!(admitted.status.work, v(1, 2));
        assert_eq!(admitted.offered, [v(2, 0), v(1, 2)]);
    }

    #[test]
    fn with_no_shared_major_both_sides_learn_which_one_is_older() {
        for (core_work, older) in [
            (v(3, 0), Incompatible::CallerTooOld),
            (v(1, 9), Incompatible::ListenerTooOld),
        ] {
            let core = CoreVersions {
                work: core_work,
                ..versions()
            };
            let (client, admitted) = admit(Lane::Work, &[v(2, 1)], core.clone());
            assert_eq!(client, Err(Refused::Incompatible { older, core }));
            assert_eq!(admitted, Err(CoreError::IncompatibleProtocol(older)));
        }
    }

    /// The service lane negotiates its own major, so a client that shares no work major still
    /// administers the Core, and one that shares no service major is refused.
    #[test]
    fn the_service_lane_negotiates_its_own_major() {
        let core = CoreVersions {
            work: v(3, 0),
            service: v(1, 4),
            ..versions()
        };
        let (client, admitted) = admit(Lane::Service, &[v(1, 2)], core.clone());
        assert_eq!(
            client,
            Ok(Negotiated {
                selected: v(1, 2),
                core: core.clone()
            })
        );
        assert!(matches!(
            admitted,
            Ok(Admission::Service(AdmittedLane { selected, .. })) if selected == v(1, 2)
        ));
        let (client, admitted) = admit(Lane::Service, &[v(2, 0)], core.clone());
        assert_eq!(
            client,
            Err(Refused::Incompatible {
                older: Incompatible::ListenerTooOld,
                core
            })
        );
        assert_eq!(
            admitted,
            Err(CoreError::IncompatibleProtocol(
                Incompatible::ListenerTooOld
            ))
        );
    }

    #[test]
    fn a_hello_must_offer_one_or_two_descending_majors() {
        for offered in [
            vec![],
            vec![v(1, 0), v(2, 0)],
            vec![v(1, 0), v(1, 1)],
            vec![v(3, 0), v(2, 0), v(1, 0)],
        ] {
            let (_, admitted) = admit(Lane::Work, &offered, versions());
            assert_eq!(admitted, Err(CoreError::Protocol), "{offered:?}");
        }
    }

    #[test]
    fn a_client_refuses_an_offer_it_did_not_negotiate() {
        for selected in [Some(v(1, 1)), Some(v(2, 0)), None] {
            let (mut client, mut core) = UnixStream::pair().unwrap();
            let thread = std::thread::spawn(move || {
                let mut wire = Wire::new(&mut core);
                let _: Hello = wire.read().unwrap();
                wire.write(&Offer {
                    selected,
                    versions: CoreVersions {
                        work: v(1, 1),
                        ..versions()
                    },
                    build: "spaceterm-development".into(),
                    core: [7; 32],
                    client: [9; 32],
                })
                .unwrap();
            });
            let result = admit_lane(
                &mut client,
                (&[9; 32], &"Build Mac".into()).into(),
                &[7; 32],
                "spaceterm-development",
                Lane::Work,
                &[v(1, 0)],
            );
            assert_eq!(result, Err(Refused::Failed(CoreError::Protocol)));
            thread.join().unwrap();
        }
    }

    #[test]
    fn substituted_build_or_core_is_refused_before_operations() {
        for (build, id) in [("spaceterm", [7; 32]), ("spaceterm-development", [8; 32])] {
            let (mut client, mut core) = UnixStream::pair().unwrap();
            let thread = std::thread::spawn(move || {
                admit_core(&mut core, &[7; 32], "spaceterm-development", &versions())
            });
            assert!(
                admit_client(&mut client, (&[9; 32], &"Build Mac".into()), &id, build).is_err()
            );
            assert_eq!(
                thread.join().unwrap(),
                Err(CoreError::AuthenticationRefused)
            );
        }
    }
    #[test]
    fn client_refuses_an_offer_for_another_build_or_core() {
        for (build, machine_id) in [("spaceterm", [7; 32]), ("spaceterm-development", [8; 32])] {
            let (mut client, mut core) = UnixStream::pair().unwrap();
            let thread = std::thread::spawn(move || {
                let mut wire = Wire::new(&mut core);
                let _: Hello = wire.read().unwrap();
                wire.write(&Offer {
                    selected: Some(version::WORK),
                    versions: versions(),
                    build: build.into(),
                    core: machine_id,
                    client: [9; 32],
                })
                .unwrap();
            });
            assert_eq!(
                admit_client(
                    &mut client,
                    (&[9; 32], &"Build Mac".into()),
                    &[7; 32],
                    "spaceterm-development",
                ),
                Err(Refused::Failed(CoreError::AuthenticationRefused))
            );
            thread.join().unwrap();
        }
    }
    #[test]
    fn oversized_frames_are_refused_before_admission() {
        let (mut client, mut core) = UnixStream::pair().unwrap();
        client.write_all(&u32::MAX.to_be_bytes()).unwrap();
        assert_eq!(
            admit_core(&mut core, &[7; 32], "spaceterm-development", &versions()),
            Err(CoreError::Protocol)
        );
    }
    #[test]
    fn stalled_connections_obey_an_absolute_deadline() {
        let (_client, mut core) = UnixStream::pair().unwrap();
        let mut wire = Wire::new(&mut core);
        wire.deadline = Instant::now() + Duration::from_millis(10);
        assert!(matches!(wire.read::<Hello>(), Err(CoreError::Protocol)));
    }
    #[test]
    fn an_agent_is_admitted_as_the_identity_the_machine_derives_from_its_base() {
        let agent: AgentName = "agent-1".parse().unwrap();
        let derived = agent.client(&[9; 32]);
        assert_ne!(derived, [9; 32]);
        assert_ne!(
            derived,
            agent.client(&[8; 32]),
            "each base derives its own agents"
        );
        let other: AgentName = "agent-2".parse().unwrap();
        assert_ne!(derived, other.client(&[9; 32]));
        let (mut client, mut core) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || {
            admit_core(&mut core, &[7; 32], "spaceterm-development", &versions())
        });
        let name: ClientName = "Build Mac CLI agent-1".into();
        admit_client(
            &mut client,
            Caller {
                client: &[9; 32],
                name: &name,
                agent: Some(&agent),
            },
            &[7; 32],
            "spaceterm-development",
        )
        .unwrap();
        let admitted = thread.join().unwrap().unwrap().work();
        assert_eq!(admitted.client, derived);
        assert_eq!(admitted.agent, Some(agent));
    }

    #[test]
    fn an_agent_whose_name_hides_it_is_refused() {
        let agent: AgentName = "agent-1".parse().unwrap();
        let (mut client, mut core) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || {
            admit_core(&mut core, &[7; 32], "spaceterm-development", &versions())
        });
        let name: ClientName = "Build Mac".into();
        assert!(
            admit_client(
                &mut client,
                Caller {
                    client: &[9; 32],
                    name: &name,
                    agent: Some(&agent),
                },
                &[7; 32],
                "spaceterm-development",
            )
            .is_err()
        );
        assert_eq!(thread.join().unwrap(), Err(CoreError::Protocol));
    }

    /// A paired connection's Hello must carry its pairing's own Client Identity, so its agents
    /// always derive from it and never from another pairing's.
    #[test]
    fn a_paired_client_cannot_admit_an_agent_of_another_pairing() {
        let agent: AgentName = "agent-1".parse().unwrap();
        let (mut client, mut core) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || {
            admit_core_transport(
                &mut core,
                &[7; 32],
                "spaceterm-development",
                &versions(),
                Some(([9; 32], [8; 32])),
            )
        });
        let name: ClientName = "Build Mac CLI agent-1".into();
        assert!(
            admit_transport(
                &mut client,
                Caller {
                    client: &[8; 32],
                    name: &name,
                    agent: Some(&agent),
                },
                &[7; 32],
                "spaceterm-development",
            )
            .is_err()
        );
        assert_eq!(
            thread.join().unwrap(),
            Err(CoreError::AuthenticationRefused)
        );
    }

    #[test]
    fn agent_names_follow_their_pattern() {
        for valid in ["a", "agent-1", "0", &"a".repeat(32)] {
            assert!(valid.parse::<AgentName>().is_ok(), "{valid}");
        }
        for invalid in ["", "-a", "Agent", "a_b", "a b", &"a".repeat(33), "é"] {
            assert!(invalid.parse::<AgentName>().is_err(), "{invalid}");
        }
    }
}
