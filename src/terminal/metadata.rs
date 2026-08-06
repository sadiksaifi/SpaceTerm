use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_TITLE_CHARS: usize = 256;
const MAX_COMMAND_CHARS: usize = 4096;
const MAX_OSC_BYTES: usize = 8192;

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
pub(crate) struct TerminalMetadataSnapshot {
    pub(crate) revision: u64,
    pub(crate) freshness: MetadataFreshness,
    pub(crate) title: TitleMetadata,
    pub(crate) directory: DirectoryMetadata,
    pub(crate) prompt_zone: PromptZone,
    pub(crate) command: Option<CommandMetadata>,
    pub(crate) progress: ProgressMetadata,
}

pub(crate) struct MetadataTracker {
    snapshot: Arc<TerminalMetadataSnapshot>,
    local_hostname: Option<Arc<str>>,
    epoch: Instant,
    command_started: Option<Instant>,
    ground_escape: bool,
    osc_buffer: Option<Vec<u8>>,
    osc_escape: bool,
    osc_overflow: bool,
}

impl MetadataTracker {
    pub(crate) fn new(
        initial_directory: &str,
        fallback_title: &str,
        local_hostname: Option<&str>,
        epoch: Instant,
    ) -> Self {
        Self {
            snapshot: Arc::new(TerminalMetadataSnapshot {
                revision: 0,
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
            local_hostname: local_hostname.map(Arc::from),
            epoch,
            command_started: None,
            ground_escape: false,
            osc_buffer: None,
            osc_escape: false,
            osc_overflow: false,
        }
    }

    pub(crate) fn feed(&mut self, bytes: &[u8], now: Instant) -> bool {
        let mut changed = false;
        for &byte in bytes {
            if self.osc_buffer.is_some() || self.osc_overflow {
                if self.osc_escape {
                    self.osc_escape = false;
                    if byte == b'\\' {
                        if !self.osc_overflow {
                            let payload = self.osc_buffer.take().unwrap_or_default();
                            changed |= self.apply_osc(&payload, now);
                        }
                        self.reset_osc();
                        continue;
                    }
                    if !self.push_osc(0x1b) || !self.push_osc(byte) {
                        self.enter_overflow();
                    }
                    continue;
                }
                match byte {
                    0x07 | 0x9c => {
                        if !self.osc_overflow {
                            let payload = self.osc_buffer.take().unwrap_or_default();
                            changed |= self.apply_osc(&payload, now);
                        }
                        self.reset_osc();
                    }
                    0x1b => self.osc_escape = true,
                    _ if !self.push_osc(byte) => self.enter_overflow(),
                    _ => {}
                }
                continue;
            }

            if self.ground_escape {
                self.ground_escape = false;
                if byte == b']' {
                    self.osc_buffer = Some(Vec::new());
                } else if byte == 0x1b {
                    self.ground_escape = true;
                }
            } else if byte == 0x1b {
                self.ground_escape = true;
            } else if byte == 0x9d {
                self.osc_buffer = Some(Vec::new());
            }
        }
        changed
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
        let Some(directory) = parse_osc7_directory(value, self.local_hostname.as_deref()) else {
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

    fn push_osc(&mut self, byte: u8) -> bool {
        let Some(buffer) = self.osc_buffer.as_mut() else {
            return false;
        };
        if buffer.len() >= MAX_OSC_BYTES {
            return false;
        }
        buffer.push(byte);
        true
    }

    fn enter_overflow(&mut self) {
        self.osc_buffer = None;
        self.osc_overflow = true;
    }

    fn reset_osc(&mut self) {
        self.osc_buffer = None;
        self.osc_escape = false;
        self.osc_overflow = false;
    }

    fn apply_osc(&mut self, payload: &[u8], now: Instant) -> bool {
        let Ok(payload) = std::str::from_utf8(payload) else {
            return false;
        };
        let Some((kind, value)) = payload.split_once(';') else {
            return false;
        };
        match kind {
            "0" | "2" => self.set_reported_title(value),
            "7" => self.set_reported_directory(value),
            "9" => self.apply_progress(value),
            "133" => self.apply_semantic_prompt(value, now),
            _ => false,
        }
    }

    fn apply_semantic_prompt(&mut self, value: &str, now: Instant) -> bool {
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

    fn apply_progress(&mut self, value: &str) -> bool {
        let mut fields = value.split(';');
        if fields.next() != Some("4") {
            return false;
        }
        let Some(state) = fields.next().and_then(|value| value.parse::<u8>().ok()) else {
            return false;
        };
        let progress = fields
            .next()
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or(0)
            .min(100);
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
    let remainder = value.strip_prefix("file://")?;
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
            parse_osc7_directory("file://mac.local/Users/me/My%20Project", Some("mac.local"))
                .expect("local OSC 7 should be accepted");
        assert_eq!(local.path.as_ref(), "/Users/me/My Project");
        assert_eq!(local.provenance, DirectoryProvenance::Osc7);

        assert!(parse_osc7_directory("file://remote.example/tmp", Some("mac.local")).is_none());
        assert!(parse_osc7_directory("file://localhost", Some("mac.local")).is_none());
        assert!(parse_osc7_directory("file:///tmp/%ZZ", Some("mac.local")).is_none());
        assert!(parse_osc7_directory("https://localhost/tmp", Some("mac.local")).is_none());
    }

    #[test]
    fn osc133_and_progress_are_parsed_across_chunks_with_injected_time() {
        let epoch = Instant::now();
        let mut tracker = MetadataTracker::new("/tmp", "zsh", Some("mac.local"), epoch);

        assert!(!tracker.feed(b"ordinary output", epoch));
        assert!(!tracker.feed(
            b"\x1b]133;C;cmdline=cargo%20test\x1b",
            epoch + Duration::from_secs(2)
        ));
        assert!(tracker.feed(b"\\", epoch + Duration::from_secs(2)));
        assert_eq!(tracker.snapshot().prompt_zone, PromptZone::CommandOutput);
        assert_eq!(
            tracker.snapshot().command,
            Some(CommandMetadata {
                line: Arc::from("cargo test"),
                state: CommandState::Running,
            })
        );

        assert!(tracker.feed(
            b"\x1b]9;4;1;140\x07\x1b]133;D;7\x07",
            epoch + Duration::from_secs(5),
        ));
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
    fn invalid_or_oversized_sequences_cannot_replace_last_valid_metadata() {
        let epoch = Instant::now();
        let mut tracker = MetadataTracker::new("/tmp", "zsh", Some("mac.local"), epoch);
        assert!(tracker.feed(b"\x1b]7;file:///Users/me\x07", epoch));
        let valid = tracker.snapshot();

        assert!(!tracker.feed(b"\x1b]7;file://remote.example/private\x07", epoch));
        let oversized = format!("\x1b]133;C;cmdline={}\x07", "x".repeat(MAX_OSC_BYTES + 1));
        assert!(!tracker.feed(oversized.as_bytes(), epoch));
        assert!(Arc::ptr_eq(&valid, &tracker.snapshot()));

        assert!(tracker.feed(b"\x1b]9;4;4;25\x07", epoch));
        assert_eq!(tracker.snapshot().progress, ProgressMetadata::Paused(25));
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
