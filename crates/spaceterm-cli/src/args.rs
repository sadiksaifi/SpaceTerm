//! The command tree. Clap enforces every flag rule here, and each rule's failure is a usage
//! error, exit 2. [`usage_failure`] renders clap errors without the rejected value.
use crate::contract::{Code, Details, Failure, UsageDetails, UsageKind};
use crate::io::cursor::Cursor;
use crate::scope::{IdPrefix, ScopeArgs, Selector};
use clap::{Args, Parser, Subcommand, ValueEnum, error::ErrorKind};
use spaceterm_core::protocol::AgentName;
use std::path::PathBuf;
use std::time::Duration;

/// SpaceTerm Core's command line: administer SpaceTerm Core on this machine and on paired
/// machines, and work in their Workspaces.
#[derive(Debug, Parser)]
#[command(
    name = "spaceterm",
    version = env!("SPACETERM_CLI_VERSION"),
    disable_help_subcommand = true,
    propagate_version = true,
    help_template = HELP
)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub command: Command,
}

/// The top-level help. Clap lists subcommands under one heading, so this template groups them by
/// what they act on. Each line repeats its command's own summary; a test keeps the two equal.
const HELP: &str = "\
{about-with-newline}
{usage-heading} {usage}

SpaceTerm Core on this machine. [--machine] marks commands that also reach a paired machine.
  setup             Set up SpaceTerm Core on this machine
  status            Show SpaceTerm Core status [--machine]
  start             Start SpaceTerm Core on this machine
  stop              Stop SpaceTerm Core on this machine. Every Terminal Session ends
  restart           Restart SpaceTerm Core. Every Terminal Session ends [--machine]
  update            Install a SpaceTerm Core update beside the running version [--machine]
  network           Show or change Network Access [--machine]
  pair              Let another device pair with this machine
  unpair            Revoke another Paired Device
  permissions       Show the System Permissions of this machine's Terminal Sessions, or set one up
  end-all-sessions  End every Terminal Session on this machine, keeping Workspaces
  uninstall         Uninstall SpaceTerm Core from this machine, keeping its data unless asked

Machines this CLI reaches. These commands reject --machine.
  machine           Add, list, rename and remove machines

Work on the selected machine:
  list              Show Workspaces, Worktrees, Tabs and Panes
  workspace         Create, rename, pin, unpin and kill Workspaces
  worktree          Create and remove Worktrees
  tab               Create and close Tabs
  pane              Split, close, run in, type into, read and release Panes

The desktop on this machine. These commands reject --machine.
  open              Open a local file, or its File Preview, in the desktop showing this Terminal Session
  show              Bring the desktop's own view to a Workspace, Tab or Pane

Shell:
  completion        Print shell completions

Options:
{options}{after-help}
";

/// Flags every command accepts.
#[derive(Debug, Args)]
pub struct Global {
    /// `local`, a machine name from `spaceterm machine list`, or `id:ROUTE`. Defaults to
    /// SPACETERM_MACHINE, then `local`.
    #[arg(long, global = true, value_name = "NAME|id:ROUTE")]
    pub machine: Option<String>,
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
    #[command(flatten)]
    Core(CoreCommand),
    /// Add, list, rename and remove machines.
    #[command(subcommand)]
    Machine(MachineCommand),
    /// Show Workspaces, Worktrees, Tabs and Panes.
    List(ListArgs),
    /// Create, rename, pin, unpin and kill Workspaces.
    #[command(subcommand)]
    Workspace(WorkspaceCommand),
    /// Create and remove Worktrees.
    #[command(subcommand)]
    Worktree(WorktreeCommand),
    /// Create and close Tabs.
    #[command(subcommand)]
    Tab(TabCommand),
    /// Split, close, run in, type into, read and release Panes.
    #[command(subcommand)]
    Pane(PaneCommand),
    /// Open a local file, or its File Preview, in the desktop showing this Terminal Session.
    Open(OpenArgs),
    /// Bring the desktop's own view to a Workspace, Tab or Pane.
    Show(ShowArgs),
    /// Print shell completions.
    Completion {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Report the desktop's Terminal Diagnostics counters.
    #[command(hide = true)]
    Diagnostics,
    /// Steps launchd, systemd, the installer, permission probes and SSH Setup run.
    #[command(hide = true, subcommand)]
    Internal(InternalCommand),
}

/// SpaceTerm Core on this machine. `status`, `restart`, `update` and `network` also reach a
/// paired machine with `--machine`.
#[derive(Debug, Subcommand)]
pub enum CoreCommand {
    /// Set up SpaceTerm Core on this machine.
    Setup {
        /// `all`, `off`, an IP, or an address with a port.
        #[arg(long, value_name = "all|IP|IP:PORT|[IPV6]:PORT|off", value_parser = network)]
        network: Option<Network>,
        /// Start SpaceTerm Core when you log in or the machine boots.
        #[arg(long, value_enum)]
        automatic_start: Option<Toggle>,
        /// The HTTP proxy SpaceTerm Core checks its update feed through, or `off`.
        #[arg(long, value_name = "http://HOST:PORT|off", value_parser = update_proxy)]
        update_proxy: Option<UpdateProxy>,
    },
    /// Show SpaceTerm Core status.
    ///
    /// Its service, version, Network Access and paired devices, on this machine or with
    /// --machine on a paired one.
    Status,
    /// Start SpaceTerm Core on this machine.
    Start,
    /// Stop SpaceTerm Core on this machine. Every Terminal Session ends.
    Stop(Confirm),
    /// Restart SpaceTerm Core. Every Terminal Session ends.
    Restart(Confirm),
    /// Install a SpaceTerm Core update beside the running version.
    ///
    /// Installing ends nothing. Restart SpaceTerm Core to use the new version. The desktop
    /// updates itself.
    Update {
        /// Install this signed SpaceTerm Core archive instead of the release the feed names.
        #[arg(long, value_name = "FILE", requires = "signature")]
        archive: Option<PathBuf>,
        /// The archive's raw 64-byte signature.
        #[arg(long, value_name = "FILE", requires = "archive")]
        signature: Option<PathBuf>,
    },
    /// Show or change Network Access.
    Network {
        /// `all`, `off`, an IP, or an address with a port. Without it, show Network Access.
        #[arg(value_name = "all|IP|IP:PORT|[IPV6]:PORT|off", value_parser = network)]
        access: Option<Network>,
        #[arg(long, value_name = "TOKEN", requires = "access")]
        confirm: Option<String>,
    },
    /// Let another device pair with this machine.
    Pair {
        /// Advertise this DNS name or IP, with an optional port.
        #[arg(long, value_name = "ADDRESS[:PORT]")]
        address: Option<String>,
    },
    /// Revoke another Paired Device.
    Unpair {
        /// An exact device name, or `id:PAIRING`.
        device: String,
        #[arg(long, value_name = "TOKEN")]
        confirm: Option<String>,
    },
    /// Show the System Permissions of this machine's Terminal Sessions, or set one up.
    ///
    /// Programs in Terminal Sessions inherit them from SpaceTerm Core on this machine.
    Permissions {
        #[command(subcommand)]
        action: Option<PermissionsCommand>,
    },
    /// End every Terminal Session on this machine, keeping Workspaces.
    EndAllSessions(Confirm),
    /// Uninstall SpaceTerm Core from this machine, keeping its data unless asked.
    ///
    /// Removes the account service, the CLI launcher and every installed version. Workspaces,
    /// Machine Settings, pairings and saved machines remain unless --delete-data is given.
    Uninstall {
        /// Also delete SpaceTerm Core's data on this machine, this CLI's saved machines and
        /// their credentials.
        #[arg(long)]
        delete_data: bool,
        #[arg(long, value_name = "TOKEN")]
        confirm: Option<String>,
    },
}

/// Setting up one System Permission on this machine.
#[derive(Debug, Subcommand)]
pub enum PermissionsCommand {
    /// Ask the system for a permission: a prompt for the microphone or camera, otherwise an entry
    /// in System Settings to turn on.
    Request { permission: PermissionName },
    /// Open System Settings at the list that holds a permission.
    Settings { permission: PermissionName },
}

/// A System Permission as the command line names it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum PermissionName {
    Microphone,
    Camera,
    ScreenRecording,
    /// Device Control on macOS 27 and later.
    Accessibility,
}

impl PermissionName {
    pub const fn permission(self) -> spaceterm_core::permissions::SystemPermission {
        use spaceterm_core::permissions::SystemPermission;
        match self {
            Self::Microphone => SystemPermission::Microphone,
            Self::Camera => SystemPermission::Camera,
            Self::ScreenRecording => SystemPermission::ScreenRecording,
            Self::Accessibility => SystemPermission::Accessibility,
        }
    }

    pub const fn of(permission: spaceterm_core::permissions::SystemPermission) -> Self {
        use spaceterm_core::permissions::SystemPermission;
        match permission {
            SystemPermission::Microphone => Self::Microphone,
            SystemPermission::Camera => Self::Camera,
            SystemPermission::ScreenRecording => Self::ScreenRecording,
            SystemPermission::Accessibility => Self::Accessibility,
        }
    }

    /// The spelling a printed command uses.
    pub const fn spelling(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::Camera => "camera",
            Self::ScreenRecording => "screen-recording",
            Self::Accessibility => "accessibility",
        }
    }
}

/// Steps launchd, systemd, the installer, permission probes and SSH Setup run. Their names and
/// flags are an interface with those callers.
#[derive(Debug, Subcommand)]
pub enum InternalCommand {
    /// Run SpaceTerm Core: as the account service, owned by a Development supervisor, or in the
    /// foreground.
    Serve {
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
    /// Run one transient service operation for SpaceTerm Core. Without an operation, finish an
    /// activation a reboot interrupted.
    Helper {
        #[arg(long)]
        operation: Option<String>,
        #[arg(long)]
        identity: String,
    },
    /// Retain an unpacked installation for the installer.
    Retain {
        installation: PathBuf,
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
    /// Report this process's own System Permissions, as SpaceTerm Core's probe child.
    PermissionReport,
    /// Ask the system for one System Permission, as SpaceTerm Core's request child.
    PermissionRequest {
        permission: PermissionName,
    },
}

impl InternalCommand {
    /// The dotted command path the envelope reports, such as `internal.serve`.
    pub const fn path(&self) -> &'static str {
        match self {
            Self::Serve { .. } => "internal.serve",
            Self::Helper { .. } => "internal.helper",
            Self::Retain { .. } => "internal.retain",
            Self::SshDiscover => "internal.ssh-discover",
            Self::SshDirectStatus => "internal.ssh-direct-status",
            Self::SshDirectPrepare { .. } => "internal.ssh-direct-prepare",
            Self::SshDirect { .. } => "internal.ssh-direct",
            Self::SshDirectCancel => "internal.ssh-direct-cancel",
            Self::SshStart => "internal.ssh-start",
            Self::SshInstall { .. } => "internal.ssh-install",
            Self::SshEnableLinger { .. } => "internal.ssh-enable-linger",
            Self::BuildInfo => "internal.build-info",
            Self::Check => "internal.check",
            Self::PermissionReport => "internal.permission-report",
            Self::PermissionRequest { .. } => "internal.permission-request",
        }
    }
}

/// `setup --update-proxy`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateProxy {
    Off,
    Proxy(spaceterm_core::update::proxy::Proxy),
}

#[derive(Debug, Args)]
pub struct Confirm {
    #[arg(long, value_name = "TOKEN")]
    pub confirm: Option<String>,
}

/// The machines this CLI reaches: its Saved Connections. Their names are the values
/// `--machine` takes.
#[derive(Debug, Subcommand)]
pub enum MachineCommand {
    /// Add a machine with its Invitation Link, read from stdin or a prompt.
    Add {
        /// The name `--machine` takes. Defaults to the machine's own name.
        name: Option<String>,
        /// Trust the invitation's pinned fingerprint without a prompt.
        #[arg(long)]
        trust: bool,
        /// Connect to this DNS name or IP, with an optional port, instead of the invitation's.
        #[arg(long, value_name = "ADDRESS[:PORT]")]
        address: Option<String>,
    },
    /// Show the machines this CLI reaches.
    List,
    /// Rename a machine for this CLI only, keeping its pairing.
    Rename { old: String, new: String },
    /// Remove a machine and unpair this device from it.
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
    #[arg(long, value_name = "WORKSPACE", value_parser = selector)]
    pub workspace: Option<Selector>,
    /// A Worktree: `current`, `path:DIR`, a branch or `main-worktree`.
    #[arg(long, value_name = "WORKTREE", value_parser = selector)]
    pub worktree: Option<Selector>,
    /// A Tab: `current`, `id:HEX` or what one of its Panes shows.
    #[arg(long, value_name = "TAB", value_parser = selector)]
    pub tab: Option<Selector>,
    /// A Pane: `current`, `id:HEX` or what it shows.
    #[arg(long, value_name = "PANE", value_parser = selector)]
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
        /// Pin the Workspace to this directory. Quote `~/` for a paired machine.
        #[arg(long, value_name = "DIR")]
        directory: Option<PathBuf>,
    },
    /// Rename a Workspace.
    Rename {
        new_name: String,
        #[arg(long, value_name = "WORKSPACE", value_parser = selector)]
        workspace: Option<Selector>,
    },
    /// Set a Workspace's Pinned Directory.
    Pin {
        /// Directory on the selected machine. Quote `~/` for a paired machine.
        #[arg(value_name = "DIR")]
        directory: PathBuf,
        #[arg(long, value_name = "WORKSPACE", value_parser = selector)]
        workspace: Option<Selector>,
    },
    /// Clear a Workspace's Pinned Directory.
    Unpin {
        #[arg(long, value_name = "WORKSPACE", value_parser = selector)]
        workspace: Option<Selector>,
    },
    /// Kill a Workspace, for every client.
    Kill {
        #[arg(long, value_name = "WORKSPACE", value_parser = selector)]
        workspace: Option<Selector>,
        #[arg(long, value_name = "TOKEN")]
        confirm: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum TabCommand {
    /// Create a Tab.
    Create {
        #[arg(long, value_name = "WORKSPACE", value_parser = selector)]
        workspace: Option<Selector>,
        #[arg(long, value_name = "WORKTREE", value_parser = selector)]
        worktree: Option<Selector>,
        /// The source Pane.
        #[arg(long, value_name = "PANE", value_parser = selector)]
        pane: Option<Selector>,
    },
    /// Close one Tab, or every Tab of the Worktree with --all.
    Close {
        #[arg(long, value_name = "WORKSPACE", value_parser = selector)]
        workspace: Option<Selector>,
        #[arg(long, value_name = "WORKTREE", value_parser = selector)]
        worktree: Option<Selector>,
        #[arg(long, value_name = "TAB", value_parser = selector, conflicts_with = "all")]
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

#[derive(Debug, Args)]
pub struct OpenArgs {
    /// A local file. Relative paths start at the current directory.
    #[arg(value_name = "PATH")]
    pub path: PathBuf,
    /// Show its File Preview instead of opening it.
    #[arg(long)]
    pub preview: bool,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    #[command(flatten)]
    pub scope: Scope,
}

#[derive(Debug, Subcommand)]
pub enum WorktreeCommand {
    /// Create a Worktree.
    Create {
        branch: String,
        #[arg(long, value_name = "WORKSPACE", value_parser = selector)]
        workspace: Option<Selector>,
        /// Start a new branch from this ref. Defaults to the Main Worktree's branch.
        #[arg(long, value_name = "REF")]
        base: Option<String>,
        #[arg(long, value_name = "DIR")]
        path: Option<PathBuf>,
    },
    /// Remove a Worktree.
    Remove {
        #[arg(long, value_name = "WORKSPACE", value_parser = selector)]
        workspace: Option<Selector>,
        #[arg(long, value_name = "WORKTREE", value_parser = selector)]
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

/// `setup --network` and `network`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Network {
    All,
    Off,
    /// An IP, or an address with a port.
    Address(spaceterm_core::direct::NetworkPolicy),
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
            Self::Core(command) => command.path(),
            Self::Machine(command) => match command {
                MachineCommand::Add { .. } => "machine.add",
                MachineCommand::List => "machine.list",
                MachineCommand::Rename { .. } => "machine.rename",
                MachineCommand::Remove { .. } => "machine.remove",
            },
            Self::List(_) => "list",
            Self::Workspace(command) => match command {
                WorkspaceCommand::Create { .. } => "workspace.create",
                WorkspaceCommand::Rename { .. } => "workspace.rename",
                WorkspaceCommand::Pin { .. } => "workspace.pin",
                WorkspaceCommand::Unpin { .. } => "workspace.unpin",
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
            Self::Open(_) => "open",
            Self::Show(_) => "show",
            Self::Diagnostics => "diagnostics",
            Self::Completion { .. } => "completion",
            Self::Internal(command) => command.path(),
        }
    }
}

impl CoreCommand {
    /// The dotted command path the envelope reports, such as `permissions.request`.
    pub const fn path(&self) -> &'static str {
        match self {
            Self::Setup { .. } => "setup",
            Self::Status => "status",
            Self::Start => "start",
            Self::Stop(_) => "stop",
            Self::Restart(_) => "restart",
            Self::Update { .. } => "update",
            Self::Network { .. } => "network",
            Self::Pair { .. } => "pair",
            Self::Unpair { .. } => "unpair",
            Self::Permissions { action } => match action {
                None => "permissions",
                Some(PermissionsCommand::Request { .. }) => "permissions.request",
                Some(PermissionsCommand::Settings { .. }) => "permissions.settings",
            },
            Self::EndAllSessions(_) => "end-all-sessions",
            Self::Uninstall { .. } => "uninstall",
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

fn update_proxy(text: &str) -> Result<UpdateProxy, String> {
    match text {
        "off" => Ok(UpdateProxy::Off),
        proxy => spaceterm_core::update::proxy::Proxy::parse(proxy)
            .map(UpdateProxy::Proxy)
            .map_err(|_| String::new()),
    }
}

fn network(text: &str) -> Result<Network, String> {
    match text {
        "all" => Ok(Network::All),
        "off" => Ok(Network::Off),
        address => spaceterm_core::direct::NetworkPolicy::parse(
            address,
            spaceterm_core::direct::default_port(env!("SPACETERM_CLI_IDENTITY")),
        )
        .map(Network::Address)
        .map_err(|_| String::new()),
    }
}

fn size(text: &str) -> Result<TerminalSize, String> {
    let (columns, rows) = text.split_once('x').ok_or_else(String::new)?;
    let columns: u16 = columns.parse().map_err(|_| String::new())?;
    let rows: u16 = rows.parse().map_err(|_| String::new())?;
    if !(2..=spaceterm_core::pane::MAX_COLUMNS).contains(&columns)
        || !(2..=spaceterm_core::pane::MAX_ROWS).contains(&rows)
    {
        return Err(String::new());
    }
    Ok(TerminalSize { columns, rows })
}

/// One command line: no newline, no C0 or C1 control character, within the protocol bound.
fn command_text(text: &str) -> Result<String, String> {
    if text.is_empty()
        || text.len() > spaceterm_core::protocol::MAX_RUN_COMMAND_BYTES
        || text.chars().any(char::is_control)
    {
        return Err(String::new());
    }
    Ok(text.to_owned())
}

/// The tree shell completions describe: the visible commands only. Clap's generators also
/// complete hidden subcommands, so they are left out here.
pub fn completion_tree() -> clap::Command {
    use clap::CommandFactory;
    let full = Cli::command();
    clap::Command::new("spaceterm")
        .version(env!("SPACETERM_CLI_VERSION"))
        .disable_help_subcommand(true)
        .args(full.get_arguments().cloned())
        .subcommands(
            full.get_subcommands()
                .filter(|subcommand| !subcommand.is_hide_set())
                .cloned(),
        )
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
            &["setup", "--network", "all", "--automatic-start", "on"][..],
            &["setup", "--network", "192.0.2.1:7000"],
            &["pair", "--address", "203.0.113.5"],
            &["status", "--machine", "penguin"],
            &["start"],
            &["stop", "--confirm", "c1-00"],
            &["restart", "--machine", "penguin"],
            &["update"],
            &["update", "--archive", "a", "--signature", "s"],
            &["setup", "--update-proxy", "http://proxy.example:3128"],
            &["setup", "--update-proxy", "off"],
            &["network"],
            &["network", "off", "--confirm", "c1-00"],
            &["unpair", "id:9c1d44e0"],
            &["permissions", "request", "microphone"],
            &["end-all-sessions"],
            &["uninstall", "--confirm", "c1-00"],
            &["uninstall", "--delete-data", "--confirm", "c1-00"],
            &["machine", "add", "penguin", "--trust"],
            &["machine", "list"],
            &["machine", "rename", "penguin", "lab"],
            &["machine", "remove", "penguin"],
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
            &["worktree", "create", "feature-y"],
            &["worktree", "remove", "--worktree", "feature-x"],
            &["open", "notes.md", "--preview"],
            &["show", "--tab", "id:0be4f3"],
            &["completion", "zsh"],
            &["--as", "agent-1", "--json", "--timeout", "30s", "list"],
        ] {
            parse(args).unwrap_or_else(|error| panic!("{args:?}: {error}"));
        }
    }

    #[test]
    fn launchd_systemd_probes_and_ssh_setup_reach_the_internal_steps() {
        for (args, path) in [
            (
                &["internal", "serve", "--service", "--identity", "spaceterm"][..],
                "internal.serve",
            ),
            (&["internal", "serve", "--owned"], "internal.serve"),
            (
                &[
                    "internal",
                    "helper",
                    "--operation",
                    "00",
                    "--identity",
                    "spaceterm",
                ],
                "internal.helper",
            ),
            (
                &["internal", "helper", "--identity", "spaceterm"][..],
                "internal.helper",
            ),
            (
                &["internal", "retain", "/tmp/installation"],
                "internal.retain",
            ),
            (&["internal", "build-info"], "internal.build-info"),
            (&["internal", "check"], "internal.check"),
            (
                &["internal", "permission-report"],
                "internal.permission-report",
            ),
            (
                &["internal", "permission-request", "camera"],
                "internal.permission-request",
            ),
            (&["internal", "ssh-discover"], "internal.ssh-discover"),
            (
                &["internal", "ssh-direct-status"],
                "internal.ssh-direct-status",
            ),
            (
                &["internal", "ssh-direct-prepare", "all"],
                "internal.ssh-direct-prepare",
            ),
            (&["internal", "ssh-direct", "all"], "internal.ssh-direct"),
            (
                &["internal", "ssh-direct-cancel"],
                "internal.ssh-direct-cancel",
            ),
            (&["internal", "ssh-start"], "internal.ssh-start"),
            (
                &["internal", "ssh-install", "a", "s", "00"],
                "internal.ssh-install",
            ),
            (
                &["internal", "ssh-enable-linger", "--password-stdin"],
                "internal.ssh-enable-linger",
            ),
        ] {
            let cli = parse(args).unwrap_or_else(|error| panic!("{args:?}: {error}"));
            assert_eq!(cli.command.path(), path, "{args:?}");
        }
    }

    #[test]
    fn removed_names_no_longer_parse() {
        for args in [
            &["server", "status"][..],
            &["connection", "list"],
            &["--server", "local", "list"],
            &["machine", "add", "--join"],
            &["worktree", "create", "feature-y", "--open"],
            &["permissions", "request", "device-control"],
            &["internal", "uninstall"],
        ] {
            parse(args).expect_err(&format!("{args:?}"));
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
            &["update", "--check"],
            &["update", "--confirm", "c1-00"],
            &["update", "--archive", "a"],
            &["setup", "--update-proxy", "https://proxy.example:3128"],
            &[
                "setup",
                "--update-proxy",
                "http://user:secret@proxy.example:3128",
            ],
            &["network", "--confirm", "c1-00"],
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
    fn grouped_help_lists_each_visible_command_once_with_its_own_summary() {
        let mut command = Cli::command();
        let help = command.render_help().to_string();
        let mut listed = 0;
        for subcommand in command.get_subcommands() {
            let name = subcommand.get_name();
            let lines: Vec<_> = help
                .lines()
                .filter(|line| {
                    line.split_whitespace().next() == Some(name) && line.starts_with("  ")
                })
                .collect();
            if subcommand.is_hide_set() {
                assert!(lines.is_empty(), "{name} is hidden but listed");
                continue;
            }
            let about = subcommand.get_about().unwrap().to_string();
            assert_eq!(lines.len(), 1, "{name} is listed {} times", lines.len());
            assert!(
                lines[0].trim_start()[name.len()..]
                    .trim_start()
                    .starts_with(&about),
                "{name}: {:?} does not start with {about:?}",
                lines[0]
            );
            listed += 1;
        }
        assert_eq!(listed, 21);
        assert!(help.contains("--machine <NAME|id:ROUTE>"));
    }

    #[test]
    fn pane_run_and_list_report_their_command_paths() {
        let cli = parse(&["pane", "run", "true"]).unwrap();
        assert_eq!(cli.command.path(), "pane.run");
        let cli = parse(&["list"]).unwrap();
        assert_eq!(cli.command.path(), "list");
    }
}
