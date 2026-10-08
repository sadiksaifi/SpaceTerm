//! Checks a branch name against git's reference format while the person types.
//!
//! These are the rules of `git check-ref-format --branch` for a name without `@{-n}`. Git still
//! decides when SpaceTerm creates the branch.

use thiserror::Error;

/// Why git would refuse a branch name.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum BranchNameError {
    #[error("the name is empty")]
    Empty,
    #[error("the name starts with a dash")]
    LeadingDash,
    #[error("the name has a space, a control character, or one of ~ ^ : ? * [ \\")]
    InvalidCharacter,
    #[error("the name has two dots, two slashes, or @{{")]
    InvalidSequence,
    #[error("a part of the name starts with a dot or ends with .lock")]
    InvalidComponent,
    #[error("the name starts or ends with a slash or ends with a dot")]
    InvalidEnd,
    #[error("the name is reserved")]
    Reserved,
}

/// Checks that git accepts `name` as a new branch name.
pub(crate) fn validate_branch_name(name: &str) -> Result<(), BranchNameError> {
    if name.is_empty() {
        return Err(BranchNameError::Empty);
    }
    if name.starts_with('-') {
        return Err(BranchNameError::LeadingDash);
    }
    if name == "@" || name == "HEAD" {
        return Err(BranchNameError::Reserved);
    }
    if name.chars().any(|character| {
        character.is_ascii_control()
            || matches!(character, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\')
    }) {
        return Err(BranchNameError::InvalidCharacter);
    }
    if name.contains("..") || name.contains("//") || name.contains("@{") {
        return Err(BranchNameError::InvalidSequence);
    }
    if name.starts_with('/') || name.ends_with('/') || name.ends_with('.') {
        return Err(BranchNameError::InvalidEnd);
    }
    if name
        .split('/')
        .any(|component| component.starts_with('.') || component.ends_with(".lock"))
    {
        return Err(BranchNameError::InvalidComponent);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_branch_names_should_be_accepted() {
        for name in [
            "main",
            "feature/login",
            "fix-42",
            "release/v1.2",
            "a.b",
            "日本",
        ] {
            assert_eq!(validate_branch_name(name), Ok(()), "{name:?}");
        }
    }

    #[test]
    fn names_git_refuses_should_be_rejected_with_their_reason() {
        use BranchNameError::*;
        for (name, error) in [
            ("", Empty),
            ("-x", LeadingDash),
            ("HEAD", Reserved),
            ("@", Reserved),
            ("my branch", InvalidCharacter),
            ("a~1", InvalidCharacter),
            ("a^", InvalidCharacter),
            ("a:b", InvalidCharacter),
            ("a?", InvalidCharacter),
            ("a*", InvalidCharacter),
            ("a[b", InvalidCharacter),
            ("a\\b", InvalidCharacter),
            ("a\tb", InvalidCharacter),
            ("a\u{7f}", InvalidCharacter),
            ("a..b", InvalidSequence),
            ("a//b", InvalidSequence),
            ("a@{b", InvalidSequence),
            ("/a", InvalidEnd),
            ("a/", InvalidEnd),
            ("a.", InvalidEnd),
            (".a", InvalidComponent),
            ("a/.b", InvalidComponent),
            ("a.lock", InvalidComponent),
            ("a.lock/b", InvalidComponent),
        ] {
            assert_eq!(validate_branch_name(name), Err(error), "{name:?}");
        }
    }
}
