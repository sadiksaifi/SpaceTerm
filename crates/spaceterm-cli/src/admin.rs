//! The machine verbs: set up SpaceTerm Core on this machine, pair devices with it, and administer
//! it.
//!
//! `status`, `restart`, `update` and `network` reach a running SpaceTerm Core through its service
//! lane, locally over account IPC and on a paired machine over the paired connection, so they
//! work across protocol majors. `setup`, `start`, `stop`, `pair`, `unpair`, `end-all-sessions`,
//! `permissions` and `uninstall` stay on this machine. While SpaceTerm Core is stopped, local
//! `status` and `update` read and change the service directly.
pub mod end_all_sessions;
pub mod lifecycle;
pub mod network;
pub mod offline_token;
pub mod pair;
pub mod permissions;
pub mod setup;
pub mod status;
pub mod uninstall;
pub mod unpair;
pub mod update;

#[cfg(test)]
mod golden_tests;

use crate::args::{CoreCommand, Global};
use crate::contract::{
    self, Code, Envelope, Failure, MachineRef, RecoveredIdentity, RecoveredKind,
    RecoveredOperation, RecoveredOutcome, RecoveredState, Warning, WarningCode, WarningDetails,
};
use crate::state::{CliState, RouteKey};
use serde::Serialize;
use spaceterm_core::service::lane::admin::{self, PendingOperations};
use spaceterm_core::service::lane::client::{Lane, LaneError, Session};
use spaceterm_core::service::lane::{LaneOperationState, LaneReceipt};
use spaceterm_core::{CliRoute, CoreError, MachineConnection};
use std::io::Write;
use std::time::Duration;

/// One finished machine verb: its envelope and its operational text.
pub struct Reply {
    envelope: Envelope<serde_json::Value>,
    /// Operational output for stdout in text mode. It may name machines, devices and paths.
    stdout: String,
    broken_pipe: bool,
}

impl Reply {
    pub fn new<D: Serialize>(envelope: &Envelope<D>, stdout: String) -> Self {
        Self {
            envelope: envelope.erased(),
            stdout,
            broken_pipe: false,
        }
    }

    /// A failure with no operational data.
    pub fn failure(command: &str, core: Option<MachineRef>, failure: Failure) -> Self {
        Self::new(
            &Envelope::error(command, core, failure, contract::Empty {}),
            String::new(),
        )
    }

    pub fn routing_failure(
        command: &str,
        error: crate::connection::route::ResolutionError,
    ) -> Self {
        let data = error.operational_data();
        Self::new(
            &Envelope::error(
                command,
                error.machine.map(|machine| *machine),
                error.failure,
                data,
            ),
            String::new(),
        )
    }

    pub fn exit(&self) -> u8 {
        if self.broken_pipe {
            contract::EXIT_BROKEN_PIPE
        } else {
            self.envelope.exit()
        }
    }

    fn broken_pipe(command: &str, core: Option<MachineRef>) -> Self {
        let mut reply = Self::failure(command, core, Failure::new(Code::OutputFailed));
        reply.broken_pipe = true;
        reply
    }

    /// Writes the reply through [`contract::render`] and returns the exit status.
    pub fn write(
        &self,
        mode: contract::Mode,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> u8 {
        if self.broken_pipe {
            return contract::EXIT_BROKEN_PIPE;
        }
        contract::render(
            &self.envelope,
            &self.stdout,
            "",
            mode,
            crate::interrupt::finish(self.envelope.exit()),
            stdout,
            stderr,
        )
    }
}

/// Runs one machine verb for SpaceTerm Core of `identity`, the identity this executable was
/// built as.
pub fn run(identity: &str, global: &Global, command: &CoreCommand) -> Reply {
    let context = Context {
        identity,
        global,
        credentials: crate::connection::CliCredentials::new(identity),
    };
    let mut reply = match command {
        CoreCommand::Setup {
            network,
            automatic_start,
        } => setup::run(&context, network.as_ref(), *automatic_start),
        CoreCommand::Pair { address } => pair::run(&context, address.as_deref()),
        CoreCommand::Status => status::run(&context),
        CoreCommand::Start => lifecycle::start(&context),
        CoreCommand::Stop(confirm) => {
            lifecycle::stop_or_restart(&context, lifecycle::Kind::Stop, confirm.confirm.as_deref())
        }
        CoreCommand::Restart(confirm) => lifecycle::stop_or_restart(
            &context,
            lifecycle::Kind::Restart,
            confirm.confirm.as_deref(),
        ),
        CoreCommand::Update {
            check,
            archive,
            signature,
            confirm,
        } => update::run(
            &context,
            *check,
            archive.as_deref().zip(signature.as_deref()),
            confirm.as_deref(),
        ),
        CoreCommand::Network { access, confirm } => {
            network::run(&context, access.as_ref(), confirm.as_deref())
        }
        CoreCommand::Permissions { action } => permissions::run(&context, action.as_ref()),
        CoreCommand::EndAllSessions(confirm) => {
            end_all_sessions::run(&context, confirm.confirm.as_deref())
        }
        CoreCommand::Unpair { device, confirm } => {
            unpair::run(&context, device, confirm.as_deref())
        }
        CoreCommand::Uninstall(confirm) => uninstall::run(&context, confirm.confirm.as_deref()),
    };
    context.credentials.report(&mut reply.envelope);
    reply
}

/// What every machine verb shares.
pub struct Context<'a> {
    pub identity: &'a str,
    pub global: &'a Global,
    pub credentials: crate::connection::CliCredentials<'a>,
}

impl Context<'_> {
    /// Whether the command acts on this machine: `--machine`, else `SPACETERM_MACHINE`, else
    /// `local`, with the same rule work commands use.
    pub fn is_local(&self) -> bool {
        crate::connection::route::selector(self.global).is_ok_and(|selected| selected == "local")
    }

    /// Whether the person can answer a prompt: a terminal on stdin and stderr, and neither
    /// `--json` nor `--no-input`.
    pub fn interactive(&self) -> bool {
        !self.global.json
            && !self.global.no_input
            && terminal(libc::STDIN_FILENO)
            && terminal(libc::STDERR_FILENO)
    }

    pub fn timeout(&self, default: Duration) -> Duration {
        self.global.timeout.unwrap_or(default)
    }

    /// The CLI's connection to this account's own SpaceTerm Core, as the base identity or `--as`.
    pub fn local_connection(&self) -> Result<MachineConnection, Failure> {
        MachineConnection::for_cli(self.identity, CliRoute::Local, self.agent())
            .map_err(contract::from_core_error)
    }

    /// The machine this command acts on, with its route. A paired machine resolves through the
    /// CLI's Saved Connections.
    pub fn routed(&self) -> Result<Routed, crate::connection::route::ResolutionError> {
        let routed = crate::connection::route::connect(
            self.identity,
            self.global,
            &self.credentials,
            self.global
                .timeout
                .map(|timeout| std::time::Instant::now() + timeout),
        )?;
        let machine = match &routed.selected {
            crate::connection::route::Route::Local => local_ref(self.identity)?,
            crate::connection::route::Route::Paired(saved) => {
                routed.selected.reference(&saved.paired.pin.core)
            }
        };
        Ok(Routed {
            connection: Ok(routed.connection),
            machine,
            route: routed.selected.key(),
            local: routed.selected.is_local(),
        })
    }

    /// Fails at once, before any network traffic, when the target is not this machine.
    pub fn require_local(&self) -> Result<(), Failure> {
        if crate::connection::route::selector(self.global)? == "local" {
            Ok(())
        } else {
            Err(Failure::new(Code::LocalOnly))
        }
    }

    /// The command line a person copies to repeat a command against the same machine as the same
    /// actor: `spaceterm --machine ROUTE [--as AGENT] WORDS…`. `machine` is the resolved target,
    /// so the copy ignores a different `SPACETERM_MACHINE` or `SPACETERM_AS` in another shell.
    /// Every word is a [`crate::shell::word`], so the line pastes into any POSIX shell or Zsh.
    pub fn command_line(&self, machine: &MachineRef, words: &[&str]) -> String {
        let agent = self.agent();
        let mut line = vec!["spaceterm", "--machine", machine.route.as_str()];
        if let Some(agent) = &agent {
            line.extend(["--as", agent.as_str()]);
        }
        line.extend(words);
        line.iter()
            .map(|word| crate::shell::word(word))
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn agent(&self) -> Option<spaceterm_core::protocol::AgentName> {
        self.global.agent.clone().or_else(|| {
            std::env::var("SPACETERM_AS")
                .ok()
                .and_then(|name| name.parse().ok())
        })
    }

    pub fn identity_kind(&self) -> RecoveredIdentity {
        match self.agent() {
            Some(agent) => RecoveredIdentity::Agent(agent),
            None => RecoveredIdentity::Base,
        }
    }
}

fn terminal(fd: libc::c_int) -> bool {
    // SAFETY: isatty only observes the descriptor.
    unsafe { libc::isatty(fd) == 1 }
}

/// A resolved machine and the route its CLI state lives under.
pub struct Routed {
    /// The route's connection. A local one may be unavailable while SpaceTerm Core is stopped.
    pub connection: Result<MachineConnection, Failure>,
    pub machine: MachineRef,
    pub route: RouteKey,
    pub local: bool,
}

/// This machine's reference for the envelope, once SpaceTerm Core has an identity.
pub fn local_ref(identity: &str) -> Result<MachineRef, Failure> {
    let service = spaceterm_core::service::Service::for_identity(identity)
        .map_err(contract::from_core_error)?;
    let core = service
        .passive_machine_id()
        .map_err(contract::from_core_error)?
        .ok_or(Failure::new(Code::NotSetUp))?;
    Ok(MachineRef {
        name: "local".to_owned(),
        route: "local".to_owned(),
        id: hex(&core),
    })
}

/// How text names a machine: "This Mac" or "This Computer" for `local`, otherwise its name.
pub fn display_name(machine: &MachineRef) -> &str {
    if machine.route == "local" {
        if cfg!(target_os = "macos") {
            "This Mac"
        } else {
            "This Computer"
        }
    } else {
        &machine.name
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}

/// A direct-management failure on this machine. The account IPC fails to connect only when
/// SpaceTerm Core is not running.
pub fn local_direct(error: spaceterm_core::direct::DirectError) -> Failure {
    match error {
        spaceterm_core::direct::DirectError::Transport => Failure::new(Code::NotRunning),
        error => contract::from_direct_error(error),
    }
}

pub fn lane_failure(error: LaneError) -> Failure {
    match error {
        LaneError::Code(code) => contract::from_lane_code(&code),
        LaneError::Core(error) => contract::from_core_error(error),
        LaneError::Interrupted => Failure::new(Code::Interrupted),
    }
}

/// An open service-lane session, after this identity's leftover operations were settled.
pub struct Opened {
    pub lane: Lane,
    pub session: Session,
    pub recovered: Vec<RecoveredOperation>,
}

/// The pending service operations of one route and identity.
pub struct Pending {
    operations: PendingOperations,
}

impl Pending {
    pub fn open(context: &Context<'_>, route: &RouteKey) -> Result<Self, Failure> {
        let state = CliState::open(context.identity).map_err(contract::from_core_error)?;
        let directory = state
            .identity_dir(route, context.agent().as_ref())
            .map_err(contract::from_core_error)?;
        Ok(Self {
            operations: PendingOperations::new(directory),
        })
    }

    pub fn list(&self) -> Result<Vec<String>, Failure> {
        self.operations.list().map_err(contract::from_core_error)
    }

    /// The shared record, which the lane flow writes right before each `Execute`.
    pub fn operations(&self) -> &PendingOperations {
        &self.operations
    }
}

/// Marks the command dispatched right before its `Execute` is sent. `SIGINT` from here on
/// resolves the operation instead of ending the command.
pub fn dispatch() -> Result<(), LaneError> {
    crate::interrupt::dispatch().map_err(|_| LaneError::Interrupted)
}

/// Opens the lane of `connection` and settles what earlier commands of this identity left on
/// it. A pending operation that has not settled stays recorded for a later command.
pub fn open_lane(
    context: &Context<'_>,
    connection: &Result<MachineConnection, Failure>,
    route: &RouteKey,
    pending: &Pending,
) -> Result<Opened, Failure> {
    let identity = &context.identity_kind();
    let connection = connection.as_ref().map_err(Clone::clone)?;
    let lane = connection
        .service_lane()
        .map_err(contract::from_core_error)?;
    let (session, settled) = admin::open(&lane, &pending.operations).map_err(lane_failure)?;
    let mut recovered: Vec<_> = settled
        .iter()
        .map(|receipt| recovered_operation(receipt, identity))
        .collect();
    recovered.extend(settle_journals(context, connection, route));
    remember_connected(context, route);
    Ok(Opened {
        lane,
        session,
        recovered,
    })
}

/// Where the route records when this CLI last reached its server's lane. Any identity of the
/// route counts, so the record lives with the base identity.
const LAST_CONNECTED: &str = "last-connected.json";
const LAST_CONNECTED_FILE: &std::ffi::CStr = c"last-connected.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct LastConnected {
    /// Seconds since the Unix epoch.
    at: u64,
}

fn remember_connected(context: &Context<'_>, route: &RouteKey) {
    let Ok(directory) =
        CliState::open(context.identity).and_then(|state| state.identity_dir(route, None))
    else {
        return;
    };
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |v| v.as_secs());
    let _ = directory.write_json(LAST_CONNECTED_FILE, &LastConnected { at });
}

/// When this CLI last reached the route's lane, as RFC 3339, if it ever did.
pub fn last_connected(context: &Context<'_>, route: &RouteKey) -> Option<String> {
    let directory = CliState::open(context.identity)
        .and_then(|state| state.identity_dir(route, None))
        .ok()?;
    let record: LastConnected = directory.read_json(LAST_CONNECTED).ok()??;
    jiff::Timestamp::from_second(i64::try_from(record.at).ok()?)
        .ok()
        .map(|v| v.to_string())
}

/// Settles the Workspace edit journals and runs this CLI holds for the route. They need the
/// work lane, which a server of another protocol major refuses, so they then wait for a later
/// command.
fn settle_journals(
    context: &Context<'_>,
    connection: &MachineConnection,
    route: &RouteKey,
) -> Vec<RecoveredOperation> {
    let Ok(state) = CliState::open(context.identity) else {
        return Vec::new();
    };
    if state.held(route).map_or(true, |held| held.is_empty()) {
        return Vec::new();
    }
    let Ok(link) = connection.link() else {
        return Vec::new();
    };
    let agent = context.agent();
    let identity = context.identity;
    let target = connection.clone();
    crate::journal::settle(
        &state,
        route,
        crate::journal::Current {
            agent: agent.as_ref(),
            connection: &target,
            link: &link,
            writer: None,
        },
        &mut |agent| match route {
            RouteKey::Local => {
                MachineConnection::for_cli(identity, CliRoute::Local, agent.cloned())
            }
            RouteKey::Paired(_) => Err(CoreError::Unsupported),
        },
        std::time::Instant::now() + Duration::from_secs(30),
    )
    .unwrap_or_default()
}

fn recovered_operation(receipt: &LaneReceipt, identity: &RecoveredIdentity) -> RecoveredOperation {
    let (state, code) = match receipt.state {
        LaneOperationState::Done => (RecoveredState::Applied, None),
        LaneOperationState::Failed | LaneOperationState::NotPerformed => (
            RecoveredState::Refused,
            receipt
                .code
                .as_deref()
                .map(|code| contract::from_lane_code(code).code),
        ),
        _ => (RecoveredState::Unknown, None),
    };
    RecoveredOperation {
        operation: receipt.operation.clone(),
        identity: identity.clone(),
        kind: RecoveredKind::Service,
        outcome: RecoveredOutcome {
            state,
            code,
            target: None,
        },
    }
}

/// The warning that accompanies settled leftovers.
pub fn recovered_warning(recovered: &[RecoveredOperation]) -> Option<Warning> {
    (!recovered.is_empty()).then(|| {
        Warning::new(
            WarningCode::RecoveredOperation,
            WarningDetails::Recovered {
                count: u32::try_from(recovered.len()).unwrap_or(u32::MAX),
            },
        )
    })
}

/// Attaches settled leftovers to an envelope.
pub fn with_recovered<D: Serialize>(
    mut envelope: Envelope<D>,
    recovered: Vec<RecoveredOperation>,
) -> Envelope<D> {
    if let Some(warning) = recovered_warning(&recovered) {
        envelope.warnings.push(warning);
    }
    envelope.data.recovered_operations = recovered;
    envelope
}

/// Asks a yes-or-no question on stderr and reads one line from stdin. Anything but `y` or
/// `yes` is no.
pub fn confirm(question: &str) -> bool {
    let _ = write!(std::io::stderr().lock(), "{question} [y/N] ");
    let _ = std::io::stderr().flush();
    matches!(
        read_line(64).as_deref().map(str::trim),
        Some("y" | "Y" | "yes" | "Yes")
    )
}

/// One line from stdin, at most `limit` bytes. `None` on end of input, interrupt or overflow.
pub fn read_line(limit: usize) -> Option<String> {
    read_line_until(limit, None)
}

/// [`read_line`] that also gives up at `deadline`.
pub fn read_line_until(limit: usize, deadline: Option<std::time::Instant>) -> Option<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if crate::interrupt::interrupted() {
            return None;
        }
        let mut wait = crate::interrupt::TICK;
        if let Some(deadline) = deadline {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            wait = wait.min(left);
        }
        // SIGINT restarts a blocking read, so the read waits for input in short polls.
        let mut ready = [libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: `ready` is a live, writable array of one `pollfd` for the whole call.
        if unsafe { libc::poll(ready.as_mut_ptr(), 1, wait.as_millis() as libc::c_int) } <= 0 {
            continue;
        }
        // SAFETY: reads at most one byte into a live buffer.
        let count = unsafe { libc::read(libc::STDIN_FILENO, byte.as_mut_ptr().cast(), 1) };
        match count {
            1 if byte[0] == b'\n' => break,
            1 => {
                line.push(byte[0]);
                if line.len() > limit {
                    return None;
                }
            }
            0 => return None,
            _ if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => {}
            _ => return None,
        }
    }
    String::from_utf8(line).ok()
}

/// The text line for a receipt's state.
pub fn receipt_state(state: LaneOperationState) -> &'static str {
    match state {
        LaneOperationState::Admitted => "admitted",
        LaneOperationState::Launched => "launched",
        LaneOperationState::Running => "running",
        LaneOperationState::Done => "done",
        LaneOperationState::Failed => "failed",
        LaneOperationState::NotPerformed => "not performed",
        LaneOperationState::Unresolved => "unresolved",
    }
}
