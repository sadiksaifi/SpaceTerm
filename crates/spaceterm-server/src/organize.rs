//! Shared organization plans, so the desktop and the CLI create and close work identically.
use crate::hierarchy::{Axis, CreateControl, Id, Inventory, Mutation};
use crate::{repository, worktree};
use std::path::Path;

/// The mutation that opens a new Tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewTab {
    Mutation(Mutation),
    Worktree(worktree::Operation),
}

/// How closing every Tab of a Worktree keeps its Workspace open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosePlan {
    /// Opens the fallback Tab first, when the Workspace would otherwise lose its last Tab.
    pub fallback: Option<worktree::Operation>,
    /// The Tabs to close.
    pub tabs: Vec<Id>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OrganizeError {
    #[error("The target is not in the inventory.")]
    Missing,
    #[error("No other Worktree can hold the fallback Tab.")]
    NoFallbackWorktree,
}

pub fn new_tab(
    _inv: &Inventory,
    _workspace: Id,
    _source: Option<Id>,
    _worktree: Option<&Path>,
    _control: CreateControl,
) -> Result<NewTab, OrganizeError> {
    Err(OrganizeError::Missing)
}

pub fn split(
    _inv: &Inventory,
    _tab: Id,
    _source: Option<Id>,
    _axis: Axis,
    _control: CreateControl,
) -> Result<Mutation, OrganizeError> {
    Err(OrganizeError::Missing)
}

pub fn worktree_close_plan(
    _inv: &Inventory,
    _repos: &repository::Snapshot,
    _workspace: Id,
    _anchor_tab: Id,
) -> Result<ClosePlan, OrganizeError> {
    Err(OrganizeError::Missing)
}
