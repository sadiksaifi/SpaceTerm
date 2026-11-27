//! The service lane exchanges bounded JSON after its own protocol admission.
use crate::diagnostics::TerminalCounterSnapshot;
use crate::permissions::{PermissionsReport, RequestOutcome, SetupReadiness, SystemPermission};
use crate::protocol::capability::Capability;
use crate::update::facts::UpdateFacts;
use crate::version::ProtocolVersion;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub mod network;
use network::{LaneNetworkEffects, NetworkAccess};

/// The contract code for an action only the server's own machine takes.
pub const LOCAL_ONLY: &str = "local_only";
/// The largest lane frame, in bytes.
pub const MAX_FRAME: usize = 128 * 1024;
/// The largest decoded `UploadChunk::data`, in bytes.
pub const MAX_CHUNK: usize = 64 * 1024;
/// How long `WaitFacts` waits for a change before it answers with the unchanged facts.
pub const WAIT_FACTS_LIMIT: Duration = Duration::from_secs(20);
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case", deny_unknown_fields)]
pub enum LaneRequest {
    Status,
    /// Reads the Core's update facts. A struct variant, so unknown fields are refused.
    Facts {},
    /// Answers with the facts once the Core's `boot` or facts `revision` differs from these, or
    /// after [`WAIT_FACTS_LIMIT`] with the unchanged facts. A client that shares no work major
    /// with the Core follows facts this way.
    WaitFacts {
        boot: String,
        revision: u64,
    },
    /// Checks the Core's own feed now, and installs the release it names when that is newer.
    /// Answers `Facts`. A Core that never fetches a feed answers `no_update_feed`.
    Check,
    /// Installs the release the Core's own feed names. Answers `Installed`.
    StageFetch,
    /// Begins a push of `length` archive bytes of `version` with the Update Floor `floor`, null
    /// for a Preflight or Development version. The Core refuses a version it would not install
    /// before any byte arrives.
    StageUpload {
        version: String,
        #[serde(deserialize_with = "crate::update::ordering::present")]
        floor: Option<String>,
        length: u64,
        sha256: String,
        /// Base64.
        signature: String,
    },
    UploadChunk {
        upload: String,
        offset: u64,
        /// Base64, at most [`MAX_CHUNK`] bytes decoded.
        data: String,
    },
    Preview {
        action: LaneAction,
    },
    Execute {
        action: LaneAction,
        token: String,
        operation: String,
    },
    Resolve {
        operation: String,
    },
    Acknowledge {
        operation: String,
    },
    /// Reads the server's System Permissions. This and the requests below answer only a caller
    /// on the server's own machine, and refuse a paired one with `local_only`.
    Permissions,
    /// Asks the system for one System Permission.
    RequestPermission {
        permission: SystemPermission,
    },
    /// Prepares a Permission Setup of one System Permission. `setup` is the client's random
    /// name for it, which [`LaneRequest::CancelPermissionSetup`] names.
    PreparePermissionSetup {
        permission: SystemPermission,
        setup: String,
    },
    /// Cancels a Permission Setup, over another connection while its preparation runs. The
    /// preparation then takes no further effect.
    CancelPermissionSetup {
        setup: String,
    },
    /// Forgets the system's decision for one System Permission and this server alone.
    ResetPermission {
        permission: SystemPermission,
    },
    /// Reads the Core's detailed update facts: the facts and what only its update engine knows,
    /// as one snapshot. Since service [`crate::update::facts::MINOR_DETAILED_FACTS`].
    DetailedFacts {},
    /// As [`LaneRequest::WaitFacts`], answering with the detailed update facts. Since service
    /// [`crate::update::facts::MINOR_DETAILED_FACTS`].
    WaitDetailedFacts {
        boot: String,
        revision: u64,
    },
    /// Service 1.3, account-local only. The helper requests the final automatic Stop grant.
    GrantStop {
        operation: String,
        boot: String,
        candidate: String,
    },
    /// Reads what the Machine's Command Line Tool path holds. Answers `CommandLineTool`. Since
    /// service [`crate::version::MINOR_COMMAND_LINE_TOOL`].
    CommandLineTool {},
    /// Links the Command Line Tool to the approved runtime as the installer does, unless
    /// another program's entry is there. Answers `CommandLineToolLinked`. Since service
    /// [`crate::version::MINOR_COMMAND_LINE_TOOL`].
    InstallCommandLineTool {},
    /// Removes SpaceTerm's own Command Line Tool link, and leaves another program's entry.
    /// Answers `CommandLineToolRemoved`. Since service
    /// [`crate::version::MINOR_COMMAND_LINE_TOOL`].
    UninstallCommandLineTool {},
}

impl LaneReply {
    /// This reply as a connection whose admission selected `selected` reads it: its facts
    /// through [`UpdateFacts::for_selected`], and a code the selected minor lacks as the nearest
    /// one it has.
    #[must_use]
    pub fn for_selected(self, selected: ProtocolVersion) -> Self {
        let facts = |facts: Box<UpdateFacts>| Box::new(facts.for_selected(selected));
        match self {
            Self::Status(mut status) => {
                status.update = status.update.map(facts);
                Self::Status(status)
            }
            Self::Facts { boot, facts: f } => Self::Facts {
                boot,
                facts: facts(f),
            },
            Self::Installed {
                installed,
                facts: f,
            } => Self::Installed {
                installed,
                facts: facts(f),
            },
            Self::Error { code }
                if code == CLOCK_BEHIND
                    && selected.minor < crate::update::facts::MINOR_RESTART_FACTS =>
            {
                Self::Error {
                    code: "candidate_unverified".to_owned(),
                }
            }
            reply => reply,
        }
    }
}

/// The code of a feed check that found this Machine's clock behind. Since service 1.1.
pub const CLOCK_BEHIND: &str = "clock_behind";

impl LaneRequest {
    /// The first service lane version that has this request. A client never sends a request
    /// that its admission's selected version lacks, and the lane refuses one as `unsupported`.
    /// A minor adds a request by appending a variant above and naming its minor here.
    pub const fn since(&self) -> ProtocolVersion {
        let minor = match self {
            Self::DetailedFacts {} | Self::WaitDetailedFacts { .. } => {
                crate::update::facts::MINOR_DETAILED_FACTS
            }
            Self::GrantStop { .. } => crate::version::MINOR_IDLE_RESTART,
            Self::CommandLineTool {}
            | Self::InstallCommandLineTool {}
            | Self::UninstallCommandLineTool {} => crate::version::MINOR_COMMAND_LINE_TOOL,
            _ => 0,
        };
        ProtocolVersion { major: 1, minor }
    }

    /// Whether a connection at `selected` carries the request and its action.
    pub fn supported_at(&self, selected: ProtocolVersion) -> bool {
        self.since() <= selected
            && !matches!(
                self,
                Self::Preview { action } | Self::Execute { action, .. }
                    if action.required_capability().is_some_and(|capability| capability.since().1 > selected)
            )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum LaneAction {
    /// Only the Core's own machine stops it. A paired caller gets `local_only`, because a
    /// stopped Core can't be started remotely.
    Stop,
    Restart,
    Network {
        access: NetworkAccess,
    },
    /// Remove Machine: stops SpaceTerm Core, revokes every pairing and removes the service,
    /// the launcher and every installed version. Workspace data stays. Gated by
    /// [`Capability::Uninstall`], as Restart is by [`Capability::Restart`]. Once the helper
    /// starts, the Core answers no further request, so the caller treats `Accepted` as the
    /// outcome.
    Uninstall,
}

impl LaneAction {
    /// The capability a previewed or executed action needs, when it is not part of every
    /// service lane.
    pub const fn required_capability(&self) -> Option<Capability> {
        match self {
            Self::Restart => Some(Capability::Restart),
            Self::Uninstall => Some(Capability::Uninstall),
            Self::Stop | Self::Network { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case", deny_unknown_fields)]
pub enum LaneReply {
    Status(LaneStatus),
    /// The update facts of the Core run `boot`.
    Facts {
        boot: String,
        facts: Box<crate::update::facts::UpdateFacts>,
    },
    /// The detailed update facts of the Core run `boot`. Only a connection that selected
    /// service [`crate::update::facts::MINOR_DETAILED_FACTS`] or later asks for them.
    DetailedFacts {
        boot: String,
        detailed: Box<crate::update::facts::DetailedUpdateFacts>,
    },
    /// `StageFetch` or a push's last `UploadChunk` finished. `installed` is false when the feed
    /// named no newer version.
    Installed {
        installed: bool,
        facts: Box<UpdateFacts>,
    },
    UploadProgress {
        upload: String,
        next_offset: u64,
    },
    Preview {
        token: String,
        expires_at: String,
        effects: LaneEffects,
    },
    /// The preview of a Network Access change.
    NetworkPreview {
        token: String,
        expires_at: String,
        effects: LaneNetworkEffects,
    },
    Accepted {
        operation: String,
        /// The run of the server that admitted it. A Restart or Update has completed its
        /// replacement once a receipt carries a different boot.
        boot: String,
    },
    Receipt(LaneReceipt),
    Acknowledged,
    Permissions(PermissionsReport),
    PermissionRequested(RequestOutcome),
    PermissionSetup(SetupReadiness),
    PermissionSetupCancelled,
    PermissionReset,
    /// `code` is a code from the CLI output contract.
    Error {
        code: String,
    },
    /// The Core refused a version because its running version is older than the version's
    /// Update Floor: the CLI output contract's `below_floor`, with the floor it names.
    BelowFloor {
        floor: crate::update::ordering::Floor,
    },
    StopGranted {},
    /// What the Command Line Tool path holds.
    CommandLineTool {
        launcher: CliLauncher,
    },
    /// What `InstallCommandLineTool` did.
    CommandLineToolLinked {
        link: CliLink,
    },
    /// `UninstallCommandLineTool` left no SpaceTerm link at the path.
    CommandLineToolRemoved {},
}

/// What the command-line launcher path holds. Only [`CliLauncher::Missing`] and SpaceTerm's
/// own links are ever replaced.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliLauncher {
    Missing,
    /// A link into this identity's retained installations whose executable exists.
    Linked,
    /// A link into this identity's retained installations whose executable is gone.
    Stale,
    /// A link into another of this identity's installations than the approved runtime, such as
    /// one an update couldn't repoint.
    Outdated,
    /// A stale or outdated managed link that can't be replaced, because another tool's entry
    /// holds the temporary name `.launcher-<identity>` beside it.
    Blocked,
    /// A link to anything else, such as another tool.
    Foreign,
    /// A file, directory or other entry that is not a link.
    NotLink,
}

/// The outcome of linking the command-line launcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliLink {
    /// The launcher names the selected runtime.
    Linked,
    /// Another tool's launcher is there and stays.
    Kept,
    /// Another tool's entry holds the temporary name a replacement needs, and stays.
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneReceipt {
    pub operation: String,
    pub action: LaneAction,
    pub state: LaneOperationState,
    pub code: Option<String>,
    /// The run of the server answering now.
    pub boot: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneOperationState {
    Admitted,
    Launched,
    Running,
    Done,
    Failed,
    NotPerformed,
    Unresolved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneVersion {
    pub version: String,
    pub commit: String,
    pub sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneActivation {
    Settled,
    Unresolved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneNetwork {
    pub enabled: bool,
    pub bind: Option<String>,
    pub listening: bool,
    pub addresses: Vec<std::net::IpAddr>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneDevice {
    pub id: String,
    pub name: String,
    pub fingerprint: String,
    pub last_connected_at: Option<String>,
}

/// Mirrors the `server.status` data of the CLI output contract. Held outcomes are the CLI's own
/// journals, so the lane leaves them out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneStatus {
    pub running: bool,
    pub stopped: bool,
    pub installed: bool,
    pub automatic_start: bool,
    pub linger: Option<bool>,
    pub runtime: LaneVersion,
    /// The Core's update facts. None while no runtime is installed.
    pub update: Option<Box<UpdateFacts>>,
    pub activation: LaneActivation,
    pub sessions: u32,
    pub network: LaneNetwork,
    pub devices: Vec<LaneDevice>,
    pub lane_major: u16,
    /// The running server's Terminal Diagnostics counters. A stopped server has none.
    pub terminal_counters: Option<TerminalCounterSnapshot>,
}

/// Mirrors the confirmation `effects` of `stop` and `restart`. A Restart names the version it
/// restarts from, unless the Core's update engine could not open; a Restart into the Pending
/// Version also names that version and its work major. Stop leaves the version fields empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneEffects {
    pub ends_sessions: u32,
    pub sessions: Vec<String>,
    pub start_requires_machine: bool,
    pub from: Option<LaneVersion>,
    pub to: Option<LaneVersion>,
    /// The work protocol major a Restart into the Pending Version activates, so a client can
    /// warn when its own work major differs.
    pub to_major: Option<u16>,
}

/// Names one service operation. The client chooses it, so a repeated `Execute` dispatches
/// nothing new.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OperationId(pub [u8; 16]);

impl OperationId {
    /// Parses the lane form: 32 lowercase hexadecimal characters.
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() != 32
            || !text
                .bytes()
                .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
        {
            return None;
        }
        let mut id = [0; 16];
        for (index, byte) in id.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
        }
        Some(Self(id))
    }

    /// The first 8 hexadecimal characters, for names such as the helper's unit.
    pub fn short(&self) -> String {
        self.to_string()[..8].to_owned()
    }
}

impl std::fmt::Display for OperationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

/// Who asked for an operation. Only the same actor acknowledges its record, and revoking a
/// pairing forgets that pairing's settled records and orphans its unsettled one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub pairing: Option<crate::direct::PairingId>,
    pub client: crate::hierarchy::Id,
}

impl Actor {
    /// The Core itself, for a restart it decides on. No client acknowledges its records, so
    /// settlement forgets them once they are terminal.
    pub const CORE: Self = Self {
        pairing: None,
        client: [0; 32],
    };
}
