use std::sync::Arc;
use std::time::{Duration, Instant};

use super::title::TitleActivity;
use crate::domain::{RemoteDirectory, SshDestination};
use crate::local_path::LocalPathSemantics;

const MAX_TITLE_CHARS: usize = 256;
const MAX_COMMAND_CHARS: usize = 4096;
pub(crate) const PROGRESS_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) use crate::domain::CurrentDirectory;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirectoryProvenance {
    Initial,
    Osc7,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DirectoryMetadata {
    pub(crate) path: Arc<str>,
    pub(crate) provenance: DirectoryProvenance,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum MetadataFreshness {
    #[default]
    Live,
    Stale,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TitleProvenance {
    #[default]
    Fallback,
    WorkingDirectory,
    TerminalControl,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TitleMetadata {
    pub(crate) value: Arc<str>,
    pub(crate) provenance: TitleProvenance,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum PromptZone {
    #[default]
    Unknown,
    Prompt,
    CommandInput,
    CommandOutput,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CommandState {
    Running,
    Finished {
        exit_status: Option<i32>,
        duration: Duration,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommandMetadata {
    pub(crate) line: Arc<str>,
    pub(crate) state: CommandState,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ProgressMetadata {
    #[default]
    None,
    Normal(u8),
    Error(u8),
    Indeterminate,
    Paused(u8),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
/// The local account, machine, and home facts a Local Terminal presents as its origin.
///
/// Composition captures these once from the host. They are presentation identity only and carry
/// no filesystem authority; `home` is a spelling used to abbreviate displayed directories.
pub(crate) struct LocalMachine {
    user: Option<Arc<str>>,
    hostname: Option<Arc<str>>,
    home: Option<Arc<str>>,
}

impl LocalMachine {
    pub(crate) fn new(user: Option<&str>, hostname: Option<&str>, home: Option<&str>) -> Self {
        Self {
            user: Self::retain(user),
            hostname: Self::retain(hostname),
            home: Self::retain(home),
        }
    }

    fn retain(value: Option<&str>) -> Option<Arc<str>> {
        retain_machine_value(value)
    }

    pub(crate) fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }

    pub(crate) fn hostname(&self) -> Option<&str> {
        self.hostname.as_deref()
    }

    pub(crate) fn home(&self) -> Option<&str> {
        self.home.as_deref()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
/// The remote account facts a Remote Terminal presents, captured when its account was discovered.
///
/// These are remote strings kept for presentation. They are never local filesystem authority, and
/// `home` is used only to abbreviate a displayed remote directory.
pub(crate) struct RemoteMachine {
    user: Option<Arc<str>>,
    home: Option<Arc<str>>,
}

impl RemoteMachine {
    pub(crate) fn new(user: Option<&str>, home: Option<&str>) -> Self {
        Self {
            user: retain_machine_value(user),
            home: retain_machine_value(home),
        }
    }

    pub(crate) fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }

    pub(crate) fn home(&self) -> Option<&str> {
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

/// The account and machine one Terminal Session runs on, as its Pane presents it.
///
/// Local and Remote are distinct so a caption can tell a local shell from a remote one without
/// reinterpreting either side's strings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalOrigin<'a> {
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
/// Immutable remote identity and startup-directory context for one Terminal Session.
///
/// The directory retains its exact remote spelling for presentation and startup. It is not local
/// filesystem authority and must never be converted to `PathBuf` or locally validated.
pub(crate) struct RemoteTerminalMetadataContext {
    destination: SshDestination,
    initial_directory: RemoteDirectory,
    machine: RemoteMachine,
}

impl RemoteTerminalMetadataContext {
    pub(crate) const fn new(
        destination: SshDestination,
        initial_directory: RemoteDirectory,
    ) -> Self {
        Self {
            destination,
            initial_directory,
            machine: RemoteMachine {
                user: None,
                home: None,
            },
        }
    }

    /// Attaches the discovered account, so the Pane can name its user and abbreviate its home.
    pub(crate) fn with_machine(mut self, machine: RemoteMachine) -> Self {
        self.machine = machine;
        self
    }

    #[cfg(test)]
    pub(crate) const fn destination(&self) -> &SshDestination {
        &self.destination
    }

    pub(crate) const fn initial_directory(&self) -> &RemoteDirectory {
        &self.initial_directory
    }

    /// Changes the Starting Directory while retaining the discovered account and machine facts.
    pub(crate) fn set_initial_directory(&mut self, directory: RemoteDirectory) {
        self.initial_directory = directory;
    }

    /// Splits the destination into the account it names, if any, and the machine.
    ///
    /// A destination may be a host alias that names no account, in which case the discovered
    /// account supplies the user instead.
    fn destination_parts(&self) -> (Option<&str>, &str) {
        match self.destination.as_str().rsplit_once('@') {
            Some((user, host)) => (Some(user), host),
            None => (None, self.destination.as_str()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// The typed location context from which terminal metadata derives its path authority.
///
/// Local context may classify and validate local paths. Remote context preserves remote strings
/// only and disables every feature that would interpret them through the local filesystem.
pub(crate) enum TerminalMetadataContext {
    Local {
        paths: LocalPathSemantics,
        initial_directory: Arc<str>,
        machine: LocalMachine,
    },
    Remote(RemoteTerminalMetadataContext),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Whether Terminal metadata may authorize features backed by the local filesystem.
///
/// This capability is derived from Local versus Remote metadata and is not user-configurable.
pub(crate) enum TerminalLocalFileCapabilities {
    Enabled,
    Disabled,
}

impl TerminalLocalFileCapabilities {
    pub(crate) const fn are_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

impl TerminalMetadataContext {
    pub(crate) fn local(
        paths: LocalPathSemantics,
        initial_directory: &str,
        machine: LocalMachine,
    ) -> Self {
        Self::Local {
            paths,
            initial_directory: Arc::from(initial_directory),
            machine,
        }
    }

    pub(crate) const fn local_paths(&self) -> Option<LocalPathSemantics> {
        match self {
            Self::Local { paths, .. } => Some(*paths),
            Self::Remote(_) => None,
        }
    }

    pub(crate) fn current_directory(&self, directory: &str) -> Option<CurrentDirectory> {
        match self {
            Self::Local { .. } => self.local_directory(directory).map(CurrentDirectory::Local),
            Self::Remote(_) => RemoteDirectory::new(directory.to_owned())
                .ok()
                .map(CurrentDirectory::Remote),
        }
    }

    pub(crate) fn local_directory(&self, directory: &str) -> Option<std::path::PathBuf> {
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

    pub(crate) const fn is_local(&self) -> bool {
        matches!(self, Self::Local { .. })
    }

    pub(crate) const fn local_file_capabilities(&self) -> TerminalLocalFileCapabilities {
        match self {
            Self::Local { .. } => TerminalLocalFileCapabilities::Enabled,
            Self::Remote(_) => TerminalLocalFileCapabilities::Disabled,
        }
    }

    #[cfg(test)]
    pub(crate) const fn remote(&self) -> Option<&RemoteTerminalMetadataContext> {
        match self {
            Self::Local { .. } => None,
            Self::Remote(context) => Some(context),
        }
    }

    pub(crate) fn initial_directory(&self) -> &str {
        match self {
            Self::Local {
                initial_directory, ..
            } => initial_directory,
            Self::Remote(context) => context.initial_directory().as_str(),
        }
    }

    pub(crate) fn local_hostname(&self) -> Option<&str> {
        match self {
            Self::Local { machine, .. } => machine.hostname(),
            Self::Remote(_) => None,
        }
    }

    /// This context's own home spelling, used to abbreviate the directories it displays.
    ///
    /// A Local context abbreviates against the local home and a Remote one against the remote
    /// home, so neither side's home can ever shorten the other side's path.
    pub(crate) fn home(&self) -> Option<&str> {
        match self {
            Self::Local { machine, .. } => machine.home(),
            Self::Remote(context) => context.machine.home(),
        }
    }

    /// The account and machine this Terminal runs on, for presentation only.
    pub(crate) fn origin(&self) -> TerminalOrigin<'_> {
        match self {
            Self::Local { machine, .. } => TerminalOrigin::Local {
                user: machine.user(),
                host: machine.hostname(),
            },
            Self::Remote(context) => {
                let (spelled_user, host) = context.destination_parts();
                TerminalOrigin::Remote {
                    // A destination that spells an account agrees with discovery; an alias does
                    // not spell one, so discovery is what names the user at all.
                    user: context.machine.user().or(spelled_user),
                    host,
                }
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminalMetadataSnapshot {
    pub(crate) revision: u64,
    pub(crate) context: TerminalMetadataContext,
    pub(crate) freshness: MetadataFreshness,
    pub(crate) title: TitleMetadata,
    pub(crate) directory: DirectoryMetadata,
    pub(crate) prompt_zone: PromptZone,
    pub(crate) command: Option<CommandMetadata>,
    /// Whether changing title frames provide recent animation evidence.
    pub(crate) title_activity: bool,
    pub(crate) progress: ProgressMetadata,
}

impl TerminalMetadataSnapshot {
    /// Whether any fact chrome presents differs: the directory, title, command, progress, or
    /// whether those facts are still live.
    pub(crate) fn presentation_differs(&self, other: &Self) -> bool {
        self.freshness != other.freshness
            || self.directory != other.directory
            || self.title != other.title
            || self.command != other.command
            || self.title_activity != other.title_activity
            || self.progress != other.progress
    }

    /// The Current Directory these facts establish, or none once they have gone stale.
    pub(crate) fn current_directory(&self) -> Option<CurrentDirectory> {
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
    progress_expiry: Option<Instant>,
    title_animation: TitleActivity,
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
                title: TitleMetadata {
                    value: Arc::from(sanitize_title(fallback_title)),
                    provenance: TitleProvenance::Fallback,
                },
                directory: DirectoryMetadata {
                    path: Arc::from(initial_directory),
                    provenance: DirectoryProvenance::Initial,
                },
                prompt_zone: PromptZone::Unknown,
                command: None,
                title_activity: false,
                progress: ProgressMetadata::None,
            }),
            epoch,
            fallback_title: Arc::from(sanitize_title(fallback_title)),
            command_started: None,
            progress_expiry: None,
            title_animation: TitleActivity::default(),
        }
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
        self.title_animation
            .observe(&self.snapshot.title.value, &value, now);
        let active = self.title_animation.active();
        self.update(|snapshot| {
            snapshot.title_activity = active;
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
        self.update(|snapshot| {
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
        self.retire_command_reports(|snapshot| {
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

    pub(crate) fn apply_semantic_prompt(&mut self, value: &str, now: Instant) -> bool {
        let mut fields = value.split(';');
        let Some(action) = fields.next() else {
            return false;
        };
        let fields = fields.collect::<Vec<_>>();
        match action {
            "A" | "P" => {
                let completed = self
                    .snapshot
                    .command
                    .as_ref()
                    .is_some_and(|command| command.state == CommandState::Running)
                    && self.finish_command(None, now);
                self.update(|snapshot| snapshot.prompt_zone = PromptZone::Prompt) || completed
            }
            "B" | "I" => self.update(|snapshot| snapshot.prompt_zone = PromptZone::CommandInput),
            "C" => {
                self.command_started = Some(now);
                let line = option(&fields, "cmdline")
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
            "D" => {
                let exit_status = fields
                    .first()
                    .and_then(|value| value.parse::<i32>().ok())
                    .or_else(|| option(&fields, "err").and_then(|value| value.parse().ok()));
                self.finish_command(exit_status, now)
            }
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
        self.update(|snapshot| {
            snapshot.progress = progress;
            snapshot.title_activity = title_activity;
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
        self.update(|snapshot| {
            snapshot.title_activity = active;
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
    use crate::domain::{RemoteDirectory, SshDestination};
    use crate::local_path::LocalPathSemantics;

    #[test]
    fn remote_metadata_context_should_preserve_typed_destination_and_directory() {
        let remote = RemoteTerminalMetadataContext::new(
            SshDestination::new("user@remote".to_owned()).unwrap(),
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
            SshDestination::new("user@remote".to_owned()).unwrap(),
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
            SshDestination::new("user@remote".to_owned()).unwrap(),
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
        assert_eq!(title.chars().count(), MAX_TITLE_CHARS);
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
        let revision = tracker.snapshot().revision;
        let keepalive = epoch + Duration::from_secs(20);
        assert!(!tracker.apply_progress_report(3, None, keepalive));
        assert_eq!(tracker.snapshot().revision, revision);
        assert!(!tracker.advance_status(epoch + PROGRESS_INACTIVITY_TIMEOUT));
        assert_eq!(tracker.snapshot().progress, ProgressMetadata::Indeterminate);
        assert!(tracker.advance_status(keepalive + PROGRESS_INACTIVITY_TIMEOUT));
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
    #[test]
    fn remote_osc7_accepts_machine_hostname_without_local_filesystem_authority() {
        let context = TerminalMetadataContext::Remote(RemoteTerminalMetadataContext::new(
            SshDestination::new("dev-alias".into()).unwrap(),
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
