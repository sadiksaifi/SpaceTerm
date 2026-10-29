//! Bounded admission: the Hello/Offer preamble every protocol major reads the same way, as
//! length-prefixed JSON. An incompatible peer therefore always receives a typed incompatibility.
//! After admission both peers switch to the postcard framing in [`super::frame`].
//! Peer-account authority comes from the socket. IDs are nonsecret fences, never credentials.
//! From major 4 on, the preamble is frozen: later majors read and write these exact messages, so
//! the service lane ([`Lane::Service`]) works across majors.
use crate::pane::control::ClientName;
use crate::{AuthenticatedStatus, ServerError};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

/// The protocol major of the pushed, multiplexed exchange.
pub const MAJOR: u16 = 4;
const MAX_FRAME: usize = 8192;
const CONNECTION_LIMIT: Duration = Duration::from_secs(5);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    versions: Vec<u16>,
    build: String,
    server: [u8; 32],
    /// The base Client Identity.
    client: [u8; 32],
    /// Composed by the client and rechecked by the server.
    name: ClientName,
    agent: Option<AgentName>,
    lane: Lane,
}

/// What an admitted connection carries. The service lane stays in bounded JSON whatever the
/// selected work major, so remote administration works across majors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lane {
    Work,
    Service,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    /// 0 means no shared work major.
    selected: u16,
    build: String,
    server: [u8; 32],
    server_major: u16,
    /// The effective Client Identity, derived from the agent name when the client sent one.
    client: [u8; 32],
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

pub(crate) fn random() -> Result<[u8; 32], ServerError> {
    let mut bytes = [0; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| ServerError::StorageUnavailable)?;
    Ok(bytes)
}

/// The transport of one admission: whole length-prefixed frames, each before a deadline. Account
/// IPC writes them back to back; a paired connection carries the same bytes on its QUIC L0 stream.
pub(crate) trait Preamble {
    fn send(&mut self, frame: &[u8], deadline: Instant) -> Result<(), ServerError>;
    /// One frame of at most `max` bytes, without its length prefix.
    fn receive(&mut self, max: usize, deadline: Instant) -> Result<Vec<u8>, ServerError>;
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
    fn send(&mut self, frame: &[u8], deadline: Instant) -> Result<(), ServerError> {
        self.set_nonblocking(true)
            .map_err(|_| ServerError::Protocol)?;
        let frame = prefixed(frame);
        let mut rest = frame.as_slice();
        while !rest.is_empty() {
            match self.write(rest) {
                Ok(0) => return Err(ServerError::Protocol),
                Ok(count) => rest = &rest[count..],
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if !crate::connection::io::wait_fd(self.as_raw_fd(), true, deadline) {
                        return Err(ServerError::Protocol);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(ServerError::Protocol),
            }
        }
        Ok(())
    }

    fn receive(&mut self, max: usize, deadline: Instant) -> Result<Vec<u8>, ServerError> {
        self.set_nonblocking(true)
            .map_err(|_| ServerError::Protocol)?;
        let mut size = [0; 4];
        read_exact(self, &mut size, deadline)?;
        let size = u32::from_be_bytes(size) as usize;
        if size > max {
            return Err(ServerError::Protocol);
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
) -> Result<(), ServerError> {
    while !bytes.is_empty() {
        match stream.read(bytes) {
            Ok(0) => return Err(ServerError::Protocol),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if !crate::connection::io::wait_fd(stream.as_raw_fd(), false, deadline) {
                    return Err(ServerError::Protocol);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(ServerError::Protocol),
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
    pub(crate) fn read<T: DeserializeOwned>(&mut self) -> Result<T, ServerError> {
        let bytes = self.stream.receive(MAX_FRAME, self.deadline)?;
        serde_json::from_slice(&bytes).map_err(|_| ServerError::Protocol)
    }
    pub(crate) fn write(&mut self, value: &impl Serialize) -> Result<(), ServerError> {
        let bytes = serde_json::to_vec(value).map_err(|_| ServerError::Protocol)?;
        if bytes.len() > MAX_FRAME {
            return Err(ServerError::Protocol);
        }
        self.stream.send(&bytes, self.deadline)
    }
}

/// A connection the server admitted: its effective Client Identity, which an agent name derives
/// from the base one, and the name it presents.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Admitted {
    pub status: AuthenticatedStatus,
    pub client: [u8; 32],
    pub agent: Option<AgentName>,
    pub name: ClientName,
}

/// Both peers verify the owning account before accepting any protocol values. The client's
/// identity and name are nonsecret and grant nothing beyond the connection's authority.
pub(crate) fn admit_server(
    stream: &mut UnixStream,
    server: &[u8; 32],
    build: &str,
    lane: Option<&std::sync::Arc<crate::service::lane::LaneServer>>,
) -> Result<Admitted, ServerError> {
    crate::host::check_peer(stream)?;
    admit_server_transport(stream, server, build, None, lane)
}

/// Only the paired host calls this after pinned-server and enrolled-device proof validation.
/// Every agent it admits derives from the pairing's own Client Identity.
pub(crate) fn admit_direct_server(
    stream: &mut dyn Preamble,
    paired: &crate::direct::PairedDevice,
    lane: Option<&std::sync::Arc<crate::service::lane::LaneServer>>,
) -> Result<Admitted, ServerError> {
    admit_server_transport(
        stream,
        &paired.pin.server,
        &paired.pin.build,
        Some((paired.client, paired.pairing)),
        lane,
    )
}

fn admit_server_transport(
    stream: &mut dyn Preamble,
    server: &[u8; 32],
    build: &str,
    paired: Option<([u8; 32], crate::direct::PairingId)>,
    lane: Option<&std::sync::Arc<crate::service::lane::LaneServer>>,
) -> Result<Admitted, ServerError> {
    let expected_client = paired.map(|(client, _)| client);
    let mut wire = Wire::new(stream);
    let hello: Hello = wire.read()?;
    if hello.build != build
        || hello.server != *server
        || hello.client == [0; 32]
        || expected_client.is_some_and(|client| hello.client != client)
    {
        return Err(ServerError::AuthenticationRefused);
    }
    if hello.versions.is_empty() || hello.versions.len() > 16 {
        return Err(ServerError::Protocol);
    }
    // An agent's Client Name keeps " CLI <agent>" whole, so every client can tell agents apart.
    if let Some(agent) = &hello.agent
        && !hello.name.as_str().ends_with(&agent.name_suffix())
    {
        return Err(ServerError::Protocol);
    }
    let client = hello
        .agent
        .as_ref()
        .map_or(hello.client, |agent| agent.client(&hello.client));
    let selected = if hello.versions.contains(&MAJOR) {
        MAJOR
    } else {
        0
    };
    wire.write(&Offer {
        selected,
        build: build.to_owned(),
        server: *server,
        server_major: MAJOR,
        client,
    })?;
    if hello.lane == Lane::Service {
        // The lane consumes the connection. The error tells the caller not to serve it further.
        let lane = lane.ok_or(ServerError::Unsupported)?;
        stream.admitted();
        lane.serve(
            stream,
            crate::service::operations::Actor {
                pairing: paired.map(|(_, pairing)| pairing),
                client,
            },
        )?;
        return Err(ServerError::Unsupported);
    }
    if selected == 0 {
        return Err(ServerError::IncompatibleProtocol);
    }
    Ok(Admitted {
        status: AuthenticatedStatus {
            protocol_major: selected,
            development_pane: false,
        },
        client,
        agent: hello.agent,
        name: hello.name,
    })
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

/// Admits a service-lane connection as `caller`. The lane is answered whatever the work majors
/// of the two peers, so this returns the server's work major for the caller to compare.
#[cfg_attr(
    not(feature = "client"),
    allow(dead_code, reason = "the CLI's server commands open the service lane")
)]
pub(crate) fn admit_service_lane(
    stream: &mut dyn Preamble,
    caller: Caller<'_>,
    server: &[u8; 32],
    build: &str,
) -> Result<u16, ServerError> {
    let mut wire = Wire::new(stream);
    wire.write(&Hello {
        versions: vec![MAJOR],
        build: build.to_owned(),
        server: *server,
        client: *caller.client,
        name: caller.name.clone(),
        agent: caller.agent.cloned(),
        lane: Lane::Service,
    })?;
    let offer: Offer = wire.read()?;
    if offer.build != build || offer.server != *server || offer.client != caller.effective() {
        return Err(ServerError::AuthenticationRefused);
    }
    stream.admitted();
    Ok(offer.server_major)
}

#[cfg_attr(
    not(feature = "client"),
    allow(dead_code, reason = "client side used by desktop and admission tests")
)]
pub(crate) fn admit_client<'a>(
    stream: &mut UnixStream,
    caller: impl Into<Caller<'a>>,
    server: &[u8; 32],
    build: &str,
    versions: &[u16],
) -> Result<AuthenticatedStatus, ServerError> {
    crate::host::check_peer(stream)?;
    admit_transport(stream, caller, server, build, versions)
}

pub(crate) fn admit_transport<'a>(
    stream: &mut dyn Preamble,
    caller: impl Into<Caller<'a>>,
    server: &[u8; 32],
    build: &str,
    versions: &[u16],
) -> Result<AuthenticatedStatus, ServerError> {
    let caller = caller.into();
    let mut wire = Wire::new(stream);
    wire.write(&Hello {
        versions: versions.to_vec(),
        build: build.to_owned(),
        server: *server,
        client: *caller.client,
        name: caller.name.clone(),
        agent: caller.agent.cloned(),
        lane: Lane::Work,
    })?;
    let offer: Offer = wire.read()?;
    if offer.build != build || offer.server != *server || offer.client != caller.effective() {
        return Err(ServerError::AuthenticationRefused);
    }
    if offer.selected == 0 {
        return Err(ServerError::IncompatibleProtocol);
    }
    if offer.selected != MAJOR || !versions.contains(&offer.selected) {
        return Err(ServerError::IncompatibleProtocol);
    }
    Ok(AuthenticatedStatus {
        protocol_major: offer.selected,
        development_pane: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_admission_preserves_nonsecret_client_identity_and_refuses_incompatible_majors() {
        for versions in [vec![3], vec![1], vec![4, 3]] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let accepted = versions.contains(&MAJOR);
            let thread = std::thread::spawn(move || {
                admit_server(&mut server, &[7; 32], "spaceterm-development", None)
            });
            let result = admit_client(
                &mut client,
                (&[9; 32], &"Build Mac".into()),
                &[7; 32],
                "spaceterm-development",
                &versions,
            );
            let server_result = thread.join().unwrap();
            if accepted {
                assert_eq!(result.unwrap().protocol_major, MAJOR);
                let admitted = server_result.unwrap();
                assert_eq!(
                    (admitted.client, admitted.name.as_str()),
                    ([9; 32], "Build Mac")
                );
            } else {
                assert_eq!(result, Err(ServerError::IncompatibleProtocol));
                assert_eq!(server_result, Err(ServerError::IncompatibleProtocol));
            }
        }
    }
    #[test]
    fn substituted_build_or_server_is_refused_before_operations() {
        for (build, id) in [("spaceterm", [7; 32]), ("spaceterm-development", [8; 32])] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let thread = std::thread::spawn(move || {
                admit_server(&mut server, &[7; 32], "spaceterm-development", None)
            });
            assert!(
                admit_client(
                    &mut client,
                    (&[9; 32], &"Build Mac".into()),
                    &id,
                    build,
                    &[MAJOR]
                )
                .is_err()
            );
            assert_eq!(
                thread.join().unwrap(),
                Err(ServerError::AuthenticationRefused)
            );
        }
    }
    #[test]
    fn client_refuses_an_offer_for_another_build_or_server() {
        for (build, server_id) in [("spaceterm", [7; 32]), ("spaceterm-development", [8; 32])] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let thread = std::thread::spawn(move || {
                let mut wire = Wire::new(&mut server);
                let _: Hello = wire.read().unwrap();
                wire.write(&Offer {
                    selected: MAJOR,
                    build: build.into(),
                    server: server_id,
                    server_major: MAJOR,
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
                    &[MAJOR]
                ),
                Err(ServerError::AuthenticationRefused)
            );
            thread.join().unwrap();
        }
    }
    #[test]
    fn oversized_frames_are_refused_before_admission() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        client.write_all(&u32::MAX.to_be_bytes()).unwrap();
        assert_eq!(
            admit_server(&mut server, &[7; 32], "spaceterm-development", None),
            Err(ServerError::Protocol)
        );
    }
    #[test]
    fn stalled_connections_obey_an_absolute_deadline() {
        let (_client, mut server) = UnixStream::pair().unwrap();
        let mut wire = Wire::new(&mut server);
        wire.deadline = Instant::now() + Duration::from_millis(10);
        assert!(matches!(wire.read::<Hello>(), Err(ServerError::Protocol)));
    }
    #[test]
    fn an_agent_is_admitted_as_the_identity_the_server_derives_from_its_base() {
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
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || {
            admit_server(&mut server, &[7; 32], "spaceterm-development", None)
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
            &[MAJOR],
        )
        .unwrap();
        let admitted = thread.join().unwrap().unwrap();
        assert_eq!(admitted.client, derived);
        assert_eq!(admitted.agent, Some(agent));
    }

    #[test]
    fn an_agent_whose_name_hides_it_is_refused() {
        let agent: AgentName = "agent-1".parse().unwrap();
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || {
            admit_server(&mut server, &[7; 32], "spaceterm-development", None)
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
                &[MAJOR],
            )
            .is_err()
        );
        assert_eq!(thread.join().unwrap(), Err(ServerError::Protocol));
    }

    /// A paired connection's Hello must carry its pairing's own Client Identity, so its agents
    /// always derive from it and never from another pairing's.
    #[test]
    fn a_paired_client_cannot_admit_an_agent_of_another_pairing() {
        let agent: AgentName = "agent-1".parse().unwrap();
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || {
            admit_server_transport(
                &mut server,
                &[7; 32],
                "spaceterm-development",
                Some(([9; 32], [8; 32])),
                None,
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
                &[MAJOR],
            )
            .is_err()
        );
        assert_eq!(
            thread.join().unwrap(),
            Err(ServerError::AuthenticationRefused)
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
