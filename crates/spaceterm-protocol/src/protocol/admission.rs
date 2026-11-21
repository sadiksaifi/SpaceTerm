//! Bounded admission: the Hello/Offer preamble every protocol major reads the same way, as
//! length-prefixed JSON. An incompatible peer therefore always receives a typed incompatibility.
//! After admission both peers switch to the postcard framing in [`super::frame`].
//! IDs are nonsecret fences, never credentials.
//!
//! The preamble is frozen: later majors read and write these exact messages, each at most
//! [`MAX_FRAME`] bytes with unknown fields refused. A Hello names the lane it opens and offers
//! the caller's versions of that lane's protocol, and the Core selects one with
//! [`super::version::negotiate`] or none. The Offer always carries the Core's own versions, so a refused client
//! knows which side is older and whether the other lane admits it.
use super::version::{self, ProtocolVersion};
use crate::pane::control::ClientName;
use crate::update::facts::Version;
use serde::{Deserialize, Serialize};
use sha2_const_stable::Sha256;

/// Maximum size of one JSON admission payload, excluding its length prefix.
pub const MAX_FRAME: usize = 8192;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub lane: Lane,
    /// The caller's versions of `lane`'s protocol: its current version and, when one exists,
    /// its previous major at that major's last minor. 1 or 2 entries, distinct majors, highest
    /// first.
    pub versions: Vec<ProtocolVersion>,
    pub build: String,
    pub core: [u8; 32],
    /// The base Client Identity.
    pub client: [u8; 32],
    /// Composed by the client and rechecked by the server.
    pub name: ClientName,
    pub agent: Option<AgentName>,
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
pub struct Offer {
    /// `negotiate(hello.versions, the Core's version of hello.lane)`. None when no major is
    /// shared; the Core then closes the connection.
    pub selected: Option<ProtocolVersion>,
    /// The Core's own versions.
    pub versions: CoreVersions,
    pub build: String,
    pub core: [u8; 32],
    /// The effective Client Identity, derived from the agent name when the client sent one.
    pub client: [u8; 32],
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
        Sha256::new()
            .update(b"spaceterm.agent.v1")
            .update(base)
            .update(self.0.as_bytes())
            .finalize()
    }

    /// The suffix every Client Name of this agent ends with.
    pub fn name_suffix(&self) -> String {
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

/// Prefixes `bytes` with their length, the admission framing on every transport.
pub fn prefixed(bytes: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(4 + bytes.len());
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend_from_slice(bytes);
    frame
}

/// 1 or 2 versions with distinct majors, highest first.
pub fn well_formed(versions: &[ProtocolVersion]) -> bool {
    match versions {
        [_] => true,
        [first, second] => first.major > second.major,
        _ => false,
    }
}
