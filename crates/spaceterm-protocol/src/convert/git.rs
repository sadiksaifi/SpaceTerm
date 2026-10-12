//! Both protocol majors share one checked Git meaning. The generated DTOs are
//! transport values only; assessments and server paths confer no native authority.

use super::directories::{bounded, decode_entity, decode_server, entity, nonzero, server};
use crate::{ProtocolError, codec::Payload};
use spaceterm_core::{
    authority::{
        CloseWorkGeneration, ControlGeneration, FieldVersion, OrganizationRevision, ServerEpoch,
        SessionGeneration,
    },
    identity::{
        ClientId, PairingId, PaneId, RepositoryId, ServerId, TabId, WorkspaceId, WorktreeId,
    },
    operations::RepositoryScope,
    organization::BranchName,
    worktrees::{queries::*, *},
};

fn failure(value: GitFailure) -> ProtocolError {
    match value {
        GitFailure::Capacity | GitFailure::OutputTooLarge => ProtocolError::LimitExceeded,
        _ => ProtocolError::Malformed,
    }
}
pub fn encode_query(major: u32, request: &GitQueryRequest) -> Result<Payload, ProtocolError> {
    match major {
        1 => Ok(Payload::Major1(crate::wire::v1::Message {
            kind: Some(crate::wire::v1::message::Kind::GitQuery(v1::encode_query(
                request,
            )?)),
        })),
        2 => Ok(Payload::Major2(crate::wire::v2::Message {
            kind: Some(crate::wire::v2::message::Kind::GitQuery(v2::encode_query(
                request,
            )?)),
        })),
        _ => Err(ProtocolError::Incompatible),
    }
}
pub fn decode_query(payload: &Payload, server: ServerId) -> Result<GitQueryRequest, ProtocolError> {
    match payload {
        Payload::Major1(m) => match &m.kind {
            Some(crate::wire::v1::message::Kind::GitQuery(q)) => v1::decode_query(q, server),
            _ => Err(ProtocolError::UnexpectedMessage),
        },
        Payload::Major2(m) => match &m.kind {
            Some(crate::wire::v2::message::Kind::GitQuery(q)) => v2::decode_query(q, server),
            _ => Err(ProtocolError::UnexpectedMessage),
        },
    }
}
pub fn encode_response(
    major: u32,
    response: &GitQueryResponse,
    server: ServerId,
) -> Result<Payload, ProtocolError> {
    match major {
        1 => Ok(Payload::Major1(crate::wire::v1::Message {
            kind: Some(crate::wire::v1::message::Kind::GitQueryResult(
                v1::encode_response(response, server)?,
            )),
        })),
        2 => Ok(Payload::Major2(crate::wire::v2::Message {
            kind: Some(crate::wire::v2::message::Kind::GitQueryResult(
                v2::encode_response(response, server)?,
            )),
        })),
        _ => Err(ProtocolError::Incompatible),
    }
}
pub fn decode_response(
    payload: &Payload,
    server: ServerId,
) -> Result<GitQueryResponse, ProtocolError> {
    match payload {
        Payload::Major1(m) => match &m.kind {
            Some(crate::wire::v1::message::Kind::GitQueryResult(r)) => {
                v1::decode_response(r, server)
            }
            _ => Err(ProtocolError::UnexpectedMessage),
        },
        Payload::Major2(m) => match &m.kind {
            Some(crate::wire::v2::message::Kind::GitQueryResult(r)) => {
                v2::decode_response(r, server)
            }
            _ => Err(ProtocolError::UnexpectedMessage),
        },
    }
}

macro_rules! failure_mapping {
    ($wire:ident; $($domain:ident => $variant:ident),+ $(,)?) => {
        pub fn encode_failure(value: GitFailure) -> i32 { (match value { $(GitFailure::$domain => $wire::GitFailure::$variant),+ }) as i32 }
        pub fn decode_failure(value: i32) -> Result<GitFailure, ProtocolError> {
            Ok(match $wire::GitFailure::try_from(value).map_err(|_| ProtocolError::Malformed)? { $($wire::GitFailure::$variant => GitFailure::$domain),+, _ => return Err(ProtocolError::Malformed) })
        }
    };
}
macro_rules! profile {
    ($p:ident) => {
        pub mod $p {
            use super::*;
            use crate::wire::git::$p as wire;
            use crate::convert::directories::$p as directories;
            failure_mapping!(wire;
                Unauthorized => GitUnauthorized, WrongServer => GitWrongServer, TargetGone => GitTargetGone,
                Offline => GitOffline, Busy => GitBusy, Capacity => GitCapacity, ExpiredAssessment => GitExpiredAssessment,
                StaleAssessment => GitStaleAssessment, MainWorktree => GitMainWorktree, LockedWorktree => GitLockedWorktree,
                UnlistedWorktree => GitUnlistedWorktree, HoldsPinnedDirectory => GitHoldsPinnedDirectory, NoFallback => GitNoFallback,
                InvalidBranch => GitInvalidBranch, InvalidTemplate => GitInvalidTemplate, InvalidDirectory => GitInvalidDirectory,
                OccupiedTarget => GitOccupiedTarget, BranchExists => GitBranchExists, BranchMissing => GitBranchMissing,
                BranchCheckedOut => GitBranchCheckedOut, IdentityReplaced => GitIdentityReplaced, GitStateChanged => GitGitStateChanged,
                Unchecked => GitUnchecked, ToolMissing => GitToolMissing, ToolTooOld => GitToolTooOld, CancelledRead => GitCancelledRead,
                TimedOutRead => GitTimedOutRead, InvalidResponse => GitInvalidResponse, OutputTooLarge => GitOutputTooLarge,
                WriteFailed => GitWriteFailed, ExternalStateUncertain => GitExternalStateUncertain, RecoveryNeedsReview => GitRecoveryNeedsReview,
                StorageLocked => GitStorageLocked, StorageUnavailable => GitStorageUnavailable, UnsafeStorage => GitUnsafeStorage,
                StorageFailure => GitStorageFailure, IndeterminateDurability => GitIndeterminateDurability, BareRepositoryWithoutWorktree => GitBareRepositoryWithoutWorktree
            );
            pub fn encode_scope(value: RepositoryScope) -> Result<wire::RepositoryScope, ProtocolError> {
                value.validate().map_err(|_| ProtocolError::Malformed)?;
                Ok(wire::RepositoryScope { server: Some(server(value.server)), repository: Some(entity(value.server, value.repository.local())) })
            }
            pub fn decode_scope(value: &wire::RepositoryScope, expected: ServerId) -> Result<RepositoryScope, ProtocolError> {
                if decode_server(value.server.as_ref())? != expected { return Err(ProtocolError::Malformed); }
                Ok(RepositoryScope { server: expected, repository: RepositoryId::new(expected, decode_entity(value.repository.as_ref(), expected)?).map_err(|_| ProtocolError::Malformed)? })
            }
            pub fn encode_token(value: &GitAssessmentToken) -> wire::GitAssessmentToken {
                let p = value.parts();
                wire::GitAssessmentToken { server: Some(server(p.server)), epoch: p.epoch.get(), pairing: Some(entity(p.server, p.pairing.local())), client: Some(entity(p.server, p.client.local())), kind: match p.kind { GitAssessmentKind::Create => wire::GitAssessmentKind::Create, GitAssessmentKind::Remove => wire::GitAssessmentKind::Remove } as i32, random: p.random.to_vec() }
            }
            pub fn decode_token(value: &wire::GitAssessmentToken, expected: ServerId) -> Result<GitAssessmentToken, ProtocolError> {
                if decode_server(value.server.as_ref())? != expected { return Err(ProtocolError::Malformed); }
                GitAssessmentToken::new(GitAssessmentTokenParts { server: expected, epoch: ServerEpoch::new(value.epoch), pairing: PairingId::new(expected, decode_entity(value.pairing.as_ref(), expected)?).map_err(|_| ProtocolError::Malformed)?, client: ClientId::new(expected, decode_entity(value.client.as_ref(), expected)?).map_err(|_| ProtocolError::Malformed)?, kind: match wire::GitAssessmentKind::try_from(value.kind).map_err(|_| ProtocolError::Malformed)? { wire::GitAssessmentKind::Create => GitAssessmentKind::Create, wire::GitAssessmentKind::Remove => GitAssessmentKind::Remove, _ => return Err(ProtocolError::Malformed) }, random: value.random.as_slice().try_into().map_err(|_| ProtocolError::Malformed)? }).map_err(failure)
            }
            pub fn encode_command(value: &GitCommand) -> Result<wire::GitCommand, ProtocolError> {
                let expected = match value { GitCommand::CreateConfirmed { .. } => GitAssessmentKind::Create, GitCommand::RemoveConfirmed { .. } => GitAssessmentKind::Remove };
                if value.assessment().kind() != expected { return Err(ProtocolError::Malformed); }
                let token = encode_token(value.assessment());
                Ok(wire::GitCommand { action: Some(match value { GitCommand::CreateConfirmed { .. } => wire::git_command::Action::CreateConfirmed(token), GitCommand::RemoveConfirmed { .. } => wire::git_command::Action::RemoveConfirmed(token) }) })
            }
            pub fn decode_command(value: &wire::GitCommand, expected: ServerId) -> Result<GitCommand, ProtocolError> {
                let result = match value.action.as_ref().ok_or(ProtocolError::Malformed)? {
                    wire::git_command::Action::CreateConfirmed(t) => GitCommand::CreateConfirmed { assessment: decode_token(t, expected)? },
                    wire::git_command::Action::RemoveConfirmed(t) => GitCommand::RemoveConfirmed { assessment: decode_token(t, expected)? },
                };
                encode_command(&result)?;
                Ok(result)
            }
            pub fn encode_cursor(value: &GitPageCursor) -> Result<wire::GitPageCursor, ProtocolError> {
                let p = value.parts();
                Ok(wire::GitPageCursor { scope: Some(encode_scope(p.scope)?), epoch: p.epoch.get(), observation: p.observation, random: p.random.to_vec() })
            }
            pub fn decode_cursor(value: &wire::GitPageCursor, expected: ServerId) -> Result<GitPageCursor, ProtocolError> {
                GitPageCursor::new(GitPageCursorParts { scope: decode_scope(value.scope.as_ref().ok_or(ProtocolError::Malformed)?, expected)?, epoch: ServerEpoch::new(value.epoch), observation: value.observation, random: value.random.as_slice().try_into().map_err(|_| ProtocolError::Malformed)? }).map_err(failure)
            }
            fn encode_branch(value: &BranchName) -> wire::BranchText { wire::BranchText { value: value.as_str().to_owned() } }
            fn decode_branch(value: Option<&wire::BranchText>) -> Result<BranchName, ProtocolError> {
                BranchName::new(value.ok_or(ProtocolError::Malformed)?.value.clone()).map_err(|_| ProtocolError::Malformed)
            }
            pub fn encode_draft(value: &WorktreeCreateDraft) -> Result<wire::WorktreeCreateDraft, ProtocolError> {
                let p = value.parts();
                let choice = match &p.branch {
                    WorktreeBranchChoice::New { name, base } => wire::worktree_branch_choice::Choice::New(wire::NewBranchChoice { name: Some(encode_branch(name)), base: Some(wire::BranchReference { reference: Some(match base { BranchReference::Local(b) => wire::branch_reference::Reference::Local(encode_branch(b)), BranchReference::Remote(b) => wire::branch_reference::Reference::Remote(encode_branch(b)) }) }) }),
                    WorktreeBranchChoice::Existing { name } => wire::worktree_branch_choice::Choice::Existing(encode_branch(name)),
                    WorktreeBranchChoice::Remote { remote, name } => wire::worktree_branch_choice::Choice::Remote(wire::RemoteBranchChoice { remote: Some(encode_branch(remote)), name: Some(encode_branch(name)) }),
                };
                let location = match &p.location { WorktreeLocationChoice::ServerTemplate { expected_version } => wire::worktree_location_choice::Choice::ServerTemplate(wire::ServerTemplateChoice { expected_version: expected_version.get() }), WorktreeLocationChoice::Explicit(d) => wire::worktree_location_choice::Choice::Explicit(directories::encode_directory(d)) };
                Ok(wire::WorktreeCreateDraft { repository: Some(encode_scope(p.repository)?), branch: Some(wire::WorktreeBranchChoice { choice: Some(choice) }), location: Some(wire::WorktreeLocationChoice { choice: Some(location) }) })
            }
            pub fn decode_draft(value: &wire::WorktreeCreateDraft, expected: ServerId) -> Result<WorktreeCreateDraft, ProtocolError> {
                let scope = decode_scope(value.repository.as_ref().ok_or(ProtocolError::Malformed)?, expected)?;
                let branch = match value.branch.as_ref().and_then(|b| b.choice.as_ref()).ok_or(ProtocolError::Malformed)? {
                    wire::worktree_branch_choice::Choice::New(n) => WorktreeBranchChoice::New { name: decode_branch(n.name.as_ref())?, base: match n.base.as_ref().and_then(|b| b.reference.as_ref()).ok_or(ProtocolError::Malformed)? { wire::branch_reference::Reference::Local(b) => BranchReference::Local(decode_branch(Some(b))?), wire::branch_reference::Reference::Remote(b) => BranchReference::Remote(decode_branch(Some(b))?) } },
                    wire::worktree_branch_choice::Choice::Existing(b) => WorktreeBranchChoice::Existing { name: decode_branch(Some(b))? },
                    wire::worktree_branch_choice::Choice::Remote(r) => WorktreeBranchChoice::Remote { remote: decode_branch(r.remote.as_ref())?, name: decode_branch(r.name.as_ref())? },
                };
                let location = match value.location.as_ref().and_then(|l| l.choice.as_ref()).ok_or(ProtocolError::Malformed)? {
                    wire::worktree_location_choice::Choice::ServerTemplate(t) => WorktreeLocationChoice::ServerTemplate { expected_version: FieldVersion::new(t.expected_version) },
                    wire::worktree_location_choice::Choice::Explicit(d) => WorktreeLocationChoice::Explicit(directories::decode_directory(d, expected)?),
                };
                WorktreeCreateDraft::new(scope, branch, location).map_err(failure)
            }
            pub fn encode_preview(value: &WorktreeCreationPreview) -> Result<wire::WorktreeCreationPreview, ProtocolError> {
                let p = value.parts();
                Ok(wire::WorktreeCreationPreview { draft: Some(encode_draft(&p.draft)?), directory: Some(directories::encode_directory(&p.directory)), template_version: p.template_version.map(|v| v.get()) })
            }
            pub fn decode_preview(value: &wire::WorktreeCreationPreview, expected: ServerId) -> Result<WorktreeCreationPreview, ProtocolError> {
                WorktreeCreationPreview::new(decode_draft(value.draft.as_ref().ok_or(ProtocolError::Malformed)?, expected)?, directories::decode_directory(value.directory.as_ref().ok_or(ProtocolError::Malformed)?, expected)?, value.template_version.map(FieldVersion::new)).map_err(failure)
            }
            pub fn encode_query(value: &GitQueryRequest) -> Result<wire::GitQuery, ProtocolError> {
                value.validate().map_err(failure)?;
                let mut cursor = None;
                let action = match &value.query {
                    GitQuery::Status { target } => wire::git_query::Action::StatusTarget(wire::GitStatusTarget { target: Some(match target {
                        GitStatusTarget::Pane(id) => wire::git_status_target::Target::Pane(entity(id.server(), id.local())),
                        GitStatusTarget::Workspace(id) => wire::git_status_target::Target::Workspace(entity(id.server(), id.local())),
                        GitStatusTarget::Worktree(id) => wire::git_status_target::Target::Worktree(entity(id.server(), id.local())),
                    }) }),
                    GitQuery::Inventory { workspace, cursor: c } => { cursor = c.as_ref().map(encode_cursor).transpose()?; wire::git_query::Action::WorkspaceInventory(wire::GitInventoryRequest { workspace: Some(entity(workspace.server(), workspace.local())) }) },
                    GitQuery::Branches { cursor: c, .. } => { cursor = c.as_ref().map(encode_cursor).transpose()?; wire::git_query::Action::Branches(true) },
                    GitQuery::PreviewCreate { request } => wire::git_query::Action::AssessCreate(wire::GitCreateAssessmentRequest { draft: Some(encode_draft(request)?), ..Default::default() }),
                    GitQuery::AssessRemove { worktree } => wire::git_query::Action::AssessRemove(entity(worktree.server(), worktree.local())),
                    GitQuery::AssessmentPage { assessment, cursor: c } => { cursor = c.as_ref().map(encode_cursor).transpose()?; wire::git_query::Action::AssessmentPage(encode_token(assessment)) },
                };
                let result = wire::GitQuery { request: value.correlation.request.get(), request_generation: value.correlation.generation.get(), scope: value.repository_scope().map(encode_scope).transpose()?, subscribe: value.subscribe, cursor, action: Some(action) };
                bounded(&result, crate::limits::MAX_MANAGEMENT_BYTES)?;
                Ok(result)
            }
            pub fn decode_query(value: &wire::GitQuery, expected: ServerId) -> Result<GitQueryRequest, ProtocolError> {
                bounded(value, crate::limits::MAX_MANAGEMENT_BYTES)?;
                let scope = value.scope.as_ref().map(|s| decode_scope(s, expected)).transpose()?;
                let cursor = value.cursor.as_ref().map(|c| decode_cursor(c, expected)).transpose()?;
                let query = match value.action.as_ref().ok_or(ProtocolError::Malformed)? {
                    wire::git_query::Action::StatusTarget(t) => GitQuery::Status { target: match t.target.as_ref().ok_or(ProtocolError::Malformed)? {
                        wire::git_status_target::Target::Pane(e) => GitStatusTarget::Pane(PaneId::new(expected, decode_entity(Some(e), expected)?).map_err(|_| ProtocolError::Malformed)?),
                        wire::git_status_target::Target::Workspace(e) => GitStatusTarget::Workspace(WorkspaceId::new(expected, decode_entity(Some(e), expected)?).map_err(|_| ProtocolError::Malformed)?),
                        wire::git_status_target::Target::Worktree(e) => GitStatusTarget::Worktree(WorktreeId::new(expected, decode_entity(Some(e), expected)?).map_err(|_| ProtocolError::Malformed)?),
                    } },
                    wire::git_query::Action::WorkspaceInventory(w) => GitQuery::Inventory { workspace: WorkspaceId::new(expected, decode_entity(w.workspace.as_ref(), expected)?).map_err(|_| ProtocolError::Malformed)?, cursor: cursor.clone() },
                    wire::git_query::Action::Branches(true) => GitQuery::Branches { repository: scope.ok_or(ProtocolError::Malformed)?, cursor: cursor.clone() },
                    wire::git_query::Action::AssessCreate(c) => {
                        let draft = if let Some(draft) = &c.draft {
                            let draft = decode_draft(draft, expected)?;
                            // Legacy representations may coexist only when every supplied
                            // operand matches the canonical choice. Do not ignore them.
                            let p = draft.parts();
                            if let Some(directory) = &c.directory {
                                if p.location != WorktreeLocationChoice::Explicit(directories::decode_directory(directory, expected)?) { return Err(ProtocolError::Malformed); }
                            }
                            if c.name.is_some() || c.base.is_some() || c.remote_base {
                                let WorktreeBranchChoice::New { name, base } = &p.branch else { return Err(ProtocolError::Malformed); };
                                if c.name.as_ref().map(decode_branch_ref).transpose()?.as_ref().is_some_and(|n| n != name) { return Err(ProtocolError::Malformed); }
                                if let Some(b) = &c.base {
                                    let b = decode_branch(Some(b))?;
                                    let legacy = if c.remote_base { BranchReference::Remote(b) } else { BranchReference::Local(b) };
                                    if &legacy != base { return Err(ProtocolError::Malformed); }
                                } else if c.remote_base { return Err(ProtocolError::Malformed); }
                            }
                            draft
                        } else {
                            WorktreeCreateDraft::new(scope.ok_or(ProtocolError::Malformed)?, WorktreeBranchChoice::New { name: decode_branch(c.name.as_ref())?, base: if c.remote_base { BranchReference::Remote(decode_branch(c.base.as_ref())?) } else { BranchReference::Local(decode_branch(c.base.as_ref())?) } }, WorktreeLocationChoice::Explicit(directories::decode_directory(c.directory.as_ref().ok_or(ProtocolError::Malformed)?, expected)?)).map_err(failure)?
                        };
                        GitQuery::PreviewCreate { request: draft }
                    }
                    wire::git_query::Action::AssessRemove(e) => GitQuery::AssessRemove { worktree: WorktreeId::new(expected, decode_entity(Some(e), expected)?).map_err(|_| ProtocolError::Malformed)? },
                    wire::git_query::Action::AssessmentPage(t) => GitQuery::AssessmentPage { assessment: decode_token(t, expected)?, cursor: cursor.clone() },
                    // Legacy repository-wide status/inventory has no qualified target.
                    // It must not be reinterpreted as an active Pane or Workspace.
                    _ => return Err(ProtocolError::Malformed),
                };
                if cursor.is_some() && !matches!(query, GitQuery::Inventory { .. } | GitQuery::Branches { .. } | GitQuery::AssessmentPage { .. }) { return Err(ProtocolError::Malformed); }
                let correlation = GitCorrelation::new(value.request, value.request_generation).map_err(failure)?;
                // Required scopes are already part of the domain query. Canonicalize
                // redundant equal copies to make both profile conversions identical.
                let optional = match &query {
                    GitQuery::Branches { repository, .. } => { if scope != Some(*repository) { return Err(ProtocolError::Malformed); } None },
                    GitQuery::PreviewCreate { request } => { if scope.is_some_and(|s| s != request.parts().repository) { return Err(ProtocolError::Malformed); } None },
                    _ => scope,
                };
                GitQueryRequest::new(correlation, query, optional, value.subscribe).map_err(failure)
            }
            fn decode_branch_ref(b: &wire::BranchText) -> Result<BranchName, ProtocolError> { decode_branch(Some(b)) }

            pub fn encode_outcome(value: &GitMutationOutcome) -> Result<wire::GitMutationOutcome, ProtocolError> {
                use wire::git_mutation_outcome::Result as R;
                let result = match value.parts() {
                    GitMutationOutcomeParts::Created { worktree, revision } => R::Created(wire::GitCreated { worktree: Some(entity(worktree.server(), worktree.local())), revision: revision.get() }),
                    GitMutationOutcomeParts::Removed { worktree, closed_tabs, ended_sessions, revision } => R::Removed(wire::GitRemoved { worktree: Some(entity(worktree.server(), worktree.local())), closed_tabs: *closed_tabs, ended_sessions: *ended_sessions, revision: revision.get() }),
                    GitMutationOutcomeParts::Failed { server: s, failure, ended_sessions, external } => R::Failed(wire::GitFailed { server: Some(server(*s)), failure: encode_failure(*failure), ended_sessions: *ended_sessions, external: match external { ExternalDisposition::NoEffectEstablished => wire::ExternalDisposition::NoEffectEstablished, ExternalDisposition::DesiredStateObserved => wire::ExternalDisposition::DesiredStateObserved, ExternalDisposition::PartialOrAmbiguous => wire::ExternalDisposition::PartialOrAmbiguous, ExternalDisposition::Unavailable => wire::ExternalDisposition::Unavailable } as i32 }),
                    GitMutationOutcomeParts::Recovered { server: s, observation, revision } => R::Recovered(wire::GitRecovered { server: Some(server(*s)), observation: match observation { RecoveryObservation::DesiredStateObserved => wire::RecoveryObservation::RecoveryDesiredState, RecoveryObservation::PreStateObserved => wire::RecoveryObservation::RecoveryPreState, RecoveryObservation::PartialOrAmbiguous => wire::RecoveryObservation::RecoveryPartialOrAmbiguous, RecoveryObservation::Unavailable => wire::RecoveryObservation::RecoveryUnavailable } as i32, revision: revision.map(|r| r.get()) }),
                };
                Ok(wire::GitMutationOutcome { result: Some(result) })
            }
            pub fn decode_outcome(value: &wire::GitMutationOutcome, expected: ServerId) -> Result<GitMutationOutcome, ProtocolError> {
                use wire::git_mutation_outcome::Result as R;
                let parts = match value.result.as_ref().ok_or(ProtocolError::Malformed)? {
                    R::Created(c) => GitMutationOutcomeParts::Created { worktree: WorktreeId::new(expected, decode_entity(c.worktree.as_ref(), expected)?).map_err(|_| ProtocolError::Malformed)?, revision: OrganizationRevision::new(c.revision) },
                    R::Removed(r) => GitMutationOutcomeParts::Removed { worktree: WorktreeId::new(expected, decode_entity(r.worktree.as_ref(), expected)?).map_err(|_| ProtocolError::Malformed)?, closed_tabs: r.closed_tabs, ended_sessions: r.ended_sessions, revision: OrganizationRevision::new(r.revision) },
                    R::Failed(f) => {
                        if decode_server(f.server.as_ref())? != expected { return Err(ProtocolError::Malformed); }
                        GitMutationOutcomeParts::Failed { server: expected, failure: decode_failure(f.failure)?, ended_sessions: f.ended_sessions, external: match wire::ExternalDisposition::try_from(f.external).map_err(|_| ProtocolError::Malformed)? { wire::ExternalDisposition::NoEffectEstablished => ExternalDisposition::NoEffectEstablished, wire::ExternalDisposition::DesiredStateObserved => ExternalDisposition::DesiredStateObserved, wire::ExternalDisposition::PartialOrAmbiguous => ExternalDisposition::PartialOrAmbiguous, wire::ExternalDisposition::Unavailable => ExternalDisposition::Unavailable, _ => return Err(ProtocolError::Malformed) } }
                    }
                    R::Recovered(r) => {
                        if decode_server(r.server.as_ref())? != expected { return Err(ProtocolError::Malformed); }
                        GitMutationOutcomeParts::Recovered { server: expected, observation: match wire::RecoveryObservation::try_from(r.observation).map_err(|_| ProtocolError::Malformed)? { wire::RecoveryObservation::RecoveryDesiredState => RecoveryObservation::DesiredStateObserved, wire::RecoveryObservation::RecoveryPreState => RecoveryObservation::PreStateObserved, wire::RecoveryObservation::RecoveryPartialOrAmbiguous => RecoveryObservation::PartialOrAmbiguous, wire::RecoveryObservation::RecoveryUnavailable => RecoveryObservation::Unavailable, _ => return Err(ProtocolError::Malformed) }, revision: r.revision.map(OrganizationRevision::new) }
                    }
                };
                GitMutationOutcome::new(parts).map_err(failure)
            }
            fn encode_removal(value: &GitRemovalSummary) -> wire::GitRemovalSummary {
                wire::GitRemovalSummary {
                    worktree: Some(entity(value.worktree.server(), value.worktree.local())),
                    directory: Some(directories::encode_directory(&value.directory)),
                    branch: value.branch.as_ref().map(encode_branch),
                    risk: match value.risk {
                        ApprovedRemoval::Clean => wire::GitRemovalRisk::Clean,
                        ApprovedRemoval::DiscardChanges => wire::GitRemovalRisk::DiscardChanges,
                        ApprovedRemoval::DeleteSubmoduleCheckouts => wire::GitRemovalRisk::DeleteSubmoduleCheckouts,
                        ApprovedRemoval::DiscardChangesAndDeleteSubmoduleCheckouts => wire::GitRemovalRisk::DiscardChangesAndDeleteSubmoduleCheckouts,
                        ApprovedRemoval::Missing => wire::GitRemovalRisk::Missing,
                    } as i32,
                }
            }
            fn decode_removal(value: &wire::GitRemovalSummary, expected: ServerId) -> Result<GitRemovalSummary, ProtocolError> {
                Ok(GitRemovalSummary {
                    worktree: WorktreeId::new(expected, decode_entity(value.worktree.as_ref(), expected)?).map_err(|_| ProtocolError::Malformed)?,
                    directory: directories::decode_directory(value.directory.as_ref().ok_or(ProtocolError::Malformed)?, expected)?,
                    branch: value.branch.as_ref().map(decode_branch_ref).transpose()?,
                    risk: match wire::GitRemovalRisk::try_from(value.risk).map_err(|_| ProtocolError::Malformed)? {
                        wire::GitRemovalRisk::Clean => ApprovedRemoval::Clean,
                        wire::GitRemovalRisk::DiscardChanges => ApprovedRemoval::DiscardChanges,
                        wire::GitRemovalRisk::DeleteSubmoduleCheckouts => ApprovedRemoval::DeleteSubmoduleCheckouts,
                        wire::GitRemovalRisk::DiscardChangesAndDeleteSubmoduleCheckouts => ApprovedRemoval::DiscardChangesAndDeleteSubmoduleCheckouts,
                        wire::GitRemovalRisk::Missing => ApprovedRemoval::Missing,
                        _ => return Err(ProtocolError::Malformed),
                    },
                })
            }
            fn encode_pull_request(value: &GitPullRequestFacts) -> wire::GitPullRequestFacts {
                wire::GitPullRequestFacts { host: value.host().to_owned(), owner: value.owner().to_owned(), repository: value.repository().to_owned(), head_branch: Some(encode_branch(value.head_branch())), head_owner: value.head_owner().map(str::to_owned) }
            }
            fn decode_pull_request(value: &wire::GitPullRequestFacts) -> Result<GitPullRequestFacts, ProtocolError> {
                GitPullRequestFacts::new(value.host.clone(), value.owner.clone(), value.repository.clone(), decode_branch(value.head_branch.as_ref())?, value.head_owner.clone()).map_err(failure)
            }
            fn encode_upstream(value: &GitUpstream) -> wire::GitUpstream {
                wire::GitUpstream { name: Some(encode_branch(&value.name)), divergence: value.divergence.map(|d| wire::GitDivergence { ahead: d.ahead, behind: d.behind }) }
            }
            fn decode_upstream(value: &wire::GitUpstream) -> Result<GitUpstream, ProtocolError> {
                Ok(GitUpstream { name: decode_branch(value.name.as_ref())?, divergence: value.divergence.as_ref().map(|d| GitDivergence { ahead: d.ahead, behind: d.behind }) })
            }
            fn encode_operation(value: GitRepositoryOperation) -> wire::GitRepositoryOperation {
                use wire::GitRepositoryOperationKind as K;
                let (kind, step) = match value {
                    GitRepositoryOperation::Rebasing { step } => (K::Rebasing, step),
                    GitRepositoryOperation::Applying { step } => (K::Applying, step),
                    GitRepositoryOperation::Merging => (K::Merging, None),
                    GitRepositoryOperation::Reverting => (K::Reverting, None),
                    GitRepositoryOperation::CherryPicking => (K::CherryPicking, None),
                    GitRepositoryOperation::Bisecting => (K::Bisecting, None),
                };
                wire::GitRepositoryOperation { kind: kind as i32, step: step.map(|s| wire::GitOperationStep { current: s.current, total: s.total }) }
            }
            fn decode_operation(value: &wire::GitRepositoryOperation) -> Result<GitRepositoryOperation, ProtocolError> {
                use wire::GitRepositoryOperationKind as K;
                let kind = K::try_from(value.kind).map_err(|_| ProtocolError::Malformed)?;
                let step = value.step.as_ref().map(|s| GitOperationStep { current: s.current, total: s.total });
                if step.is_some() && !matches!(kind, K::Rebasing | K::Applying) { return Err(ProtocolError::Malformed); }
                Ok(match kind {
                    K::Rebasing => GitRepositoryOperation::Rebasing { step },
                    K::Applying => GitRepositoryOperation::Applying { step },
                    K::Merging => GitRepositoryOperation::Merging,
                    K::Reverting => GitRepositoryOperation::Reverting,
                    K::CherryPicking => GitRepositoryOperation::CherryPicking,
                    K::Bisecting => GitRepositoryOperation::Bisecting,
                    _ => return Err(ProtocolError::Malformed),
                })
            }
            fn encode_counts(value: GitChangeCounts) -> wire::GitChangeCounts {
                use wire::git_change_counts::Total as T;
                wire::GitChangeCounts { total: Some(match value.total { GitChangeTotal::Exact(n) => T::Exact(n), GitChangeTotal::AtLeast(n) => T::AtLeast(n) }), staged: value.staged, modified: value.modified, deleted: value.deleted, renamed: value.renamed, conflicted: value.conflicted, untracked: value.untracked }
            }
            fn decode_counts(value: &wire::GitChangeCounts) -> Result<GitChangeCounts, ProtocolError> {
                use wire::git_change_counts::Total as T;
                Ok(GitChangeCounts { total: match value.total.ok_or(ProtocolError::Malformed)? { T::Exact(n) => GitChangeTotal::Exact(n), T::AtLeast(n) => GitChangeTotal::AtLeast(n) }, staged: value.staged, modified: value.modified, deleted: value.deleted, renamed: value.renamed, conflicted: value.conflicted, untracked: value.untracked })
            }
            pub fn encode_response(value: &GitQueryResponse, expected: ServerId) -> Result<wire::GitQueryResult, ProtocolError> {
                value.validate(expected).map_err(failure)?;
                use wire::git_query_result::Result as R;
                let result = match &value.result {
                    Err(f) => R::TypedFailure(encode_failure(*f)),
                    Ok(GitQueryResult::Inventory(p)) => R::Inventory(wire::GitInventoryPage { records: p.records.iter().map(|r| wire::GitInventoryRecord { worktree: Some(entity(r.worktree.server(), r.worktree.local())), directory: Some(directories::encode_directory(&r.directory)), main: r.main, missing: r.missing, listed: r.listed, version: r.version.get() }).collect(), next: p.next.as_ref().map(encode_cursor).transpose()?, version: p.version.get() }),
                    Ok(GitQueryResult::Branches(p)) => R::Branches(wire::GitBranchPage { records: p.records.iter().map(|r| wire::GitBranchRecord { name: Some(encode_branch(&r.name)), remote: r.remote, checked_out: r.checked_out }).collect(), next: p.next.as_ref().map(encode_cursor).transpose()? }),
                    Ok(GitQueryResult::Status(s)) => R::Status(wire::GitStatusFacts { revision: s.revision.get(), branch: s.branch.as_ref().map(encode_branch), changes: s.changes.iter().map(|c| wire::GitChange { path: c.path().to_vec(), kind: c.kind }).collect(), dirty: s.dirty, unchecked: s.unchecked, pull_request: s.pull_request.as_ref().map(encode_pull_request), upstream: s.upstream.as_ref().map(encode_upstream), operation: s.operation.map(encode_operation), counts: s.counts.map(encode_counts) }),
                    Ok(GitQueryResult::Assessment(p)) => R::Assessment(wire::GitAssessmentPage { token: Some(encode_token(&p.token)), tabs: p.tabs.iter().map(|t| wire::GitAffectedTab { workspace: Some(entity(t.workspace.server(), t.workspace.local())), tab: Some(entity(t.tab.server(), t.tab.local())), worktree: t.worktree.map(|w| entity(w.server(), w.local())), membership_version: t.membership_version.get() }).collect(), panes: p.panes.iter().map(|a| wire::GitAffectedPane { pane: Some(entity(a.pane.server(), a.pane.local())), tab: Some(entity(a.tab.server(), a.tab.local())), session_generation: a.session_generation.get(), control_generation: a.control_generation.get(), work_generation: a.work_generation.get(), starting: a.starting }).collect(), fallbacks: p.fallbacks.iter().map(|f| wire::GitFallback { workspace: Some(entity(f.workspace.server(), f.workspace.local())), worktree: f.worktree.map(|w| entity(w.server(), w.local())), directory: Some(directories::encode_directory(&f.directory)) }).collect(), next: p.next.as_ref().map(encode_cursor).transpose()?, requires_confirmation: p.requires_confirmation, creation_preview: p.creation_preview.as_ref().map(encode_preview).transpose()?.map(Box::new), removal: p.removal.as_ref().map(encode_removal) }),
                };
                let response = wire::GitQueryResult { request: value.correlation.request.get(), request_generation: value.correlation.generation.get(), scope: value.scope.map(encode_scope).transpose()?, result: Some(result) };
                bounded(&response, crate::limits::MAX_MANAGEMENT_BYTES)?;
                Ok(response)
            }
            pub fn decode_response(value: &wire::GitQueryResult, expected: ServerId) -> Result<GitQueryResponse, ProtocolError> {
                bounded(value, crate::limits::MAX_MANAGEMENT_BYTES)?;
                use wire::git_query_result::Result as R;
                let result = match value.result.as_ref().ok_or(ProtocolError::Malformed)? {
                    R::TypedFailure(f) => Err(decode_failure(*f)?),
                    R::Failure(_) => return Err(ProtocolError::Malformed), // no guess from legacy common.Failure
                    R::Inventory(p) => {
                        page_count(p.records.len())?;
                        Ok(GitQueryResult::Inventory(GitInventoryPage { records: p.records.iter().map(|r| Ok(GitInventoryRecord { worktree: WorktreeId::new(expected, decode_entity(r.worktree.as_ref(), expected)?).map_err(|_| ProtocolError::Malformed)?, directory: directories::decode_directory(r.directory.as_ref().ok_or(ProtocolError::Malformed)?, expected)?, main: r.main, missing: r.missing, listed: r.listed, version: FieldVersion::new(r.version) })).collect::<Result<_,ProtocolError>>()?, next: p.next.as_ref().map(|c| decode_cursor(c, expected)).transpose()?, version: OrganizationRevision::new(p.version) }))
                    }
                    R::Branches(p) => {
                        page_count(p.records.len())?;
                        Ok(GitQueryResult::Branches(GitBranchPage { records: p.records.iter().map(|r| Ok(GitBranchRecord { name: decode_branch(r.name.as_ref())?, remote: r.remote, checked_out: r.checked_out })).collect::<Result<_,ProtocolError>>()?, next: p.next.as_ref().map(|c| decode_cursor(c, expected)).transpose()? }))
                    }
                    R::Status(s) => {
                        if s.changes.len() > MAX_GIT_STATUS_CHANGES { return Err(ProtocolError::LimitExceeded); }
                        Ok(GitQueryResult::Status(GitStatusFacts { revision: nonzero(s.revision)?, branch: s.branch.as_ref().map(decode_branch_ref).transpose()?, changes: s.changes.iter().map(|c| GitChange::new(c.path.clone(), c.kind).map_err(failure)).collect::<Result<_,ProtocolError>>()?, dirty: s.dirty, unchecked: s.unchecked, pull_request: s.pull_request.as_ref().map(decode_pull_request).transpose()?, upstream: s.upstream.as_ref().map(decode_upstream).transpose()?, operation: s.operation.as_ref().map(decode_operation).transpose()?, counts: s.counts.as_ref().map(decode_counts).transpose()? }))
                    }
                    R::Assessment(p) => {
                        page_count(p.tabs.len() + p.panes.len() + p.fallbacks.len())?;
                        Ok(GitQueryResult::Assessment(Box::new(GitAssessmentPage { token: decode_token(p.token.as_ref().ok_or(ProtocolError::Malformed)?, expected)?, tabs: p.tabs.iter().map(|t| Ok(GitAffectedTab { workspace: WorkspaceId::new(expected, decode_entity(t.workspace.as_ref(), expected)?).map_err(|_| ProtocolError::Malformed)?, tab: TabId::new(expected, decode_entity(t.tab.as_ref(), expected)?).map_err(|_| ProtocolError::Malformed)?, worktree: t.worktree.as_ref().map(|e| WorktreeId::new(expected, decode_entity(Some(e), expected)?).map_err(|_| ProtocolError::Malformed)).transpose()?, membership_version: FieldVersion::new(t.membership_version) })).collect::<Result<_,ProtocolError>>()?, panes: p.panes.iter().map(|a| Ok(GitAffectedPane { pane: PaneId::new(expected, decode_entity(a.pane.as_ref(), expected)?).map_err(|_| ProtocolError::Malformed)?, tab: TabId::new(expected, decode_entity(a.tab.as_ref(), expected)?).map_err(|_| ProtocolError::Malformed)?, session_generation: SessionGeneration::new(a.session_generation), control_generation: ControlGeneration::new(a.control_generation), work_generation: CloseWorkGeneration::new(a.work_generation), starting: a.starting })).collect::<Result<_,ProtocolError>>()?, fallbacks: p.fallbacks.iter().map(|f| Ok(GitFallback { workspace: WorkspaceId::new(expected, decode_entity(f.workspace.as_ref(), expected)?).map_err(|_| ProtocolError::Malformed)?, worktree: f.worktree.as_ref().map(|e| WorktreeId::new(expected, decode_entity(Some(e), expected)?).map_err(|_| ProtocolError::Malformed)).transpose()?, directory: directories::decode_directory(f.directory.as_ref().ok_or(ProtocolError::Malformed)?, expected)? })).collect::<Result<_,ProtocolError>>()?, next: p.next.as_ref().map(|c| decode_cursor(c, expected)).transpose()?, requires_confirmation: p.requires_confirmation, creation_preview: p.creation_preview.as_ref().map(|p| decode_preview(p, expected)).transpose()?, removal: p.removal.as_ref().map(|r| decode_removal(r, expected)).transpose()? })))
                    }
                };
                let response = GitQueryResponse { correlation: GitCorrelation::new(value.request, value.request_generation).map_err(failure)?, scope: value.scope.as_ref().map(|s| decode_scope(s, expected)).transpose()?, result };
                response.validate(expected).map_err(failure)?;
                Ok(response)
            }
            fn page_count(count: usize) -> Result<(), ProtocolError> { if count > MAX_GIT_PAGE_RECORDS { Err(ProtocolError::LimitExceeded) } else { Ok(()) } }
        }
    };
}
profile!(v1);
profile!(v2);
