//! `spaceterm server`: make this machine a server, pair devices with it, and administer it.
//!
//! Status, Stop, Restart and Update reach a running server through its service lane, locally
//! over account IPC and remotely over the paired connection, so they work across protocol majors.
//! Setup, Start, pairing and Unpair stay on the server's own machine. While the server is
//! stopped, local Status and Update read and change the service directly.
pub mod lifecycle;
pub mod pair;
pub mod setup;
pub mod status;
pub mod unpair;
pub mod update;

#[cfg(test)]
mod golden_tests;

use crate::args::{Global, ServerCommand};
use crate::contract::{
    self, Code, Envelope, Failure, RecoveredIdentity, RecoveredKind, RecoveredOperation,
    RecoveredOutcome, RecoveredState, ServerRef, Warning, WarningCode, WarningDetails,
};
use crate::state::{CliState, RouteKey};
use serde::Serialize;
use spaceterm_server::service::lane::client::{Lane, LaneError, Leftover, Session};
use spaceterm_server::service::lane::{LaneOperationState, LaneReceipt};
use spaceterm_server::service::operations::OperationId;
use spaceterm_server::{CliRoute, ServerConnection, ServerError};
use std::io::Write;
use std::time::Duration;

/// The CLI's record of the service operations it admitted and has not acknowledged, per route
/// and identity, so the next lane connection settles them.
const PENDING: &str = "service-operations.json";
const PENDING_FILE: &std::ffi::CStr = c"service-operations.json";
/// At most this many pending operations are kept. The server holds no more than 16.
const MAX_PENDING: usize = 16;

/// One finished server command: its envelope, its operational text and its exit status.
pub struct Reply {
    json: String,
    /// Operational output for stdout in text mode. It may name servers, devices and paths.
    stdout: String,
    /// The fixed error line for stderr, when the command failed.
    stderr: Option<String>,
    exit: u8,
}

impl Reply {
    pub fn new<D: Serialize>(envelope: &Envelope<D>, stdout: String) -> Self {
        let stderr = envelope.error.as_ref().map(|error| {
            let mut line = format!("error: {} ({})", error.message, error.code);
            if let contract::Details::Usage(contract::UsageDetails {
                argument: Some(argument),
                ..
            }) = &error.details
            {
                line.push_str(&format!("\nargument: {argument}"));
            }
            line
        });
        Self {
            json: serde_json::to_string(envelope).unwrap_or_default(),
            stdout,
            stderr,
            exit: envelope.exit(),
        }
    }

    /// A failure with no operational data.
    pub fn failure(command: &str, server: Option<ServerRef>, failure: Failure) -> Self {
        Self::new(
            &Envelope::error(command, server, failure, contract::Empty {}),
            String::new(),
        )
    }

    pub fn exit(&self) -> u8 {
        self.exit
    }

    /// The envelope, as `--json` prints it.
    pub fn json(&self) -> &str {
        &self.json
    }

    /// Writes the reply and returns the exit status. A broken stdout pipe exits 141 without a
    /// message.
    pub fn write(&self, json: bool, quiet: bool) -> u8 {
        let mut output = std::io::stdout().lock();
        let written = if json {
            output
                .write_all(self.json.as_bytes())
                .and_then(|()| output.write_all(b"\n"))
        } else if quiet && self.exit == 0 {
            Ok(())
        } else {
            output.write_all(self.stdout.as_bytes())
        }
        .and_then(|()| output.flush());
        if let Err(error) = written {
            if error.kind() == std::io::ErrorKind::BrokenPipe {
                return contract::EXIT_BROKEN_PIPE;
            }
            let _ = writeln!(
                std::io::stderr().lock(),
                "{}",
                Failure::new(Code::OutputFailed).text()
            );
            return Code::OutputFailed.exit();
        }
        if !json && let Some(line) = &self.stderr {
            let _ = writeln!(std::io::stderr().lock(), "{line}");
        }
        self.exit
    }
}

/// Runs one `server` command for the server of `identity`, the identity this executable was
/// built as.
pub fn run(identity: &str, global: &Global, command: &ServerCommand) -> Reply {
    let context = Context { identity, global };
    let path = path(command);
    match command {
        ServerCommand::Setup {
            network,
            automatic_start,
        } => setup::run(&context, network.as_ref(), *automatic_start),
        ServerCommand::Pair { address } => pair::run(&context, address.as_deref()),
        ServerCommand::Status => status::run(&context),
        ServerCommand::Start => lifecycle::start(&context),
        ServerCommand::Stop(confirm) => {
            lifecycle::stop_or_restart(&context, lifecycle::Kind::Stop, confirm.confirm.as_deref())
        }
        ServerCommand::Restart(confirm) => lifecycle::stop_or_restart(
            &context,
            lifecycle::Kind::Restart,
            confirm.confirm.as_deref(),
        ),
        ServerCommand::Update {
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
        ServerCommand::Unpair { device, confirm } => {
            unpair::run(&context, device, confirm.as_deref())
        }
        ServerCommand::Run { .. } | ServerCommand::Provision(_) => {
            Reply::failure(path, None, Failure::new(Code::Unsupported))
        }
    }
}

/// The dotted command path of a server command.
fn path(command: &ServerCommand) -> &'static str {
    match command {
        ServerCommand::Setup { .. } => "server.setup",
        ServerCommand::Pair { .. } => "server.pair",
        ServerCommand::Status => "server.status",
        ServerCommand::Start => "server.start",
        ServerCommand::Stop(_) => "server.stop",
        ServerCommand::Restart(_) => "server.restart",
        ServerCommand::Update { .. } => "server.update",
        ServerCommand::Unpair { .. } => "server.unpair",
        ServerCommand::Run { .. } => "server.run",
        ServerCommand::Provision(_) => "server.provision",
    }
}

/// What every server command shares.
pub struct Context<'a> {
    pub identity: &'a str,
    pub global: &'a Global,
}

/// Which server a command acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Local,
    /// A Saved Connection name or `id:ROUTE`.
    Remote(String),
}

impl Context<'_> {
    /// `--server`, else `SPACETERM_SERVER`, else `local`.
    pub fn target(&self) -> Target {
        let named = self
            .global
            .server
            .clone()
            .or_else(|| std::env::var("SPACETERM_SERVER").ok())
            .filter(|name| !name.is_empty());
        match named {
            None => Target::Local,
            Some(name) if name == "local" => Target::Local,
            Some(name) => Target::Remote(name),
        }
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

    /// The CLI's connection to this account's own server, as the base identity or `--as`.
    pub fn local_connection(&self) -> Result<ServerConnection, Failure> {
        ServerConnection::for_cli(self.identity, CliRoute::Local, self.agent())
            .map_err(contract::from_server_error)
    }

    /// The server this command acts on, with its route. A remote target resolves through the
    /// CLI's Saved Connections.
    pub fn routed(&self) -> Result<Routed, (Option<ServerRef>, Failure)> {
        match self.target() {
            Target::Local => {
                let server = local_ref(self.identity).map_err(|failure| (None, failure))?;
                Ok(Routed {
                    connection: self.local_connection(),
                    server,
                    route: RouteKey::Local,
                    local: true,
                })
            }
            Target::Remote(_) => Err((None, Failure::new(Code::Unsupported))),
        }
    }

    /// Fails at once, before any network traffic, when the target is not this machine.
    pub fn require_local(&self) -> Result<(), Failure> {
        match self.target() {
            Target::Local => Ok(()),
            Target::Remote(_) => Err(Failure::new(Code::LocalOnly)),
        }
    }

    pub fn agent(&self) -> Option<spaceterm_server::protocol::AgentName> {
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

fn terminal_stdout() -> bool {
    terminal(libc::STDOUT_FILENO)
}

fn terminal(fd: libc::c_int) -> bool {
    // SAFETY: isatty only observes the descriptor.
    unsafe { libc::isatty(fd) == 1 }
}

/// A resolved server and the route its CLI state lives under.
pub struct Routed {
    /// The route's connection. A local one may be unavailable while the server is stopped.
    pub connection: Result<ServerConnection, Failure>,
    pub server: ServerRef,
    pub route: RouteKey,
    pub local: bool,
}

/// The local server's reference for the envelope, when it has an identity.
pub fn local_ref(identity: &str) -> Result<ServerRef, Failure> {
    let service = spaceterm_server::service::Service::for_identity(identity)
        .map_err(contract::from_server_error)?;
    let server = service
        .passive_server_id()
        .map_err(contract::from_server_error)?
        .ok_or(Failure::new(Code::NotSetUp))?;
    Ok(ServerRef {
        name: "local".to_owned(),
        route: "local".to_owned(),
        id: hex(&server),
    })
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}

/// A direct-management failure on this machine. The account IPC fails to connect only when the
/// server is not running.
pub fn local_direct(error: spaceterm_server::direct::DirectError) -> Failure {
    match error {
        spaceterm_server::direct::DirectError::Transport => Failure::new(Code::NotRunning),
        error => contract::from_direct_error(error),
    }
}

pub fn lane_failure(error: LaneError) -> Failure {
    match error {
        LaneError::Code(code) => contract::from_lane_code(&code),
        LaneError::Server(error) => contract::from_server_error(error),
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
    directory: spaceterm_server::Directory,
}

impl Pending {
    pub fn open(context: &Context<'_>, route: &RouteKey) -> Result<Self, Failure> {
        let state = CliState::open(context.identity).map_err(contract::from_server_error)?;
        let directory = state
            .identity_dir(route, context.agent().as_ref())
            .map_err(contract::from_server_error)?;
        Ok(Self { directory })
    }

    fn change(
        &self,
        apply: impl FnOnce(&mut Vec<String>) -> Result<(), Failure>,
    ) -> Result<Vec<String>, Failure> {
        let _lock = self
            .directory
            .lock("service-operations.lock", false)
            .map_err(contract::from_server_error)?;
        let mut pending: Vec<String> = self
            .directory
            .read_json(PENDING)
            .map_err(contract::from_server_error)?
            .unwrap_or_default();
        let before = pending.clone();
        apply(&mut pending)?;
        if pending != before {
            self.directory
                .write_json(PENDING_FILE, &pending)
                .map_err(contract::from_server_error)?;
        }
        Ok(pending)
    }

    pub fn list(&self) -> Result<Vec<String>, Failure> {
        self.change(|_| Ok(()))
    }

    /// Records an operation before its `Execute` is sent.
    pub fn add(&self, operation: &OperationId) -> Result<(), ServerError> {
        self.change(|pending| {
            if pending.len() >= MAX_PENDING {
                return Err(Failure::new(Code::Capacity));
            }
            pending.push(operation.to_string());
            Ok(())
        })
        .map(drop)
        .map_err(|_| ServerError::StorageUnavailable)
    }

    pub fn remove(&self, operation: &str) -> Result<(), Failure> {
        self.change(|pending| {
            pending.retain(|v| v != operation);
            Ok(())
        })
        .map(drop)
    }
}

/// Opens the lane of `connection` and settles what earlier commands of this identity left on
/// it. A pending operation that has not settled stays recorded for a later command.
pub fn open_lane(
    context: &Context<'_>,
    connection: &Result<ServerConnection, Failure>,
    route: &RouteKey,
    pending: &Pending,
) -> Result<Opened, Failure> {
    let identity = &context.identity_kind();
    let connection = connection.as_ref().map_err(Clone::clone)?;
    let lane = connection
        .service_lane()
        .map_err(contract::from_server_error)?;
    let mut session = lane.session().map_err(contract::from_server_error)?;
    let mut recovered = Vec::new();
    for operation in pending.list()? {
        let Some(id) = OperationId::parse(&operation) else {
            pending.remove(&operation)?;
            continue;
        };
        match spaceterm_server::service::lane::client::settle_leftover(&mut session, &id)
            .map_err(lane_failure)?
        {
            Leftover::Settled(receipt) => {
                recovered.push(recovered_operation(&receipt, identity));
                pending.remove(&operation)?;
            }
            Leftover::Gone => pending.remove(&operation)?,
            Leftover::Pending => {}
        }
    }
    recovered.extend(settle_journals(context, connection, route));
    Ok(Opened {
        lane,
        session,
        recovered,
    })
}

/// Settles the Workspace edit journals and runs this CLI holds for the route. They need the
/// work lane, which a server of another protocol major refuses, so they then wait for a later
/// command.
fn settle_journals(
    context: &Context<'_>,
    connection: &ServerConnection,
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
            RouteKey::Local => ServerConnection::for_cli(identity, CliRoute::Local, agent.cloned()),
            RouteKey::Paired(_) => Err(ServerError::Unsupported),
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
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if signal::interrupted() {
            return None;
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

/// `SIGINT` while a server command waits on the person or the server.
pub mod signal {
    use std::sync::atomic::{AtomicBool, Ordering};

    static INTERRUPTED: AtomicBool = AtomicBool::new(false);

    extern "C" fn record(_: libc::c_int) {
        INTERRUPTED.store(true, Ordering::SeqCst);
    }

    /// Records `SIGINT` instead of ending the process, and interrupts blocking reads so a
    /// waiting command can withdraw what it opened.
    pub fn install() {
        // SAFETY: the handler only stores to an atomic, which is async-signal-safe. The action
        // is fully initialized before use and has no SA_RESTART, so reads return EINTR.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = record as *const () as libc::sighandler_t;
            libc::sigemptyset(&mut action.sa_mask);
            action.sa_flags = 0;
            libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
        }
    }

    pub fn interrupted() -> bool {
        INTERRUPTED.load(Ordering::SeqCst)
    }
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
