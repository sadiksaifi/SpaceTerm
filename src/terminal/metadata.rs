use std::sync::Arc;
use std::time::{Duration, Instant};

use super::title::{TitleActivity, TitleGlyph};
use crate::domain::RemoteDirectory;
use crate::local_path::LocalPathSemantics;

const MAX_TITLE_CHARS: usize = 256;
const MAX_COMMAND_CHARS: usize = 4096;
/// The most markers kept for a pending run between two reads. More mean the run lost track.
const MAX_RUN_MARKERS: usize = 64;
pub(crate) const PROGRESS_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(30);

pub use crate::domain::workspace_collection::CurrentDirectory;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectoryProvenance {
    Initial,
    Osc7,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryMetadata {
    pub path: Arc<str>,
    pub provenance: DirectoryProvenance,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MetadataFreshness {
    #[default]
    Live,
    Stale,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TitleProvenance {
    #[default]
    Fallback,
    WorkingDirectory,
    TerminalControl,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleMetadata {
    pub value: Arc<str>,
    pub provenance: TitleProvenance,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PromptZone {
    #[default]
    Unknown,
    Prompt,
    CommandInput,
    CommandOutput,
}

/// The owning shell's prompt, as `pane run` admission reads it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct OwnedPrompt {
    /// The zone the owning shell's own markers last established, or `None` before its first.
    /// A `B` without the owner still ends an owned prompt, because no integration marks it.
    pub(crate) zone: Option<PromptZone>,
    /// Input bytes written to the PTY since the owning shell drew a fresh prompt.
    pub(crate) input_bytes: u64,
}

/// One marker a pending run follows, in the order the shell wrote it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RunMarker {
    pub(crate) marker: spaceterm_server::pane::run::Marker,
    /// The Terminal Session's finished-command count once the marker applied.
    pub(crate) finished_commands: u64,
    pub(crate) at: Instant,
}

/// The markers a pending run follows until its owning shell finishes the command or prompts.
#[derive(Default)]
struct RunFollower {
    markers: Vec<RunMarker>,
    overflowed: bool,
    finished: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandState {
    Running,
    Finished {
        exit_status: Option<i32>,
        duration: Duration,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandMetadata {
    pub line: Arc<str>,
    pub state: CommandState,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProgressMetadata {
    #[default]
    None,
    Normal(u8),
    Error(u8),
    Indeterminate,
    Paused(u8),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
/// The local account, machine, and home facts a Local Terminal Session presents as its origin.
///
/// Composition captures these once from the host. They are presentation identity only and carry
/// no filesystem authority; `home` is a spelling used to abbreviate displayed directories.
pub struct LocalMachine {
    user: Option<Arc<str>>,
    hostname: Option<Arc<str>>,
    home: Option<Arc<str>>,
}

impl LocalMachine {
    pub fn new(user: Option<&str>, hostname: Option<&str>, home: Option<&str>) -> Self {
        Self {
            user: Self::retain(user),
            hostname: Self::retain(hostname),
            home: Self::retain(home),
        }
    }

    fn retain(value: Option<&str>) -> Option<Arc<str>> {
        retain_machine_value(value)
    }

    pub fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }

    pub fn hostname(&self) -> Option<&str> {
        self.hostname.as_deref()
    }

    pub fn home(&self) -> Option<&str> {
        self.home.as_deref()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
/// The remote account facts a Remote Terminal Session presents, captured at account discovery.
///
/// These are remote strings kept for presentation. They are never local filesystem authority, and
/// `home` is used only to abbreviate a displayed remote directory.
pub struct RemoteMachine {
    user: Option<Arc<str>>,
    home: Option<Arc<str>>,
}

impl RemoteMachine {
    pub fn new(user: Option<&str>, home: Option<&str>) -> Self {
        Self {
            user: retain_machine_value(user),
            home: retain_machine_value(home),
        }
    }

    pub fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }

    pub fn home(&self) -> Option<&str> {
        self.home.as_deref()
    }
}

/// Keeps only a machine value that can be presented as written.
fn retain_machine_value(value: Option<&str>) -> Option<Arc<str>> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
        .map(Arc::from)
}

/// The Local or Remote account and machine presented by one Pane's Terminal Session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalOrigin<'a> {
    Local {
        user: Option<&'a str>,
        host: Option<&'a str>,
    },
    Remote {
        user: Option<&'a str>,
        host: &'a str,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Remote display facts and starting directory for one Terminal Session.
///
/// The directory retains its exact remote spelling for presentation and startup. It is not local
/// filesystem authority and must never be converted to `PathBuf` or locally validated.
pub struct RemoteTerminalMetadataContext {
    server_display: String,
    initial_directory: RemoteDirectory,
    machine: RemoteMachine,
}

impl RemoteTerminalMetadataContext {
    pub const fn new(server_display: String, initial_directory: RemoteDirectory) -> Self {
        Self {
            server_display,
            initial_directory,
            machine: RemoteMachine {
                user: None,
                home: None,
            },
        }
    }

    /// Attaches the discovered account, so the Pane can name its user and abbreviate its home.
    pub fn with_machine(mut self, machine: RemoteMachine) -> Self {
        self.machine = machine;
        self
    }

    pub fn server_display(&self) -> &str {
        &self.server_display
    }

    pub const fn initial_directory(&self) -> &RemoteDirectory {
        &self.initial_directory
    }
}

/// A per-session secret that SpaceTerm's shell integration attaches to its own prompt markers.
///
/// A nested shell, such as one inside `ssh` or `docker`, cannot know it, so its markers cannot end
/// the owning shell's command and move the Repository Source Directory. Debug never shows it.
#[derive(Clone, Eq, PartialEq)]
pub struct PromptOwner(Arc<str>);

impl PromptOwner {
    /// The variable that carries the secret to the shell integration.
    pub(crate) const ENVIRONMENT: &str = "SPACETERM_PROMPT_OWNER";
    /// The OSC 133 option that carries it back.
    const OPTION: &str = "spaceterm";

    /// 128 random bits, or none when the system has no randomness to give.
    pub(crate) fn generate() -> Option<Self> {
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes).ok()?;
        Some(Self(Arc::from(
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        )))
    }

    pub(crate) fn value(&self) -> &str {
        &self.0
    }

    #[cfg(test)]
    pub(crate) fn fixture(value: &str) -> Self {
        Self(Arc::from(value))
    }
}

impl std::fmt::Debug for PromptOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PromptOwner(..)")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Local metadata may authorize local path actions. Remote directory strings carry no local
/// filesystem authority.
pub enum TerminalMetadataContext {
    Local {
        paths: LocalPathSemantics,
        initial_directory: Arc<str>,
        machine: LocalMachine,
        prompt_owner: Option<PromptOwner>,
    },
    Remote(RemoteTerminalMetadataContext),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Whether Terminal metadata may authorize features backed by the local filesystem.
///
/// This capability is derived from Local versus Remote metadata and is not user-configurable.
pub enum TerminalLocalFileCapabilities {
    Enabled,
    Disabled,
}

impl TerminalLocalFileCapabilities {
    pub const fn are_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

impl TerminalMetadataContext {
    pub fn local(
        paths: LocalPathSemantics,
        initial_directory: &str,
        machine: LocalMachine,
    ) -> Self {
        Self::Local {
            paths,
            initial_directory: Arc::from(initial_directory),
            machine,
            prompt_owner: None,
        }
    }

    /// Lets only prompt markers that carry `owner` move the Repository Source Directory, once the
    /// shell has sent one.
    pub(crate) fn with_prompt_owner(mut self, owner: PromptOwner) -> Self {
        if let Self::Local { prompt_owner, .. } = &mut self {
            *prompt_owner = Some(owner);
        }
        self
    }

    fn prompt_owner(&self) -> Option<&PromptOwner> {
        match self {
            Self::Local { prompt_owner, .. } => prompt_owner.as_ref(),
            Self::Remote(_) => None,
        }
    }

    pub const fn local_paths(&self) -> Option<LocalPathSemantics> {
        match self {
            Self::Local { paths, .. } => Some(*paths),
            Self::Remote(_) => None,
        }
    }

    pub fn current_directory(&self, directory: &str) -> Option<CurrentDirectory> {
        match self {
            Self::Local { .. } => self.local_directory(directory).map(CurrentDirectory::Local),
            Self::Remote(_) => RemoteDirectory::new(directory.to_owned())
                .ok()
                .map(CurrentDirectory::Remote),
        }
    }

    pub fn local_directory(&self, directory: &str) -> Option<std::path::PathBuf> {
        let paths = self.local_paths()?;
        paths
            .is_absolute(std::path::Path::new(directory))
            .then(|| directory.into())
    }

    fn directory_basename(&self, directory: &str) -> Option<String> {
        match self.local_paths() {
            Some(paths) => paths.directory_basename(directory),
            // Remote directory strings follow the remote POSIX shell protocol.
            None => directory
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
        }
    }

    pub const fn is_local(&self) -> bool {
        matches!(self, Self::Local { .. })
    }

    pub const fn local_file_capabilities(&self) -> TerminalLocalFileCapabilities {
        match self {
            Self::Local { .. } => TerminalLocalFileCapabilities::Enabled,
            Self::Remote(_) => TerminalLocalFileCapabilities::Disabled,
        }
    }

    #[cfg(test)]
    pub const fn remote(&self) -> Option<&RemoteTerminalMetadataContext> {
        match self {
            Self::Local { .. } => None,
            Self::Remote(context) => Some(context),
        }
    }

    pub fn initial_directory(&self) -> &str {
        match self {
            Self::Local {
                initial_directory, ..
            } => initial_directory,
            Self::Remote(context) => context.initial_directory().as_str(),
        }
    }

    pub fn local_hostname(&self) -> Option<&str> {
        match self {
            Self::Local { machine, .. } => machine.hostname(),
            Self::Remote(_) => None,
        }
    }

    /// This context's own home spelling, used to abbreviate the directories it displays.
    ///
    /// A Local context abbreviates against the local home and a Remote one against the remote
    /// home, so neither side's home can ever shorten the other side's path.
    pub fn home(&self) -> Option<&str> {
        match self {
            Self::Local { machine, .. } => machine.home(),
            Self::Remote(context) => context.machine.home(),
        }
    }

    /// The account and machine this Terminal Session runs on, for presentation only.
    pub fn origin(&self) -> TerminalOrigin<'_> {
        match self {
            Self::Local { machine, .. } => TerminalOrigin::Local {
                user: machine.user(),
                host: machine.hostname(),
            },
            Self::Remote(context) => TerminalOrigin::Remote {
                user: context.machine.user(),
                host: context.server_display(),
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalMetadataSnapshot {
    pub revision: u64,
    pub context: TerminalMetadataContext,
    pub freshness: MetadataFreshness,
    /// The executable basename selected by this Terminal Session's host.
    pub shell_name: Arc<str>,
    pub title: TitleMetadata,
    pub directory: DirectoryMetadata,
    /// The Repository Source Directory: the reported directory as of the last prompt. Reports
    /// that arrive while a command runs apply only when it finishes.
    pub repository_directory: Arc<str>,
    /// Directory confirmed by a prompt carrying this Terminal Session's Prompt Owner.
    pub confirmed_directory: Option<Arc<str>>,
    /// Completed commands since the session started.
    pub finished_commands: u64,
    pub prompt_zone: PromptZone,
    pub command: Option<CommandMetadata>,
    /// Whether changing title frames provide recent animation evidence.
    pub title_activity: bool,
    pub title_glyph: TitleGlyph,
    pub progress: ProgressMetadata,
}

impl TerminalMetadataSnapshot {
    /// Whether any fact chrome presents differs: the directory, title, command, progress, or
    /// whether those facts are still live.
    pub fn presentation_differs(&self, other: &Self) -> bool {
        self.freshness != other.freshness
            || self.shell_name != other.shell_name
            || self.directory != other.directory
            || self.title != other.title
            || self.command != other.command
            || self.title_activity != other.title_activity
            || self.title_glyph != other.title_glyph
            || self.progress != other.progress
    }

    /// The Current Directory these facts establish, or none once they have gone stale.
    pub fn current_directory(&self) -> Option<CurrentDirectory> {
        (self.freshness == MetadataFreshness::Live)
            .then(|| self.context.current_directory(&self.directory.path))
            .flatten()
    }
}

pub(crate) struct MetadataTracker {
    snapshot: Arc<TerminalMetadataSnapshot>,
    epoch: Instant,
    fallback_title: Arc<str>,
    command_started: Option<Instant>,
    deferred_repository_directory: Option<Arc<str>>,
    /// Whether a prompt marker carrying the session's Prompt Owner has arrived. From then on
    /// only such markers move the Repository Source Directory.
    prompt_owner_seen: bool,
    /// The latest directory report since the owning shell's last prompt marker. The owning shell
    /// reports its directory just before each prompt marker, so that marker commits it, and a
    /// nested session's reports are replaced before then.
    owner_reported_directory: Option<Arc<str>>,
    progress_expiry: Option<Instant>,
    title_animation: TitleActivity,
    owned_prompt: OwnedPrompt,
    /// Whether the owning shell finished its command and has not drawn its prompt yet.
    finishing: bool,
    /// Input written while finishing. Only the next prompt's line editor can read it, so the
    /// fresh prompt carries it.
    typeahead: u64,
    run: Option<RunFollower>,
}

impl MetadataTracker {
    pub(crate) fn new(
        paths: LocalPathSemantics,
        initial_directory: &str,
        fallback_title: &str,
        machine: LocalMachine,
        epoch: Instant,
    ) -> Self {
        Self::new_with_context(
            TerminalMetadataContext::local(paths, initial_directory, machine),
            fallback_title,
            epoch,
        )
    }

    pub(crate) fn new_with_context(
        context: TerminalMetadataContext,
        fallback_title: &str,
        epoch: Instant,
    ) -> Self {
        let initial_directory = context.initial_directory().to_owned();
        Self {
            snapshot: Arc::new(TerminalMetadataSnapshot {
                revision: 0,
                context,
                freshness: MetadataFreshness::Live,
                shell_name: Arc::from(sanitize_title(fallback_title)),
                title: TitleMetadata {
                    value: Arc::from(sanitize_title(fallback_title)),
                    provenance: TitleProvenance::Fallback,
                },
                repository_directory: Arc::from(initial_directory.as_str()),
                confirmed_directory: None,
                directory: DirectoryMetadata {
                    path: Arc::from(initial_directory),
                    provenance: DirectoryProvenance::Initial,
                },
                finished_commands: 0,
                prompt_zone: PromptZone::Unknown,
                command: None,
                title_activity: false,
                title_glyph: TitleGlyph::Reported,
                progress: ProgressMetadata::None,
            }),
            epoch,
            fallback_title: Arc::from(sanitize_title(fallback_title)),
            command_started: None,
            deferred_repository_directory: None,
            prompt_owner_seen: false,
            owner_reported_directory: None,
            progress_expiry: None,
            title_animation: TitleActivity::default(),
            owned_prompt: OwnedPrompt::default(),
            finishing: false,
            typeahead: 0,
            run: None,
        }
    }

    pub(crate) fn owned_prompt(&self) -> OwnedPrompt {
        self.owned_prompt
    }

    /// Counts input written to the PTY, so a run never pastes into a line someone typed.
    pub(crate) fn note_input(&mut self, bytes: usize) {
        let counter = if self.finishing {
            &mut self.typeahead
        } else {
            &mut self.owned_prompt.input_bytes
        };
        *counter = counter.saturating_add(bytes as u64);
    }

    /// Records the markers a run admitted now follows, replacing those of an earlier run.
    pub(crate) fn follow_run(&mut self) {
        self.run = Some(RunFollower::default());
    }

    /// The markers recorded since the last call. Recording stops after the owning shell's
    /// finish or fresh prompt. Markers beyond the bound end as one marker without the owner.
    pub(crate) fn take_run_markers(&mut self) -> Vec<RunMarker> {
        let Some(run) = &mut self.run else {
            return Vec::new();
        };
        let mut markers = std::mem::take(&mut run.markers);
        if std::mem::take(&mut run.overflowed) {
            markers.push(RunMarker {
                marker: spaceterm_server::pane::run::Marker::Unowned,
                finished_commands: self.snapshot.finished_commands,
                at: Instant::now(),
            });
        }
        if run.finished {
            self.run = None;
        }
        markers
    }

    fn record_run_marker(&mut self, marker: spaceterm_server::pane::run::Marker, now: Instant) {
        use spaceterm_server::pane::run::Marker;
        let finished_commands = self.snapshot.finished_commands;
        let Some(run) = self.run.as_mut().filter(|run| !run.finished) else {
            return;
        };
        run.finished = matches!(marker, Marker::PromptStart | Marker::CommandFinished { .. });
        if run.markers.len() >= MAX_RUN_MARKERS {
            run.overflowed = true;
            return;
        }
        run.markers.push(RunMarker {
            marker,
            finished_commands,
            at: now,
        });
    }

    pub(crate) fn snapshot(&self) -> Arc<TerminalMetadataSnapshot> {
        Arc::clone(&self.snapshot)
    }

    pub(crate) fn set_reported_title(&mut self, title: &str, now: Instant) -> bool {
        let title = sanitize_title(title);
        let (value, provenance) = if title.is_empty() {
            (
                self.snapshot
                    .context
                    .directory_basename(&self.snapshot.directory.path)
                    .unwrap_or_else(|| self.fallback_title.to_string()),
                TitleProvenance::WorkingDirectory,
            )
        } else {
            (title, TitleProvenance::TerminalControl)
        };
        let previous = if self.snapshot.title.provenance == TitleProvenance::TerminalControl {
            &self.snapshot.title.value
        } else {
            ""
        };
        self.title_animation.observe(previous, &value, now);
        let active = self.title_animation.active();
        let glyph = self.title_animation.glyph();
        self.update(|snapshot| {
            snapshot.title_activity = active;
            snapshot.title_glyph = glyph;
            snapshot.title = TitleMetadata {
                value: Arc::from(value),
                provenance,
            };
        })
    }

    pub(crate) fn set_reported_directory(&mut self, value: &str) -> bool {
        let Some(directory) = parse_osc7_directory(value, &self.snapshot.context) else {
            return false;
        };
        if self.snapshot.context.prompt_owner().is_some() {
            // Integrations send OSC 7 before the first owned prompt. Keep the candidate,
            // but only a matching Prompt Owner can confirm it for restoration.
            self.owner_reported_directory = Some(Arc::clone(&directory.path));
        }
        let command_running = if self.prompt_owner_seen {
            true
        } else {
            let running = self
                .snapshot
                .command
                .as_ref()
                .is_some_and(|command| command.state == CommandState::Running);
            if running {
                self.deferred_repository_directory = Some(Arc::clone(&directory.path));
            }
            running
        };
        self.update(|snapshot| {
            if !command_running {
                snapshot.repository_directory = Arc::clone(&directory.path);
            }
            snapshot.directory = directory;
            if snapshot.title.provenance != TitleProvenance::TerminalControl
                && let Some(title) = snapshot
                    .context
                    .directory_basename(&snapshot.directory.path)
            {
                snapshot.title = TitleMetadata {
                    value: Arc::from(title),
                    provenance: TitleProvenance::WorkingDirectory,
                };
            }
        })
    }

    pub(crate) fn mark_stale(&mut self) -> bool {
        self.command_started = None;
        self.retire_command_reports(|snapshot| {
            snapshot.freshness = MetadataFreshness::Stale;
        })
    }

    fn retire_command_reports(
        &mut self,
        change: impl FnOnce(&mut TerminalMetadataSnapshot),
    ) -> bool {
        self.progress_expiry = None;
        self.title_animation = TitleActivity::default();
        let value = self
            .snapshot
            .context
            .directory_basename(&self.snapshot.directory.path)
            .map_or_else(|| Arc::clone(&self.fallback_title), Arc::from);
        self.update(|snapshot| {
            snapshot.title = TitleMetadata {
                value,
                provenance: TitleProvenance::WorkingDirectory,
            };
            snapshot.title_activity = false;
            snapshot.title_glyph = TitleGlyph::Reported;
            snapshot.progress = ProgressMetadata::None;
            change(snapshot);
        })
    }

    fn finish_command(&mut self, exit_status: Option<i32>, now: Instant) -> bool {
        if self
            .snapshot
            .command
            .as_ref()
            .is_some_and(|command| matches!(command.state, CommandState::Finished { .. }))
        {
            return false;
        }
        let started = self.command_started.take().unwrap_or(self.epoch);
        let deferred_directory = self.deferred_repository_directory.take();
        self.retire_command_reports(|snapshot| {
            if let Some(directory) = deferred_directory {
                snapshot.repository_directory = directory;
            }
            snapshot.finished_commands = snapshot.finished_commands.saturating_add(1);
            snapshot.command = Some(CommandMetadata {
                line: snapshot
                    .command
                    .as_ref()
                    .map_or_else(|| Arc::from(""), |command| Arc::clone(&command.line)),
                state: CommandState::Finished {
                    exit_status,
                    duration: now.saturating_duration_since(started),
                },
            });
        })
    }

    /// Whether the marker carries the session's Prompt Owner, and if so the directory it
    /// commits. The owning shell reports its directory before each fresh prompt, never before a
    /// prompt it only redraws.
    fn observe_prompt_owner(&mut self, action: &str, fields: &[&str]) -> Option<Option<Arc<str>>> {
        let owner = self.snapshot.context.prompt_owner()?;
        if option(fields, PromptOwner::OPTION) != Some(owner.value()) {
            return None;
        }
        self.prompt_owner_seen = true;
        self.deferred_repository_directory = None;
        Some(match action {
            "A" | "P" => self.owner_reported_directory.take(),
            _ => None,
        })
    }

    /// Follows the owning shell's prompt for run admission and the pending run.
    fn observe_owned_prompt(
        &mut self,
        action: &str,
        fields: &[&str],
        owned: Option<bool>,
        now: Instant,
    ) {
        use spaceterm_server::pane::run::Marker;
        let marker = match (action, owned) {
            ("A" | "P", Some(fresh)) => {
                self.owned_prompt.zone = Some(PromptZone::Prompt);
                if !fresh {
                    return;
                }
                self.finishing = false;
                self.owned_prompt.input_bytes = std::mem::take(&mut self.typeahead);
                Marker::PromptStart
            }
            ("B" | "I", _) => {
                if self.owned_prompt.zone == Some(PromptZone::Prompt) {
                    self.owned_prompt.zone = Some(PromptZone::CommandInput);
                }
                return;
            }
            ("C", Some(_)) => {
                self.owned_prompt.zone = Some(PromptZone::CommandOutput);
                Marker::CommandStart {
                    // A cmdline that does not decode never matches a command.
                    cmdline: option(fields, "cmdline")
                        .map(|line| percent_decode(line).unwrap_or_default()),
                }
            }
            ("D", Some(_)) => {
                self.owned_prompt.zone = Some(PromptZone::CommandOutput);
                self.finishing = true;
                Marker::CommandFinished {
                    exit_status: exit_status(fields),
                }
            }
            // Another shell's prompt or command, such as one inside `ssh`.
            ("A" | "P" | "N" | "C" | "D", None) if self.prompt_owner_seen => Marker::Unowned,
            _ => return,
        };
        self.record_run_marker(marker, now);
    }

    pub(crate) fn apply_semantic_prompt(&mut self, value: &str, now: Instant) -> bool {
        let mut fields = value.split(';');
        let Some(action) = fields.next() else {
            return false;
        };
        let fields = fields.collect::<Vec<_>>();
        let owned = self.observe_prompt_owner(action, &fields);
        let fresh = owned.as_ref().map(Option::is_some);
        let committed = owned.flatten();
        let changed = self.apply_prompt_action(action, &fields, committed, now);
        self.observe_owned_prompt(action, &fields, fresh, now);
        changed
    }

    fn apply_prompt_action(
        &mut self,
        action: &str,
        fields: &[&str],
        committed: Option<Arc<str>>,
        now: Instant,
    ) -> bool {
        match action {
            "A" | "P" => {
                let completed = self
                    .snapshot
                    .command
                    .as_ref()
                    .is_some_and(|command| command.state == CommandState::Running)
                    && self.finish_command(None, now);
                self.update(|snapshot| {
                    snapshot.prompt_zone = PromptZone::Prompt;
                    if let Some(directory) = committed {
                        snapshot.confirmed_directory = Some(Arc::clone(&directory));
                        snapshot.repository_directory = directory;
                    }
                }) || completed
            }
            "B" | "I" => self.update(|snapshot| snapshot.prompt_zone = PromptZone::CommandInput),
            "C" => {
                self.command_started = Some(now);
                let line = option(fields, "cmdline")
                    .and_then(percent_decode)
                    .unwrap_or_default();
                let line = sanitize_bounded(&line, MAX_COMMAND_CHARS);
                self.retire_command_reports(|snapshot| {
                    snapshot.prompt_zone = PromptZone::CommandOutput;
                    snapshot.command = Some(CommandMetadata {
                        line: Arc::from(line),
                        state: CommandState::Running,
                    });
                    snapshot.title_activity = false;
                })
            }
            "D" => self.finish_command(exit_status(fields), now),
            _ => false,
        }
    }

    pub(crate) fn apply_progress_report(
        &mut self,
        state: u8,
        progress: Option<u8>,
        now: Instant,
    ) -> bool {
        let progress = progress.unwrap_or(0).min(100);
        let progress = match state {
            0 => {
                self.progress_expiry = None;
                self.title_animation = TitleActivity::default();
                ProgressMetadata::None
            }
            1 => ProgressMetadata::Normal(progress),
            2 => ProgressMetadata::Error(progress),
            3 => ProgressMetadata::Indeterminate,
            4 => ProgressMetadata::Paused(progress),
            _ => return false,
        };
        if progress != ProgressMetadata::None {
            self.progress_expiry = Some(now + PROGRESS_INACTIVITY_TIMEOUT);
        }
        let title_activity = self.title_animation.active();
        let glyph = self.title_animation.glyph();
        self.update(|snapshot| {
            snapshot.progress = progress;
            snapshot.title_activity = title_activity;
            snapshot.title_glyph = glyph;
        })
    }

    /// Returns the next point when retained status presentation must change.
    pub(crate) fn status_deadline(&self) -> Option<Instant> {
        [self.progress_expiry, self.title_animation.deadline()]
            .into_iter()
            .flatten()
            .min()
    }

    /// Applies every delayed status transition due by `now`.
    pub(crate) fn advance_status(&mut self, now: Instant) -> bool {
        let expire_progress = self.progress_expiry.is_some_and(|deadline| now >= deadline);
        if expire_progress {
            self.progress_expiry = None;
        }
        self.title_animation.advance(now);
        let active = self.title_animation.active();
        let glyph = self.title_animation.glyph();
        self.update(|snapshot| {
            snapshot.title_activity = active;
            snapshot.title_glyph = glyph;
            if expire_progress {
                snapshot.progress = ProgressMetadata::None;
            }
        })
    }

    fn update(&mut self, change: impl FnOnce(&mut TerminalMetadataSnapshot)) -> bool {
        let mut next = (*self.snapshot).clone();
        change(&mut next);
        if next == *self.snapshot {
            return false;
        }
        next.revision = self.snapshot.revision.saturating_add(1);
        self.snapshot = Arc::new(next);
        true
    }
}

fn exit_status(fields: &[&str]) -> Option<i32> {
    fields
        .first()
        .and_then(|value| value.parse::<i32>().ok())
        .or_else(|| option(fields, "err").and_then(|value| value.parse().ok()))
}

fn option<'a>(fields: &'a [&str], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find_map(|field| field.strip_prefix(name)?.strip_prefix('='))
}

fn sanitize_bounded(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_owned()
}

pub(crate) fn sanitize_title(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_TITLE_CHARS)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

pub(crate) fn parse_osc7_directory(
    value: &str,
    context: &TerminalMetadataContext,
) -> Option<DirectoryMetadata> {
    let remainder = value
        .get(..7)?
        .eq_ignore_ascii_case("file://")
        .then(|| &value[7..])?;
    let slash = remainder.find('/')?;
    let (authority, path) = remainder.split_at(slash);
    let authority_is_local = authority.is_empty()
        || authority.eq_ignore_ascii_case("localhost")
        || context
            .local_hostname()
            .is_some_and(|hostname| authority.eq_ignore_ascii_case(hostname));
    if (context.is_local() && !authority_is_local)
        || !path.starts_with('/')
        || path.contains(['?', '#'])
        || path.chars().any(char::is_control)
    {
        return None;
    }

    let path = percent_decode(path)?;
    let path = match context.local_paths() {
        Some(paths) => paths.decode_directory_uri_path(path)?,
        None => path,
    };
    if path.is_empty() || path.chars().any(char::is_control) {
        return None;
    }
    Some(DirectoryMetadata {
        path: Arc::from(path),
        provenance: DirectoryProvenance::Osc7,
    })
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let high = *bytes.get(index + 1)?;
        let low = *bytes.get(index + 2)?;
        decoded.push(hex_digit(high)? << 4 | hex_digit(low)?);
        index += 3;
    }
    String::from_utf8(decoded).ok()
}

const fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::RemoteDirectory;
    use crate::local_path::LocalPathSemantics;

    #[test]
    fn remote_metadata_context_should_preserve_server_display_and_directory() {
        let remote = RemoteTerminalMetadataContext::new(
            "user@remote".to_owned(),
            RemoteDirectory::new("~/project".to_owned()).unwrap(),
        );
        let context = TerminalMetadataContext::Remote(remote.clone());

        let tracker =
            MetadataTracker::new_with_context(context, "Remote Workspace", Instant::now());
        let snapshot = tracker.snapshot();

        assert_eq!(snapshot.context.remote(), Some(&remote));
        assert_eq!(snapshot.directory.path.as_ref(), "~/project");
        assert_eq!(snapshot.title.value.as_ref(), "Remote Workspace");
        assert!(!snapshot.context.is_local());
        assert_eq!(
            snapshot.context.local_file_capabilities(),
            TerminalLocalFileCapabilities::Disabled
        );
    }

    #[test]
    fn local_file_capabilities_should_be_derived_only_from_terminal_context() {
        let local = TerminalMetadataContext::local(
            crate::local_path::LocalPathSemantics::Posix,
            "/Users/test",
            LocalMachine::new(Some("test"), Some("mac.local"), Some("/Users/test")),
        );
        let remote = TerminalMetadataContext::Remote(RemoteTerminalMetadataContext::new(
            "user@remote".to_owned(),
            RemoteDirectory::new("~/project".to_owned()).unwrap(),
        ));

        assert_eq!(
            local.local_file_capabilities(),
            TerminalLocalFileCapabilities::Enabled
        );
        assert_eq!(
            remote.local_file_capabilities(),
            TerminalLocalFileCapabilities::Disabled
        );
    }

    #[test]
    fn selected_local_paths_preserve_reported_spelling_without_granting_remote_authority() {
        let local = TerminalMetadataContext::local(
            LocalPathSemantics::Posix,
            "/fixture",
            LocalMachine::default(),
        );
        let remote = TerminalMetadataContext::Remote(RemoteTerminalMetadataContext::new(
            "user@remote".to_owned(),
            RemoteDirectory::new("~/project".to_owned()).unwrap(),
        ));
        for context in [&local, &remote] {
            let mut tracker =
                MetadataTracker::new_with_context(context.clone(), "shell", Instant::now());
            assert!(tracker.set_reported_directory("file://localhost//project/My%20Directory/"));
            let snapshot = tracker.snapshot();
            assert_eq!(&*snapshot.directory.path, "//project/My Directory/");
            assert_eq!(&*snapshot.title.value, "My Directory");
            assert_eq!(
                context.local_directory(&snapshot.directory.path).is_some(),
                context.is_local()
            );
            assert!(context.local_directory("relative").is_none());
            assert!(context.local_directory("C:\\project").is_none());
            assert_eq!(
                tracker.set_reported_directory("file://remote/other"),
                !context.is_local()
            );
            assert!(!tracker.set_reported_directory("file:///bad%00path"));
        }
    }

    #[test]
    fn title_metadata_strips_controls_and_is_bounded() {
        let hostile = format!("  cargo\u{1b}]0;forged\u{7} test  {}", "x".repeat(400));

        let title = sanitize_title(&hostile);

        assert!(!title.chars().any(char::is_control));
        assert!(!title.starts_with(char::is_whitespace));
        assert!(!title.ends_with(char::is_whitespace));
        assert_eq!(title.chars().count(), 256);
    }

    #[test]
    fn osc7_accepts_only_local_absolute_file_urls() {
        let context = TerminalMetadataContext::local(
            LocalPathSemantics::Posix,
            "/fixture",
            LocalMachine::new(None, Some("mac.local"), None),
        );
        let local = parse_osc7_directory("FiLe://MAC.LOCAL/Users/me/My%20Project", &context)
            .expect("local OSC 7 should be accepted");
        assert_eq!(local.path.as_ref(), "/Users/me/My Project");
        assert_eq!(local.provenance, DirectoryProvenance::Osc7);
        for invalid in [
            "file://remote.example/tmp",
            "file://localhost",
            "file:///tmp/%ZZ",
            "https://localhost/tmp",
        ] {
            assert!(parse_osc7_directory(invalid, &context).is_none());
        }
    }

    #[test]
    fn pending_title_glyph_is_bounded_and_does_not_cross_command_boundaries() {
        let epoch = Instant::now();
        let mut tracker = MetadataTracker::new(
            LocalPathSemantics::Posix,
            "/tmp",
            "zsh",
            LocalMachine::default(),
            epoch,
        );
        let glyph = |tracker: &MetadataTracker| {
            let snapshot = tracker.snapshot();
            let reported = super::super::title::reported_title(&snapshot.title.value);
            snapshot
                .title_glyph
                .resolve(reported.glyph)
                .map(str::to_owned)
        };
        tracker.apply_semantic_prompt("C;cmdline=agent", epoch);
        tracker.set_reported_title("✳ agent", epoch);
        tracker.set_reported_title("◐ agent", epoch);
        assert_eq!(glyph(&tracker).as_deref(), Some("✳"));

        // An unchanging symbol eventually remains a reported icon, without claiming activity.
        tracker.advance_status(epoch + Duration::from_secs(2));
        assert_eq!(glyph(&tracker).as_deref(), Some("◐"));
        assert!(!tracker.snapshot().title_activity);
        assert_eq!(tracker.status_deadline(), None);
        tracker.set_reported_title("◐ agent", epoch + Duration::from_millis(2100));
        assert_eq!(glyph(&tracker).as_deref(), Some("◐"));
        assert_eq!(tracker.status_deadline(), None);

        tracker.set_reported_title("✳ agent", epoch + Duration::from_secs(3));
        tracker.set_reported_title("◐ agent", epoch + Duration::from_secs(3));
        tracker.apply_semantic_prompt("D;0", epoch + Duration::from_secs(4));
        tracker.apply_semantic_prompt("C;cmdline=another", epoch + Duration::from_secs(4));
        tracker.set_reported_title("◐ another", epoch + Duration::from_secs(4));
        assert_eq!(glyph(&tracker).as_deref(), Some("◐"));
    }

    #[test]
    fn first_activity_frame_remains_reported_while_confirmation_is_pending() {
        let epoch = Instant::now();
        let mut tracker = MetadataTracker::new(
            LocalPathSemantics::Posix,
            "/tmp",
            "zsh",
            LocalMachine::default(),
            epoch,
        );

        tracker.set_reported_title("◐ agent", epoch);

        let snapshot = tracker.snapshot();
        let reported = super::super::title::reported_title(&snapshot.title.value);
        assert_eq!(snapshot.title_glyph.resolve(reported.glyph), Some("◐"));
    }

    #[test]
    fn expired_static_activity_glyph_remains_visible_during_new_confirmation() {
        let epoch = Instant::now();
        let mut tracker = MetadataTracker::new(
            LocalPathSemantics::Posix,
            "/tmp",
            "zsh",
            LocalMachine::default(),
            epoch,
        );
        tracker.set_reported_title("◐ agent", epoch);
        tracker.advance_status(epoch + Duration::from_secs(2));

        tracker.set_reported_title("◑ agent", epoch + Duration::from_millis(2100));

        let snapshot = tracker.snapshot();
        let reported = super::super::title::reported_title(&snapshot.title.value);
        assert_eq!(snapshot.title_glyph.resolve(reported.glyph), Some("◐"));
    }

    #[test]
    fn command_boundaries_retire_reported_title_and_progress() {
        for boundary in ["D;0", "A", "C;cmdline=ls"] {
            let epoch = Instant::now();
            let mut tracker = MetadataTracker::new(
                LocalPathSemantics::Posix,
                "/tmp/project",
                "zsh",
                LocalMachine::default(),
                epoch,
            );
            tracker.apply_semantic_prompt("C;cmdline=agent", epoch);
            tracker.set_reported_title("π - project", epoch);
            tracker.apply_progress_report(3, None, epoch);

            tracker.apply_semantic_prompt(boundary, epoch + Duration::from_secs(1));

            assert_eq!(
                tracker.snapshot().title.provenance,
                TitleProvenance::WorkingDirectory,
                "{boundary}"
            );
            assert_eq!(
                tracker.snapshot().title.value.as_ref(),
                "project",
                "{boundary}"
            );
            assert_eq!(
                tracker.snapshot().progress,
                ProgressMetadata::None,
                "{boundary}"
            );
            assert_eq!(tracker.status_deadline(), None, "{boundary}");
        }
    }

    #[test]
    fn duplicate_completion_preserves_the_new_prompt_title() {
        let epoch = Instant::now();
        let mut tracker = MetadataTracker::new(
            LocalPathSemantics::Posix,
            "/tmp",
            "zsh",
            LocalMachine::default(),
            epoch,
        );
        tracker.apply_semantic_prompt("C;cmdline=agent", epoch);
        tracker.set_reported_title("π agent", epoch);
        tracker.apply_semantic_prompt("D;0", epoch + Duration::from_secs(1));
        tracker.set_reported_title("shell prompt", epoch + Duration::from_secs(1));
        tracker.apply_semantic_prompt("D;0", epoch + Duration::from_secs(2));
        tracker.apply_semantic_prompt("A", epoch + Duration::from_secs(2));
        assert_eq!(tracker.snapshot().title.value.as_ref(), "shell prompt");
    }

    #[test]
    fn waiting_interactive_command_does_not_report_work() {
        let epoch = Instant::now();
        let mut tracker = MetadataTracker::new(
            LocalPathSemantics::Posix,
            "/tmp",
            "zsh",
            LocalMachine::default(),
            epoch,
        );
        tracker.apply_semantic_prompt("C;cmdline=interactive", epoch);
        tracker.advance_status(epoch + Duration::from_secs(60));
        assert!(!tracker.snapshot().title_activity);
    }

    #[test]
    fn accepted_semantic_and_progress_events_update_metadata() {
        let epoch = Instant::now();
        let mut tracker = MetadataTracker::new(
            crate::local_path::LocalPathSemantics::Posix,
            "/tmp",
            "zsh",
            LocalMachine::new(None, Some("mac.local"), None),
            epoch,
        );

        assert!(
            tracker
                .apply_semantic_prompt("C;cmdline=cargo%20test", epoch + Duration::from_secs(2),)
        );
        assert_eq!(tracker.snapshot().prompt_zone, PromptZone::CommandOutput);
        assert_eq!(
            tracker.snapshot().command,
            Some(CommandMetadata {
                line: Arc::from("cargo test"),
                state: CommandState::Running,
            })
        );

        assert!(tracker.apply_progress_report(1, Some(140), epoch + Duration::from_secs(3)));
        assert!(tracker.apply_semantic_prompt("D;7", epoch + Duration::from_secs(5)));
        assert_eq!(tracker.snapshot().progress, ProgressMetadata::None);
        assert_eq!(
            tracker.snapshot().command,
            Some(CommandMetadata {
                line: Arc::from("cargo test"),
                state: CommandState::Finished {
                    exit_status: Some(7),
                    duration: Duration::from_secs(3),
                },
            })
        );
    }

    #[test]
    fn progress_reports_preserve_every_state_and_remove_explicitly() {
        let epoch = Instant::now();
        let mut tracker = MetadataTracker::new(
            crate::local_path::LocalPathSemantics::Posix,
            "/tmp",
            "zsh",
            LocalMachine::default(),
            epoch,
        );

        for (state, value, expected) in [
            (1, Some(140), ProgressMetadata::Normal(100)),
            (2, Some(35), ProgressMetadata::Error(35)),
            (3, None, ProgressMetadata::Indeterminate),
            (4, Some(65), ProgressMetadata::Paused(65)),
            (0, None, ProgressMetadata::None),
        ] {
            assert!(tracker.apply_progress_report(state, value, epoch));
            assert_eq!(tracker.snapshot().progress, expected);
        }
        assert_eq!(tracker.status_deadline(), None);
    }

    #[test]
    fn identical_progress_reports_refresh_inactivity_without_revising_presentation() {
        let epoch = Instant::now();
        let mut tracker = MetadataTracker::new(
            crate::local_path::LocalPathSemantics::Posix,
            "/tmp",
            "zsh",
            LocalMachine::default(),
            epoch,
        );

        assert!(tracker.apply_progress_report(3, None, epoch));
        assert_eq!(
            tracker.status_deadline(),
            Some(epoch + Duration::from_secs(30))
        );
        let revision = tracker.snapshot().revision;
        let keepalive = epoch + Duration::from_secs(20);
        assert!(!tracker.apply_progress_report(3, None, keepalive));
        assert_eq!(tracker.snapshot().revision, revision);
        assert_eq!(
            tracker.status_deadline(),
            Some(keepalive + Duration::from_secs(30))
        );
        assert!(!tracker.advance_status(epoch + Duration::from_secs(30)));
        assert_eq!(tracker.snapshot().progress, ProgressMetadata::Indeterminate);
        assert!(tracker.advance_status(keepalive + Duration::from_secs(30)));
        assert_eq!(tracker.snapshot().progress, ProgressMetadata::None);
    }

    #[test]
    fn stale_metadata_clears_progress_and_delayed_activity() {
        let epoch = Instant::now();
        let mut tracker = MetadataTracker::new(
            crate::local_path::LocalPathSemantics::Posix,
            "/tmp",
            "zsh",
            LocalMachine::default(),
            epoch,
        );
        assert!(tracker.apply_semantic_prompt("C", epoch));
        assert!(tracker.apply_progress_report(4, Some(70), epoch));
        tracker.set_reported_title("◐ agent", epoch);
        tracker.set_reported_title("◑ agent", epoch + Duration::from_millis(100));
        tracker.advance_status(epoch + Duration::from_millis(200));
        assert!(tracker.snapshot().title_activity);
        assert!(tracker.status_deadline().is_some());

        assert!(tracker.mark_stale());

        assert_eq!(tracker.snapshot().progress, ProgressMetadata::None);
        assert!(!tracker.snapshot().title_activity);
        assert_eq!(tracker.status_deadline(), None);
    }

    #[test]
    fn stale_transition_does_not_mutate_previously_published_metadata() {
        let epoch = Instant::now();
        let mut tracker = MetadataTracker::new(
            crate::local_path::LocalPathSemantics::Posix,
            "/tmp",
            "zsh",
            LocalMachine::default(),
            epoch,
        );
        let live = tracker.snapshot();

        assert!(tracker.mark_stale());
        let stale = tracker.snapshot();

        assert_eq!(live.freshness, MetadataFreshness::Live);
        assert_eq!(stale.freshness, MetadataFreshness::Stale);
        assert_eq!(stale.revision, live.revision + 1);
        assert!(!Arc::ptr_eq(&live, &stale));
    }
    fn local_tracker(epoch: Instant) -> MetadataTracker {
        MetadataTracker::new(
            LocalPathSemantics::Posix,
            "/initial",
            "zsh",
            LocalMachine::new(None, Some("mac.local"), None),
            epoch,
        )
    }

    #[test]
    fn repository_source_follows_directory_reports_at_the_prompt() {
        let mut tracker = local_tracker(Instant::now());
        assert_eq!(&*tracker.snapshot().repository_directory, "/initial");

        assert!(tracker.set_reported_directory("file://mac.local/project"));

        assert_eq!(&*tracker.snapshot().repository_directory, "/project");
    }

    #[test]
    fn repository_source_ignores_reports_while_a_command_runs_until_it_finishes() {
        let epoch = Instant::now();
        let mut tracker = local_tracker(epoch);
        tracker.set_reported_directory("file://mac.local/project");
        tracker.apply_semantic_prompt("C;cmdline=docker%20run", epoch);

        assert!(tracker.set_reported_directory("file://localhost/container"));
        assert_eq!(&*tracker.snapshot().directory.path, "/container");
        assert_eq!(&*tracker.snapshot().repository_directory, "/project");

        tracker.apply_semantic_prompt("D;0", epoch);
        assert_eq!(&*tracker.snapshot().repository_directory, "/container");
        tracker.set_reported_directory("file://mac.local/project");
        assert_eq!(&*tracker.snapshot().repository_directory, "/project");
    }

    #[test]
    fn a_prompt_without_completion_finishes_the_command_for_the_repository_source() {
        let epoch = Instant::now();
        let mut tracker = local_tracker(epoch);
        tracker.apply_semantic_prompt("C", epoch);
        tracker.set_reported_directory("file://mac.local/elsewhere");
        assert_eq!(tracker.snapshot().finished_commands, 0);

        tracker.apply_semantic_prompt("A", epoch);

        assert_eq!(&*tracker.snapshot().repository_directory, "/elsewhere");
        assert_eq!(tracker.snapshot().finished_commands, 1);
    }

    fn owned_local_tracker(epoch: Instant) -> MetadataTracker {
        MetadataTracker::new_with_context(
            TerminalMetadataContext::local(
                LocalPathSemantics::Posix,
                "/initial",
                LocalMachine::new(None, Some("mac.local"), None),
            )
            .with_prompt_owner(PromptOwner::fixture("0f1e")),
            "zsh",
            epoch,
        )
    }

    #[test]
    fn restoration_directory_requires_the_owning_shell_prompt() {
        let epoch = Instant::now();
        let mut tracker = owned_local_tracker(epoch);
        tracker.set_reported_directory("file://mac.local/project");
        tracker.apply_semantic_prompt("A", epoch);
        assert_eq!(tracker.snapshot().confirmed_directory, None);
        tracker.set_reported_directory("file://mac.local/project");
        tracker.apply_semantic_prompt("A;spaceterm=0f1e", epoch);
        assert_eq!(
            tracker.snapshot().confirmed_directory.as_deref(),
            Some("/project")
        );
        tracker.set_reported_directory("file://localhost/container");
        tracker.apply_semantic_prompt("A;spaceterm=foreign", epoch);
        assert_eq!(
            tracker.snapshot().confirmed_directory.as_deref(),
            Some("/project")
        );
    }

    #[test]
    fn a_nested_shell_should_not_move_the_repository_source_of_its_owning_shell() {
        let epoch = Instant::now();
        let mut tracker = owned_local_tracker(epoch);
        tracker.set_reported_directory("file://mac.local/project");
        tracker.apply_semantic_prompt("A;redraw=1;spaceterm=0f1e", epoch);
        tracker.apply_semantic_prompt("C;cmdline=ssh%20host;spaceterm=0f1e", epoch);

        // The remote shell reports its directory and prompts as its own commands finish.
        for marker in ["D;0", "A;redraw=1", "D;0;spaceterm=forged"] {
            tracker.set_reported_directory("file://localhost/srv/project");
            tracker.apply_semantic_prompt(marker, epoch);
            assert_eq!(&*tracker.snapshot().repository_directory, "/project");
        }
        tracker.set_reported_directory("file://localhost/srv/project");
        assert_eq!(&*tracker.snapshot().directory.path, "/srv/project");

        tracker.apply_semantic_prompt("D;0;spaceterm=0f1e", epoch);
        assert_eq!(&*tracker.snapshot().repository_directory, "/project");
        tracker.set_reported_directory("file://mac.local/project/next");
        tracker.apply_semantic_prompt("A;redraw=1;spaceterm=0f1e", epoch);
        assert_eq!(&*tracker.snapshot().repository_directory, "/project/next");
    }

    #[test]
    fn a_nested_shell_should_not_move_the_repository_source_of_a_shell_without_command_markers() {
        // Bash 3.2 does not expand PS0, so its owning shell never reports a command start.
        let epoch = Instant::now();
        let mut tracker = owned_local_tracker(epoch);
        tracker.set_reported_directory("file://mac.local/project");
        tracker.apply_semantic_prompt("A;redraw=last;spaceterm=0f1e", epoch);

        tracker.set_reported_directory("file://localhost/srv/project");
        tracker.apply_semantic_prompt("A;redraw=1", epoch);
        assert_eq!(&*tracker.snapshot().directory.path, "/srv/project");
        assert_eq!(&*tracker.snapshot().repository_directory, "/project");

        tracker.apply_semantic_prompt("D;0;spaceterm=0f1e", epoch);
        tracker.set_reported_directory("file://mac.local/project");
        tracker.apply_semantic_prompt("A;redraw=last;spaceterm=0f1e", epoch);
        assert_eq!(&*tracker.snapshot().repository_directory, "/project");
    }

    #[test]
    fn a_shell_that_reports_its_directory_before_its_prompt_without_completion_should_move_it() {
        // Nushell reports its directory just before its prompt marker and sends no completion.
        let epoch = Instant::now();
        let mut tracker = owned_local_tracker(epoch);
        tracker.set_reported_directory("file://mac.local/project");
        tracker.apply_semantic_prompt("A;redraw=1;spaceterm=0f1e", epoch);
        tracker.apply_semantic_prompt("C;spaceterm=0f1e", epoch);

        tracker.set_reported_directory("file://mac.local/other");
        assert_eq!(&*tracker.snapshot().repository_directory, "/project");
        tracker.apply_semantic_prompt("A;redraw=1;spaceterm=0f1e", epoch);

        assert_eq!(&*tracker.snapshot().repository_directory, "/other");
    }

    #[test]
    fn a_shell_that_never_sends_its_prompt_owner_should_keep_the_unowned_rule() {
        let epoch = Instant::now();
        let mut tracker = owned_local_tracker(epoch);
        tracker.apply_semantic_prompt("C", epoch);
        tracker.set_reported_directory("file://mac.local/elsewhere");
        assert_eq!(&*tracker.snapshot().repository_directory, "/initial");

        tracker.apply_semantic_prompt("D;0", epoch);

        assert_eq!(&*tracker.snapshot().repository_directory, "/elsewhere");
    }

    /// A fresh prompt follows the owning shell's directory report; a redraw does not.
    fn fresh_prompt(tracker: &mut MetadataTracker, now: Instant) {
        tracker.set_reported_directory("file://mac.local/project");
        tracker.apply_semantic_prompt("A;redraw=1;spaceterm=0f1e", now);
    }

    #[test]
    fn only_a_fresh_owned_prompt_empties_the_input_line() {
        let now = Instant::now();
        let mut tracker = owned_local_tracker(now);
        assert_eq!(tracker.owned_prompt().zone, None);
        tracker.apply_semantic_prompt("A", now);
        assert_eq!(
            tracker.owned_prompt().zone,
            None,
            "an unowned prompt is not the shell's"
        );

        fresh_prompt(&mut tracker, now);
        tracker.apply_semantic_prompt("B", now);
        assert_eq!(
            tracker.owned_prompt(),
            OwnedPrompt {
                zone: Some(PromptZone::CommandInput),
                input_bytes: 0
            }
        );
        tracker.note_input(2);
        // A redraw, as zsh prints on resize, keeps what was typed.
        tracker.apply_semantic_prompt("A;redraw=1;spaceterm=0f1e", now);
        assert_eq!(tracker.owned_prompt().input_bytes, 2);
        fresh_prompt(&mut tracker, now);
        assert_eq!(tracker.owned_prompt().input_bytes, 0);

        // Input a running command may read is the command's. Input written after the command
        // finished, before the prompt, can reach only the next prompt's line editor.
        tracker.apply_semantic_prompt("C;spaceterm=0f1e", now);
        assert_eq!(tracker.owned_prompt().zone, Some(PromptZone::CommandOutput));
        tracker.note_input(5);
        tracker.apply_semantic_prompt("D;0;spaceterm=0f1e", now);
        tracker.note_input(3);
        fresh_prompt(&mut tracker, now);
        assert_eq!(tracker.owned_prompt().input_bytes, 3);
        fresh_prompt(&mut tracker, now);
        assert_eq!(tracker.owned_prompt().input_bytes, 0);
    }

    #[test]
    fn a_followed_run_records_owned_markers_until_its_finish() {
        use spaceterm_server::pane::run::Marker;
        let now = Instant::now();
        let mut tracker = owned_local_tracker(now);
        fresh_prompt(&mut tracker, now);
        tracker.apply_semantic_prompt("C", now);
        assert!(
            tracker.take_run_markers().is_empty(),
            "nothing is followed before a run"
        );

        fresh_prompt(&mut tracker, now);
        tracker.follow_run();
        tracker.apply_semantic_prompt("B", now);
        tracker.apply_semantic_prompt("C;cmdline=echo%20a;spaceterm=0f1e", now);
        tracker.apply_semantic_prompt("D;0", now);
        tracker.apply_semantic_prompt("D;7;spaceterm=0f1e", now);
        fresh_prompt(&mut tracker, now);
        let markers: Vec<_> = tracker
            .take_run_markers()
            .into_iter()
            .map(|marker| (marker.marker, marker.finished_commands))
            .collect();
        assert_eq!(
            markers,
            [
                (
                    Marker::CommandStart {
                        cmdline: Some("echo a".into())
                    },
                    1
                ),
                (Marker::Unowned, 2),
                (
                    Marker::CommandFinished {
                        exit_status: Some(7)
                    },
                    2
                ),
            ]
        );
        assert!(
            tracker.take_run_markers().is_empty(),
            "recording stopped at the finish"
        );
    }

    #[test]
    fn a_prompt_owner_should_never_appear_in_debug_output() {
        let context = TerminalMetadataContext::local(
            LocalPathSemantics::Posix,
            "/initial",
            LocalMachine::default(),
        )
        .with_prompt_owner(PromptOwner::fixture("0f1e"));

        assert!(!format!("{context:?}").contains("0f1e"));
    }

    #[test]
    fn finished_commands_count_each_completion_once() {
        let epoch = Instant::now();
        let mut tracker = local_tracker(epoch);
        for _ in 0..2 {
            tracker.apply_semantic_prompt("C", epoch);
            tracker.apply_semantic_prompt("D;0", epoch);
            tracker.apply_semantic_prompt("D;0", epoch);
        }

        assert_eq!(tracker.snapshot().finished_commands, 2);
    }

    #[test]
    fn remote_osc7_accepts_machine_hostname_without_local_filesystem_authority() {
        let context = TerminalMetadataContext::Remote(RemoteTerminalMetadataContext::new(
            "dev-alias".into(),
            RemoteDirectory::new("~".into()).unwrap(),
        ));
        let report = parse_osc7_directory("file://actual-hostname/srv/my%20app", &context).unwrap();
        assert_eq!(
            context.current_directory(&report.path),
            Some(CurrentDirectory::Remote(
                RemoteDirectory::new("/srv/my app".into()).unwrap()
            ))
        );
        assert!(context.local_directory(&report.path).is_none());
        assert!(!context.local_file_capabilities().are_enabled());
    }
}
