use crate::terminal::{metadata as local, title::TitleGlyph};
use spaceterm_server::pane::metadata as wire;

pub(super) fn encode(snapshot: &local::TerminalMetadataSnapshot) -> wire::Snapshot {
    let (user, host) = match snapshot.context.origin() {
        local::TerminalOrigin::Local { user, host } => (user, host),
        local::TerminalOrigin::Remote { user, host } => (user, Some(host)),
    };
    let clean = wire::sanitize;
    wire::Snapshot {
        revision: snapshot.revision,
        title: clean(&snapshot.title.value, wire::MAX_TITLE_CHARS),
        title_provenance: match snapshot.title.provenance {
            local::TitleProvenance::Fallback => wire::TitleProvenance::Fallback,
            local::TitleProvenance::WorkingDirectory => wire::TitleProvenance::WorkingDirectory,
            local::TitleProvenance::TerminalControl => wire::TitleProvenance::TerminalControl,
        },
        directory: clean(&snapshot.directory.path, wire::MAX_DIRECTORY_CHARS),
        directory_provenance: match snapshot.directory.provenance {
            local::DirectoryProvenance::Initial => wire::DirectoryProvenance::Initial,
            local::DirectoryProvenance::Osc7 => wire::DirectoryProvenance::Osc7,
        },
        user: user.map(|s| clean(s, wire::MAX_MACHINE_CHARS)),
        host: host.map(|s| clean(s, wire::MAX_MACHINE_CHARS)),
        home: snapshot
            .context
            .home()
            .map(|s| clean(s, wire::MAX_DIRECTORY_CHARS)),
        freshness: match snapshot.freshness {
            local::MetadataFreshness::Live => wire::Freshness::Live,
            local::MetadataFreshness::Stale => wire::Freshness::Stale,
        },
        finished_commands: snapshot.finished_commands,
        prompt_zone: match snapshot.prompt_zone {
            local::PromptZone::Unknown => wire::PromptZone::Unknown,
            local::PromptZone::Prompt => wire::PromptZone::Prompt,
            local::PromptZone::CommandInput => wire::PromptZone::CommandInput,
            local::PromptZone::CommandOutput => wire::PromptZone::CommandOutput,
        },
        command: snapshot.command.as_ref().map(|c| wire::Command {
            line: clean(&c.line, wire::MAX_COMMAND_CHARS),
            state: match c.state {
                local::CommandState::Running => wire::CommandState::Running,
                local::CommandState::Finished {
                    exit_status,
                    duration,
                } => wire::CommandState::Finished {
                    exit_status,
                    duration_ms: duration.as_millis().min(u128::from(u64::MAX)) as u64,
                },
            },
        }),
        title_activity: snapshot.title_activity,
        title_glyph: match &snapshot.title_glyph {
            TitleGlyph::Reported => wire::TitleGlyph::Reported,
            TitleGlyph::Retained(glyph) => {
                wire::TitleGlyph::Retained(glyph.as_ref().map(|s| clean(s, wire::MAX_TITLE_CHARS)))
            }
        },
        progress: match snapshot.progress {
            local::ProgressMetadata::None => wire::Progress::None,
            local::ProgressMetadata::Normal(n) => wire::Progress::Normal(n.min(100)),
            local::ProgressMetadata::Error(n) => wire::Progress::Error(n.min(100)),
            local::ProgressMetadata::Indeterminate => wire::Progress::Indeterminate,
            local::ProgressMetadata::Paused(n) => wire::Progress::Paused(n.min(100)),
        },
    }
}
