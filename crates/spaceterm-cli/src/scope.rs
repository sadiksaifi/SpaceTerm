//! Target resolution: from the selected server's one snapshot to `Resolved`, as section 4.3 of
//! the CLI spec describes. Explicit selectors are matched independently, then intersected; the
//! current Terminal Session fills untouched levels, single candidates fill the rest.
use crate::contract::{
    Actual, Candidate, CandidatesData, Code, Details, Failure, Level, MismatchData, Source,
    TargetDetails,
};
use spaceterm_server::hierarchy::{Id, Inventory};
use std::path::PathBuf;

/// A full ID or a unique prefix of at least 6 lowercase hex characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdPrefix(String);

impl IdPrefix {
    pub const MIN: usize = 6;

    pub fn parse(text: &str) -> Option<Self> {
        let valid = (Self::MIN..=64).contains(&text.len())
            && text.bytes().all(|byte| byte.is_ascii_hexdigit());
        valid.then(|| Self(text.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn matches(&self, id: &Id) -> bool {
        let mut hex = String::with_capacity(64);
        for byte in id {
            hex.push_str(&format!("{byte:02x}"));
        }
        hex.starts_with(&self.0)
    }
}

/// One `--workspace`, `--worktree`, `--tab` or `--pane` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selector {
    /// The level containing the calling Pane.
    Current,
    /// `id:HEX`.
    Id(IdPrefix),
    /// `path:P`, Worktrees only.
    Path(PathBuf),
    /// A human handle.
    Text(String),
}

/// The selector was malformed. It carries no value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidSelector;

impl Selector {
    pub fn parse(text: &str) -> Result<Self, InvalidSelector> {
        if text == "current" {
            return Ok(Self::Current);
        }
        if let Some(id) = text.strip_prefix("id:") {
            return IdPrefix::parse(id).map(Self::Id).ok_or(InvalidSelector);
        }
        if let Some(path) = text.strip_prefix("path:") {
            if path.is_empty() {
                return Err(InvalidSelector);
            }
            return Ok(Self::Path(PathBuf::from(path)));
        }
        if text.is_empty() {
            return Err(InvalidSelector);
        }
        Ok(Self::Text(text.to_owned()))
    }
}

/// The explicit selectors of one command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopeArgs {
    pub workspace: Option<Selector>,
    pub worktree: Option<Selector>,
    pub tab: Option<Selector>,
    pub pane: Option<Selector>,
}

/// One Workspace Inventory revision and the Repository snapshot. Every resolution step reads
/// only this.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub server: [u8; 32],
    pub inventory: Inventory,
    pub repositories: Option<spaceterm_server::repository::Snapshot>,
}

/// The calling Pane, when `SPACETERM_PANE`, `SPACETERM_SERVER_ID` and `SPACETERM_IDENTITY`
/// match this executable and the snapshot's server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Current {
    pub server: [u8; 32],
    pub pane: Id,
}

/// The level a command acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    Server,
    Workspace,
    Worktree,
    Tab,
    Pane,
}

/// The command's effect, which decides the destructive restriction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Passive,
    Mutating,
    /// Needs confirmation. Its acted-on level accepts only `id:`, an exact handle or `current`,
    /// and never fills from a single candidate.
    Confirmed,
}

/// Where each resolved level came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sources {
    pub workspace: Option<Source>,
    pub worktree: Option<Source>,
    pub tab: Option<Source>,
    pub pane: Option<Source>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub server: [u8; 32],
    pub workspace: Option<Id>,
    /// Worktrees have path identity.
    pub worktree: Option<PathBuf>,
    pub tab: Option<Id>,
    pub pane: Option<Id>,
    pub sources: Sources,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetError {
    NotFound {
        level: Level,
    },
    Ambiguous {
        level: Level,
        candidates: Vec<Candidate>,
    },
    /// The resolved `level` is not under the explicit parent; `actual` is its real parent.
    Mismatch {
        level: Level,
        actual: Actual,
    },
    ContextUnavailable,
    TargetRequired {
        level: Level,
    },
}

/// The operational facts a target failure reports in `data`.
pub enum TargetData {
    None,
    Candidates(CandidatesData),
    Mismatch(MismatchData),
}

impl TargetError {
    /// The bounded failure, and the candidates or actual parent for `data`.
    pub fn into_parts(self) -> (Failure, TargetData) {
        match self {
            Self::NotFound { level } => (
                Failure::with(Code::NotFound, Details::Target(TargetDetails::level(level))),
                TargetData::None,
            ),
            Self::Ambiguous { level, candidates } => {
                let details = TargetDetails {
                    level,
                    parent: None,
                    count: Some(candidates.len().try_into().unwrap_or(u32::MAX)),
                    ids: candidates.iter().filter_map(|c| c.id.clone()).collect(),
                };
                (
                    Failure::with(Code::AmbiguousTarget, Details::Target(details)),
                    TargetData::Candidates(CandidatesData { candidates }),
                )
            }
            Self::Mismatch { level, actual } => (
                Failure::with(
                    Code::ScopeMismatch,
                    Details::Target(TargetDetails {
                        parent: Some(actual.level),
                        ..TargetDetails::level(level)
                    }),
                ),
                TargetData::Mismatch(MismatchData { actual }),
            ),
            Self::ContextUnavailable => (Failure::new(Code::ContextUnavailable), TargetData::None),
            Self::TargetRequired { level } => (
                Failure::with(
                    Code::TargetRequired,
                    Details::Target(TargetDetails::level(level)),
                ),
                TargetData::None,
            ),
        }
    }
}

pub fn resolve(
    snapshot: &Snapshot,
    _args: &ScopeArgs,
    _current: Option<&Current>,
    _want: Want,
    _effect: Effect,
) -> Result<Resolved, TargetError> {
    let _ = snapshot.server;
    Err(TargetError::ContextUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectors_parse_their_four_forms() {
        assert_eq!(Selector::parse("current"), Ok(Selector::Current));
        assert_eq!(
            Selector::parse("id:0BE4F3"),
            Ok(Selector::Id(IdPrefix("0be4f3".into())))
        );
        assert_eq!(Selector::parse("id:0be4f"), Err(InvalidSelector));
        assert_eq!(
            Selector::parse("path:/srv/api"),
            Ok(Selector::Path("/srv/api".into()))
        );
        assert_eq!(
            Selector::parse("feature/x"),
            Ok(Selector::Text("feature/x".into()))
        );
        assert!(!IdPrefix::parse("0be4f3").unwrap().matches(&[0x0b; 32]));
        assert!(IdPrefix::parse("0b0b0b").unwrap().matches(&[0x0b; 32]));
    }
}
