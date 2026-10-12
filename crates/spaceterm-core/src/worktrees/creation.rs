//! Portable creation choices and checked preview comparisons. Git-server owns
//! template expansion, native validation, assessment caching and confirmation.

use serde::{Deserialize, Serialize};

use super::{GitFailure, WorktreeBranchChoice};
use crate::authority::FieldVersion;
use crate::operations::RepositoryScope;
use crate::organization::ServerDirectory;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum WorktreeLocationChoice {
    ServerTemplate { expected_version: FieldVersion },
    Explicit(ServerDirectory),
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorktreeCreateDraftParts {
    pub repository: RepositoryScope,
    pub branch: WorktreeBranchChoice,
    pub location: WorktreeLocationChoice,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    try_from = "WorktreeCreateDraftParts",
    into = "WorktreeCreateDraftParts"
)]
pub struct WorktreeCreateDraft(WorktreeCreateDraftParts);
impl WorktreeCreateDraft {
    pub fn new(
        repository: RepositoryScope,
        branch: WorktreeBranchChoice,
        location: WorktreeLocationChoice,
    ) -> Result<Self, GitFailure> {
        repository.validate().map_err(|_| GitFailure::WrongServer)?;
        if let WorktreeLocationChoice::Explicit(directory) = &location
            && directory.server() != repository.server
        {
            return Err(GitFailure::WrongServer);
        }
        Ok(Self(WorktreeCreateDraftParts {
            repository,
            branch,
            location,
        }))
    }
    pub fn parts(&self) -> &WorktreeCreateDraftParts {
        &self.0
    }
}
impl TryFrom<WorktreeCreateDraftParts> for WorktreeCreateDraft {
    type Error = GitFailure;
    fn try_from(parts: WorktreeCreateDraftParts) -> Result<Self, Self::Error> {
        Self::new(parts.repository, parts.branch, parts.location)
    }
}
impl From<WorktreeCreateDraft> for WorktreeCreateDraftParts {
    fn from(value: WorktreeCreateDraft) -> Self {
        value.0
    }
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorktreeCreationPreviewParts {
    pub draft: WorktreeCreateDraft,
    pub directory: ServerDirectory,
    pub template_version: Option<FieldVersion>,
}
/// The preview preserves every requested choice. Its version is comparison data,
/// not evidence that the current Server Settings or filesystem still match.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    try_from = "WorktreeCreationPreviewParts",
    into = "WorktreeCreationPreviewParts"
)]
pub struct WorktreeCreationPreview(WorktreeCreationPreviewParts);
impl WorktreeCreationPreview {
    pub fn new(
        draft: WorktreeCreateDraft,
        directory: ServerDirectory,
        template_version: Option<FieldVersion>,
    ) -> Result<Self, GitFailure> {
        if directory.server() != draft.parts().repository.server {
            return Err(GitFailure::WrongServer);
        }
        match &draft.parts().location {
            WorktreeLocationChoice::ServerTemplate { expected_version }
                if template_version != Some(*expected_version) =>
            {
                return Err(GitFailure::StaleAssessment);
            }
            WorktreeLocationChoice::Explicit(expected)
                if expected != &directory || template_version.is_some() =>
            {
                return Err(GitFailure::InvalidDirectory);
            }
            _ => {}
        }
        Ok(Self(WorktreeCreationPreviewParts {
            draft,
            directory,
            template_version,
        }))
    }
    pub fn parts(&self) -> &WorktreeCreationPreviewParts {
        &self.0
    }
    pub fn validate_template_version(&self, current: FieldVersion) -> Result<(), GitFailure> {
        if self
            .0
            .template_version
            .is_some_and(|version| version != current)
        {
            Err(GitFailure::StaleAssessment)
        } else {
            Ok(())
        }
    }
}
impl TryFrom<WorktreeCreationPreviewParts> for WorktreeCreationPreview {
    type Error = GitFailure;
    fn try_from(parts: WorktreeCreationPreviewParts) -> Result<Self, Self::Error> {
        Self::new(parts.draft, parts.directory, parts.template_version)
    }
}
impl From<WorktreeCreationPreview> for WorktreeCreationPreviewParts {
    fn from(value: WorktreeCreationPreview) -> Self {
        value.0
    }
}
