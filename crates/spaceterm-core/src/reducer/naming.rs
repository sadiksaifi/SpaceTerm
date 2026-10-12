use super::contracts::ReducerFailure;
use super::state::ReducerState;
use crate::identity::*;
use crate::organization::*;
use crate::organization_retained::*;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn directory_name(
    directory: &ServerDirectory,
    home: &ServerDirectory,
    max: usize,
) -> Result<NameText, ReducerFailure> {
    let text = if directory == home {
        "Default".to_owned()
    } else {
        let component = directory
            .bytes()
            .rsplit(|b| *b == b'/')
            .find(|v| !v.is_empty());
        match component {
            Some(value) => String::from_utf8_lossy(value)
                .chars()
                .filter(|c| !c.is_control())
                .collect::<String>(),
            None => "/".to_owned(),
        }
    };
    // Directory display is presentation, not filesystem validation. Oversized
    // names reject atomically instead of slicing UTF-8 or silently truncating.
    if text.len() > max || text.trim().is_empty() {
        return Err(ReducerFailure::QuotaExceeded);
    }
    NameText::new(text).map_err(|_| ReducerFailure::InvalidFacts)
}

pub(crate) fn refresh_root_names(
    state: &ReducerState,
    org: &mut OrganizationState,
) -> Result<(), ReducerFailure> {
    let old = state.organization();
    let changed: Vec<_> = org
        .workspaces
        .iter()
        .filter(|workspace| {
            let previous = old.workspaces.iter().find(|v| v.id == workspace.id);
            previous.is_none_or(|previous| {
                previous.pinned_directory.value != workspace.pinned_directory.value
                    || previous.root_tab != workspace.root_tab
                    || old
                        .tabs
                        .iter()
                        .find(|v| v.id == previous.root_tab)
                        .map(|v| v.root_pane)
                        != org
                            .tabs
                            .iter()
                            .find(|v| v.id == workspace.root_tab)
                            .map(|v| v.root_pane)
            })
        })
        .map(|v| v.id)
        .collect();
    refresh_workspace_names(state, org, &changed)
}

/// Derive bases only for explicit naming triggers. Lifecycle calls this for an
/// admitted Root display-directory change; repair and unrelated edits retain bases.
pub(crate) fn refresh_workspace_names(
    state: &ReducerState,
    org: &mut OrganizationState,
    changed: &[WorkspaceId],
) -> Result<(), ReducerFailure> {
    let mut bases = BTreeMap::new();
    for workspace in org.workspaces.iter().filter(|v| changed.contains(&v.id)) {
        let tab = org
            .tabs
            .iter()
            .find(|v| v.id == workspace.root_tab)
            .ok_or(ReducerFailure::InvalidFacts)?;
        let pane = org
            .panes
            .iter()
            .find(|v| v.id == tab.root_pane)
            .ok_or(ReducerFailure::InvalidFacts)?;
        let display = state
            .observations
            .get(&pane.id)
            .and_then(|v| v.display.as_ref())
            .filter(|v| v.presentation() == super::DirectoryPresentation::OwningServer);
        let directory = workspace
            .pinned_directory
            .value
            .as_ref()
            .or_else(|| display.map(|v| v.directory()))
            .unwrap_or(pane.restoration_directory());
        bases.insert(
            workspace.id,
            directory_name(directory, &state.server_home, state.limits.name_bytes)?,
        );
    }
    recalculate(state, org, &bases, state.limits.name_bytes)
}

fn recalculate(
    state: &ReducerState,
    org: &mut OrganizationState,
    bases: &BTreeMap<WorkspaceId, NameText>,
    max: usize,
) -> Result<(), ReducerFailure> {
    let mut facts = org
        .retained
        .as_ref()
        .ok_or(ReducerFailure::InvalidFacts)?
        .parts()
        .clone();
    let previous: BTreeMap<_, _> = facts
        .workspace_names
        .iter()
        .map(|v| (v.workspace(), v.clone()))
        .collect();
    let mut occupied: BTreeSet<String> = org
        .workspaces
        .iter()
        .filter(|v| v.name.value.mode == NameMode::Custom)
        .map(|v| v.name.value.display.as_str().to_owned())
        .collect();
    let mut pending = Vec::new();
    let mut assigned = BTreeMap::new();
    let mut ordered: Vec<_> = org.workspaces.iter().collect();
    ordered.sort_by_key(|v| v.creation_ordinal);
    for workspace in ordered {
        let old = previous
            .get(&workspace.id)
            .ok_or(ReducerFailure::InvalidFacts)?;
        let base = bases.get(&workspace.id).unwrap_or(old.automatic_base());
        if workspace.name.value.mode == NameMode::Custom {
            // Dormant assignments occupy no display-name slot. A changed base
            // restarts at the base; selecting Automatic resolves current collisions.
            let automatic = if old.automatic_base() == base {
                old.assigned_automatic()
            } else {
                base
            };
            assigned.insert(workspace.id, automatic.clone());
        } else if state
            .organization()
            .workspaces
            .iter()
            .any(|v| v.id == workspace.id && v.name.value.mode == NameMode::Automatic)
            && old.automatic_base() == base
            && occupied.insert(old.assigned_automatic().as_str().to_owned())
        {
            assigned.insert(workspace.id, old.assigned_automatic().clone());
        } else {
            pending.push((workspace.id, base.clone()));
        }
    }
    for (id, base) in pending {
        let mut value = base.as_str().to_owned();
        let mut suffix = 2u64;
        while occupied.contains(&value) {
            value = format!("{} ({suffix})", base.as_str());
            if value.len() > max {
                return Err(ReducerFailure::QuotaExceeded);
            }
            suffix = suffix
                .checked_add(1)
                .ok_or(ReducerFailure::CounterExhausted)?;
        }
        occupied.insert(value.clone());
        assigned.insert(
            id,
            NameText::new(value).map_err(|_| ReducerFailure::QuotaExceeded)?,
        );
    }
    facts.workspace_names.clear();
    for workspace in &mut org.workspaces {
        let old = previous
            .get(&workspace.id)
            .ok_or(ReducerFailure::InvalidFacts)?;
        let base = bases
            .get(&workspace.id)
            .unwrap_or(old.automatic_base())
            .clone();
        let automatic = assigned
            .remove(&workspace.id)
            .ok_or(ReducerFailure::InvalidFacts)?;
        if workspace.name.value.mode == NameMode::Automatic
            && workspace.name.value.display != automatic
        {
            workspace.name.value.display = automatic.clone();
            workspace.name.version = workspace
                .name
                .version
                .checked_successor()
                .ok_or(ReducerFailure::CounterExhausted)?;
        }
        facts
            .workspace_names
            .push(WorkspaceNameBasis::new(workspace.id, base, automatic));
    }
    facts
        .workspace_names
        .sort_by_key(WorkspaceNameBasis::workspace);
    org.retained =
        Some(OrganizationRetainedFacts::new(facts).map_err(|_| ReducerFailure::InvalidFacts)?);
    Ok(())
}
