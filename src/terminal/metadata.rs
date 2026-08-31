use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::domain::{RemoteWorkspaceDirectory, SshDestination};

const MAX_TITLE_CHARS: usize = 256;
const MAX_COMMAND_CHARS: usize = 4096;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteTerminalMetadataContext {
    destination: SshDestination,
    initial_directory: RemoteWorkspaceDirectory,
}

impl RemoteTerminalMetadataContext {
    pub(crate) const fn new(
        destination: SshDestination,
        initial_directory: RemoteWorkspaceDirectory,
    ) -> Self {
        Self {
            destination,
            initial_directory,
        }
    }

    pub(crate) const fn destination(&self) -> &SshDestination {
        &self.destination
    }

    pub(crate) const fn initial_directory(&self) -> &RemoteWorkspaceDirectory {
        &self.initial_directory
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TerminalMetadataContext {
    Local {
        initial_directory: Arc<str>,
        local_hostname: Option<Arc<str>>,
    },
    Remote(RemoteTerminalMetadataContext),
}

impl TerminalMetadataContext {
    pub(crate) fn local(initial_directory: &str, local_hostname: Option<&str>) -> Self {
        Self::Local {
            initial_directory: Arc::from(initial_directory),
            local_hostname: local_hostname.map(Arc::from),
        }
    }

    pub(crate) const fn is_local(&self) -> bool {
        matches!(self, Self::Local { .. })
    }

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
            Self::Local { local_hostname, .. } => local_hostname.as_deref(),
            Self::Remote(_) => None,
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
    pub(crate) progress: ProgressMetadata,
}

pub(crate) struct MetadataTracker {
    snapshot: Arc<TerminalMetadataSnapshot>,
    epoch: Instant,
    command_started: Option<Instant>,
}

impl MetadataTracker {
    pub(crate) fn new(
        initial_directory: &str,
        fallback_title: &str,
        local_hostname: Option<&str>,
        epoch: Instant,
    ) -> Self {
        Self::new_with_context(
            TerminalMetadataContext::local(initial_directory, local_hostname),
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
                progress: ProgressMetadata::None,
            }),
            epoch,
            command_started: None,
        }
    }

    pub(crate) fn snapshot(&self) -> Arc<TerminalMetadataSnapshot> {
        Arc::clone(&self.snapshot)
    }

    pub(crate) fn set_reported_title(&mut self, title: &str) -> bool {
        let title = sanitize_title(title);
        let (value, provenance) = if title.is_empty() {
            (
                directory_basename(&self.snapshot.directory.path)
                    .unwrap_or_else(|| self.snapshot.title.value.to_string()),
                TitleProvenance::WorkingDirectory,
            )
        } else {
            (title, TitleProvenance::TerminalControl)
        };
        self.update(|snapshot| {
            snapshot.title = TitleMetadata {
                value: Arc::from(value),
                provenance,
            };
        })
    }

    pub(crate) fn set_reported_directory(&mut self, value: &str) -> bool {
        let Some(directory) = parse_osc7_directory(value, self.snapshot.context.local_hostname())
        else {
            return false;
        };
        self.update(|snapshot| {
            snapshot.directory = directory;
            if snapshot.title.provenance != TitleProvenance::TerminalControl
                && let Some(title) = directory_basename(&snapshot.directory.path)
            {
                snapshot.title = TitleMetadata {
                    value: Arc::from(title),
                    provenance: TitleProvenance::WorkingDirectory,
                };
            }
        })
    }

    pub(crate) fn mark_stale(&mut self) -> bool {
        self.update(|snapshot| snapshot.freshness = MetadataFreshness::Stale)
    }

    pub(crate) fn apply_semantic_prompt(&mut self, value: &str, now: Instant) -> bool {
        let mut fields = value.split(';');
        let Some(action) = fields.next() else {
            return false;
        };
        let fields = fields.collect::<Vec<_>>();
        match action {
            "A" | "P" => self.update(|snapshot| snapshot.prompt_zone = PromptZone::Prompt),
            "B" | "I" => self.update(|snapshot| snapshot.prompt_zone = PromptZone::CommandInput),
            "C" => {
                self.command_started = Some(now);
                let line = option(&fields, "cmdline")
                    .and_then(percent_decode)
                    .unwrap_or_default();
                let line = sanitize_bounded(&line, MAX_COMMAND_CHARS);
                self.update(|snapshot| {
                    snapshot.prompt_zone = PromptZone::CommandOutput;
                    snapshot.command = Some(CommandMetadata {
                        line: Arc::from(line),
                        state: CommandState::Running,
                    });
                })
            }
            "D" => {
                let started = self.command_started.take().unwrap_or(self.epoch);
                let exit_status = fields
                    .first()
                    .and_then(|value| value.parse::<i32>().ok())
                    .or_else(|| option(&fields, "err").and_then(|value| value.parse().ok()));
                self.update(|snapshot| {
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
            _ => false,
        }
    }

    pub(crate) fn apply_progress_report(&mut self, state: u8, progress: Option<u8>) -> bool {
        let progress = progress.unwrap_or(0).min(100);
        let progress = match state {
            0 => ProgressMetadata::None,
            1 => ProgressMetadata::Normal(progress),
            2 => ProgressMetadata::Error(progress),
            3 => ProgressMetadata::Indeterminate,
            4 => ProgressMetadata::Paused(progress),
            _ => return false,
        };
        self.update(|snapshot| snapshot.progress = progress)
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

fn directory_basename(path: &str) -> Option<String> {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
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
    local_hostname: Option<&str>,
) -> Option<DirectoryMetadata> {
    let remainder = value
        .get(..7)?
        .eq_ignore_ascii_case("file://")
        .then(|| &value[7..])?;
    let slash = remainder.find('/')?;
    let (authority, path) = remainder.split_at(slash);
    let authority_is_local = authority.is_empty()
        || authority.eq_ignore_ascii_case("localhost")
        || local_hostname.is_some_and(|hostname| authority.eq_ignore_ascii_case(hostname));
    if !authority_is_local
        || !path.starts_with('/')
        || path.contains(['?', '#'])
        || path.chars().any(char::is_control)
    {
        return None;
    }

    let path = percent_decode(path)?;
    if path.is_empty() || path.chars().any(char::is_control) {
        return None;
    }
    Some(DirectoryMetadata {
        path: Arc::from(path),
        provenance: DirectoryProvenance::Osc7,
    })
}

pub(crate) fn local_hostname() -> Option<String> {
    let mut buffer = [0_u8; 256];
    // SAFETY: `buffer` is writable for its complete length and gethostname writes at most that
    // many bytes. A missing terminator is handled by using the full initialized buffer.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return None;
    }
    let length = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    std::str::from_utf8(&buffer[..length])
        .ok()
        .filter(|hostname| !hostname.is_empty() && !hostname.chars().any(char::is_control))
        .map(ToOwned::to_owned)
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
    use crate::domain::{RemoteWorkspaceDirectory, SshDestination};

    #[test]
    fn remote_metadata_context_should_preserve_typed_destination_and_directory() {
        let remote = RemoteTerminalMetadataContext::new(
            SshDestination::new("user@remote".to_owned()).unwrap(),
            RemoteWorkspaceDirectory::new("~/project".to_owned()).unwrap(),
        );
        let context = TerminalMetadataContext::Remote(remote.clone());

        let tracker = MetadataTracker::new_with_context(context, "Remote Project", Instant::now());
        let snapshot = tracker.snapshot();

        assert_eq!(snapshot.context.remote(), Some(&remote));
        assert_eq!(snapshot.directory.path.as_ref(), "~/project");
        assert_eq!(snapshot.title.value.as_ref(), "Remote Project");
        assert!(!snapshot.context.is_local());
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
        let local =
            parse_osc7_directory("FiLe://MAC.LOCAL/Users/me/My%20Project", Some("mac.local"))
                .expect("local OSC 7 should be accepted");
        assert_eq!(local.path.as_ref(), "/Users/me/My Project");
        assert_eq!(local.provenance, DirectoryProvenance::Osc7);

        assert!(parse_osc7_directory("file://remote.example/tmp", Some("mac.local")).is_none());
        assert!(parse_osc7_directory("file://localhost", Some("mac.local")).is_none());
        assert!(parse_osc7_directory("file:///tmp/%ZZ", Some("mac.local")).is_none());
        assert!(parse_osc7_directory("https://localhost/tmp", Some("mac.local")).is_none());
    }

    #[test]
    fn accepted_semantic_and_progress_events_update_metadata() {
        let epoch = Instant::now();
        let mut tracker = MetadataTracker::new("/tmp", "zsh", Some("mac.local"), epoch);

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

        assert!(tracker.apply_progress_report(1, Some(140)));
        assert!(tracker.apply_semantic_prompt("D;7", epoch + Duration::from_secs(5)));
        assert_eq!(tracker.snapshot().progress, ProgressMetadata::Normal(100));
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
    fn stale_transition_does_not_mutate_previously_published_metadata() {
        let epoch = Instant::now();
        let mut tracker = MetadataTracker::new("/tmp", "zsh", None, epoch);
        let live = tracker.snapshot();

        assert!(tracker.mark_stale());
        let stale = tracker.snapshot();

        assert_eq!(live.freshness, MetadataFreshness::Live);
        assert_eq!(stale.freshness, MetadataFreshness::Stale);
        assert_eq!(stale.revision, live.revision + 1);
        assert!(!Arc::ptr_eq(&live, &stale));
    }
}
