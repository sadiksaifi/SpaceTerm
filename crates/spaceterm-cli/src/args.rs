//! The command tree. Clap enforces every flag rule here, and each rule's failure is a usage
//! error, exit 2. [`usage_failure`] renders clap errors without the rejected value.
use crate::contract::{Code, Details, Failure, UsageDetails, UsageKind};
use crate::io::cursor::Cursor;
use crate::scope::{IdPrefix, ScopeArgs, Selector};
use clap::{Args, Parser, Subcommand, ValueEnum, error::ErrorKind};
use spaceterm_server::protocol::AgentName;
use std::path::PathBuf;
use std::time::Duration;

/// Runs the SpaceTerm Server, and controls servers on this machine and on paired machines.
#[derive(Debug, Parser)]
#[command(
    name = "spaceterm",
    version = env!("SPACETERM_SERVER_VERSION"),
    disable_help_subcommand = true,
    propagate_version = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub command: Command,
}

/// Flags every command accepts.
#[derive(Debug, Args)]
pub struct Global {
    /// `local`, a Saved Connection name, or `id:ROUTE`. Defaults to SPACETERM_SERVER, then
    /// `local`.
    #[arg(long, global = true, value_name = "NAME|id:ROUTE")]
    pub server: Option<String>,
    /// Act as this agent identity. Defaults to SPACETERM_AS.
    #[arg(long = "as", global = true, value_name = "AGENT")]
    pub agent: Option<AgentName>,
    /// Print one JSON envelope on stdout. Implies --no-input.
    #[arg(long, global = true)]
    pub json: bool,
    /// Print nothing on stdout on success in text mode.
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// Print target resolution and progress on stderr.
    #[arg(short, long, global = true)]
    pub verbose: bool,
    /// Never prompt.
    #[arg(long, global = true)]
    pub no_input: bool,
    /// Give up after this long, from 1s to 24h.
    #[arg(long, global = true, value_name = "DURATION", value_parser = timeout)]
    pub timeout: Option<Duration>,
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant, reason = "parsed once per process")]
pub enum Command {
    /// Set up, pair and administer servers.
    #[command(
        subcommand,
        after_help = "Uninstall:\n  curl -fsSL https://github.com/sadiksaifi/SpaceTerm/releases/latest/download/install.sh | sh -s -- --headless --uninstall"
    )]
    Server(ServerCommand),
    /// Join servers, and list or remove Saved Connections.
    #[command(subcommand)]
    Connection(ConnectionCommand),
    /// Show Workspaces, Worktrees, Tabs and Panes.
    List(ListArgs),
    /// Create, rename and kill Workspaces.
    #[command(subcommand)]
    Workspace(WorkspaceCommand),
    /// Open and close Tabs.
    #[command(subcommand)]
    Tab(TabCommand),
    /// Split, close, run in, type into, read and release Panes.
    #[command(subcommand)]
    Pane(PaneCommand),
    /// Create and remove Worktrees.
    #[command(subcommand)]
    Worktree(WorktreeCommand),
    /// Print shell completions.
    #[command(hide = true)]
    Completion {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Debug, Subcommand)]
pub enum ServerCommand {
    /// Make this machine a server.
    Setup {
        /// `all`, `off`, an IP, or an address with a port.
        #[arg(long, value_name = "all|IP|IP:PORT|[IPV6]:PORT|off", value_parser = network)]
        network: Option<Network>,
        /// Start the server when you log in or the machine boots.
        #[arg(long, value_enum)]
        automatic_start: Option<Toggle>,
    },
    /// Let another device pair with this server.
    Pair {
        /// Advertise this DNS name or IP, with an optional port.
        #[arg(long, value_name = "HOST[:PORT]")]
        address: Option<String>,
    },
    /// Show the server's service, Network Access and paired devices.
    Status,
    /// Start the server.
    Start,
    /// Stop the server.
    Stop(Confirm),
    /// Restart the server.
    Restart(Confirm),
    /// Update the server, or return to the previous version.
    Update {
        /// Only check for an update.
        #[arg(long, conflicts_with = "confirm")]
        check: bool,
        /// Install this server archive instead of the signed feed's.
        #[arg(long, value_name = "FILE", requires = "signature")]
        archive: Option<PathBuf>,
        #[arg(long, value_name = "FILE", requires = "archive")]
        signature: Option<PathBuf>,
        #[arg(long, value_name = "TOKEN")]
        confirm: Option<String>,
    },
    /// Change the server's Network Access, on its own machine or from a Paired Device.
    Network {
        /// `all`, `off`, an IP, or an address with a port.
        #[arg(value_name = "all|IP|IP:PORT|[IPV6]:PORT|off", value_parser = network)]
        access: Network,
        #[arg(long, value_name = "TOKEN")]
        confirm: Option<String>,
    },
    /// End every Terminal Session on this machine, retaining the server and organization.
    EndAllSessions(Confirm),
    /// Revoke another Paired Device.
    Unpair {
        /// An exact device name, or `id:PAIRING`.
        device: String,
        #[arg(long, value_name = "TOKEN")]
        confirm: Option<String>,
    },
    /// Run the server.
    #[command(hide = true)]
    Run {
        /// Run as the account service.
        #[arg(long, conflicts_with = "owned")]
        service: bool,
        /// Run until the supervisor closes standard input. Development only.
        #[arg(long)]
        owned: bool,
        /// The identity the service unit names. It must be this executable's.
        #[arg(long)]
        identity: Option<String>,
    },
    #[command(hide = true, subcommand)]
    Provision(ProvisionCommand),
}

/// Steps the installer, the runtime verifier, SSH Setup and the service helper run.
#[derive(Debug, Subcommand)]
pub enum ProvisionCommand {
    /// Retain an unpacked installation for the installer.
    Retain {
        installation: PathBuf,
    },
    /// Remove the account service and only its managed CLI launcher.
    Uninstall {
        #[arg(long, value_name = "TOKEN")]
        confirm: Option<String>,
    },
    SshDiscover,
    SshDirectStatus,
    SshDirectPrepare {
        address: String,
    },
    SshDirect {
        address: String,
    },
    SshDirectCancel,
    SshStart,
    SshInstall {
        archive: PathBuf,
        signature: PathBuf,
        sha256: String,
    },
    SshEnableLinger {
        /// Read the sudo password from stdin.
        #[arg(long)]
        password_stdin: bool,
    },
    BuildInfo,
    Check,
    ServiceRun {
        #[arg(long)]
        operation: String,
        #[arg(long)]
        identity: String,
    },
}

#[derive(Debug, Args)]
pub struct Confirm {
    #[arg(long, value_name = "TOKEN")]
    pub confirm: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum ConnectionCommand {
    /// Join a server with its Invitation Link, read from stdin or a prompt.
    Add {
        name: Option<String>,
        /// Give consent without a prompt.
        #[arg(long)]
        join: bool,
        /// Connect to this DNS name or IP, with an optional port, instead of the invitation's.
        #[arg(long, value_name = "HOST[:PORT]")]
        address: Option<String>,
    },
    /// Show Saved Connections.
    List,
    /// Unpair This Computer from a server.
    Remove {
        name: String,
        #[arg(long, value_name = "TOKEN")]
        confirm: Option<String>,
    },
}

/// `--workspace`, `--worktree`, `--tab` and `--pane`.
#[derive(Debug, Args)]
pub struct Scope {
    /// A Workspace: `current`, `id:HEX` or its name.
    #[arg(long, value_name = "W", value_parser = selector)]
    pub workspace: Option<Selector>,
    /// A Worktree: `current`, `path:DIR`, a branch or `main-worktree`.
    #[arg(long, value_name = "T", value_parser = selector)]
    pub worktree: Option<Selector>,
    /// A Tab: `current`, `id:HEX` or what one of its Panes shows.
    #[arg(long, value_name = "B", value_parser = selector)]
    pub tab: Option<Selector>,
    /// A Pane: `current`, `id:HEX` or what it shows.
    #[arg(long, value_name = "P", value_parser = selector)]
    pub pane: Option<Selector>,
}

impl Scope {
    pub fn args(&self) -> ScopeArgs {
        ScopeArgs {
            workspace: self.workspace.clone(),
            worktree: self.worktree.clone(),
            tab: self.tab.clone(),
            pane: self.pane.clone(),
        }
    }
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub scope: Scope,
}

#[derive(Debug, Subcommand)]
pub enum WorkspaceCommand {
    /// Create a Workspace.
    Create {
        name: String,
        /// Pin the Workspace to this directory. Quote `~/` for a remote server.
        #[arg(long, value_name = "DIR")]
        directory: Option<PathBuf>,
    },
    /// Rename a Workspace.
    Rename {
        new_name: String,
        #[arg(long, value_name = "W", value_parser = selector)]
        workspace: Option<Selector>,
    },
    /// Kill a Workspace, for every client.
    Kill {
        #[arg(long, value_name = "W", value_parser = selector)]
        workspace: Option<Selector>,
        #[arg(long, value_name = "TOKEN")]
        confirm: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum TabCommand {
    /// Open a Tab.
    Create {
        #[arg(long, value_name = "W", value_parser = selector)]
        workspace: Option<Selector>,
        #[arg(long, value_name = "T", value_parser = selector)]
        worktree: Option<Selector>,
        /// The source Pane.
        #[arg(long, value_name = "P", value_parser = selector)]
        pane: Option<Selector>,
    },
    /// Close one Tab, or every Tab of the Worktree with --all.
    Close {
        #[arg(long, value_name = "W", value_parser = selector)]
        workspace: Option<Selector>,
        #[arg(long, value_name = "T", value_parser = selector)]
        worktree: Option<Selector>,
        #[arg(long, value_name = "B", value_parser = selector, conflicts_with = "all")]
        tab: Option<Selector>,
        #[arg(long)]
        all: bool,
        #[arg(long, value_name = "TOKEN")]
        confirm: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant, reason = "parsed once per process")]
pub enum PaneCommand {
    /// Split a Pane.
    Split {
        /// Split to the right. This is the default.
        #[arg(long, conflicts_with = "down")]
        right: bool,
        #[arg(long)]
        down: bool,
        #[command(flatten)]
        scope: Scope,
    },
    /// Close one Pane.
    Close {
        #[command(flatten)]
        scope: Scope,
        #[arg(long, value_name = "TOKEN")]
        confirm: Option<String>,
    },
    /// Run one command at the prompt and return its output and exit status.
    Run(RunArgs),
    /// Type text or keys into a running Session.
    Send(SendArgs),
    /// Observe the screen and Scrollback.
    Read(ReadArgs),
    /// Give up this identity's control of a Pane.
    Release {
        #[command(flatten)]
        scope: Scope,
    },
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// One command in the shell's own language, or `-` to read one line from stdin.
    #[arg(value_name = "COMMAND", value_parser = command_text)]
    pub command: String,
    #[command(flatten)]
    pub scope: Scope,
    /// Act only on this Terminal Session.
    #[arg(long, value_name = "ID", value_parser = id_prefix)]
    pub session: Option<IdPrefix>,
    /// Take control from another client.
    #[arg(long)]
    pub take_over: bool,
    #[arg(long, value_name = "COLSxROWS", value_parser = size)]
    pub size: Option<TerminalSize>,
    /// Repair a Pane whose directory is unavailable by starting here.
    #[arg(long, value_name = "DIR")]
    pub directory: Option<PathBuf>,
    /// Keep only the last N rows of output.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=10_000))]
    pub tail: Option<u32>,
}

#[derive(Debug, Args)]
pub struct SendArgs {
    /// Text to type, or `-` with --paste to read stdin.
    pub text: Option<String>,
    #[command(flatten)]
    pub scope: Scope,
    /// Act only on this Terminal Session.
    #[arg(long, value_name = "ID", value_parser = id_prefix)]
    pub session: Option<IdPrefix>,
    #[arg(long)]
    pub take_over: bool,
    /// A key to press, such as Enter, C-c or M-x. Repeatable.
    #[arg(long = "key", value_name = "NAME", conflicts_with = "paste")]
    pub keys: Vec<String>,
    /// Press Enter after the text.
    #[arg(long)]
    pub enter: bool,
    /// Send the text as a paste.
    #[arg(long)]
    pub paste: bool,
    /// Send a paste that the terminal would ask to confirm.
    #[arg(long)]
    pub allow_unsafe_paste: bool,
    #[arg(long, value_name = "COLSxROWS", value_parser = size)]
    pub size: Option<TerminalSize>,
}

#[derive(Debug, Args)]
pub struct ReadArgs {
    #[command(flatten)]
    pub scope: Scope,
    /// Act only on this Terminal Session.
    #[arg(long, value_name = "ID", value_parser = id_prefix)]
    pub session: Option<IdPrefix>,
    /// The last N Scrollback rows, then the screen.
    #[arg(
        long,
        value_name = "N",
        value_parser = clap::value_parser!(u32).range(1..=10_000),
        conflicts_with_all = ["before", "after"]
    )]
    pub tail: Option<u32>,
    /// Rows ending before this cursor.
    #[arg(long, value_name = "CURSOR", value_parser = cursor, conflicts_with = "after")]
    pub before: Option<Cursor>,
    /// Rows from this cursor on.
    #[arg(long, value_name = "CURSOR", value_parser = cursor)]
    pub after: Option<Cursor>,
    /// The most rows to return, up to 10000.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=10_000))]
    pub rows: Option<u32>,
    /// Wait until this text appears.
    #[arg(long, value_name = "TEXT", conflicts_with_all = ["wait_regex", "wait_idle"])]
    pub wait_for: Option<String>,
    /// Wait until this regular expression matches.
    #[arg(long, value_name = "RE", conflicts_with = "wait_idle")]
    pub wait_regex: Option<String>,
    /// Wait until the Session writes no output for this long.
    #[arg(long, value_name = "DURATION", value_parser = idle)]
    pub wait_idle: Option<Duration>,
    /// Plain text, or text with synthesized colors and styles.
    #[arg(long, value_enum, value_name = "text|ansi")]
    pub format: Option<ReadFormat>,
    /// The most bytes to return, up to 8 MiB.
    #[arg(
        long,
        value_name = "N",
        value_parser = clap::value_parser!(u32).range(1..=8 * 1024 * 1024)
    )]
    pub max_bytes: Option<u32>,
}

#[derive(Debug, Subcommand)]
pub enum WorktreeCommand {
    /// Create a Worktree.
    Create {
        branch: String,
        #[arg(long, value_name = "W", value_parser = selector)]
        workspace: Option<Selector>,
        /// Start a new branch from this ref. Defaults to the Main Worktree's branch.
        #[arg(long, value_name = "REF")]
        base: Option<String>,
        #[arg(long, value_name = "DIR")]
        path: Option<PathBuf>,
        /// Also open a Tab in the new Worktree.
        #[arg(long)]
        open: bool,
    },
    /// Remove a Worktree.
    Remove {
        #[arg(long, value_name = "W", value_parser = selector)]
        workspace: Option<Selector>,
        #[arg(long, value_name = "T", value_parser = selector)]
        worktree: Option<Selector>,
        #[arg(long, value_name = "TOKEN")]
        confirm: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Toggle {
    On,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ReadFormat {
    Text,
    Ansi,
}

/// `server setup --network`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Network {
    All,
    Off,
    /// An IP, or an address with a port.
    Address(spaceterm_server::direct::NetworkPolicy),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalSize {
    pub columns: u16,
    pub rows: u16,
}

impl Command {
    /// The dotted command path the envelope reports, such as `pane.run`.
    pub fn path(&self) -> &'static str {
        match self {
            Self::Server(command) => match command {
                ServerCommand::Setup { .. } => "server.setup",
                ServerCommand::Pair { .. } => "server.pair",
                ServerCommand::Status => "server.status",
                ServerCommand::Start => "server.start",
                ServerCommand::Stop(_) => "server.stop",
                ServerCommand::Restart(_) => "server.restart",
                ServerCommand::Update { .. } => "server.update",
                ServerCommand::Network { .. } => "server.network",
                ServerCommand::EndAllSessions(_) => "server.end-all-sessions",
                ServerCommand::Unpair { .. } => "server.unpair",
                ServerCommand::Run { .. } => "server.run",
                ServerCommand::Provision(_) => "server.provision",
            },
            Self::Connection(command) => match command {
                ConnectionCommand::Add { .. } => "connection.add",
                ConnectionCommand::List => "connection.list",
                ConnectionCommand::Remove { .. } => "connection.remove",
            },
            Self::List(_) => "list",
            Self::Workspace(command) => match command {
                WorkspaceCommand::Create { .. } => "workspace.create",
                WorkspaceCommand::Rename { .. } => "workspace.rename",
                WorkspaceCommand::Kill { .. } => "workspace.kill",
            },
            Self::Tab(command) => match command {
                TabCommand::Create { .. } => "tab.create",
                TabCommand::Close { .. } => "tab.close",
            },
            Self::Pane(command) => match command {
                PaneCommand::Split { .. } => "pane.split",
                PaneCommand::Close { .. } => "pane.close",
                PaneCommand::Run(_) => "pane.run",
                PaneCommand::Send(_) => "pane.send",
                PaneCommand::Read(_) => "pane.read",
                PaneCommand::Release { .. } => "pane.release",
            },
            Self::Worktree(command) => match command {
                WorktreeCommand::Create { .. } => "worktree.create",
                WorktreeCommand::Remove { .. } => "worktree.remove",
            },
            Self::Completion { .. } => "completion",
        }
    }
}

// Value parsers. Their errors carry no value: clap errors are rendered by `usage_failure`.

fn selector(text: &str) -> Result<Selector, String> {
    Selector::parse(text).map_err(|_| String::new())
}

fn id_prefix(text: &str) -> Result<IdPrefix, String> {
    IdPrefix::parse(text.strip_prefix("id:").unwrap_or(text)).ok_or_else(String::new)
}

fn cursor(text: &str) -> Result<Cursor, String> {
    Cursor::decode(text).map_err(|_| String::new())
}

fn positive_duration(text: &str, min: Duration) -> Result<Duration, String> {
    let duration: jiff::SignedDuration = text.parse().map_err(|_| String::new())?;
    let duration = Duration::try_from(duration).map_err(|_| String::new())?;
    if duration < min || duration > Duration::from_secs(24 * 60 * 60) {
        return Err(String::new());
    }
    Ok(duration)
}

fn timeout(text: &str) -> Result<Duration, String> {
    positive_duration(text, Duration::from_secs(1))
}

fn idle(text: &str) -> Result<Duration, String> {
    positive_duration(text, Duration::from_millis(1))
}

fn network(text: &str) -> Result<Network, String> {
    match text {
        "all" => Ok(Network::All),
        "off" => Ok(Network::Off),
        address => spaceterm_server::direct::NetworkPolicy::parse(address)
            .map(Network::Address)
            .map_err(|_| String::new()),
    }
}

fn size(text: &str) -> Result<TerminalSize, String> {
    let (columns, rows) = text.split_once('x').ok_or_else(String::new)?;
    let columns: u16 = columns.parse().map_err(|_| String::new())?;
    let rows: u16 = rows.parse().map_err(|_| String::new())?;
    if !(2..=spaceterm_server::pane::MAX_COLUMNS).contains(&columns)
        || !(2..=spaceterm_server::pane::MAX_ROWS).contains(&rows)
    {
        return Err(String::new());
    }
    Ok(TerminalSize { columns, rows })
}

/// One command line: no newline, no C0 or C1 control character, within the protocol bound.
fn command_text(text: &str) -> Result<String, String> {
    if text.is_empty()
        || text.len() > spaceterm_server::protocol::MAX_RUN_COMMAND_BYTES
        || text.chars().any(char::is_control)
    {
        return Err(String::new());
    }
    Ok(text.to_owned())
}

/// A clap error as a bounded usage failure. It names the argument from the command tree and
/// the error kind, never the value given.
pub fn usage_failure(error: &clap::Error) -> Failure {
    let kind = match error.kind() {
        ErrorKind::InvalidValue | ErrorKind::ValueValidation => UsageKind::InvalidValue,
        ErrorKind::UnknownArgument => UsageKind::UnknownArgument,
        ErrorKind::InvalidSubcommand => UsageKind::InvalidSubcommand,
        ErrorKind::NoEquals => UsageKind::NoEquals,
        ErrorKind::TooManyValues => UsageKind::TooManyValues,
        ErrorKind::WrongNumberOfValues | ErrorKind::TooFewValues => UsageKind::WrongNumberOfValues,
        ErrorKind::ArgumentConflict => UsageKind::ArgumentConflict,
        ErrorKind::MissingRequiredArgument => UsageKind::MissingArgument,
        ErrorKind::MissingSubcommand | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            UsageKind::MissingSubcommand
        }
        ErrorKind::InvalidUtf8 => UsageKind::InvalidUtf8,
        _ => UsageKind::Other,
    };
    let code = match kind {
        UsageKind::InvalidSubcommand | UsageKind::MissingSubcommand => Code::UnknownCommand,
        _ => Code::InvalidArguments,
    };
    let argument = match kind {
        // These name what the person typed, which this tree does not define.
        UsageKind::UnknownArgument | UsageKind::InvalidSubcommand => None,
        _ => known_argument(error),
    };
    Failure::with(code, Details::Usage(UsageDetails { argument, kind }))
}

/// The argument clap blamed, when it is one this tree defines: `--long` or `<VALUE>`.
fn known_argument(error: &clap::Error) -> Option<String> {
    use clap::error::{ContextKind, ContextValue};
    let blamed = match error.get(ContextKind::InvalidArg)? {
        ContextValue::String(text) => text.clone(),
        ContextValue::Strings(texts) => texts.first()?.clone(),
        _ => return None,
    };
    let name = blamed.split_whitespace().next()?.to_owned();
    let command = <Cli as clap::CommandFactory>::command();
    let mut known = Vec::new();
    collect_arguments(&command, &mut known);
    known.contains(&name).then_some(name)
}

fn collect_arguments(command: &clap::Command, known: &mut Vec<String>) {
    for argument in command.get_arguments() {
        if let Some(long) = argument.get_long() {
            known.push(format!("--{long}"));
        }
        if let Some(names) = argument.get_value_names() {
            known.extend(names.iter().map(|name| format!("<{name}>")));
        }
    }
    for subcommand in command.get_subcommands() {
        collect_arguments(subcommand, known);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("spaceterm").chain(args.iter().copied()))
    }

    #[test]
    fn the_command_tree_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_visible_command_parses() {
        for args in [
            &[
                "server",
                "setup",
                "--network",
                "all",
                "--automatic-start",
                "on",
            ][..],
            &["server", "setup", "--network", "192.0.2.1:7000"],
            &["server", "pair", "--address", "203.0.113.5"],
            &["server", "status"],
            &["server", "start"],
            &["server", "stop", "--confirm", "c1-00"],
            &["server", "restart"],
            &["server", "update", "--check"],
            &["server", "update", "--archive", "a", "--signature", "s"],
            &["server", "unpair", "id:9c1d44e0"],
            &["connection", "add", "penguin", "--join"],
            &["connection", "list"],
            &["connection", "remove", "penguin"],
            &["list", "--workspace", "api"],
            &[
                "workspace",
                "create",
                "api",
                "--directory",
                "~/Projects/api",
            ],
            &["workspace", "rename", "api-v2", "--workspace", "api"],
            &["workspace", "kill", "--workspace", "id:0be4f3"],
            &["tab", "create", "--worktree", "main-worktree"],
            &["tab", "close", "--worktree", "path:/srv/x", "--all"],
            &["pane", "split", "--down"],
            &["pane", "close", "--pane", "current"],
            &[
                "pane",
                "run",
                "cargo build",
                "--size",
                "120x40",
                "--tail",
                "5",
            ],
            &["pane", "send", "--key", "C-c", "--key", "Enter"],
            &["pane", "read", "--tail", "20", "--wait-for", "Listening on"],
            &["pane", "read", "--wait-idle", "2s", "--format", "ansi"],
            &["pane", "release"],
            &["worktree", "create", "feature-y", "--open"],
            &["worktree", "remove", "--worktree", "feature-x"],
            &["--as", "agent-1", "--json", "--timeout", "30s", "list"],
        ] {
            parse(args).unwrap_or_else(|error| panic!("{args:?}: {error}"));
        }
    }

    #[test]
    fn flag_rules_are_usage_errors() {
        for args in [
            &["tab", "close", "--tab", "x", "--all"][..],
            &[
                "pane", "read", "--tail", "1", "--rows", "2", "--after", "h1-AA",
            ],
            &["pane", "read", "--wait-for", "x", "--wait-idle", "1s"],
            &["pane", "read", "--wait-regex", "x", "--wait-for", "y"],
            &["pane", "send", "--key", "Enter", "--paste"],
            &["server", "update", "--check", "--confirm", "c1-00"],
            &["server", "update", "--archive", "a"],
            &["pane", "split", "--right", "--down"],
            &["--timeout", "0s", "list"],
            &["--timeout", "-5s", "list"],
            &["--timeout", "25h", "list"],
            &["--as", "Agent", "list"],
            &["pane", "run", "echo a\necho b"],
            &["pane", "run", "printf \u{1b}"],
            &["pane", "read", "--rows", "10001"],
            &["pane", "run", "true", "--size", "1x40"],
            &["list", "--pane", "id:abc"],
        ] {
            let error = parse(args).expect_err(&format!("{args:?}"));
            assert_eq!(usage_failure(&error).code.exit(), 2, "{args:?}");
        }
    }

    #[test]
    fn usage_failures_never_carry_the_rejected_value() {
        let secret = "private-value-must-not-appear";
        for args in [
            vec!["--timeout", secret, "list"],
            vec!["list", "--private-value-must-not-appear"],
            vec!["private-value-must-not-appear"],
            vec!["pane", "read", "--before", secret],
        ] {
            let error = parse(&args).unwrap_err();
            let failure = usage_failure(&error);
            let text = format!(
                "{}{}",
                failure.text(),
                serde_json::to_string(&crate::contract::ErrorBody::from(failure.clone())).unwrap()
            );
            assert!(!text.contains(secret), "{text}");
        }
        let failure = usage_failure(&parse(&["--timeout", secret, "list"]).unwrap_err());
        assert_eq!(
            failure.details,
            Details::Usage(UsageDetails {
                argument: Some("--timeout".into()),
                kind: UsageKind::InvalidValue,
            })
        );
    }

    #[test]
    fn help_lists_the_visible_tree_without_hidden_commands() {
        let mut command = Cli::command();
        let help = command.render_long_help().to_string();
        for visible in [
            "server",
            "connection",
            "list",
            "workspace",
            "tab",
            "pane",
            "worktree",
        ] {
            assert!(help.contains(visible), "{visible}");
        }
        assert!(!help.contains("completion"));
        let server = command.find_subcommand_mut("server").unwrap();
        let help = server.render_long_help().to_string();
        assert!(help.contains("setup") && help.contains("unpair"));
        assert!(!help.contains("provision") && !help.contains("  run"));
    }

    #[test]
    fn every_command_has_a_dotted_path() {
        let cli = parse(&["pane", "run", "true"]).unwrap();
        assert_eq!(cli.command.path(), "pane.run");
        let cli = parse(&["list"]).unwrap();
        assert_eq!(cli.command.path(), "list");
    }
}
