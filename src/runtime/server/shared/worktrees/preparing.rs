//! Git preflight and verification run off the terminal request lock.
use super::*;

pub(super) enum Prepared {
    Open {
        workspace: Id,
        root: PathBuf,
        path: PathBuf,
    },
    Write(Box<Write>),
}
pub(super) struct Write {
    pub git: LocalWorktreeGit,
    pub run: Box<dyn FnOnce(LocalWorktreeGit) -> Result<Option<PathBuf>, Error> + Send>,
    pub removal: Option<RemovalPlan>,
}
pub(super) struct RemovalPlan {
    review: Removal,
    fallback: PathBuf,
    sources: Vec<(Id, PathBuf)>,
    pins: Vec<(Id, PathBuf)>,
    contained: Vec<PathBuf>,
}
fn sources(inventory: &Inventory) -> Vec<(Id, PathBuf)> {
    inventory
        .workspaces
        .iter()
        .flat_map(|w| &w.tabs)
        .flat_map(|t| &t.panes)
        .map(|p| {
            (
                p.id,
                crate::runtime::server::repositories::pane_directory(p).to_owned(),
            )
        })
        .collect()
}
fn pins(inventory: &Inventory) -> Vec<(Id, PathBuf)> {
    inventory
        .workspaces
        .iter()
        .filter_map(|w| w.pinned_directory.clone().map(|pin| (w.id, pin)))
        .collect()
}
impl RemovalPlan {
    pub fn apply(&self, device: Id, current: &Inventory, next: &mut State) -> Result<(), Error> {
        // New or changed directory sources require another physical check. Session changes are
        // compared only for the exact affected Tabs, so unrelated terminal input remains live.
        if sources(current)
            .iter()
            .any(|source| !self.sources.contains(source))
            || pins(current) != self.pins
            || current
                .close
                .as_ref()
                .is_none_or(|close| close.boot != self.review.boot)
            || current.affected_worktree_tabs_in(
                &self.review.root,
                &self.review.path,
                |directory| {
                    directory.starts_with(&self.review.path)
                        || self.contained.iter().any(|path| path == directory)
                },
            ) != self.review.tabs
        {
            return Err(Error::Changed);
        }
        for affected in &self.review.tabs {
            let workspace = next
                .inventory
                .workspaces
                .iter()
                .find(|w| w.id == affected.workspace)
                .ok_or(Error::Changed)?;
            if workspace
                .tabs
                .iter()
                .all(|t| self.review.tabs.iter().any(|a| a.tab == t.id))
            {
                next.add_worktree_tab(
                    device,
                    workspace.id,
                    self.review.root.clone(),
                    self.fallback.clone(),
                )
                .map_err(|_| Error::Unavailable)?;
            }
            for pane in &affected.panes {
                next.remove_pane(pane.id);
            }
        }
        Ok(())
    }
}

pub(super) fn prepare(
    operation: Operation,
    approval: Option<Approval>,
    inventory: Inventory,
    lock: Option<std::fs::File>,
) -> Result<Prepared, Error> {
    let token = SshCancellationToken::default();
    let (git, _) = git(lock, &token)?;
    match operation {
        Operation::OpenTab {
            workspace,
            root,
            path,
        } => {
            let list = list(&git, &root, &token)?;
            if !list.worktrees.iter().any(|r| {
                r.root == path
                    && !r.missing
                    && r.head != crate::worktrees::listing::WorktreeHead::Bare
            }) {
                return Err(Error::Missing);
            }
            local_filesystem()
                .validate_directory(&path)
                .map_err(|_| Error::Location)?;
            Ok(Prepared::Open {
                workspace,
                root,
                path,
            })
        }
        Operation::Create {
            workspace,
            root,
            path,
            branch,
        } => {
            if !inventory.workspaces.iter().any(|w| w.id == workspace) {
                return Err(Error::Missing);
            }
            list(&git, &root, &token)?;
            if local_filesystem()
                .probe_new_directory(&path)
                .map_err(|_| Error::Location)?
                != crate::platform::local_filesystem::NewDirectoryTarget::Free
            {
                return Err(Error::Location);
            }
            Ok(Prepared::Write(Box::new(Write {
                git,
                removal: None,
                run: Box::new(move |git| {
                    let token = SshCancellationToken::default();
                    git.create(&root, &path, &branch, &token)
                        .map_err(|_| Error::Branch)?;
                    let snapshot = list(&git, &root, &token).map_err(|_| Error::Interrupted)?;
                    let physical = local_filesystem()
                        .validate_directory(&path)
                        .ok()
                        .and_then(|directory| std::fs::canonicalize(directory.path()).ok())
                        .filter(|path| {
                            snapshot.worktrees.iter().any(|r| {
                                r.root == *path
                                    && !r.missing
                                    && r.head
                                        == crate::worktrees::listing::WorktreeHead::Branch(
                                            branch.name().into(),
                                        )
                            })
                        })
                        .ok_or(Error::Changed)?;
                    Ok(Some(physical))
                }),
            })))
        }
        Operation::Remove {
            root,
            path,
            boot,
            confirmation,
        } => {
            let approval = approval
                .filter(|a| {
                    a.review.boot == boot
                        && a.review.confirmation == confirmation
                        && a.review.root == root
                        && a.review.path == path
                        && a.expires > Instant::now()
                })
                .ok_or(Error::Changed)?;
            if inventory
                .close
                .as_ref()
                .is_none_or(|close| close.boot != boot)
            {
                return Err(Error::Changed);
            }
            let sources = sources(&inventory);
            let contained: Vec<_> = sources
                .iter()
                .map(|(_, directory)| directory)
                .filter(|directory| {
                    directory.starts_with(&path)
                        || local_filesystem().physically_contains(&path, directory)
                })
                .cloned()
                .collect();
            if inventory.affected_worktree_tabs_in(&root, &path, |directory| {
                contained.iter().any(|path| path == directory)
            }) != approval.review.tabs
            {
                return Err(Error::Changed);
            }
            let list = list(&git, &root, &token)?;
            if path == root {
                return Err(Error::Main);
            }
            if !list.worktrees.iter().any(|r| r == &approval.record) {
                return Err(Error::Changed);
            }
            if pinned_in_worktree(&inventory, &path)? {
                return Err(Error::Pinned);
            }
            git.confirm_location(&path, &approval.expected, &token)
                .map_err(removal_error)?;
            let fallback = list
                .worktrees
                .iter()
                .find(|r| {
                    r.root != path
                        && !r.missing
                        && r.head != crate::worktrees::listing::WorktreeHead::Bare
                })
                .ok_or(Error::Unavailable)?
                .root
                .clone();
            let force = approval.review.changes || approval.review.submodules;
            let expected = approval.expected;
            let removal = RemovalPlan {
                pins: pins(&inventory),
                sources,
                contained,
                review: approval.review,
                fallback,
            };
            Ok(Prepared::Write(Box::new(Write {
                git,
                removal: Some(removal),
                run: Box::new(move |git| {
                    git.remove(
                        &root,
                        &path,
                        force,
                        &expected,
                        &SshCancellationToken::default(),
                    )
                    .map_err(removal_error)?;
                    Ok(None)
                }),
            })))
        }
        Operation::SetLocation { .. } => Err(Error::Unavailable),
    }
}
