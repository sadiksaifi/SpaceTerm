//! Decode server presentation facts using locality supplied by the client's route.
use super::{metadata::*, title::TitleGlyph};
use crate::domain::{RemoteDirectory, SshDestination};
use spaceterm_server::{ServerError, pane::metadata as wire};
use std::{sync::Arc, time::Duration};

pub(crate) fn decode(
    wire: wire::Snapshot,
    local_files: Option<&crate::platform::local_filesystem::LocalFilesystemAuthority>,
    remote: Option<&RemoteTerminalMetadataContext>,
) -> Result<Arc<TerminalMetadataSnapshot>, ServerError> {
    wire.validate()?;
    // Only constructor-injected local authority selects a Local context. The wire has no such bit.
    let context = if local_files.is_some() {
        TerminalMetadataContext::local(
            crate::local_path::LocalPathSemantics::Posix,
            &wire.directory,
            LocalMachine::new(
                wire.user.as_deref(),
                wire.host.as_deref(),
                wire.home.as_deref(),
            ),
        )
    } else {
        let destination = remote
            .map(|r| r.destination().clone())
            .or_else(|| {
                wire.host
                    .as_ref()
                    .and_then(|host| SshDestination::new(host.clone()).ok())
            })
            .unwrap_or(SshDestination::new("server".into()).map_err(|_| ServerError::Protocol)?);
        let user = remote
            .and_then(|context| {
                context
                    .destination()
                    .as_str()
                    .rsplit_once('@')
                    .map(|(user, _)| user)
            })
            .or(wire.user.as_deref());
        TerminalMetadataContext::Remote(
            RemoteTerminalMetadataContext::new(
                destination,
                RemoteDirectory::new(wire.directory.clone())
                    .unwrap_or_else(|_| RemoteDirectory::home()),
            )
            .with_machine(RemoteMachine::new(user, wire.home.as_deref())),
        )
    };
    Ok(Arc::new(TerminalMetadataSnapshot {
        revision: wire.revision,
        context,
        freshness: match wire.freshness {
            wire::Freshness::Live => MetadataFreshness::Live,
            wire::Freshness::Stale => MetadataFreshness::Stale,
        },
        title: TitleMetadata {
            value: wire.title.into(),
            provenance: match wire.title_provenance {
                wire::TitleProvenance::Fallback => TitleProvenance::Fallback,
                wire::TitleProvenance::WorkingDirectory => TitleProvenance::WorkingDirectory,
                wire::TitleProvenance::TerminalControl => TitleProvenance::TerminalControl,
            },
        },
        directory: DirectoryMetadata {
            path: Arc::from(wire.directory.as_str()),
            provenance: match wire.directory_provenance {
                wire::DirectoryProvenance::Initial => DirectoryProvenance::Initial,
                wire::DirectoryProvenance::Osc7 => DirectoryProvenance::Osc7,
            },
        },
        // The desktop's presentation cache carries no restoration or owning-shell authority.
        repository_directory: wire.directory.into(),
        confirmed_directory: None,
        finished_commands: wire.finished_commands,
        prompt_zone: match wire.prompt_zone {
            wire::PromptZone::Unknown => PromptZone::Unknown,
            wire::PromptZone::Prompt => PromptZone::Prompt,
            wire::PromptZone::CommandInput => PromptZone::CommandInput,
            wire::PromptZone::CommandOutput => PromptZone::CommandOutput,
        },
        command: wire.command.map(|c| CommandMetadata {
            line: c.line.into(),
            state: match c.state {
                wire::CommandState::Running => CommandState::Running,
                wire::CommandState::Finished {
                    exit_status,
                    duration_ms,
                } => CommandState::Finished {
                    exit_status,
                    duration: Duration::from_millis(duration_ms),
                },
            },
        }),
        title_activity: wire.title_activity,
        title_glyph: match wire.title_glyph {
            wire::TitleGlyph::Reported => TitleGlyph::Reported,
            wire::TitleGlyph::Retained(g) => TitleGlyph::Retained(g.map(Arc::from)),
        },
        progress: match wire.progress {
            wire::Progress::None => ProgressMetadata::None,
            wire::Progress::Normal(n) => ProgressMetadata::Normal(n),
            wire::Progress::Error(n) => ProgressMetadata::Error(n),
            wire::Progress::Indeterminate => ProgressMetadata::Indeterminate,
            wire::Progress::Paused(n) => ProgressMetadata::Paused(n),
        },
    }))
}
