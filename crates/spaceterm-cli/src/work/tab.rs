use super::*;
use crate::scope::{self, Effect, ScopeArgs, Want};
use spaceterm_server::{hierarchy::CloseTarget, organize};

pub fn create(
    snapshot: &Snapshot,
    args: &ScopeArgs,
    current: Option<&Current>,
    control: CreateControl,
) -> Result<Planned, WorkError> {
    let mut target = scope::resolve(snapshot, args, current, Want::Workspace, Effect::Mutating)?;
    let workspace = target.workspace.ok_or(WorkError::Code(Code::NotFound))?;
    let plan = organize::new_tab(
        &snapshot.inventory,
        workspace,
        target.pane,
        target.worktree.as_deref(),
        control,
    )
    .map_err(|e| WorkError::Code(organize_error(e)))?;
    let mutation = match plan {
        organize::NewTab::Mutation(m) => m,
        organize::NewTab::Worktree(op) => Mutation::Worktree(op),
    };
    let source = match &mutation {
        Mutation::CreateTab { source, .. }
        | Mutation::Worktree(spaceterm_server::worktree::Operation::OpenTab { source, .. }) => {
            *source
        }
        _ => None,
    };
    if let Some(source) = source {
        root_source(snapshot, &mut target, source);
    }
    Ok(Planned { target, mutation })
}

pub struct Close {
    pub target: Resolved,
    pub close: CloseTarget,
    /// Dispatch and confirm this accepted fallback before obtaining fresh close consent.
    pub fallback: Option<Mutation>,
}

pub fn close(
    snapshot: &Snapshot,
    args: &ScopeArgs,
    current: Option<&Current>,
    all: bool,
) -> Result<Close, WorkError> {
    let target = scope::resolve(
        snapshot,
        args,
        current,
        if all { Want::Worktree } else { Want::Tab },
        Effect::Confirmed,
    )?;
    let workspace = target.workspace.ok_or(WorkError::Code(Code::NotFound))?;
    if !all {
        let tab = target.tab.ok_or(WorkError::Code(Code::NotFound))?;
        return Ok(Close {
            target,
            close: CloseTarget::Tab { workspace, tab },
            fallback: None,
        });
    }
    let tab = snapshot
        .inventory
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .and_then(|w| w.tabs.iter().find(|t| t.worktree == target.worktree))
        .ok_or(WorkError::Code(Code::NotFound))?
        .id;
    let repos = snapshot
        .repositories
        .as_ref()
        .or(snapshot.inventory.repositories.as_ref())
        .ok_or(WorkError::Code(Code::NoFallbackWorktree))?;
    let plan = organize::worktree_close_plan(&snapshot.inventory, repos, workspace, tab)
        .map_err(|e| WorkError::Code(organize_error(e)))?;
    Ok(Close {
        target,
        close: CloseTarget::Worktree { workspace, tab },
        fallback: plan.fallback.map(Mutation::Worktree),
    })
}
