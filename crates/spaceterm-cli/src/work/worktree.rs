use super::*;
use crate::scope::{self, Effect, ScopeArgs, Want};
use spaceterm_server::{
    organize,
    repository::WorktreeHead,
    worktree::{self, Branch, BranchName, Branches, Operation},
};
use std::path::{Path, PathBuf};

pub struct Create {
    pub target: Resolved,
    pub mutation: Mutation,
    /// Run only after creation is accepted. It uses the same server and writer guard.
    pub open: Option<Mutation>,
}

pub struct Context<'a> {
    pub branches: &'a Branches,
    pub settings: &'a worktree::Settings,
    pub paths: &'a paths::Paths<'a>,
}

pub fn branch(
    name: &str,
    base: Option<&str>,
    branches: &Branches,
    main: Option<&str>,
) -> Result<Branch, Code> {
    if branches.local.iter().any(|b| b == name) {
        return Ok(Branch::Existing { name: name.into() });
    }
    if branches.remote.iter().any(|b| b == name) {
        let (remote, name) = name.split_once('/').ok_or(Code::InvalidBranchName)?;
        return Ok(Branch::Remote {
            remote: remote.into(),
            name: name.into(),
        });
    }
    let base = base.or(main).ok_or(Code::NotFound)?;
    let base = if branches.local.iter().any(|b| b == base) {
        BranchName::Local(base.into())
    } else if branches.remote.iter().any(|b| b == base) {
        BranchName::Remote(base.into())
    } else {
        return Err(Code::NotFound);
    };
    let branch = Branch::New {
        name: name.into(),
        base,
    };
    if !branch.is_bounded() {
        return Err(Code::InvalidBranchName);
    }
    Ok(branch)
}

pub struct CreateArgs<'a> {
    pub name: &'a str,
    pub base: Option<&'a str>,
    pub path: Option<&'a Path>,
    pub open: bool,
}

pub fn create(
    snapshot: &Snapshot,
    args: &ScopeArgs,
    current: Option<&Current>,
    options: &CreateArgs<'_>,
    control: CreateControl,
    context: &Context<'_>,
) -> Result<Create, WorkError> {
    let target = scope::resolve(snapshot, args, current, Want::Workspace, Effect::Mutating)?;
    let workspace = target.workspace.ok_or(WorkError::Code(Code::NotFound))?;
    let repos = snapshot
        .repositories
        .as_ref()
        .or(snapshot.inventory.repositories.as_ref())
        .ok_or(WorkError::Code(Code::NotFound))?;
    let trees = organize::workspace_worktrees(&snapshot.inventory, repos, workspace)
        .ok_or(WorkError::Code(Code::NotFound))?;
    let main = trees
        .worktrees
        .iter()
        .find(|t| t.root == trees.main_root)
        .and_then(|t| match &t.head {
            WorktreeHead::Branch(b) => Some(b.as_str()),
            _ => None,
        });
    let branch =
        branch(options.name, options.base, context.branches, main).map_err(WorkError::Code)?;
    let path = match options.path {
        Some(path) => context.paths.resolve(path).map_err(WorkError::Code)?,
        None => {
            let repository = trees
                .main_root
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or(WorkError::Code(Code::InvalidLocation))?;
            worktree::path_template::expand(
                &context.settings.template,
                context.paths.home,
                repository,
                branch.name(),
            )
            .map_err(|_| WorkError::Code(Code::InvalidLocation))?
        }
    };
    let root = trees.main_root.clone();
    let mutation = Mutation::Worktree(Operation::Create {
        workspace,
        root: root.clone(),
        path: path.clone(),
        branch,
    });
    let open = options.open.then_some({
        Mutation::Worktree(Operation::OpenTab {
            workspace,
            source: None,
            root,
            path,
            control,
        })
    });
    Ok(Create {
        target,
        mutation,
        open,
    })
}

pub fn remove(
    snapshot: &Snapshot,
    args: &ScopeArgs,
    current: Option<&Current>,
) -> Result<(Resolved, PathBuf, PathBuf), WorkError> {
    let target = scope::resolve(snapshot, args, current, Want::Worktree, Effect::Confirmed)?;
    let workspace = target.workspace.ok_or(WorkError::Code(Code::NotFound))?;
    let path = target
        .worktree
        .clone()
        .ok_or(WorkError::Code(Code::NotFound))?;
    let repos = snapshot
        .repositories
        .as_ref()
        .or(snapshot.inventory.repositories.as_ref())
        .ok_or(WorkError::Code(Code::NotFound))?;
    let root = organize::workspace_worktrees(&snapshot.inventory, repos, workspace)
        .ok_or(WorkError::Code(Code::NotFound))?
        .main_root
        .clone();
    Ok((target, root, path))
}

pub fn confirmed_removal(removal: &worktree::Removal) -> Mutation {
    Mutation::Worktree(Operation::Remove {
        root: removal.root.clone(),
        path: removal.path.clone(),
        boot: removal.boot,
        confirmation: removal.confirmation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_selection_prefers_local_then_remote_then_new_from_main() {
        let branches = Branches {
            local: vec!["main".into(), "origin/feature".into()],
            remote: vec!["origin/feature".into(), "origin/next".into()],
        };
        assert_eq!(
            branch("origin/feature", None, &branches, Some("main")).unwrap(),
            Branch::Existing {
                name: "origin/feature".into()
            }
        );
        assert_eq!(
            branch("origin/next", None, &branches, Some("main")).unwrap(),
            Branch::Remote {
                remote: "origin".into(),
                name: "next".into()
            }
        );
        assert_eq!(
            branch("feature/x", None, &branches, Some("main")).unwrap(),
            Branch::New {
                name: "feature/x".into(),
                base: BranchName::Local("main".into())
            }
        );
        assert_eq!(
            branch("new", Some("origin/next"), &branches, Some("main")).unwrap(),
            Branch::New {
                name: "new".into(),
                base: BranchName::Remote("origin/next".into())
            }
        );
    }
}
