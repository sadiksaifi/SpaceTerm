#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the path model lands before the Remote Workspace Picker GPUI surface"
    )
)]

use std::cmp::Ordering;

use crate::domain::{RemoteWorkspaceDirectory, RemoteWorkspaceValueError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RemoteWorkspacePathFormatError {
    Relative,
    BareTilde,
    UnsupportedTilde,
    InvalidControlCharacter,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ParsedRemoteWorkspacePath {
    display: String,
    exact_directory: RemoteWorkspaceDirectory,
    enumeration_directory: RemoteWorkspaceDirectory,
    descend_prefix: String,
    leaf_filter: String,
    trailing_separator: bool,
}

impl ParsedRemoteWorkspacePath {
    pub(super) fn display(&self) -> &str {
        &self.display
    }

    pub(super) const fn exact_directory(&self) -> &RemoteWorkspaceDirectory {
        &self.exact_directory
    }

    pub(super) const fn enumeration_directory(&self) -> &RemoteWorkspaceDirectory {
        &self.enumeration_directory
    }

    pub(super) fn leaf_filter(&self) -> &str {
        &self.leaf_filter
    }

    pub(super) const fn trailing_separator(&self) -> bool {
        self.trailing_separator
    }

    pub(super) fn reveals_hidden_directories(&self) -> bool {
        self.leaf_filter.starts_with('.')
    }
}

pub(super) fn parse_remote_workspace_path(
    input: &str,
) -> Result<ParsedRemoteWorkspacePath, RemoteWorkspacePathFormatError> {
    if input == "~" {
        return Err(RemoteWorkspacePathFormatError::BareTilde);
    }
    if input.starts_with('~') && !input.starts_with("~/") {
        return Err(RemoteWorkspacePathFormatError::UnsupportedTilde);
    }
    if !input.starts_with('/') && !input.starts_with("~/") {
        return Err(RemoteWorkspacePathFormatError::Relative);
    }

    let exact_directory = RemoteWorkspaceDirectory::new(input.to_owned())
        .map_err(|_| RemoteWorkspacePathFormatError::InvalidControlCharacter)?;
    let trailing_separator = input.ends_with('/');
    let (enumeration_spelling, descend_prefix, leaf_filter) = if trailing_separator {
        (input, input.to_owned(), String::new())
    } else {
        let separator = input
            .rfind('/')
            .ok_or(RemoteWorkspacePathFormatError::Relative)?;
        let directory_with_separator = &input[..=separator];
        let enumeration_spelling =
            if directory_with_separator == "/" || directory_with_separator == "~/" {
                directory_with_separator
            } else {
                &directory_with_separator[..directory_with_separator.len() - 1]
            };
        (
            enumeration_spelling,
            directory_with_separator.to_owned(),
            input[separator + 1..].to_owned(),
        )
    };
    let enumeration_directory = RemoteWorkspaceDirectory::new(enumeration_spelling.to_owned())
        .map_err(|_| RemoteWorkspacePathFormatError::InvalidControlCharacter)?;

    Ok(ParsedRemoteWorkspacePath {
        display: input.to_owned(),
        exact_directory,
        enumeration_directory,
        descend_prefix,
        leaf_filter,
        trailing_separator,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RemoteWorkspaceDirectoryRowError {
    InvalidName,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RemoteWorkspaceDirectoryRow {
    name: String,
}

impl RemoteWorkspaceDirectoryRow {
    pub(super) fn new(name: String) -> Result<Self, RemoteWorkspaceDirectoryRowError> {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.chars().any(char::is_control)
        {
            return Err(RemoteWorkspaceDirectoryRowError::InvalidName);
        }
        Ok(Self { name })
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }
}

pub(super) fn filter_remote_workspace_rows(
    parsed: &ParsedRemoteWorkspacePath,
    entries: &[RemoteWorkspaceDirectoryRow],
) -> Vec<RemoteWorkspaceDirectoryRow> {
    let folded_filter = parsed.leaf_filter.to_lowercase();
    let reveal_hidden = parsed.reveals_hidden_directories();
    let mut rows = entries
        .iter()
        .filter(|entry| reveal_hidden || !entry.name.starts_with('.'))
        .filter(|entry| entry.name.to_lowercase().starts_with(&folded_filter))
        .cloned()
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        let folded = left.name.to_lowercase().cmp(&right.name.to_lowercase());
        if folded == Ordering::Equal {
            left.name.cmp(&right.name)
        } else {
            folded
        }
    });
    rows
}

pub(super) fn descend_remote_workspace_query(
    parsed: &ParsedRemoteWorkspacePath,
    row: &RemoteWorkspaceDirectoryRow,
) -> Result<RemoteWorkspaceDirectory, RemoteWorkspaceValueError> {
    RemoteWorkspaceDirectory::new(format!("{}{}/", parsed.descend_prefix, row.name()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RemoteWorkspaceExactPathState {
    ReadableDirectory,
    Missing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum RemoteWorkspaceConfirmation {
    OpenRemoteProject(RemoteWorkspaceDirectory),
    CreateFolder(RemoteWorkspaceDirectory),
}

pub(super) fn remote_workspace_confirmation(
    parsed: &ParsedRemoteWorkspacePath,
    state: RemoteWorkspaceExactPathState,
) -> RemoteWorkspaceConfirmation {
    match state {
        RemoteWorkspaceExactPathState::ReadableDirectory => {
            RemoteWorkspaceConfirmation::OpenRemoteProject(parsed.exact_directory.clone())
        }
        RemoteWorkspaceExactPathState::Missing => {
            RemoteWorkspaceConfirmation::CreateFolder(parsed.exact_directory.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_path_parser_should_accept_root_without_rewriting_it() {
        let parsed = parse_remote_workspace_path("/").unwrap();

        assert_eq!(
            (
                parsed.display(),
                parsed.exact_directory().as_str(),
                parsed.enumeration_directory().as_str(),
                parsed.leaf_filter(),
                parsed.trailing_separator(),
            ),
            ("/", "/", "/", "", true)
        );
    }

    #[test]
    fn remote_path_parser_should_preserve_home_relative_spelling() {
        let parsed = parse_remote_workspace_path("~/Projects/SpaceTerm").unwrap();

        assert_eq!(
            (
                parsed.display(),
                parsed.exact_directory().as_str(),
                parsed.enumeration_directory().as_str(),
                parsed.leaf_filter(),
            ),
            (
                "~/Projects/SpaceTerm",
                "~/Projects/SpaceTerm",
                "~/Projects",
                "SpaceTerm",
            )
        );
    }

    #[test]
    fn remote_path_parser_should_preserve_repeated_separators() {
        let home_relative = parse_remote_workspace_path("~//Projects//SpaceTerm").unwrap();
        let absolute = parse_remote_workspace_path("//srv///projects//SpaceTerm").unwrap();

        assert_eq!(
            (
                home_relative.exact_directory().as_str(),
                home_relative.enumeration_directory().as_str(),
                absolute.exact_directory().as_str(),
                absolute.enumeration_directory().as_str(),
            ),
            (
                "~//Projects//SpaceTerm",
                "~//Projects/",
                "//srv///projects//SpaceTerm",
                "//srv///projects/",
            )
        );
    }

    #[test]
    fn trailing_separator_should_enumerate_the_exact_remote_directory() {
        let parsed = parse_remote_workspace_path("~/Projects//SpaceTerm/").unwrap();

        assert_eq!(
            (
                parsed.exact_directory().as_str(),
                parsed.enumeration_directory().as_str(),
                parsed.leaf_filter(),
            ),
            ("~/Projects//SpaceTerm/", "~/Projects//SpaceTerm/", "",)
        );
    }

    #[test]
    fn remote_path_parser_should_reject_relative_and_unsupported_tilde_forms() {
        assert_eq!(
            parse_remote_workspace_path("Projects"),
            Err(RemoteWorkspacePathFormatError::Relative)
        );
        assert_eq!(
            parse_remote_workspace_path("~"),
            Err(RemoteWorkspacePathFormatError::BareTilde)
        );
        assert_eq!(
            parse_remote_workspace_path("~other/Projects"),
            Err(RemoteWorkspacePathFormatError::UnsupportedTilde)
        );
    }

    #[test]
    fn hidden_directories_should_be_revealed_only_from_a_dot_leaf() {
        let ordinary = parse_remote_workspace_path("~/Projects/").unwrap();
        let dotted = parse_remote_workspace_path("~/Projects/.").unwrap();
        let entries = remote_rows([".config", "SpaceTerm", ".ssh"]);

        assert_eq!(
            row_names(filter_remote_workspace_rows(&ordinary, &entries)),
            vec!["SpaceTerm"]
        );
        assert_eq!(
            row_names(filter_remote_workspace_rows(&dotted, &entries)),
            vec![".config", ".ssh"]
        );
    }

    #[test]
    fn rows_should_filter_case_insensitive_prefixes_and_sort_deterministically() {
        let parsed = parse_remote_workspace_path("~/Projects/sp").unwrap();
        let entries = remote_rows(["spaceTerm", "Spatial", "SpaceTerm", "tools"]);

        assert_eq!(
            row_names(filter_remote_workspace_rows(&parsed, &entries)),
            vec!["SpaceTerm", "spaceTerm", "Spatial"]
        );
    }

    #[test]
    fn directory_rows_should_reject_non_one_level_names() {
        assert!(RemoteWorkspaceDirectoryRow::new("nested/project".to_owned()).is_err());
        assert!(RemoteWorkspaceDirectoryRow::new("project\nname".to_owned()).is_err());
        assert!(RemoteWorkspaceDirectoryRow::new(String::new()).is_err());
    }

    #[test]
    fn activating_a_row_should_rewrite_the_query_to_descend() {
        let parsed = parse_remote_workspace_path("~//Projects//sp").unwrap();
        let row = RemoteWorkspaceDirectoryRow::new("SpaceTerm".to_owned()).unwrap();

        assert_eq!(
            descend_remote_workspace_query(&parsed, &row)
                .unwrap()
                .as_str(),
            "~//Projects//SpaceTerm/"
        );
    }

    #[test]
    fn exact_path_state_should_choose_open_or_create_without_rewriting_the_path() {
        let parsed = parse_remote_workspace_path("~/Projects//SpaceTerm").unwrap();

        assert_eq!(
            remote_workspace_confirmation(
                &parsed,
                RemoteWorkspaceExactPathState::ReadableDirectory
            ),
            RemoteWorkspaceConfirmation::OpenRemoteProject(
                RemoteWorkspaceDirectory::new("~/Projects//SpaceTerm".to_owned()).unwrap()
            )
        );
        assert_eq!(
            remote_workspace_confirmation(&parsed, RemoteWorkspaceExactPathState::Missing),
            RemoteWorkspaceConfirmation::CreateFolder(
                RemoteWorkspaceDirectory::new("~/Projects//SpaceTerm".to_owned()).unwrap()
            )
        );
    }

    fn remote_rows<const N: usize>(names: [&str; N]) -> Vec<RemoteWorkspaceDirectoryRow> {
        names
            .into_iter()
            .map(|name| RemoteWorkspaceDirectoryRow::new(name.to_owned()).unwrap())
            .collect()
    }

    fn row_names(rows: Vec<RemoteWorkspaceDirectoryRow>) -> Vec<String> {
        rows.into_iter().map(|row| row.name().to_owned()).collect()
    }
}
