//! Parses `git worktree list --porcelain` output.

use std::path::PathBuf;

use crate::repository_status::RepositoryReadError;
use crate::repository_status::discovery::SHORT_COMMIT_LENGTH;

/// What a Worktree has checked out.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorktreeHead {
    /// A branch's short name, such as `feature/login`.
    Branch(String),
    /// A detached commit's short object name.
    Detached(String),
    /// A bare repository has no work tree and no checkout.
    Bare,
}

/// One entry of `git worktree list`.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct WorktreeRecord {
    /// The work tree root as git reports it.
    pub(crate) root: PathBuf,
    pub(crate) head: WorktreeHead,
    pub(crate) locked: bool,
    /// Git reports the work tree as prunable: its directory no longer exists.
    pub(crate) missing: bool,
}

impl std::fmt::Debug for WorktreeRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorktreeRecord")
            .field("root", &"<redacted>")
            .field("locked", &self.locked)
            .field("missing", &self.missing)
            .finish_non_exhaustive()
    }
}

/// Parses porcelain records separated by `separator`: NUL with `-z`, else newline.
///
/// The first record is the Main Worktree. Records end at an empty field. Without `-z`, git quotes
/// a lock or prune reason that contains a newline, so splitting on newlines stays unambiguous for
/// the fields read here.
pub(crate) fn parse_worktree_list(
    output: &[u8],
    separator: u8,
) -> Result<Vec<WorktreeRecord>, RepositoryReadError> {
    let mut records = Vec::new();
    let mut current: Option<PartialRecord> = None;
    for field in output.split(|&byte| byte == separator) {
        if field.is_empty() {
            if let Some(record) = current.take() {
                records.push(record.finish()?);
            }
            continue;
        }
        let field = std::str::from_utf8(field).map_err(|_| RepositoryReadError::InvalidResponse)?;
        let (label, value) = field.split_once(' ').unwrap_or((field, ""));
        if label == "worktree" {
            if let Some(record) = current.take() {
                records.push(record.finish()?);
            }
            if value.is_empty() {
                return Err(RepositoryReadError::InvalidResponse);
            }
            current = Some(PartialRecord::new(value));
            continue;
        }
        let record = current
            .as_mut()
            .ok_or(RepositoryReadError::InvalidResponse)?;
        match label {
            "HEAD" => record.commit = Some(value.to_owned()),
            "branch" => {
                let name = value.strip_prefix("refs/heads/").unwrap_or(value);
                record.branch = Some(name.to_owned());
            }
            "detached" => record.detached = true,
            "bare" => record.bare = true,
            "locked" => record.locked = true,
            "prunable" => record.missing = true,
            // Newer git versions may add fields.
            _ => {}
        }
    }
    if let Some(record) = current.take() {
        records.push(record.finish()?);
    }
    if records.is_empty() {
        return Err(RepositoryReadError::InvalidResponse);
    }
    Ok(records)
}

struct PartialRecord {
    root: PathBuf,
    commit: Option<String>,
    branch: Option<String>,
    detached: bool,
    bare: bool,
    locked: bool,
    missing: bool,
}

impl PartialRecord {
    fn new(root: &str) -> Self {
        Self {
            root: PathBuf::from(root),
            commit: None,
            branch: None,
            detached: false,
            bare: false,
            locked: false,
            missing: false,
        }
    }

    fn finish(self) -> Result<WorktreeRecord, RepositoryReadError> {
        let head = if self.bare {
            WorktreeHead::Bare
        } else if let Some(branch) = self.branch {
            WorktreeHead::Branch(branch)
        } else if self.detached {
            let commit = self.commit.ok_or(RepositoryReadError::InvalidResponse)?;
            WorktreeHead::Detached(commit.chars().take(SHORT_COMMIT_LENGTH).collect())
        } else {
            return Err(RepositoryReadError::InvalidResponse);
        };
        Ok(WorktreeRecord {
            root: self.root,
            head,
            locked: self.locked,
            missing: self.missing,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "1bccb23a55670b76916d70699c1b04ef507bb38d";

    fn record(root: &str, head: WorktreeHead, locked: bool, missing: bool) -> WorktreeRecord {
        WorktreeRecord {
            root: root.into(),
            head,
            locked,
            missing,
        }
    }

    #[test]
    fn nul_separated_records_should_cover_every_checkout_state() {
        let output = format!(
            "worktree /src/app\0HEAD {OID}\0branch refs/heads/main\0\0\
             worktree /wt/feature\0HEAD {OID}\0branch refs/heads/feature/login\0locked\0\0\
             worktree /wt/detached\0HEAD {OID}\0detached\0\0\
             worktree /wt/gone\0HEAD {OID}\0branch refs/heads/gone\0locked moved\nto a disk\0\
             prunable gitdir file points to non-existent location\0\0"
        );

        let records = parse_worktree_list(output.as_bytes(), 0).unwrap();

        assert_eq!(
            records,
            vec![
                record(
                    "/src/app",
                    WorktreeHead::Branch("main".into()),
                    false,
                    false
                ),
                record(
                    "/wt/feature",
                    WorktreeHead::Branch("feature/login".into()),
                    true,
                    false
                ),
                record(
                    "/wt/detached",
                    WorktreeHead::Detached("1bccb23".into()),
                    false,
                    false
                ),
                record("/wt/gone", WorktreeHead::Branch("gone".into()), true, true),
            ]
        );
    }

    #[test]
    fn newline_separated_records_should_parse_a_bare_main_repository() {
        let output = format!(
            "worktree /src/app.git\nbare\n\nworktree /wt/main\nHEAD {OID}\nbranch refs/heads/main\n\n"
        );

        let records = parse_worktree_list(output.as_bytes(), b'\n').unwrap();

        assert_eq!(
            records,
            vec![
                record("/src/app.git", WorktreeHead::Bare, false, false),
                record(
                    "/wt/main",
                    WorktreeHead::Branch("main".into()),
                    false,
                    false
                ),
            ]
        );
    }

    #[test]
    fn malformed_output_should_be_an_invalid_response() {
        for output in [
            &b""[..],
            b"HEAD 1bccb23\0\0",
            b"worktree /src/app\0\0",
            b"worktree \0bare\0\0",
            b"worktree /src/\xff\0bare\0\0",
        ] {
            assert_eq!(
                parse_worktree_list(output, 0),
                Err(RepositoryReadError::InvalidResponse),
                "{output:?}"
            );
        }
    }
}
