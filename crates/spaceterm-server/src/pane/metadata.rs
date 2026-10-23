//! Bounded presentation facts. These strings never grant filesystem or shell authority.
use crate::ServerError;
use serde::{Deserialize, Serialize};

pub const MAX_TITLE_CHARS: usize = 256;
pub const MAX_COMMAND_CHARS: usize = 4096;
pub const MAX_DIRECTORY_CHARS: usize = 4096;
pub const MAX_MACHINE_CHARS: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub revision: u64,
    pub shell_name: String,
    pub title: String,
    pub title_provenance: TitleProvenance,
    pub directory: String,
    pub directory_provenance: DirectoryProvenance,
    pub user: Option<String>,
    pub host: Option<String>,
    pub home: Option<String>,
    pub freshness: Freshness,
    pub finished_commands: u64,
    pub prompt_zone: PromptZone,
    pub command: Option<Command>,
    pub title_activity: bool,
    pub title_glyph: TitleGlyph,
    pub progress: Progress,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TitleProvenance {
    Fallback,
    WorkingDirectory,
    TerminalControl,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DirectoryProvenance {
    Initial,
    Osc7,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Freshness {
    Live,
    Stale,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PromptZone {
    Unknown,
    Prompt,
    CommandInput,
    CommandOutput,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub line: String,
    pub state: CommandState,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CommandState {
    Running,
    Finished {
        exit_status: Option<i32>,
        duration_ms: u64,
    },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TitleGlyph {
    Reported,
    Retained(Option<String>),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Progress {
    None,
    Normal(u8),
    Error(u8),
    Indeterminate,
    Paused(u8),
}

fn presentable(character: char) -> bool {
    !character.is_control() && !super::control::invisible(character)
}
pub fn sanitize(value: &str, limit: usize) -> String {
    value
        .chars()
        .filter(|c| presentable(*c))
        .take(limit)
        .collect()
}
impl Snapshot {
    /// Reject malformed peers before any presentation string enters the client cache.
    pub fn validate(&self) -> Result<(), ServerError> {
        let valid =
            |value: &str, limit| value.chars().count() <= limit && value.chars().all(presentable);
        let optional =
            |value: &Option<String>, limit| value.as_deref().is_none_or(|s| valid(s, limit));
        if !valid(&self.title, MAX_TITLE_CHARS)
            || !valid(&self.shell_name, MAX_TITLE_CHARS)
            || !valid(&self.directory, MAX_DIRECTORY_CHARS)
            || !optional(&self.user, MAX_MACHINE_CHARS)
            || !optional(&self.host, MAX_MACHINE_CHARS)
            || !optional(&self.home, MAX_DIRECTORY_CHARS)
            || self
                .command
                .as_ref()
                .is_some_and(|c| !valid(&c.line, MAX_COMMAND_CHARS))
            || matches!(&self.title_glyph, TitleGlyph::Retained(Some(s)) if !valid(s, MAX_TITLE_CHARS))
            || matches!(self.progress, Progress::Normal(n) | Progress::Error(n) | Progress::Paused(n) if n > 100)
        {
            return Err(ServerError::Protocol);
        }
        Ok(())
    }
}
