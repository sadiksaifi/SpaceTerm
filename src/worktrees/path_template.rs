//! The Worktree Path Template: where SpaceTerm proposes a new Worktree's directory.

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Gives each repository its own folder and each branch its own directory in it.
pub(crate) const DEFAULT_WORKTREE_PATH_TEMPLATE: &str = "~/.worktrees/{repository}/{branch}";

/// Keeps a template a short line a person can read and edit.
const MAX_TEMPLATE_LENGTH: usize = 1024;

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum WorktreePathTemplateError {
    #[error("the template is empty")]
    Empty,
    #[error("the template is too long")]
    TooLong,
    #[error("the template does not start at the home folder or the root folder")]
    NotAbsolute,
    #[error("the template has a parent folder segment")]
    ParentSegment,
    #[error("the template has an unclosed brace")]
    UnclosedBrace,
    #[error("the template names an unknown placeholder")]
    UnknownPlaceholder,
    #[error("the template does not name the branch")]
    MissingBranch,
}

/// Checks that `template` expands to one absolute directory per branch.
pub(crate) fn validate(template: &str) -> Result<(), WorktreePathTemplateError> {
    parse(template).map(drop)
}

/// The directory `template` proposes for `branch` of `repository`. A branch's slashes become
/// dashes, so `feature/login` names one directory rather than nesting two.
pub(crate) fn expand(
    template: &str,
    home: &Path,
    repository: &str,
    branch: &str,
) -> Result<PathBuf, WorktreePathTemplateError> {
    let (from_home, pieces) = parse(template)?;
    let mut text = String::new();
    for piece in pieces {
        match piece {
            Piece::Text(literal) => text.push_str(literal),
            Piece::Repository => text.push_str(&flatten(repository)),
            Piece::Branch => text.push_str(&flatten(branch)),
        }
    }
    Ok(if from_home {
        home.join(text.trim_start_matches('/'))
    } else {
        PathBuf::from(text)
    })
}

enum Piece<'a> {
    Text(&'a str),
    Repository,
    Branch,
}

/// Whether the template starts at the home folder, and its pieces after any leading `~`.
fn parse(template: &str) -> Result<(bool, Vec<Piece<'_>>), WorktreePathTemplateError> {
    if template.trim().is_empty() {
        return Err(WorktreePathTemplateError::Empty);
    }
    if template.len() > MAX_TEMPLATE_LENGTH {
        return Err(WorktreePathTemplateError::TooLong);
    }
    let (from_home, rest) = match template.strip_prefix('~') {
        Some(rest) if rest.starts_with('/') => (true, rest),
        Some(_) => return Err(WorktreePathTemplateError::NotAbsolute),
        None if template.starts_with('/') => (false, template),
        None => return Err(WorktreePathTemplateError::NotAbsolute),
    };
    if rest.split('/').any(|segment| segment == "..") {
        return Err(WorktreePathTemplateError::ParentSegment);
    }
    let mut pieces = Vec::new();
    let mut remaining = rest;
    while let Some(open) = remaining.find('{') {
        if open > 0 {
            pieces.push(Piece::Text(&remaining[..open]));
        }
        let after = &remaining[open + 1..];
        let close = after
            .find('}')
            .ok_or(WorktreePathTemplateError::UnclosedBrace)?;
        pieces.push(match &after[..close] {
            "repository" => Piece::Repository,
            "branch" => Piece::Branch,
            _ => return Err(WorktreePathTemplateError::UnknownPlaceholder),
        });
        remaining = &after[close + 1..];
    }
    if !remaining.is_empty() {
        pieces.push(Piece::Text(remaining));
    }
    if !pieces.iter().any(|piece| matches!(piece, Piece::Branch)) {
        return Err(WorktreePathTemplateError::MissingBranch);
    }
    Ok((from_home, pieces))
}

/// One directory name for a value that may contain slashes.
fn flatten(value: &str) -> String {
    value.replace('/', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/Users/someone";

    fn expanded(template: &str, branch: &str) -> Result<PathBuf, WorktreePathTemplateError> {
        expand(template, Path::new(HOME), "app", branch)
    }

    #[test]
    fn the_default_template_should_give_each_branch_one_directory_under_its_repository() {
        assert_eq!(
            expanded(DEFAULT_WORKTREE_PATH_TEMPLATE, "feature/login"),
            Ok(PathBuf::from("/Users/someone/.worktrees/app/feature-login"))
        );
    }

    #[test]
    fn a_template_may_start_at_the_root_folder_and_place_the_branch_anywhere() {
        assert_eq!(
            expanded("/src/{branch}-{repository}", "fix/typo"),
            Ok(PathBuf::from("/src/fix-typo-app"))
        );
        assert_eq!(
            expanded("~/{branch}", "main"),
            Ok(PathBuf::from("/Users/someone/main"))
        );
    }

    #[test]
    fn templates_that_cannot_name_one_directory_per_branch_should_be_rejected() {
        use WorktreePathTemplateError::*;
        for (template, error) in [
            ("", Empty),
            ("   ", Empty),
            ("worktrees/{branch}", NotAbsolute),
            ("~someone/{branch}", NotAbsolute),
            ("~/../{branch}", ParentSegment),
            ("~/wt/{branch", UnclosedBrace),
            ("~/wt/{name}", UnknownPlaceholder),
            ("~/wt/{repository}", MissingBranch),
        ] {
            assert_eq!(validate(template), Err(error), "{template:?}");
        }
        assert_eq!(
            validate(&format!("/{}{{branch}}", "a".repeat(1024))),
            Err(TooLong)
        );
        assert_eq!(validate(DEFAULT_WORKTREE_PATH_TEMPLATE), Ok(()));
    }
}
