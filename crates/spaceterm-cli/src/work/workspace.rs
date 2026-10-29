use super::*;
use crate::scope::{self, Effect, ScopeArgs, TargetError, Want};
use std::path::PathBuf;

pub fn create(
    name: String,
    directory: Option<PathBuf>,
    control: CreateControl,
) -> Result<Mutation, Code> {
    if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err(Code::InvalidArguments);
    }
    Ok(Mutation::CreateWorkspace {
        name,
        directory,
        control,
    })
}

pub fn rename(
    snapshot: &Snapshot,
    args: &ScopeArgs,
    current: Option<&Current>,
    name: String,
) -> Result<Planned, WorkError> {
    if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err(WorkError::Code(Code::InvalidArguments));
    }
    let target = scope::resolve(snapshot, args, current, Want::Workspace, Effect::Mutating)?;
    let workspace = target.workspace.ok_or(WorkError::Code(Code::NotFound))?;
    let version = snapshot
        .inventory
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .ok_or(WorkError::Code(Code::NotFound))?
        .name_version;
    Ok(Planned {
        target,
        mutation: Mutation::RenameWorkspace {
            workspace,
            version,
            name,
        },
    })
}

pub fn kill(
    snapshot: &Snapshot,
    args: &ScopeArgs,
    current: Option<&Current>,
) -> Result<(Resolved, spaceterm_server::hierarchy::CloseTarget), TargetError> {
    let target = scope::resolve(snapshot, args, current, Want::Workspace, Effect::Confirmed)?;
    let workspace = target.workspace.ok_or(TargetError::NotFound {
        level: crate::contract::Level::Workspace,
    })?;
    Ok((
        target,
        spaceterm_server::hierarchy::CloseTarget::Workspace { workspace },
    ))
}
