use super::contracts::*;
use super::state::{ReducerState, ReductionPatch};
use crate::operations::*;
use crate::organization::*;

pub(crate) fn plan(
    state: &ReducerState,
    command: &Command,
    context: &ReductionContext,
) -> Result<ReductionPatch, ReducerFailure> {
    let mut patch = ReductionPatch::unchanged(state, Outcome::Applied);
    let mut org = state.organization().clone();
    let mut runtime = state.snapshot.runtime().to_vec();
    match command {
        Command::RenameWorkspace {
            workspace,
            expected_version,
            draft,
        } => {
            let Some(record) = org.workspaces.iter_mut().find(|v| v.id == *workspace) else {
                return Ok(ReductionPatch::rejected(
                    state,
                    OperationFailure::TargetDeleted,
                ));
            };
            if record.name.version != *expected_version {
                return Ok(ReductionPatch::unchanged(
                    state,
                    Outcome::Conflict(FieldConflict::WorkspaceName {
                        workspace: *workspace,
                        current_value: record.name.value.clone(),
                        current_version: record.name.version,
                    }),
                ));
            }
            let replacement = match draft {
                NameDraft::Blank => return Ok(patch),
                NameDraft::Custom(value) => {
                    let text = value.as_str().trim();
                    if text.is_empty() {
                        return Ok(patch);
                    }
                    if text.len() > state.limits.name_bytes {
                        return Ok(ReductionPatch::rejected(
                            state,
                            OperationFailure::InvalidValue,
                        ));
                    }
                    WorkspaceName {
                        mode: NameMode::Custom,
                        display: NameText::new(text.to_owned())
                            .map_err(|_| ReducerFailure::InvalidFacts)?,
                    }
                }
                NameDraft::Automatic => {
                    let facts = org.retained.as_ref().ok_or(ReducerFailure::InvalidFacts)?;
                    let name = facts
                        .workspace_names()
                        .iter()
                        .find(|v| v.workspace() == *workspace)
                        .ok_or(ReducerFailure::InvalidFacts)?;
                    WorkspaceName {
                        mode: NameMode::Automatic,
                        display: name.assigned_automatic().clone(),
                    }
                }
            };
            if record.name.value == replacement {
                return Ok(patch);
            }
            record.name.value = replacement;
            super::naming::refresh_root_names(state, &mut org)?;
            // One accepted name edit advances its own version once, even if the
            // chosen automatic assignment also changes during collision resolution.
            let old_version = state
                .organization()
                .workspaces
                .iter()
                .find(|v| v.id == *workspace)
                .ok_or(ReducerFailure::InvalidFacts)?
                .name
                .version;
            org.workspaces
                .iter_mut()
                .find(|v| v.id == *workspace)
                .ok_or(ReducerFailure::InvalidFacts)?
                .name
                .version = old_version
                .checked_successor()
                .ok_or(ReducerFailure::CounterExhausted)?;
        }
        Command::SetPinnedDirectory {
            workspace,
            expected_version,
            value,
        } => {
            let Some(record) = org.workspaces.iter_mut().find(|v| v.id == *workspace) else {
                return Ok(ReductionPatch::rejected(
                    state,
                    OperationFailure::TargetDeleted,
                ));
            };
            if record.pinned_directory.version != *expected_version {
                return Ok(ReductionPatch::unchanged(
                    state,
                    Outcome::Conflict(FieldConflict::PinnedDirectory {
                        workspace: *workspace,
                        current_value: record.pinned_directory.value.clone(),
                        current_version: record.pinned_directory.version,
                    }),
                ));
            }
            if record.pinned_directory.value == *value {
                return Ok(patch);
            }
            if let Some(value) = value {
                let validation = match super::creation::validation(state, command, context) {
                    Ok(v) => v,
                    Err(e) => return Ok(ReductionPatch::rejected(state, e)),
                };
                if !validation
                    .requested
                    .as_ref()
                    .is_some_and(|v| v.available && v.directory == *value)
                {
                    return Ok(ReductionPatch::rejected(
                        state,
                        OperationFailure::DirectoryUnavailable,
                    ));
                }
            }
            record.pinned_directory.value = value.clone();
            record.pinned_directory.version = record
                .pinned_directory
                .version
                .checked_successor()
                .ok_or(ReducerFailure::CounterExhausted)?;
            super::naming::refresh_root_names(state, &mut org)?;
        }
        Command::RepairPaneDirectory {
            pane,
            expected_version,
            value,
        } => {
            let Some(record) = org.panes.iter_mut().find(|v| v.id == *pane) else {
                return Ok(ReductionPatch::rejected(
                    state,
                    OperationFailure::TargetDeleted,
                ));
            };
            if record.confirmed_restoration_directory.version != *expected_version {
                return Ok(ReductionPatch::unchanged(
                    state,
                    Outcome::Conflict(FieldConflict::PaneDirectory {
                        pane: *pane,
                        current_value: record.restoration_directory().clone(),
                        current_version: record.confirmed_restoration_directory.version,
                    }),
                ));
            }
            if matches!(
                state.runtime(*pane)?.phase,
                SessionPhase::Starting { .. } | SessionPhase::Running { .. }
            ) || super::lifecycle::pane_pending(state, *pane)
            {
                return Ok(ReductionPatch::rejected(
                    state,
                    OperationFailure::BusyRetiring,
                ));
            }
            let validation = match super::creation::validation(state, command, context) {
                Ok(v) => v,
                Err(e) => return Ok(ReductionPatch::rejected(state, e)),
            };
            if !validation
                .requested
                .as_ref()
                .is_some_and(|v| v.available && v.directory == *value)
            {
                return Ok(ReductionPatch::rejected(
                    state,
                    OperationFailure::DirectoryUnavailable,
                ));
            }
            let unchanged = record.starting_directory == *value
                && record.confirmed_restoration_directory.value.is_none();
            let current = runtime
                .iter_mut()
                .find(|v| v.pane == *pane)
                .ok_or(ReducerFailure::InvalidFacts)?;
            let unavailable = current.phase == SessionPhase::DirectoryUnavailable;
            if unchanged && !unavailable {
                return Ok(patch);
            }
            if !unchanged {
                record.starting_directory = value.clone();
                record.confirmed_restoration_directory.value = None;
                record.confirmed_restoration_directory.version = record
                    .confirmed_restoration_directory
                    .version
                    .checked_successor()
                    .ok_or(ReducerFailure::CounterExhausted)?;
            }
            if unavailable {
                current.phase = SessionPhase::NotStarted;
            }
            // Directory repair is not display metadata and cannot repin or rename
            // a Workspace, reassign Worktree membership, or launch a shell.
        }
        _ => return Err(ReducerFailure::WrongReducer),
    }
    patch.candidate.replace_organization(org, runtime)?;
    Ok(patch)
}
