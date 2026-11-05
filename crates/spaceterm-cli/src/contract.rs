//! The CLI output contract, `spaceterm.cli.v1`: the envelope, codes, exit classes, bounded
//! details and per-command data.
//!
//! Errors and warnings carry fixed messages and closed `details` structs only: IDs, counts,
//! enums, versions, durations, exit statuses, operation IDs and timestamps. Labels, titles,
//! paths, selector values, rejected arguments, regex sources, Git output and native error
//! strings never appear in them. Operational facts such as candidates and captured output go in
//! `data`.
//!
//! Within v1, fields are only added. Removing, renaming or retyping one creates
//! `spaceterm.cli.v2`.
use serde::{Serialize, Serializer};
use spaceterm_core::{
    CoreError,
    direct::DirectError,
    hierarchy,
    migration::{DataKind, ResetReason},
    protocol::{Fatal, version::Incompatible},
    update::ordering::Floor,
    worktree,
};
use std::borrow::Cow;

pub const SCHEMA: &str = "spaceterm.cli.v1";
/// A broken stdout pipe stops output and exits with this status, without a message.
pub const EXIT_BROKEN_PIPE: u8 = 141;

/// Streaming output keeps a closed reader distinct from other write failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputError {
    BrokenPipe,
    Failed,
}
impl From<std::io::Error> for OutputError {
    fn from(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::BrokenPipe {
            Self::BrokenPipe
        } else {
            Self::Failed
        }
    }
}

/// The empty object, for `data` or `details` with nothing to report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Empty {}

// Envelope

/// One command's single JSON document on stdout.
#[derive(Clone, Debug, Serialize)]
pub struct Envelope<D: Serialize = Empty> {
    pub schema: &'static str,
    /// The dotted command path, such as `pane.run`. Empty when parsing failed before a command.
    pub command: String,
    pub status: Status,
    /// Null for `machine` commands, and before the command identified its machine.
    pub machine: Option<MachineRef>,
    pub data: Data<D>,
    pub error: Option<ErrorBody>,
    pub warnings: Vec<Warning>,
}

impl<D: Serialize> Envelope<D> {
    pub fn ok(command: impl Into<String>, machine: Option<MachineRef>, data: D) -> Self {
        Self {
            schema: SCHEMA,
            command: command.into(),
            status: Status::Ok,
            machine,
            data: Data::new(data),
            error: None,
            warnings: Vec::new(),
        }
    }

    /// An error. `data` may still hold operational results.
    pub fn error(
        command: impl Into<String>,
        machine: Option<MachineRef>,
        failure: Failure,
        data: D,
    ) -> Self {
        Self {
            schema: SCHEMA,
            command: command.into(),
            status: Status::Error,
            machine,
            data: Data::new(data),
            error: Some(ErrorBody::from(failure)),
            warnings: Vec::new(),
        }
    }

    /// The exit status this envelope's command ends with.
    pub fn exit(&self) -> u8 {
        match (&self.status, &self.error) {
            (_, Some(error)) => error.class.exit(),
            (Status::ConfirmationRequired, None) => Class::ActionRequired.exit(),
            _ => Class::Ok.exit(),
        }
    }
}

impl<E: Serialize> Envelope<ConfirmationData<E>> {
    /// The preview of a confirmed command. It exits 3, `confirmation_required`.
    pub fn confirmation(
        command: impl Into<String>,
        machine: Option<MachineRef>,
        confirmation: Confirmation<E>,
    ) -> Self {
        Self {
            schema: SCHEMA,
            command: command.into(),
            status: Status::ConfirmationRequired,
            machine,
            data: Data::new(ConfirmationData { confirmation }),
            error: Some(ErrorBody::from(Failure::new(Code::ConfirmationRequired))),
            warnings: Vec::new(),
        }
    }
}

impl<D: Serialize> Envelope<D> {
    /// The same envelope with `data` as a JSON value, so commands of every type share one
    /// renderer.
    pub fn erased(&self) -> Envelope<serde_json::Value> {
        Envelope {
            schema: self.schema,
            command: self.command.clone(),
            status: self.status,
            machine: self.machine.clone(),
            data: Data {
                body: serde_json::to_value(&self.data.body).unwrap_or_default(),
                recovered_operations: self.data.recovered_operations.clone(),
            },
            error: self.error.clone(),
            warnings: self.warnings.clone(),
        }
    }

    /// Records that `SIGINT` arrived after dispatch. The envelope keeps its data, the confirmed
    /// outcome, and exits 130 unless its own class outranks `interrupted`, as `unconfirmed` does.
    pub fn interrupt(&mut self) {
        let class = self.error.as_ref().map_or(Class::Ok, |error| error.class);
        if decisive([class, Class::Interrupted]) == Class::Interrupted {
            self.status = Status::Error;
            self.error = Some(ErrorBody::from(Failure::new(Code::Interrupted)));
        }
    }
}

// Rendering

/// The output flags every command shares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mode {
    pub json: bool,
    pub quiet: bool,
    pub verbose: bool,
}

impl From<&crate::args::Global> for Mode {
    fn from(global: &crate::args::Global) -> Self {
        Self {
            json: global.json,
            quiet: global.quiet,
            verbose: global.verbose,
        }
    }
}

thread_local! {
    static ANNOUNCED: std::cell::RefCell<Vec<Warning>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Adds `warning` to the next result this thread renders, whichever command produces it. The
/// CLI announces what happened to its state before the command ran.
pub fn announce(warning: Warning) {
    ANNOUNCED.with_borrow_mut(|announced| announced.push(warning));
}

/// Writes a command's one result and returns its exit status. Every command ends here.
///
/// JSON mode prints the envelope and a newline on stdout. Text mode prints `text` on stdout,
/// except with `--quiet` after success, then the error line, a usage error's argument and every
/// warning on stderr. `--verbose` adds `diagnostics` on stderr. A broken stdout pipe stops output
/// and exits 141 with no message. `interrupted` means `SIGINT` arrived after dispatch: the
/// envelope reports it, so the exit status follows the precedence of section 5.2.
/// Warnings [`announce`]d on this thread join the envelope's own.
pub fn render<D: Serialize>(
    envelope: &Envelope<D>,
    text: &str,
    diagnostics: &str,
    mode: Mode,
    interrupted: bool,
    stdout: &mut dyn std::io::Write,
    stderr: &mut dyn std::io::Write,
) -> u8 {
    let announced = ANNOUNCED.take();
    if !announced.is_empty() {
        let mut envelope = envelope.erased();
        envelope.warnings.extend(announced);
        return render(
            &envelope,
            text,
            diagnostics,
            mode,
            interrupted,
            stdout,
            stderr,
        );
    }
    if interrupted {
        let mut envelope = envelope.erased();
        envelope.interrupt();
        return render(&envelope, text, diagnostics, mode, false, stdout, stderr);
    }
    let exit = envelope.exit();
    let written = if mode.json {
        serde_json::to_writer(&mut *stdout, envelope)
            .map_err(std::io::Error::from)
            .and_then(|()| stdout.write_all(b"\n"))
            .and_then(|()| stdout.flush())
    } else if mode.quiet && exit == 0 {
        Ok(())
    } else {
        stdout
            .write_all(text.as_bytes())
            .and_then(|()| stdout.flush())
    };
    if let Err(error) = written {
        if error.kind() == std::io::ErrorKind::BrokenPipe {
            return EXIT_BROKEN_PIPE;
        }
        let _ = writeln!(stderr, "{}", Failure::new(Code::OutputFailed).text());
        return Code::OutputFailed.exit();
    }
    if !mode.json {
        if let Some(error) = &envelope.error {
            let _ = writeln!(stderr, "error: {} ({})", error.message, error.code.as_str());
            if let Details::Usage(UsageDetails {
                argument: Some(argument),
                ..
            }) = &error.details
            {
                let _ = writeln!(stderr, "argument: {argument}");
            }
            if let Details::StateTooOld(StateTooOldDetails { floor, .. }) = &error.details {
                let _ = writeln!(stderr, "install first: SpaceTerm Core {floor}");
            }
        }
        write_warnings(&envelope.warnings, stderr);
    }
    if mode.verbose {
        let _ = stderr.write_all(diagnostics.as_bytes());
    }
    exit
}

/// The text-mode warning lines, which a command also prints before it asks for confirmation.
pub fn write_warnings(warnings: &[Warning], stderr: &mut dyn std::io::Write) {
    for warning in warnings {
        let _ = writeln!(stderr, "warning: {}", warning.message);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    ConfirmationRequired,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MachineRef {
    /// `local` or the machine's name in `machine list`.
    pub name: String,
    /// `local` or `id:<64 hex>`.
    pub route: String,
    /// The Machine ID, 64 hex.
    pub id: String,
}

/// A command's `data` and the leftovers it settled first.
#[derive(Clone, Debug, Serialize)]
pub struct Data<D: Serialize> {
    #[serde(flatten)]
    pub body: D,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recovered_operations: Vec<RecoveredOperation>,
}

impl<D: Serialize> Data<D> {
    pub fn new(body: D) -> Self {
        Self {
            body,
            recovered_operations: Vec::new(),
        }
    }
}

/// A leftover journal entry or run this command settled before its own work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RecoveredOperation {
    /// The operation's sequence or run ID.
    pub operation: String,
    pub identity: RecoveredIdentity,
    pub kind: RecoveredKind,
    pub outcome: RecoveredOutcome,
}

/// `base`, or the agent name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveredIdentity {
    Base,
    Agent(spaceterm_core::protocol::AgentName),
}

impl Serialize for RecoveredIdentity {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Base => serializer.serialize_str("base"),
            Self::Agent(name) => serializer.serialize_str(name.as_str()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveredKind {
    Mutation,
    Run,
    /// A Stop, Restart or Update the service lane admitted.
    Service,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RecoveredOutcome {
    pub state: RecoveredState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<Code>,
    /// The ID the applied outcome named, 64 hex.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveredState {
    Applied,
    Refused,
    Unknown,
}

// Errors

/// A failure before it is written: its code and bounded details.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: Code,
    pub details: Details,
}

impl Failure {
    pub const fn new(code: Code) -> Self {
        Self {
            code,
            details: Details::None,
        }
    }

    pub const fn with(code: Code, details: Details) -> Self {
        Self { code, details }
    }

    /// The fixed message: the code's own, or for targeting codes the one for its level.
    pub fn message(&self) -> &'static str {
        match (&self.code, &self.details) {
            (Code::AmbiguousTarget, Details::Target(target)) => ambiguous_message(target.level),
            (Code::NotFound, Details::Target(target)) => not_found_message(target.level),
            (Code::ScopeMismatch, Details::Target(target)) => target
                .parent
                .and_then(|parent| mismatch_message(target.level, parent))
                .unwrap_or(self.code.message()),
            _ => self.code.message(),
        }
    }

    /// The text-mode line on stderr, for example
    /// `error: More than one Tab matches. (ambiguous_target)`.
    pub fn text(&self) -> String {
        format!("error: {} ({})", self.message(), self.code.as_str())
    }
}

impl From<Code> for Failure {
    fn from(code: Code) -> Self {
        Self::new(code)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ErrorBody {
    pub code: Code,
    pub class: Class,
    /// The fixed message of `code`.
    pub message: &'static str,
    pub retryable: bool,
    pub details: Details,
}

impl From<Failure> for ErrorBody {
    fn from(failure: Failure) -> Self {
        Self {
            code: failure.code,
            class: failure.code.class(),
            message: failure.message(),
            retryable: failure.code.retryable(),
            details: failure.details,
        }
    }
}

/// The closed `details` of an error. Each code uses at most one variant. All serialize as an
/// object, `{}` for `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Details {
    #[default]
    None,
    /// `invalid_arguments`, `unknown_command`.
    Usage(UsageDetails),
    /// `not_found`, `ambiguous_target`, `scope_mismatch`, `target_required`.
    Target(TargetDetails),
    /// `controlled`.
    Controlled(ControlledDetails),
    /// `session_not_started`, `session_ended`.
    Session(SessionDetails),
    /// `unsupported_session`.
    UnsupportedSession(UnsupportedSessionDetails),
    /// `history_reset`.
    History(HistoryDetails),
    /// `invalid_location` from a Worktree Location template.
    Template(TemplateDetails),
    /// `command_failed`.
    CommandFailed(CommandFailedDetails),
    /// `run_unconfirmed`.
    RunUnconfirmed(RunUnconfirmedDetails),
    /// `outcome_unconfirmed`, `activation_unresolved`, `operation_failed`, and lane failures that
    /// name a service operation.
    Operation(OperationDetails),
    /// `timeout`.
    Timeout(TimeoutDetails),
    /// `incompatible_protocol`, and `desktop_incompatible` when the two share no Desktop
    /// Channel major.
    Incompatible(IncompatibleDetails),
    /// `below_floor`.
    Floor(FloorDetails),
    /// `state_reset_required`.
    StateReset(StateResetDetails),
    /// `state_too_old`.
    StateTooOld(StateTooOldDetails),
}

impl Serialize for Details {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::None => Empty {}.serialize(serializer),
            Self::Usage(details) => details.serialize(serializer),
            Self::Target(details) => details.serialize(serializer),
            Self::Controlled(details) => details.serialize(serializer),
            Self::Session(details) => details.serialize(serializer),
            Self::UnsupportedSession(details) => details.serialize(serializer),
            Self::History(details) => details.serialize(serializer),
            Self::Template(details) => details.serialize(serializer),
            Self::CommandFailed(details) => details.serialize(serializer),
            Self::RunUnconfirmed(details) => details.serialize(serializer),
            Self::Operation(details) => details.serialize(serializer),
            Self::Timeout(details) => details.serialize(serializer),
            Self::Incompatible(details) => details.serialize(serializer),
            Self::Floor(details) => details.serialize(serializer),
            Self::StateReset(details) => details.serialize(serializer),
            Self::StateTooOld(details) => details.serialize(serializer),
        }
    }
}

/// A parse failure names the argument and the error kind only, never the value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UsageDetails {
    /// The argument's own name from the command tree, such as `--tail`, never what was typed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument: Option<String>,
    pub kind: UsageKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageKind {
    InvalidValue,
    UnknownArgument,
    MissingArgument,
    MissingSubcommand,
    InvalidSubcommand,
    ArgumentConflict,
    TooManyValues,
    WrongNumberOfValues,
    NoEquals,
    InvalidUtf8,
    /// A path a paired machine cannot resolve, such as a relative one.
    RelativePath,
    /// Command text with a newline or a control character.
    ControlCharacter,
    /// A reserved or duplicate name.
    ReservedName,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Machine,
    Workspace,
    Worktree,
    Tab,
    Pane,
    Session,
    Branch,
    Device,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TargetDetails {
    pub level: Level,
    /// The explicit parent level that disagreed, for `scope_mismatch`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<Level>,
    /// How many candidates matched, for `ambiguous_target`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// The candidates' IDs, 64 hex, for `ambiguous_target`. Worktrees have none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
}

impl TargetDetails {
    pub const fn level(level: Level) -> Self {
        Self {
            level,
            parent: None,
            count: None,
            ids: Vec::new(),
        }
    }
}

const fn ambiguous_message(level: Level) -> &'static str {
    match level {
        Level::Machine => "More than one machine matches.",
        Level::Workspace => "More than one Workspace matches.",
        Level::Worktree => "More than one Worktree matches.",
        Level::Tab => "More than one Tab matches.",
        Level::Pane => "More than one Pane matches.",
        Level::Session => "More than one Terminal Session matches.",
        Level::Branch => "More than one branch matches.",
        Level::Device => "More than one Paired Device matches.",
    }
}

const fn not_found_message(level: Level) -> &'static str {
    match level {
        Level::Machine => "No machine matches.",
        Level::Workspace => "No Workspace matches.",
        Level::Worktree => "No Worktree matches.",
        Level::Tab => "No Tab matches.",
        Level::Pane => "No Pane matches.",
        Level::Session => "No Terminal Session matches.",
        Level::Branch => "No branch matches.",
        Level::Device => "No Paired Device matches.",
    }
}

const fn mismatch_message(level: Level, parent: Level) -> Option<&'static str> {
    Some(match (level, parent) {
        (Level::Worktree, Level::Workspace) => "The Worktree is not in the given Workspace.",
        (Level::Tab, Level::Workspace) => "The Tab is not in the given Workspace.",
        (Level::Tab, Level::Worktree) => "The Tab is not in the given Worktree.",
        (Level::Pane, Level::Workspace) => "The Pane is not in the given Workspace.",
        (Level::Pane, Level::Worktree) => "The Pane is not in the given Worktree.",
        (Level::Pane, Level::Tab) => "The Pane is not in the given Tab.",
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlledReason {
    /// Another client controls the Pane.
    Other,
    /// This connection no longer controls the Terminal Session.
    Stale,
    /// Another client took over during a run.
    TakenOver,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ControlledDetails {
    pub reason: ControlledReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStateName {
    NotStarted,
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SessionDetails {
    pub state: SessionStateName,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedSessionReason {
    /// The shell is not one with SpaceTerm shell integration.
    Shell,
    /// No owner-marked prompt was seen in this Session.
    NoIntegration,
    /// Bracketed paste mode is off.
    NoBracketedPaste,
    /// SpaceTerm Core on a Mac needs a logged-in desktop session.
    NoGuiSession,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct UnsupportedSessionDetails {
    pub reason: UnsupportedSessionReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct HistoryDetails {
    pub epoch: u64,
    pub oldest: u64,
    pub total: u64,
}

/// A `WorktreePathTemplateError` variant name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateError {
    Empty,
    TooLong,
    NotAbsolute,
    ParentSegment,
    UnclosedBrace,
    UnknownPlaceholder,
    MissingBranch,
    InvalidCharacter,
    NotWritable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct TemplateDetails {
    pub template: TemplateError,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct CommandFailedDetails {
    pub exit_status: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunDoubtName {
    NoExitStatus,
    NoCommandStart,
    CmdlineMismatch,
    IntegrationLost,
    /// The connection was lost and `RunStatus` did not settle the run before the deadline.
    Disconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RunUnconfirmedDetails {
    pub reason: RunDoubtName,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OperationDetails {
    /// A service operation ID, or a journal sequence.
    pub operation: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct TimeoutDetails {
    pub elapsed_ms: u64,
}

/// `details` when the CLI and the other side share no protocol major.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct IncompatibleDetails {
    /// The older side, which the person updates.
    pub update: Update,
}

/// `details` when SpaceTerm Core on the Machine runs a version older than the Update Floor of
/// the version it was offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct FloorDetails {
    /// The release that Machine updates to first, `MAJOR.MINOR.PATCH`.
    pub floor: Floor,
}

/// `details` when this CLI's saved state waits for a confirmed reset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StateResetDetails {
    pub reason: ResetReason,
    /// The kinds of data `spaceterm reset` replaces.
    pub kinds: Vec<DataKind>,
}

/// `details` when this CLI's saved state is older than any generation this build migrates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StateTooOldDetails {
    /// The Stepping Stone release to install first, this build's Update Floor,
    /// `MAJOR.MINOR.PATCH`.
    pub floor: Floor,
    /// The kinds of data `spaceterm reset` replaces instead.
    pub kinds: Vec<DataKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Update {
    /// SpaceTerm Core on the machine the command reached.
    Machine,
    /// The desktop app.
    Desktop,
    /// SpaceTerm Core on this machine, which ships this CLI.
    Core,
}

// Exit classes and codes

/// An exit class and its status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    Ok,
    Failed,
    Usage,
    ActionRequired,
    Target,
    Unavailable,
    Deadline,
    Unconfirmed,
    Interrupted,
}

impl Class {
    pub const fn exit(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Failed => 1,
            Self::Usage => 2,
            Self::ActionRequired => 3,
            Self::Target => 4,
            Self::Unavailable => 5,
            Self::Deadline => 6,
            Self::Unconfirmed => 7,
            Self::Interrupted => 130,
        }
    }

    /// Higher wins when one command meets several outcomes. Usage errors are found before any
    /// effect. After dispatch, 7 wins over everything, then 130, then 5, 3, 4, 6 and 1.
    const fn rank(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Failed => 1,
            Self::Deadline => 2,
            Self::Target => 3,
            Self::ActionRequired => 4,
            Self::Unavailable => 5,
            Self::Usage => 6,
            Self::Interrupted => 7,
            Self::Unconfirmed => 8,
        }
    }
}

/// The class a command exits with when it met every class in `classes`.
pub fn decisive(classes: impl IntoIterator<Item = Class>) -> Class {
    classes
        .into_iter()
        .max_by_key(|class| class.rank())
        .unwrap_or(Class::Ok)
}

macro_rules! codes {
    ($($variant:ident = $name:literal, $class:ident, $retryable:literal, $message:literal;)*) => {
        /// Every error code of the contract.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Code {
            $($variant,)*
        }

        impl Code {
            pub const ALL: &'static [Code] = &[$(Code::$variant,)*];

            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Code::$variant => $name,)*
                }
            }

            pub const fn class(self) -> Class {
                match self {
                    $(Code::$variant => Class::$class,)*
                }
            }

            /// Whether the same command may succeed when repeated unchanged.
            pub const fn retryable(self) -> bool {
                match self {
                    $(Code::$variant => $retryable,)*
                }
            }

            /// The fixed message. It names no label, path or value.
            pub const fn message(self) -> &'static str {
                match self {
                    $(Code::$variant => $message,)*
                }
            }

            pub fn parse(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(Code::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

codes! {
    // CLI-native: usage.
    InvalidArguments = "invalid_arguments", Usage, false, "Invalid arguments. See --help.";
    UnknownCommand = "unknown_command", Usage, false, "Unknown command. See --help.";
    TargetRequired = "target_required", Usage, false, "This command needs an explicit target.";
    // CLI-native: action required.
    ConfirmationRequired = "confirmation_required", ActionRequired, false, "Review the effects, then repeat the command with --confirm.";
    InteractionRequired = "interaction_required", ActionRequired, false, "This command needs a terminal.";
    PasteConfirmationRequired = "paste_confirmation_required", ActionRequired, false, "This paste needs --allow-unsafe-paste.";
    // CLI-native: targeting.
    NotFound = "not_found", Target, false, "Nothing matches the target.";
    AmbiguousTarget = "ambiguous_target", Target, false, "More than one target matches.";
    ScopeMismatch = "scope_mismatch", Target, false, "The target is not in the given scope.";
    ContextUnavailable = "context_unavailable", Target, false, "There is no current Pane on this machine.";
    // CLI-native: cursors.
    CursorMismatch = "cursor_mismatch", Target, false, "The cursor belongs to another Pane or Terminal Session.";
    HistoryReset = "history_reset", Target, true, "The Scrollback was reset since this cursor.";
    HistoryHidden = "history_hidden", Target, false, "The Scrollback is hidden by the alternate screen.";
    // CLI-native: Pane state.
    SessionChanged = "session_changed", Target, false, "The Terminal Session changed.";
    SessionNotStarted = "session_not_started", Target, false, "The Pane has no running Terminal Session.";
    SessionEnded = "session_ended", Target, false, "The Terminal Session ended.";
    NotAtPrompt = "not_at_prompt", Target, true, "The shell is not waiting at an empty prompt.";
    RepairNotNeeded = "repair_not_needed", Target, false, "The Terminal Session's directory is available.";
    NoFallbackWorktree = "no_fallback_worktree", Target, false, "No other Worktree can keep the Workspace open.";
    // CLI-native: the desktop on this machine.
    NotShown = "not_shown", Target, false, "The desktop on this machine doesn\u{2019}t show this work.";
    RemoteSession = "remote_session", Target, false, "This Terminal Session runs on another machine. Its paths grant no local file access.";
    FileUnavailable = "file_unavailable", Target, false, "The path names no regular file this account can read.";
    DesktopUnavailable = "desktop_unavailable", Unavailable, true, "No SpaceTerm desktop is running for this account on this machine.";
    DesktopIncompatible = "desktop_incompatible", Unavailable, false, "This CLI and the desktop are incompatible. Update the older one.";
    PreviewUnavailable = "preview_unavailable", Unavailable, false, "This desktop has no File Preview.";
    DesktopBusy = "desktop_busy", Unavailable, true, "The desktop is still checking earlier files. Try again.";
    // CLI-native: route or environment.
    LocalOnly = "local_only", Unavailable, false, "This command runs only on the machine itself. Run it there without --machine.";
    DisconnectsEveryDevice = "disconnects_every_device", Unavailable, false, "This change would leave no connected device a route to that machine. Make it on the machine itself.";
    UnsupportedSession = "unsupported_session", Unavailable, false, "This session does not support the command.";
    IdentityBusy = "identity_busy", Unavailable, true, "Another command of this identity is running.";
    CredentialUnavailable = "credential_unavailable", Unavailable, false, "The pairing credential store is unavailable or unsafe.";
    // CLI-native: deadline.
    Timeout = "timeout", Deadline, true, "The deadline passed.";
    // CLI-native: uncertainty.
    OutcomeUnconfirmed = "outcome_unconfirmed", Unconfirmed, false, "SpaceTerm couldn\u{2019}t confirm the outcome.";
    InputUnconfirmed = "input_unconfirmed", Unconfirmed, false, "SpaceTerm couldn\u{2019}t confirm the input was delivered.";
    RunUnconfirmed = "run_unconfirmed", Unconfirmed, false, "SpaceTerm couldn\u{2019}t confirm how the command finished.";
    // CLI-native: failure.
    CommandFailed = "command_failed", Failed, false, "The command exited with a nonzero status.";
    OutputFailed = "output_failed", Failed, false, "SpaceTerm couldn\u{2019}t write the output.";
    OperationFailed = "operation_failed", Failed, false, "SpaceTerm Core did not perform the operation.";
    AppRunning = "app_running", ActionRequired, false, "SpaceTerm is running. Quit SpaceTerm, then run this again.";
    AppNotRemoved = "app_not_removed", Failed, false, "SpaceTerm Core is uninstalled, but SpaceTerm couldn\u{2019}t remove the SpaceTerm app.";
    PermissionFailed = "permission_failed", Failed, false, "The system didn\u{2019}t complete the permission operation.";
    Interrupted = "interrupted", Interrupted, false, "Interrupted.";
    // CoreError.
    NotSetUp = "not_set_up", Unavailable, false, "SpaceTerm Core is not set up on this machine. Set it up with: spaceterm setup.";
    ServiceUnavailable = "service_unavailable", Unavailable, true, "The SpaceTerm Core service is unavailable.";
    ServiceBusy = "service_busy", Unavailable, true, "Another SpaceTerm Core operation is running.";
    NotInstalled = "not_installed", Unavailable, false, "The SpaceTerm Core service is not installed.";
    Stopped = "stopped", Unavailable, false, "SpaceTerm Core was stopped. Start it with: spaceterm start.";
    LoginItemOff = "login_item_off", ActionRequired, false, "SpaceTerm Core is off in Login Items. Turn it on in System Settings > General > Login Items & Extensions.";
    Changed = "changed", Target, true, "The target changed. Review it and try again.";
    ActivationUnresolved = "activation_unresolved", Unconfirmed, false, "SpaceTerm couldn\u{2019}t confirm whether SpaceTerm Core restarted on the new version.";
    CandidateUnverified = "candidate_unverified", Failed, false, "SpaceTerm couldn\u{2019}t verify the SpaceTerm Core package.";
    NoCandidate = "no_candidate", Target, false, "There is no SpaceTerm Core package to install.";
    // Update sources.
    NotNewer = "not_newer", Target, false, "SpaceTerm Core on that machine already has this version or a newer one.";
    BelowFloor = "below_floor", Target, false, "SpaceTerm Core on that machine is too old to update to this version directly. Update it to the version in details.floor first.";
    VersionSkipped = "version_skipped", Target, false, "This SpaceTerm Core version failed to start on that machine, so it is skipped until a newer one appears.";
    NoUpdateFeed = "no_update_feed", Unavailable, false, "This SpaceTerm Core build takes updates only from the desktop or a local archive.";
    FeedUnreachable = "feed_unreachable", Unavailable, true, "SpaceTerm Core couldn\u{2019}t reach its update feed. Check the network.";
    ProxyUnreachable = "proxy_unreachable", Unavailable, true, "SpaceTerm Core couldn\u{2019}t reach its update feed through the proxy. Check the proxy setting.";
    SystemTooOld = "system_too_old", Unavailable, false, "The newest SpaceTerm Core needs a newer operating system on that machine.";
    DiskFull = "disk_full", Failed, true, "SpaceTerm Core couldn\u{2019}t install the update because the disk is full. Nothing changed.";
    DownloadFailed = "download_failed", Failed, true, "SpaceTerm Core couldn\u{2019}t download the update. Nothing changed.";
    AuthorizationRequired = "authorization_required", ActionRequired, false, "Administrator authorization is required.";
    LingerRequired = "linger_required", ActionRequired, false, "Paired devices need SpaceTerm Core to keep running after you log out. Authorize it with: spaceterm setup.";
    NotRunning = "not_running", Unavailable, true, "SpaceTerm Core is not running.";
    StorageUnavailable = "storage_unavailable", Unavailable, false, "Storage is unavailable.";
    IncompatibleState = "incompatible_state", Unavailable, false, "The saved state is from an incompatible SpaceTerm.";
    DataResetRequired = "data_reset_required", ActionRequired, false, "Saved data on this machine needs a reset before SpaceTerm can read it.";
    StateResetRequired = "state_reset_required", ActionRequired, false, "This version can\u{2019}t read some of this CLI\u{2019}s saved data. Review its reset with: spaceterm reset.";
    StateTooOld = "state_too_old", ActionRequired, false, "This CLI\u{2019}s saved data is too old for this version. Install the version in details.floor first, or review its reset with: spaceterm reset.";
    UnsafeStorage = "unsafe_storage", Unavailable, false, "Storage has unsafe permissions.";
    AuthenticationRefused = "authentication_refused", ActionRequired, false, "SpaceTerm couldn\u{2019}t verify the machine or account for this connection.";
    IncompatibleProtocol = "incompatible_protocol", Unavailable, false, "This CLI and SpaceTerm Core on that machine are incompatible. Update the older one.";
    CandidateChanged = "candidate_changed", Unavailable, false, "The installation changed while the command ran.";
    ProtocolError = "protocol_error", Unavailable, false, "The connection to SpaceTerm Core did not complete.";
    StartupFailed = "startup_failed", Failed, false, "The Terminal Session could not start.";
    Controlled = "controlled", ActionRequired, false, "Another client controls the Pane. Use --take-over to take it.";
    Unsupported = "unsupported", Unavailable, false, "This operation isn\u{2019}t supported.";
    DirectoryUnavailable = "directory_unavailable", Target, false, "The Terminal Session's directory is unavailable. Repair it with --directory.";
    CleanupUnresolved = "cleanup_unresolved", Unconfirmed, false, "SpaceTerm couldn\u{2019}t confirm the Terminal Sessions ended.";
    Capacity = "capacity", Unavailable, true, "SpaceTerm Core reached a capacity limit.";
    // DirectError.
    Unreachable = "unreachable", Unavailable, true, "The machine is unreachable.";
    CredentialInteractionRequired = "credential_interaction_required", ActionRequired, false, "The credential store needs interaction. Run the command in a terminal.";
    InvalidInvitation = "invalid_invitation", Target, false, "This Invitation Link is invalid or used.";
    InvitationExpired = "invitation_expired", Target, false, "This Invitation Link expired.";
    PairingRejected = "pairing_rejected", Failed, false, "The machine's owner rejected this pairing.";
    MachineIdentityChanged = "machine_identity_changed", ActionRequired, false, "The machine no longer matches this connection.";
    Revoked = "revoked", ActionRequired, false, "This device was unpaired.";
    NetworkOff = "network_off", Unavailable, false, "Network Access is off on that machine.";
    InvalidAddress = "invalid_address", Target, false, "Enter a DNS name or IP address and an optional nonzero port.";
    AddressUnavailable = "address_unavailable", Unavailable, true, "SpaceTerm Core couldn\u{2019}t listen at this address and port. Nothing changed.";
    PortInUse = "port_in_use", Unavailable, true, "Another app is using this UDP port. Nothing changed. Choose another port with: spaceterm network ADDRESS:PORT.";
    IdentityMismatch = "identity_mismatch", Target, false, "This invitation is for another edition of SpaceTerm. Pair with a SpaceTerm Core of the same edition.";
    ClaimDecided = "claim_decided", Target, false, "This pairing claim was already decided.";
    ComparisonMismatch = "comparison_mismatch", Failed, false, "The Comparison Code did not match.";
    WrongMachine = "wrong_machine", Target, false, "This invitation or connection names another machine.";
    // worktree::Error.
    MainWorktree = "main_worktree", Target, false, "This is the Main Worktree.";
    WorktreeLocked = "worktree_locked", Failed, false, "The Worktree is locked.";
    WorktreePinned = "worktree_pinned", Target, false, "A Workspace is pinned inside this Worktree.";
    InvalidLocation = "invalid_location", Failed, false, "The Worktree location is invalid.";
    GitFailed = "git_failed", Failed, false, "Git could not finish the Worktree change.";
    InvalidBranchName = "invalid_branch_name", Failed, false, "Git refuses this branch name.";
    BranchExists = "branch_exists", Target, false, "A branch with this name already exists.";
    BranchCheckedOut = "branch_checked_out", Target, false, "Another Worktree has this branch checked out.";
    GitUnavailable = "git_unavailable", Unavailable, false, "Git is unavailable on that machine.";
    WorktreeInterrupted = "worktree_interrupted", Unconfirmed, false, "SpaceTerm couldn\u{2019}t confirm the Worktree change finished.";
    // Fatal.
    Stopping = "stopping", Unavailable, true, "SpaceTerm Core is stopping.";
    // hierarchy::Outcome.
    Conflict = "conflict", Target, true, "Another change came first. Review it and try again.";
    InvalidTarget = "invalid_target", Target, false, "The target can\u{2019}t take this change.";
    LimitReached = "limit_reached", Failed, false, "SpaceTerm Core reached a limit for this change.";
}

impl Code {
    pub const fn exit(self) -> u8 {
        self.class().exit()
    }
}

impl Serialize for Code {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// Mappings

pub fn from_core_error(error: CoreError) -> Failure {
    let code = match error {
        CoreError::Direct(error) => return from_direct_error(error),
        CoreError::NotSetUp => Code::NotSetUp,
        CoreError::ServiceUnavailable => Code::ServiceUnavailable,
        CoreError::ServiceBusy => Code::ServiceBusy,
        CoreError::NotInstalled => Code::NotInstalled,
        CoreError::Stopped => Code::Stopped,
        CoreError::LoginItemOff => Code::LoginItemOff,
        CoreError::Changed => Code::Changed,
        CoreError::ActivationUnresolved => Code::ActivationUnresolved,
        CoreError::RuntimeUnavailable => Code::CandidateUnverified,
        CoreError::CandidateMissing => Code::NoCandidate,
        CoreError::AuthorizationRequired => Code::AuthorizationRequired,
        CoreError::NotRunning => Code::NotRunning,
        CoreError::StorageUnavailable => Code::StorageUnavailable,
        CoreError::IncompatibleState => Code::IncompatibleState,
        CoreError::DataResetRequired => Code::DataResetRequired,
        CoreError::UnsafeStorage => Code::UnsafeStorage,
        CoreError::AuthenticationRefused => Code::AuthenticationRefused,
        CoreError::IncompatibleProtocol(older) => {
            return Failure::with(
                Code::IncompatibleProtocol,
                Details::Incompatible(IncompatibleDetails {
                    update: match older {
                        Incompatible::ListenerTooOld => Update::Machine,
                        Incompatible::CallerTooOld => Update::Core,
                    },
                }),
            );
        }
        CoreError::CandidateChanged => Code::CandidateChanged,
        CoreError::Protocol => Code::ProtocolError,
        CoreError::StartupFailed => Code::StartupFailed,
        CoreError::Controlled => {
            return Failure::with(
                Code::Controlled,
                Details::Controlled(ControlledDetails {
                    reason: ControlledReason::Other,
                }),
            );
        }
        CoreError::SessionUnavailable => Code::SessionEnded,
        CoreError::StaleControl => {
            return Failure::with(
                Code::Controlled,
                Details::Controlled(ControlledDetails {
                    reason: ControlledReason::Stale,
                }),
            );
        }
        CoreError::Unsupported => Code::Unsupported,
        CoreError::DirectoryUnavailable => Code::DirectoryUnavailable,
        CoreError::CleanupUnresolved => Code::CleanupUnresolved,
        CoreError::Capacity => Code::Capacity,
    };
    Failure::new(code)
}

pub fn from_direct_error(error: DirectError) -> Failure {
    Failure::new(match error {
        DirectError::Transport => Code::Unreachable,
        DirectError::StorageUnavailable => Code::StorageUnavailable,
        DirectError::CredentialUnavailable => Code::CredentialUnavailable,
        DirectError::InteractionRequired => Code::CredentialInteractionRequired,
        DirectError::InvalidInvitation => Code::InvalidInvitation,
        DirectError::Expired => Code::InvitationExpired,
        DirectError::Rejected => Code::PairingRejected,
        DirectError::ChangedClaim | DirectError::PolicyChanged => Code::Changed,
        DirectError::AuthenticationRefused => Code::AuthenticationRefused,
        DirectError::PinMismatch => Code::MachineIdentityChanged,
        DirectError::Revoked => Code::Revoked,
        DirectError::IncompatibleProtocol => Code::IncompatibleProtocol,
        DirectError::Capacity => Code::Capacity,
        DirectError::Disabled => Code::NetworkOff,
        DirectError::InvalidRequest => Code::ProtocolError,
        DirectError::InvalidAddress => Code::InvalidAddress,
        DirectError::AddressUnavailable => Code::AddressUnavailable,
        DirectError::PortInUse => Code::PortInUse,
        DirectError::IdentityMismatch => Code::IdentityMismatch,
        DirectError::AlreadyDecided => Code::ClaimDecided,
        DirectError::Substituted => Code::ComparisonMismatch,
        DirectError::WrongMachine => Code::WrongMachine,
    })
}

pub fn from_worktree_error(error: worktree::Error) -> Failure {
    use worktree::path_template::WorktreePathTemplateError as T;
    let target =
        |level| Failure::with(Code::NotFound, Details::Target(TargetDetails::level(level)));
    Failure::new(match error {
        worktree::Error::Missing => return target(Level::Worktree),
        worktree::Error::Main => Code::MainWorktree,
        worktree::Error::Locked => Code::WorktreeLocked,
        worktree::Error::Pinned => Code::WorktreePinned,
        worktree::Error::Changed => Code::Changed,
        worktree::Error::Location => Code::InvalidLocation,
        worktree::Error::Branch => Code::GitFailed,
        worktree::Error::InvalidName => Code::InvalidBranchName,
        worktree::Error::BranchExists => Code::BranchExists,
        worktree::Error::BranchCheckedOut => Code::BranchCheckedOut,
        worktree::Error::BranchMissing => return target(Level::Branch),
        worktree::Error::Unavailable => Code::GitUnavailable,
        worktree::Error::Interrupted => Code::WorktreeInterrupted,
        worktree::Error::Template(template) => {
            let template = match template {
                T::Empty => TemplateError::Empty,
                T::TooLong => TemplateError::TooLong,
                T::NotAbsolute => TemplateError::NotAbsolute,
                T::ParentSegment => TemplateError::ParentSegment,
                T::UnclosedBrace => TemplateError::UnclosedBrace,
                T::UnknownPlaceholder => TemplateError::UnknownPlaceholder,
                T::MissingBranch => TemplateError::MissingBranch,
                T::InvalidCharacter => TemplateError::InvalidCharacter,
                T::NotWritable => TemplateError::NotWritable,
            };
            return Failure::with(
                Code::InvalidLocation,
                Details::Template(TemplateDetails { template }),
            );
        }
    })
}

/// A connection-ending `Fatal`. Inside a service operation, `Stopping` is an observation, and
/// the caller handles it before mapping.
pub fn from_fatal(fatal: Fatal) -> Failure {
    Failure::new(match fatal {
        Fatal::ProtocolViolation | Fatal::Internal => Code::ProtocolError,
        Fatal::Capacity => Code::Capacity,
        Fatal::Stalled => Code::Unreachable,
        Fatal::Revoked => Code::Revoked,
        Fatal::Stopping => Code::Stopping,
        Fatal::Incompatible => Code::IncompatibleProtocol,
    })
}

/// A Workspace edit's outcome. `Accepted` is success. `Pending` maps to `outcome_unconfirmed`;
/// the caller waits up to its deadline before it maps a pending outcome.
pub fn from_outcome(outcome: &hierarchy::Outcome) -> Result<(), Failure> {
    Err(Failure::new(match outcome {
        hierarchy::Outcome::Accepted { .. } => return Ok(()),
        hierarchy::Outcome::Conflict => Code::Conflict,
        hierarchy::Outcome::Missing => Code::NotFound,
        hierarchy::Outcome::Invalid => Code::InvalidTarget,
        hierarchy::Outcome::Limit => Code::LimitReached,
        hierarchy::Outcome::StaleConfirmation => Code::Changed,
        hierarchy::Outcome::WorktreeFailed(error) => return Err(from_worktree_error(*error)),
        hierarchy::Outcome::Pending => Code::OutcomeUnconfirmed,
    }))
}

/// A service lane `Error { code }`. A code this CLI does not know is a protocol error.
pub fn from_lane_code(code: &str) -> Failure {
    Failure::new(Code::parse(code).unwrap_or(Code::ProtocolError))
}

/// `below_floor`, naming the release the Machine updates to first.
pub fn below_floor(floor: Floor) -> Failure {
    Failure::with(Code::BelowFloor, Details::Floor(FloorDetails { floor }))
}

// Warnings

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WarningCode {
    RecoveredOperation,
    LauncherNotOnPath,
    UnpairPending,
    InvitationWithdrawn,
    HistoryGap,
    CoverageCoalesced,
    ClientMajorDiffers,
    PrivateCredentialFile,
    DataReset,
}

impl WarningCode {
    pub const ALL: &'static [WarningCode] = &[
        Self::RecoveredOperation,
        Self::LauncherNotOnPath,
        Self::UnpairPending,
        Self::InvitationWithdrawn,
        Self::HistoryGap,
        Self::CoverageCoalesced,
        Self::ClientMajorDiffers,
        Self::PrivateCredentialFile,
        Self::DataReset,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RecoveredOperation => "recovered_operation",
            Self::LauncherNotOnPath => "launcher_not_on_path",
            Self::UnpairPending => "unpair_pending",
            Self::InvitationWithdrawn => "invitation_withdrawn",
            Self::HistoryGap => "history_gap",
            Self::CoverageCoalesced => "coverage_coalesced",
            Self::ClientMajorDiffers => "client_major_differs",
            Self::PrivateCredentialFile => "private_credential_file",
            Self::DataReset => "data_reset",
        }
    }

    pub const fn message(self) -> &'static str {
        match self {
            Self::RecoveredOperation => {
                "SpaceTerm settled an earlier command first. See data.recovered_operations."
            }
            Self::LauncherNotOnPath => "The spaceterm launcher is not on PATH.",
            Self::PrivateCredentialFile => {
                "This pairing uses a private credential file selected when Secret Service was unavailable."
            }
            Self::UnpairPending => {
                "The machine has not confirmed the unpairing yet. Later machine commands retry it."
            }
            Self::InvitationWithdrawn => "The Invitation Link was withdrawn.",
            Self::HistoryGap => "Some rows were discarded before they could be read.",
            Self::CoverageCoalesced => "Some screen states were coalesced and are not shown.",
            Self::ClientMajorDiffers => {
                "After this restart, work commands need a CLI with SpaceTerm Core's protocol major."
            }
            Self::DataReset => {
                "SpaceTerm reset saved data of this CLI that this version couldn't read. See details.kinds."
            }
        }
    }
}

impl Serialize for WarningCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Warning {
    pub code: WarningCode,
    /// The code's fixed message, or for `data_reset` one built from fixed names of data kinds.
    pub message: Cow<'static, str>,
    pub details: WarningDetails,
}

impl Warning {
    pub fn new(code: WarningCode, details: WarningDetails) -> Self {
        Self {
            code,
            message: Cow::Borrowed(code.message()),
            details,
        }
    }

    /// `data_reset`: SpaceTerm moved the rebuildable data of `kinds` aside and started it over.
    pub fn data_reset(kinds: Vec<DataKind>) -> Self {
        Self {
            code: WarningCode::DataReset,
            message: Cow::Owned(format!(
                "SpaceTerm reset this CLI's {}, which this version couldn't read. The previous files were set aside.",
                kind_names(&kinds)
            )),
            details: WarningDetails::Reset { kinds },
        }
    }
}

/// The name a person reads for one kind of saved data. It names no path or label.
pub const fn kind_name(kind: DataKind) -> &'static str {
    match kind {
        DataKind::View => "view state",
        DataKind::WorkspaceOrder => "Workspace order",
        DataKind::UpdateHistory => "update history",
        DataKind::Settings => "Settings",
        DataKind::WorkspaceOrganization => "Workspace organization",
        DataKind::MachineId => "Machine ID",
        DataKind::Pairings => "pairings",
        DataKind::SavedConnections => "saved machines with their credential references",
        DataKind::ClientRecords => "client records",
        DataKind::CoreRecords => "SpaceTerm Core records",
    }
}

/// `kinds` as one English list, such as `view state and Workspace order`.
pub fn kind_names(kinds: &[DataKind]) -> String {
    let names: Vec<_> = kinds.iter().map(|kind| kind_name(*kind)).collect();
    match names.as_slice() {
        [] => "saved data".to_owned(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The closed `details` of a warning, with the same restrictions as an error's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum WarningDetails {
    #[default]
    None,
    /// `recovered_operation`.
    Recovered { count: u32 },
    /// `history_gap`.
    Gap { missing_rows: u64 },
    /// `client_major_differs`.
    Major { client_major: u16, core_major: u16 },
    /// `data_reset`.
    Reset { kinds: Vec<DataKind> },
}

impl Serialize for WarningDetails {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Recovered {
            count: u32,
        }
        #[derive(Serialize)]
        struct Gap {
            missing_rows: u64,
        }
        #[derive(Serialize)]
        struct Major {
            client_major: u16,
            core_major: u16,
        }
        #[derive(Serialize)]
        struct Reset<'a> {
            kinds: &'a [DataKind],
        }
        match self {
            Self::None => Empty {}.serialize(serializer),
            &Self::Recovered { count } => Recovered { count }.serialize(serializer),
            &Self::Gap { missing_rows } => Gap { missing_rows }.serialize(serializer),
            &Self::Major {
                client_major,
                core_major,
            } => Major {
                client_major,
                core_major,
            }
            .serialize(serializer),
            Self::Reset { kinds } => Reset { kinds }.serialize(serializer),
        }
    }
}

// Confirmation

/// A confirmation SpaceTerm Core issued. On a terminal, answering "y" binds to this token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Confirmation<E: Serialize> {
    /// `c1-<hex>`.
    pub token: String,
    /// RFC 3339.
    pub expires_at: String,
    pub effects: E,
}

/// `data` of a `confirmation_required` envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConfirmationData<E: Serialize> {
    pub confirmation: Confirmation<E>,
}

/// `effects` of `reset`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResetEffects {
    pub reason: ResetReason,
    /// The kinds of data the reset replaces.
    pub kinds: Vec<DataKind>,
    /// For state too old for this build: the Stepping Stone release that carries it forward
    /// instead. Null otherwise, and in a build without an Update Floor.
    pub floor: Option<Floor>,
}

/// `data` of `reset`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResetData {
    /// The kinds of data the reset replaced. Empty when nothing needed a reset.
    pub reset: Vec<DataKind>,
}

/// `effects` of `stop`, `restart` and `uninstall`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ServiceEffects {
    pub ends_sessions: u32,
    pub sessions: Vec<String>,
    /// `restart` into a Pending Version: the versions it moves between. `null` for a restart
    /// into the running version, and for `stop` and `uninstall`.
    pub pending: Option<RestartTarget>,
}

/// The Pending Version a restart moves SpaceTerm Core to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RestartTarget {
    /// The version SpaceTerm Core runs now.
    pub from: String,
    /// The Pending Version.
    pub to: String,
    /// Its work or service protocol major differs from the running version's.
    pub breaking: bool,
    /// Paired Devices that can't connect after the restart until they update. Empty unless
    /// `breaking`.
    pub stranded: Vec<DeviceRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EndedSession {
    pub pane: String,
    pub session: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OtherController {
    pub pane: String,
    pub client_name: String,
}

/// `effects` of `workspace.kill`, `tab.close`, `pane.close` and `worktree.remove`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CloseEffects {
    pub ends_sessions: u32,
    pub sessions: Vec<EndedSession>,
    pub controlled_by_other_clients: Vec<OtherController>,
    pub removes_tab: bool,
    pub removes_workspace: bool,
    pub fallback: Option<Ref>,
}

// Shared data types

/// A Workspace, Tab or Pane. `id` is 64 hex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Ref {
    pub id: String,
    pub label: String,
    pub line: String,
}

/// Worktrees have path identity, not IDs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorktreeRef {
    pub path: String,
    pub branch: Option<String>,
    pub main: bool,
    pub missing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Flag,
    Current,
    Only,
    Root,
    Created,
}

/// One resolved level of `data.target`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TargetStep {
    pub level: Level,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub source: Source,
}

/// An ambiguity candidate, in `data.candidates`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub level: Level,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub label: String,
    pub directory: Option<String>,
    pub worktree: Option<String>,
    pub panes: u32,
}

/// `data` of `ambiguous_target`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CandidatesData {
    pub candidates: Vec<Candidate>,
}

/// The actual parent in a `scope_mismatch`, in `data.actual`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Actual {
    pub level: Level,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub label: String,
}

/// `data` of `scope_mismatch`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MismatchData {
    pub actual: Actual,
}

/// A runtime version, or a staged candidate's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Version {
    pub version: String,
    pub commit: String,
    pub sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct HistoryRef {
    pub epoch: u64,
    pub oldest: u64,
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Row {
    pub row: u64,
    pub text: String,
    pub wrapped: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct CursorPosition {
    pub row: u16,
    pub column: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ScreenSize {
    pub columns: u16,
    pub rows: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Screen {
    pub rows: Vec<String>,
    pub cursor: CursorPosition,
    pub size: ScreenSize,
    pub alternate_screen: bool,
}

/// Cursors to continue reading from, `h1-…`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Next {
    pub before: String,
    pub after: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GapReason {
    Discarded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Gap {
    pub reason: GapReason,
    pub missing_rows: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Text,
    Ansi,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    Coalesced,
    Complete,
}

/// Captured terminal presentation: Scrollback rows and the screen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Rows {
    pub session: String,
    pub history: HistoryRef,
    pub rows: Vec<Row>,
    /// Omitted by `--after` when rows were capped.
    pub screen: Option<Screen>,
    pub next: Next,
    pub gap: Option<Gap>,
    pub truncated: bool,
    pub format: Format,
    pub coverage: Coverage,
}

// Per-command data

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupStepName {
    InstallService,
    Linger,
    Start,
    Network,
    AutomaticStart,
    UpdateProxy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepResult {
    Done,
    AlreadyDone,
    Skipped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SetupStep {
    pub step: SetupStepName,
    pub result: StepResult,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SetupNetwork {
    pub enabled: bool,
    pub bind: Option<String>,
    pub addresses: Vec<std::net::IpAddr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Launcher {
    Linked,
    Foreign,
    Missing,
}

/// `setup`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CoreSetup {
    pub steps: Vec<SetupStep>,
    pub runtime: Version,
    pub network: SetupNetwork,
    pub automatic_start: bool,
    pub launcher: Launcher,
    pub path_ok: bool,
    /// The HTTP proxy SpaceTerm Core checks its update feed through. `null` for a direct
    /// connection.
    pub update_proxy: Option<String>,
}

/// `pending` of `status` and `update`: the installed version that waits for a restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PendingStatus {
    pub version: String,
    /// Its work or service protocol major differs from the running version's, so its restart
    /// always asks and `stranded` devices stop working until they update.
    pub breaking: bool,
    /// RFC 3339.
    pub installed_at: String,
    /// RFC 3339, one day after `installed_at`. `null` before then.
    pub overdue_since: Option<String>,
    pub stranded: Vec<DeviceRef>,
}

/// `update_check` of `status`: the state of SpaceTerm Core's own feed checks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UpdateCheckStatus {
    pub state: UpdateCheckState,
    /// RFC 3339. `null` when no check has succeeded.
    pub last_success_at: Option<String>,
    /// RFC 3339. `null` unless `state` is `failing`.
    pub failing_since: Option<String>,
    /// `null` unless `state` is `failing`.
    pub reason: Option<UpdateCheckReason>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateCheckState {
    /// Preflight and Development builds never check a feed.
    Disabled,
    Never,
    Checking,
    Succeeded,
    Failing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateCheckReason {
    Network,
    Proxy,
    Verification,
    SystemTooOld,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Activation {
    Settled,
    Unresolved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StatusNetwork {
    pub enabled: bool,
    pub bind: Option<String>,
    pub listening: bool,
    pub addresses: Vec<std::net::IpAddr>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub fingerprint: String,
    /// RFC 3339.
    pub last_connected_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HeldOutcomes {
    pub identity: RecoveredIdentity,
    pub count: u32,
}

/// `status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CoreStatus {
    pub running: bool,
    pub stopped: bool,
    pub installed: bool,
    pub automatic_start: bool,
    pub linger: Option<bool>,
    pub runtime: Version,
    pub pending: Option<PendingStatus>,
    pub update_check: UpdateCheckStatus,
    pub activation: Activation,
    pub sessions: u32,
    pub network: StatusNetwork,
    pub devices: Vec<Device>,
    pub held_outcomes: Vec<HeldOutcomes>,
    pub lane_major: u16,
    /// SpaceTerm Core's Terminal Diagnostics counters, totals since it started.
    pub terminal_counters: Option<spaceterm_core::diagnostics::TerminalCounterSnapshot>,
}

/// `permissions`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CorePermissions {
    pub permissions: Vec<spaceterm_core::permissions::PermissionReading>,
    pub accessibility_naming: spaceterm_core::permissions::AccessibilityNaming,
    /// Whose entry System Settings lists the permissions under.
    pub holder: spaceterm_core::permissions::PermissionHolder,
}

/// `permissions.request`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PermissionRequested {
    pub permission: spaceterm_core::permissions::SystemPermission,
    #[serde(flatten)]
    pub outcome: spaceterm_core::permissions::RequestOutcome,
}

/// `permissions.settings`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PermissionSettings {
    pub permission: spaceterm_core::permissions::SystemPermission,
}

/// `data` of `unreachable` from `status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UnreachableData {
    /// RFC 3339, when known.
    pub last_connected_at: Option<String>,
}

/// `start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct CoreStart {
    pub running: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAction {
    Stop,
    Restart,
    Network,
    Uninstall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptState {
    Admitted,
    Launched,
    Running,
    Done,
    Failed,
    NotPerformed,
    Unresolved,
}

/// A service operation's receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Receipt {
    pub operation: String,
    pub action: ServiceAction,
    pub state: ReceiptState,
    pub code: Option<Code>,
    pub boot: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Observed {
    Shutdown,
    Restarted,
}

/// `stop` and `restart` after confirmation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ServiceOperation {
    pub operation: String,
    pub receipt: Receipt,
    pub observed: Option<Observed>,
}

/// Network Access before or after a change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NetworkSetting {
    pub enabled: bool,
    pub bind: Option<String>,
}

/// What a Network Access change does to the paired connection that asked for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CallerEffect {
    StaysConnected,
    Reconnects,
    Disconnected,
}

/// `effects` of `network`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NetworkEffects {
    pub from: NetworkSetting,
    pub to: NetworkSetting,
    /// Every paired connection ends, including the caller's.
    pub ends_connections: bool,
    /// Null on the machine itself.
    pub caller: Option<CallerEffect>,
    /// Connected Paired Devices whose route the change removes.
    pub disconnects: Vec<DeviceRef>,
}

/// `network`. `operation` and `receipt` are null when Network Access already matched.
/// `network` is null when this device could not read Network Access after the change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CoreNetwork {
    pub network: Option<NetworkSetting>,
    pub operation: Option<String>,
    pub receipt: Option<Receipt>,
}

/// `update`: what installing found, and the version that now waits for a restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CoreUpdate {
    /// Whether this command installed a version. False when SpaceTerm Core already had it or
    /// its feed named nothing newer.
    pub installed: bool,
    /// The version SpaceTerm Core runs.
    pub running: String,
    pub pending: Option<PendingStatus>,
    /// The Terminal Sessions a restart into `pending` ends.
    pub sessions: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeviceRef {
    pub id: String,
    pub name: String,
}

/// `effects` of `end-all-sessions`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EndAllSessionsEffects {
    pub ends_sessions: u32,
    pub sessions: Vec<EndedSession>,
}

/// `end-all-sessions` after confirmation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CoreEndAllSessions {
    pub ended: Vec<EndedSession>,
}

/// `unpair`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CoreUnpair {
    pub revoked: DeviceRef,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreKind {
    Keychain,
    SecretService,
    PrivateFile,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SavedMachine {
    /// The name `--machine` takes.
    pub name: String,
    /// `id:<64 hex>`.
    pub route: String,
    /// The Machine ID, 64 hex. Two routes to one machine share it.
    pub id: String,
    pub endpoint: String,
    pub fingerprint: String,
    pub store: StoreKind,
}

/// `machine.add`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MachineAdd {
    pub machine: SavedMachine,
}

/// `machine.rename`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MachineRename {
    pub previous_name: String,
    pub name: String,
    pub route: String,
}

/// The first NDJSON line of `machine add --json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ComparisonEvent {
    pub schema: &'static str,
    pub event: &'static str,
    pub data: ComparisonCode,
}

impl ComparisonEvent {
    pub fn new(code: String, expires_at: String) -> Self {
        Self {
            schema: SCHEMA,
            event: "comparison_code",
            data: ComparisonCode { code, expires_at },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ComparisonCode {
    pub code: String,
    /// RFC 3339.
    pub expires_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ListedMachine {
    pub name: String,
    pub route: String,
    /// The Machine ID, 64 hex. Two routes to one machine share it.
    pub id: String,
    pub endpoint: String,
    pub fingerprint: String,
    pub store: StoreKind,
    pub unpair_pending: bool,
    pub held_outcomes: u32,
}

/// `machine.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MachineList {
    pub machines: Vec<ListedMachine>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Revocation {
    Confirmed,
    Pending,
}

/// `machine.remove`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MachineRemove {
    pub removed: String,
    pub revocation: Revocation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ListMachine {
    pub name: String,
    pub os: String,
    pub version: String,
    pub sessions: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStateLabel {
    Running,
    NotStarted,
    Ended,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PaneSession {
    pub id: Option<String>,
    pub state: SessionStateLabel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlState {
    #[serde(rename = "self")]
    Own,
    Other,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PaneControl {
    pub state: ControlState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ListPane {
    #[serde(rename = "ref")]
    pub reference: Ref,
    pub directory: String,
    pub session: PaneSession,
    pub control: PaneControl,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ListTab {
    #[serde(rename = "ref")]
    pub reference: Ref,
    pub panes: Vec<ListPane>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ListWorktree {
    pub worktree: WorktreeRef,
    pub tabs: Vec<ListTab>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ListWorkspace {
    #[serde(rename = "ref")]
    pub reference: Ref,
    pub directory: Option<String>,
    pub worktrees: Vec<ListWorktree>,
    /// Tabs outside any Worktree.
    pub tabs: Vec<ListTab>,
}

/// `list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct List {
    pub revision: u64,
    pub machine: ListMachine,
    pub workspaces: Vec<ListWorkspace>,
}

/// `workspace.create` and `workspace.rename`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkspaceChange {
    pub target: Vec<TargetStep>,
    pub workspace: Ref,
    /// Present for Workspace creation, from the Root Pane SpaceTerm Core created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    pub revision: u64,
}

/// `workspace.pin` and `workspace.unpin`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkspacePin {
    pub target: Vec<TargetStep>,
    pub workspace: Ref,
    /// The Pinned Directory SpaceTerm Core accepted, or null after unpinning.
    pub pinned_directory: Option<String>,
    pub revision: u64,
}

/// `workspace.kill`, `tab.close`, `pane.close` and `worktree.remove` after confirmation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Closed {
    pub target: Vec<TargetStep>,
    pub closed: Vec<String>,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CreatedControl {
    #[serde(rename = "self")]
    Own,
    Origin,
    Unclaimed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CreatedPane {
    pub id: String,
    pub directory: String,
    pub control: CreatedControl,
}

/// `tab.create` and `pane.split`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Created {
    pub target: Vec<TargetStep>,
    pub directory: String,
    pub tab: Ref,
    pub pane: CreatedPane,
    pub source_pane: Option<String>,
    pub revision: u64,
}

/// `worktree.create`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorktreeCreate {
    pub target: Vec<TargetStep>,
    pub directory: String,
    /// Worktree Create opens one creator-controlled Tab on the machine.
    pub control: CreatedControl,
    pub worktree: WorktreeRef,
    pub tab: Option<Ref>,
}

/// A run receipt's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStateName {
    Admitted,
    Running,
    Finished,
    Unconfirmed,
    SessionEnded,
    TakenOver,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Run {
    /// The run ID, 32 hex.
    pub id: String,
    pub state: RunStateName,
    /// Why the run is `unconfirmed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<RunDoubtName>,
    pub exit_status: Option<i32>,
    pub duration_ms: Option<u64>,
    pub started_shell: bool,
    pub repaired: bool,
}

/// `pane.run`. `output` is present on success and on `command_failed`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PaneRun {
    pub target: Vec<TargetStep>,
    pub run: Run,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Rows>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Delivery {
    pub serial: u32,
    pub delivered: bool,
}

/// `pane.send`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PaneSend {
    pub target: Vec<TargetStep>,
    pub input: Delivery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Wait {
    pub matched: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row: Option<u64>,
    pub elapsed_ms: u64,
}

/// `pane.read`. `rows` is null when the Pane has no Session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PaneRead {
    pub target: Vec<TargetStep>,
    pub rows: Option<Rows>,
    pub wait: Option<Wait>,
}

/// `pane.release`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PaneRelease {
    pub target: Vec<TargetStep>,
    pub released: bool,
}

/// `open`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Opened {
    /// The absolute path the desktop opened.
    pub path: String,
    pub preview: bool,
    pub pane: String,
    pub session: String,
}

/// `diagnostics`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DesktopDiagnostics {
    /// The desktop's Terminal Diagnostics counters, totals since it started.
    pub terminal_counters: spaceterm_core::diagnostics::TerminalCounterSnapshot,
}

/// `show`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Shown {
    pub target: Vec<TargetStep>,
}

#[cfg(test)]
pub(crate) mod golden {
    //! Golden envelopes in `crates/spaceterm-cli/golden/`. Set `SPACETERM_GOLDEN_UPDATE=1` to
    //! write the current output instead of comparing.
    use serde::Serialize;
    use std::path::PathBuf;

    pub(crate) fn assert_golden(name: &str, value: &impl Serialize) {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("golden")
            .join(format!("{name}.json"));
        let mut actual = serde_json::to_value(value).expect("contract values serialize");
        actual.sort_all_objects();
        if std::env::var_os("SPACETERM_GOLDEN_UPDATE").is_some() {
            std::fs::create_dir_all(path.parent().expect("golden directory")).unwrap();
            std::fs::write(
                &path,
                format!("{}\n", serde_json::to_string_pretty(&actual).unwrap()),
            )
            .unwrap();
            return;
        }
        let expected = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing golden file {name}.json"));
        let expected: serde_json::Value =
            serde_json::from_str(&expected).expect("valid golden JSON");
        assert_eq!(actual, expected, "golden file {name}.json differs");
    }
}

#[cfg(test)]
mod tests {
    use super::golden::assert_golden;
    use super::*;

    #[test]
    fn an_interrupt_after_dispatch_keeps_the_outcome_and_yields_only_to_unconfirmed() {
        let mut done = Envelope::ok("pane.run", None, Empty {});
        done.interrupt();
        assert_eq!(done.exit(), 130);
        assert_eq!(done.status, Status::Error);
        let mut timed_out =
            Envelope::error("pane.run", None, Failure::new(Code::Timeout), Empty {});
        timed_out.interrupt();
        assert_eq!(timed_out.exit(), 130);
        let mut unconfirmed = Envelope::error(
            "restart",
            None,
            Failure::new(Code::OutcomeUnconfirmed),
            Empty {},
        );
        unconfirmed.interrupt();
        assert_eq!(unconfirmed.exit(), 7);
        assert_eq!(unconfirmed.error.unwrap().code, Code::OutcomeUnconfirmed);
    }

    #[test]
    fn text_mode_writes_the_text_then_the_error_and_every_warning_on_stderr() {
        let mut envelope = Envelope::error(
            "update",
            None,
            Failure::new(Code::ConfirmationRequired),
            Empty {},
        );
        envelope.warnings.push(Warning::new(
            WarningCode::RecoveredOperation,
            WarningDetails::Recovered { count: 1 },
        ));
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let exit = render(
            &envelope,
            "Update SpaceTerm Core?\n",
            "diagnostics\n",
            Mode::default(),
            false,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(exit, 3);
        assert_eq!(stdout, b"Update SpaceTerm Core?\n");
        let stderr = String::from_utf8(stderr).unwrap();
        assert!(stderr.starts_with(&format!(
            "{}\nwarning: {}\n",
            Failure::new(Code::ConfirmationRequired).text(),
            WarningCode::RecoveredOperation.message()
        )));
        assert!(!stderr.contains("diagnostics"));
        // JSON mode prints only the envelope, even when it failed.
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let json = Mode {
            json: true,
            ..Mode::default()
        };
        render(
            &envelope,
            "Update SpaceTerm Core?\n",
            "",
            json,
            false,
            &mut stdout,
            &mut stderr,
        );
        assert!(stderr.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(value["warnings"][0]["code"], "recovered_operation");
    }

    #[test]
    fn every_code_has_one_name_a_message_and_a_round_trip() {
        let mut names = std::collections::BTreeSet::new();
        for &code in Code::ALL {
            assert!(names.insert(code.as_str()), "{code} is duplicated");
            assert_eq!(Code::parse(code.as_str()), Some(code));
            assert!(!code.message().is_empty());
            assert!(code.message().ends_with('.'), "{code}");
            assert_eq!(code.exit(), code.class().exit());
        }
        assert_eq!(Code::parse("private-unknown"), None);
    }

    #[test]
    fn cli_native_codes_exit_with_their_class() {
        use Code::*;
        for (codes, exit) in [
            (&[InvalidArguments, UnknownCommand, TargetRequired][..], 2),
            (
                &[
                    ConfirmationRequired,
                    InteractionRequired,
                    PasteConfirmationRequired,
                ],
                3,
            ),
            (
                &[
                    NotFound,
                    AmbiguousTarget,
                    ScopeMismatch,
                    ContextUnavailable,
                    CursorMismatch,
                    HistoryReset,
                    HistoryHidden,
                    SessionChanged,
                    SessionNotStarted,
                    SessionEnded,
                    NotAtPrompt,
                    RepairNotNeeded,
                    NoFallbackWorktree,
                    NotShown,
                    RemoteSession,
                    FileUnavailable,
                ],
                4,
            ),
            (
                &[
                    LocalOnly,
                    UnsupportedSession,
                    IdentityBusy,
                    CredentialUnavailable,
                    DesktopUnavailable,
                    DesktopIncompatible,
                    PreviewUnavailable,
                    DesktopBusy,
                ],
                5,
            ),
            (&[Timeout], 6),
            (&[OutcomeUnconfirmed, InputUnconfirmed, RunUnconfirmed], 7),
            (&[CommandFailed, OutputFailed, OperationFailed], 1),
            (&[Interrupted], 130),
        ] {
            for code in codes {
                assert_eq!(code.exit(), exit, "{code}");
            }
        }
    }

    #[test]
    fn machine_errors_map_to_their_table_rows() {
        use CoreError as E;
        for (error, code, exit) in [
            (E::NotSetUp, "not_set_up", 5),
            (E::ServiceUnavailable, "service_unavailable", 5),
            (E::ServiceBusy, "service_busy", 5),
            (E::NotInstalled, "not_installed", 5),
            (E::Stopped, "stopped", 5),
            (E::Changed, "changed", 4),
            (E::ActivationUnresolved, "activation_unresolved", 7),
            (E::RuntimeUnavailable, "candidate_unverified", 1),
            (E::CandidateMissing, "no_candidate", 4),
            (E::AuthorizationRequired, "authorization_required", 3),
            (E::NotRunning, "not_running", 5),
            (E::StorageUnavailable, "storage_unavailable", 5),
            (E::IncompatibleState, "incompatible_state", 5),
            (E::DataResetRequired, "data_reset_required", 3),
            (E::UnsafeStorage, "unsafe_storage", 5),
            (E::AuthenticationRefused, "authentication_refused", 3),
            (
                E::IncompatibleProtocol(
                    spaceterm_core::protocol::version::Incompatible::ListenerTooOld,
                ),
                "incompatible_protocol",
                5,
            ),
            (E::CandidateChanged, "candidate_changed", 5),
            (E::Protocol, "protocol_error", 5),
            (E::StartupFailed, "startup_failed", 1),
            (E::Controlled, "controlled", 3),
            (E::SessionUnavailable, "session_ended", 4),
            (E::StaleControl, "controlled", 3),
            (E::Unsupported, "unsupported", 5),
            (E::DirectoryUnavailable, "directory_unavailable", 4),
            (E::CleanupUnresolved, "cleanup_unresolved", 7),
            (E::Capacity, "capacity", 5),
            (E::Direct(DirectError::Transport), "unreachable", 5),
        ] {
            let failure = from_core_error(error);
            assert_eq!((failure.code.as_str(), failure.code.exit()), (code, exit));
        }
        assert_eq!(
            from_core_error(E::StaleControl).details,
            Details::Controlled(ControlledDetails {
                reason: ControlledReason::Stale
            })
        );
    }

    #[test]
    fn an_incompatible_protocol_names_the_side_to_update() {
        for (older, update) in [
            (Incompatible::ListenerTooOld, "machine"),
            (Incompatible::CallerTooOld, "core"),
        ] {
            let failure = from_core_error(CoreError::IncompatibleProtocol(older));
            let envelope =
                serde_json::to_value(Envelope::error("list", None, failure, ())).unwrap();
            assert_eq!(envelope["error"]["code"], "incompatible_protocol");
            assert_eq!(
                envelope["error"]["details"],
                serde_json::json!({"update": update})
            );
        }
    }

    #[test]
    fn direct_errors_map_to_their_table_rows() {
        use DirectError as E;
        for (error, code, exit) in [
            (E::Transport, "unreachable", 5),
            (E::StorageUnavailable, "storage_unavailable", 5),
            (E::CredentialUnavailable, "credential_unavailable", 5),
            (E::InteractionRequired, "credential_interaction_required", 3),
            (E::InvalidInvitation, "invalid_invitation", 4),
            (E::Expired, "invitation_expired", 4),
            (E::Rejected, "pairing_rejected", 1),
            (E::ChangedClaim, "changed", 4),
            (E::PolicyChanged, "changed", 4),
            (E::AuthenticationRefused, "authentication_refused", 3),
            (E::PinMismatch, "machine_identity_changed", 3),
            (E::Revoked, "revoked", 3),
            (E::IncompatibleProtocol, "incompatible_protocol", 5),
            (E::Capacity, "capacity", 5),
            (E::Disabled, "network_off", 5),
            (E::InvalidRequest, "protocol_error", 5),
            (E::InvalidAddress, "invalid_address", 4),
            (E::AlreadyDecided, "claim_decided", 4),
            (E::Substituted, "comparison_mismatch", 1),
            (E::WrongMachine, "wrong_machine", 4),
            (E::PortInUse, "port_in_use", 5),
            (E::IdentityMismatch, "identity_mismatch", 4),
        ] {
            let failure = from_direct_error(error);
            assert_eq!((failure.code.as_str(), failure.code.exit()), (code, exit));
        }
    }

    #[test]
    fn worktree_errors_map_to_their_table_rows() {
        use worktree::Error as E;
        use worktree::path_template::WorktreePathTemplateError as T;
        for (error, code, exit) in [
            (E::Missing, "not_found", 4),
            (E::Main, "main_worktree", 4),
            (E::Locked, "worktree_locked", 1),
            (E::Pinned, "worktree_pinned", 4),
            (E::Changed, "changed", 4),
            (E::Location, "invalid_location", 1),
            (E::Branch, "git_failed", 1),
            (E::InvalidName, "invalid_branch_name", 1),
            (E::BranchExists, "branch_exists", 4),
            (E::BranchCheckedOut, "branch_checked_out", 4),
            (E::BranchMissing, "not_found", 4),
            (E::Unavailable, "git_unavailable", 5),
            (E::Interrupted, "worktree_interrupted", 7),
            (E::Template(T::NotAbsolute), "invalid_location", 1),
        ] {
            let failure = from_worktree_error(error);
            assert_eq!((failure.code.as_str(), failure.code.exit()), (code, exit));
        }
        assert_eq!(
            serde_json::to_value(from_worktree_error(E::BranchMissing).details).unwrap(),
            serde_json::json!({ "level": "branch" })
        );
        assert_eq!(
            serde_json::to_value(from_worktree_error(E::Template(T::MissingBranch)).details)
                .unwrap(),
            serde_json::json!({ "template": "missing_branch" })
        );
    }

    #[test]
    fn fatals_outcomes_and_lane_codes_map_to_their_table_rows() {
        for (fatal, code) in [
            (Fatal::ProtocolViolation, Code::ProtocolError),
            (Fatal::Capacity, Code::Capacity),
            (Fatal::Stalled, Code::Unreachable),
            (Fatal::Revoked, Code::Revoked),
            (Fatal::Stopping, Code::Stopping),
            (Fatal::Incompatible, Code::IncompatibleProtocol),
            (Fatal::Internal, Code::ProtocolError),
        ] {
            assert_eq!(from_fatal(fatal).code, code);
        }
        use hierarchy::Outcome as O;
        assert_eq!(from_outcome(&O::Accepted { target: [1; 32] }), Ok(()));
        for (outcome, code) in [
            (O::Conflict, Code::Conflict),
            (O::Missing, Code::NotFound),
            (O::Invalid, Code::InvalidTarget),
            (O::Limit, Code::LimitReached),
            (O::StaleConfirmation, Code::Changed),
            (O::Pending, Code::OutcomeUnconfirmed),
            (O::WorktreeFailed(worktree::Error::Main), Code::MainWorktree),
        ] {
            assert_eq!(from_outcome(&outcome).unwrap_err().code, code);
        }
        assert_eq!(Code::Conflict.exit(), 4);
        assert_eq!(Code::LimitReached.exit(), 1);
        assert_eq!(from_lane_code("service_busy").code, Code::ServiceBusy);
        assert_eq!(from_lane_code("not-a-code").code, Code::ProtocolError);
    }

    #[test]
    fn exit_precedence_puts_unconfirmed_first_then_interrupted_then_the_table_order() {
        use Class::*;
        assert_eq!(decisive([]), Ok);
        assert_eq!(decisive([Interrupted, Unconfirmed]), Unconfirmed);
        assert_eq!(decisive([Unavailable, Interrupted]), Interrupted);
        assert_eq!(
            decisive([Failed, Deadline, Target, ActionRequired, Unavailable]),
            Unavailable
        );
        assert_eq!(
            decisive([Failed, Deadline, Target, ActionRequired]),
            ActionRequired
        );
        assert_eq!(decisive([Failed, Deadline, Target]), Target);
        assert_eq!(decisive([Failed, Deadline]), Deadline);
        assert_eq!(decisive([Ok, Failed]), Failed);
        assert_eq!(decisive([Usage, Failed]), Usage);
        assert_eq!(EXIT_BROKEN_PIPE, 141);
    }

    #[test]
    fn targeting_messages_name_the_level_and_nothing_else() {
        let ambiguous = Failure::with(
            Code::AmbiguousTarget,
            Details::Target(TargetDetails::level(Level::Tab)),
        );
        assert_eq!(
            ambiguous.text(),
            "error: More than one Tab matches. (ambiguous_target)"
        );
        let mismatch = Failure::with(
            Code::ScopeMismatch,
            Details::Target(TargetDetails {
                parent: Some(Level::Tab),
                ..TargetDetails::level(Level::Pane)
            }),
        );
        assert_eq!(
            mismatch.text(),
            "error: The Pane is not in the given Tab. (scope_mismatch)"
        );
        assert_eq!(
            ErrorBody::from(mismatch).message,
            "The Pane is not in the given Tab."
        );
    }

    #[test]
    fn an_announced_reset_reaches_the_next_result_once_in_both_forms() {
        let envelope = Envelope::ok("list", None, Empty {});
        let write = |json| {
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            let mode = Mode {
                json,
                ..Mode::default()
            };
            render(&envelope, "", "", mode, false, &mut stdout, &mut stderr);
            (
                String::from_utf8(stdout).unwrap(),
                String::from_utf8(stderr).unwrap(),
            )
        };
        announce(Warning::data_reset(vec![
            DataKind::View,
            DataKind::WorkspaceOrder,
        ]));
        let (_, stderr) = write(false);
        assert_eq!(
            stderr,
            "warning: SpaceTerm reset this CLI's view state and Workspace order, which this version couldn't read. The previous files were set aside.\n"
        );
        assert_eq!(write(false).1, "", "the notice prints once");
        announce(Warning::data_reset(vec![DataKind::View]));
        let (stdout, _) = write(true);
        let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(
            value["warnings"],
            serde_json::json!([{
                "code": "data_reset",
                "message": "SpaceTerm reset this CLI's view state, which this version couldn't read. The previous files were set aside.",
                "details": {"kinds": ["view"]},
            }])
        );
    }

    #[test]
    fn state_too_old_names_the_version_to_install_in_text() {
        let envelope = Envelope::error(
            "list",
            None,
            Failure::with(
                Code::StateTooOld,
                Details::StateTooOld(StateTooOldDetails {
                    floor: Floor::parse("1.1.0").unwrap(),
                    kinds: vec![DataKind::SavedConnections],
                }),
            ),
            Empty {},
        );
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        render(
            &envelope,
            "",
            "",
            Mode::default(),
            false,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            format!(
                "error: {} (state_too_old)\ninstall first: SpaceTerm Core 1.1.0\n",
                Code::StateTooOld.message()
            )
        );
    }

    #[test]
    fn warnings_have_fixed_messages() {
        for &code in WarningCode::ALL {
            assert!(code.message().ends_with('.'), "{}", code.as_str());
        }
    }

    #[test]
    fn golden_unsupported_envelope() {
        let envelope = Envelope::error("tab.create", None, Code::Unsupported.into(), Empty {});
        assert_eq!(envelope.exit(), 5);
        assert_golden("unsupported", &envelope);
    }

    #[test]
    fn golden_ambiguous_target_envelope() {
        let mut envelope = Envelope::error(
            "pane.read",
            Some(MachineRef {
                name: "local".into(),
                route: "local".into(),
                id: "5b".repeat(32),
            }),
            Failure::with(
                Code::AmbiguousTarget,
                Details::Target(TargetDetails {
                    level: Level::Tab,
                    parent: None,
                    count: Some(2),
                    ids: vec!["0b".repeat(32), "0c".repeat(32)],
                }),
            ),
            CandidatesData {
                candidates: vec![Candidate {
                    level: Level::Tab,
                    id: Some("0b".repeat(32)),
                    path: None,
                    label: "cargo test".into(),
                    directory: Some("/home/build/api".into()),
                    worktree: None,
                    panes: 1,
                }],
            },
        );
        envelope.warnings.push(Warning::new(
            WarningCode::RecoveredOperation,
            WarningDetails::Recovered { count: 1 },
        ));
        envelope.data.recovered_operations.push(RecoveredOperation {
            operation: "7".into(),
            identity: RecoveredIdentity::Base,
            kind: RecoveredKind::Mutation,
            outcome: RecoveredOutcome {
                state: RecoveredState::Applied,
                code: None,
                target: Some("0d".repeat(32)),
            },
        });
        assert_eq!(envelope.exit(), 4);
        assert_golden("ambiguous_target", &envelope);
    }

    #[test]
    fn golden_confirmation_envelope() {
        let envelope = Envelope::confirmation(
            "stop",
            None,
            Confirmation {
                token: "c1-00".into(),
                expires_at: "2026-10-30T12:00:00Z".into(),
                effects: ServiceEffects {
                    ends_sessions: 1,
                    sessions: vec!["0e".repeat(32)],
                    pending: None,
                },
            },
        );
        assert_eq!(envelope.exit(), 3);
        assert_golden("confirmation_required", &envelope);
    }
}
