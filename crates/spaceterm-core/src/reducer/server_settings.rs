use super::contracts::*;
use super::state::{ReducerState, ReductionPatch};
use crate::operations::*;
use crate::organization::*;
use crate::organization_retained::*;

pub(crate) fn plan(
    state: &ReducerState,
    command: &Command,
) -> Result<ReductionPatch, ReducerFailure> {
    let mut patch = ReductionPatch::unchanged(state, Outcome::Applied);
    let mut org = state.organization().clone();
    match command {
        Command::SetWorktreeLocation {
            expected_version,
            template,
            ..
        } => {
            let field = &mut org.settings.worktree_location;
            if field.version != *expected_version {
                return Ok(ReductionPatch::unchanged(
                    state,
                    Outcome::Conflict(FieldConflict::WorktreeLocation {
                        server: org.server,
                        current_value: field.value.clone(),
                        current_version: field.version,
                    }),
                ));
            }
            let text = template.as_str().trim();
            if !valid_template(text, state.limits.template_bytes) {
                return Ok(ReductionPatch::rejected(
                    state,
                    OperationFailure::InvalidValue,
                ));
            }
            let value = WorktreeLocationTemplate::new(text.to_owned())
                .map_err(|_| ReducerFailure::InvalidFacts)?;
            if field.value == value {
                return Ok(patch);
            }
            field.value = value;
            field.version = field
                .version
                .checked_successor()
                .ok_or(ReducerFailure::CounterExhausted)?;
        }
        Command::SetRemoteAccess {
            expected_version,
            value,
            ..
        } => {
            let mut facts = org
                .retained
                .as_ref()
                .ok_or(ReducerFailure::InvalidFacts)?
                .parts()
                .clone();
            if facts.remote_access.version != *expected_version {
                return Ok(ReductionPatch::unchanged(
                    state,
                    Outcome::Conflict(FieldConflict::RemoteAccess {
                        server: org.server,
                        current_value: facts.remote_access.value.clone(),
                        current_version: facts.remote_access.version,
                    }),
                ));
            }
            if facts.remote_access.value == *value {
                return Ok(patch);
            }
            // The canonical constructor already enforces eight explicit endpoints.
            // This is policy publication only. Server-core authorizes listener effects.
            facts.remote_access.value = value.clone();
            facts.remote_access.version = facts
                .remote_access
                .version
                .checked_successor()
                .ok_or(ReducerFailure::CounterExhausted)?;
            org.retained = Some(
                OrganizationRetainedFacts::new(facts).map_err(|_| ReducerFailure::InvalidFacts)?,
            );
        }
        _ => return Err(ReducerFailure::WrongReducer),
    }
    patch
        .candidate
        .replace_organization(org, state.snapshot.runtime().to_vec())?;
    Ok(patch)
}

/// Portable grammar from the existing Worktree path-template policy. Expansion
/// and filesystem containment remain with the owning native Git adapter.
pub(crate) fn valid_template(template: &str, max: usize) -> bool {
    if template.is_empty() || template.len() > max || template.chars().any(char::is_control) {
        return false;
    }
    let rest = if let Some(rest) = template.strip_prefix('~') {
        if !rest.starts_with('/') {
            return false;
        }
        rest
    } else if template.starts_with('/') {
        template
    } else {
        return false;
    };
    if rest.split('/').any(|v| v == "..") {
        return false;
    }
    let mut remaining = rest;
    let mut branch = false;
    while let Some(open) = remaining.find('{') {
        let after = &remaining[open + 1..];
        let Some(close) = after.find('}') else {
            return false;
        };
        match &after[..close] {
            "repository" => {}
            "branch" => branch = true,
            _ => return false,
        }
        remaining = &after[close + 1..];
    }
    branch
}
