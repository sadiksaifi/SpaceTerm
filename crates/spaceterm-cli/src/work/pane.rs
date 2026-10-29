use super::*;
use crate::scope::{self, Effect, ScopeArgs, Want};
use spaceterm_server::{
    hierarchy::{Axis, CloseTarget},
    organize,
};

pub fn split(
    snapshot: &Snapshot,
    args: &ScopeArgs,
    current: Option<&Current>,
    axis: Axis,
    control: CreateControl,
) -> Result<Planned, WorkError> {
    let mut target = scope::resolve(snapshot, args, current, Want::Tab, Effect::Mutating)?;
    let tab = target.tab.ok_or(WorkError::Code(Code::NotFound))?;
    let mutation = organize::split(&snapshot.inventory, tab, target.pane, axis, control)
        .map_err(|e| WorkError::Code(organize_error(e)))?;
    if let Mutation::SplitPane { source, .. } = &mutation {
        root_source(snapshot, &mut target, *source);
    }
    Ok(Planned { target, mutation })
}

pub fn close(
    snapshot: &Snapshot,
    args: &ScopeArgs,
    current: Option<&Current>,
) -> Result<(Resolved, CloseTarget), WorkError> {
    let target = scope::resolve(snapshot, args, current, Want::Pane, Effect::Confirmed)?;
    let workspace = target.workspace.ok_or(WorkError::Code(Code::NotFound))?;
    let tab = target.tab.ok_or(WorkError::Code(Code::NotFound))?;
    let pane = target.pane.ok_or(WorkError::Code(Code::NotFound))?;
    Ok((
        target,
        CloseTarget::Pane {
            workspace,
            tab,
            pane,
        },
    ))
}
