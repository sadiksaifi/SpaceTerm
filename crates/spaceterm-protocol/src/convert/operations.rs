//! Identified operations use one canonical domain meaning in both wire majors.

use super::{control, identity::*, management, organization, startup};
use crate::ProtocolError;
use spaceterm_core::{
    authority::*,
    identity::*,
    operation_ledger_contracts::{CanonicalOperationRequest, LedgerCodec},
    operations::*,
    organization::*,
};
use std::num::NonZeroU64;

fn version(value: u64) -> Result<FieldVersion, ProtocolError> {
    if value == 0 {
        return Err(ProtocolError::Malformed);
    }
    Ok(FieldVersion::new(value))
}
fn name(mode: u32, value: &str) -> Result<NameDraft, ProtocolError> {
    match mode {
        1 if value.is_empty() => Ok(NameDraft::Automatic),
        2 => Ok(NameDraft::Custom(
            NameText::new(value.into()).map_err(|_| ProtocolError::Malformed)?,
        )),
        3 if value.is_empty() => Ok(NameDraft::Blank),
        _ => Err(ProtocolError::Malformed),
    }
}
fn encode_name(value: &NameDraft) -> (u32, String) {
    match value {
        NameDraft::Automatic => (1, String::new()),
        NameDraft::Custom(value) => (2, value.as_str().into()),
        NameDraft::Blank => (3, String::new()),
    }
}
macro_rules! profile {
    ($p:ident) => {
        pub mod $p {
            use super::*;
            use crate::wire::$p as wire;
            use wire::operation::Action;

            pub fn encode_assessment_reference(value: AssessmentRef) -> wire::AssessmentReference {
                wire::AssessmentReference {
                    operation: Some(encode_operation(value.operation)),
                    digest: value.digest.bytes().to_vec(),
                }
            }
            pub fn decode_assessment_reference(
                value: &wire::AssessmentReference,
            ) -> Result<AssessmentRef, ProtocolError> {
                Ok(AssessmentRef {
                    operation: decode_operation(required(&value.operation)?)?,
                    digest: RequestDigest::from_bytes(
                        value
                            .digest
                            .as_slice()
                            .try_into()
                            .map_err(|_| ProtocolError::Malformed)?,
                    ),
                })
            }
            pub fn encode_close_target(value: CloseTarget) -> wire::ExactCloseTarget {
                use wire::exact_close_target::Target;
                wire::ExactCloseTarget {
                    target: Some(match value {
                        CloseTarget::Pane(value) => Target::Pane(encode_pane(value)),
                        CloseTarget::Tab(value) => Target::Tab(encode_tab(value)),
                        CloseTarget::Workspace(value) => Target::Workspace(encode_workspace(value)),
                        CloseTarget::WorktreeTabs {
                            workspace,
                            worktree,
                        } => Target::WorktreeTabs(wire::WorktreeTabs {
                            workspace: Some(encode_workspace(workspace)),
                            worktree: Some(encode_worktree(worktree)),
                        }),
                    }),
                }
            }
            pub fn decode_close_target(
                value: &wire::ExactCloseTarget,
            ) -> Result<CloseTarget, ProtocolError> {
                use wire::exact_close_target::Target;
                let target = match required(&value.target)? {
                    Target::Pane(value) => CloseTarget::Pane(decode_pane(value)?),
                    Target::Tab(value) => CloseTarget::Tab(decode_tab(value)?),
                    Target::Workspace(value) => CloseTarget::Workspace(decode_workspace(value)?),
                    Target::WorktreeTabs(value) => CloseTarget::WorktreeTabs {
                        workspace: decode_workspace(required(&value.workspace)?)?,
                        worktree: decode_worktree(required(&value.worktree)?)?,
                    },
                };
                target
                    .validate_scope(target.server())
                    .map_err(|_| ProtocolError::Malformed)?;
                Ok(target)
            }
            pub fn decode_request(
                value: &wire::Operation,
                pairing: PairingId,
                client: ClientId,
                codec: &dyn LedgerCodec,
            ) -> Result<CanonicalOperationRequest, ProtocolError> {
                if prost::Message::encoded_len(value) > MAX_MANAGEMENT_BYTES {
                    return Err(ProtocolError::LimitExceeded);
                }
                let id = decode_operation(required(&value.id)?)?;
                if id.stream().pairing() != pairing || id.stream().client() != client {
                    return Err(ProtocolError::Authentication);
                }
                let server = id.stream().server();
                let command = match required(&value.action)? {
                    Action::CreateWorkspace(v) => {
                        if v.pin.is_some() {
                            return Err(ProtocolError::Malformed);
                        }
                        let owner = decode_server(required(&v.server)?)?;
                        let source = match (&v.source_directory, &v.source_pane) {
                            (None, None) => WorkspaceCreationSource::ServerHome { server: owner },
                            (Some(v), None) => WorkspaceCreationSource::Directory(
                                organization::$p::decode_directory(v)?,
                            ),
                            (None, Some(v)) => WorkspaceCreationSource::Pane(decode_pane(v)?),
                            _ => return Err(ProtocolError::Malformed),
                        };
                        Command::CreateWorkspace {
                            server: owner,
                            source,
                            name: name(v.draft_mode, &v.name)?,
                        }
                    }
                    Action::CreateTab(v) => {
                        let source = match (&v.source_directory, &v.source_pane, &v.worktree) {
                            (Some(v), None, None) => {
                                TabCreationSource::Directory(organization::$p::decode_directory(v)?)
                            }
                            (None, Some(v), None) => TabCreationSource::Pane(decode_pane(v)?),
                            (None, None, Some(v)) => {
                                TabCreationSource::Worktree(decode_worktree(v)?)
                            }
                            _ => return Err(ProtocolError::Malformed),
                        };
                        Command::CreateTab {
                            workspace: decode_workspace(required(&v.workspace)?)?,
                            source,
                        }
                    }
                    Action::CreatePane(v) => {
                        let (axis, edge) = organization::decode_edge(v.edge)?;
                        Command::CreatePane {
                            tab: decode_tab(required(&v.tab)?)?,
                            source: decode_pane(required(&v.source)?)?,
                            axis,
                            edge,
                        }
                    }
                    Action::RenameWorkspace(v) => {
                        let mode = if v.blank {
                            if v.mode != 0 {
                                return Err(ProtocolError::Malformed);
                            }
                            3
                        } else {
                            u32::try_from(v.mode).map_err(|_| ProtocolError::Malformed)?
                        };
                        Command::RenameWorkspace {
                            workspace: decode_workspace(required(&v.workspace)?)?,
                            expected_version: version(v.expected_name_version)?,
                            draft: name(mode, &v.name)?,
                        }
                    }
                    Action::PinWorkspace(v) => {
                        if v.pin.is_some() || v.clear_pin == v.pin_value.is_some() {
                            return Err(ProtocolError::Malformed);
                        }
                        Command::SetPinnedDirectory {
                            workspace: decode_workspace(required(&v.workspace)?)?,
                            expected_version: version(v.expected_pin_version)?,
                            value: v
                                .pin_value
                                .as_ref()
                                .map(organization::$p::decode_directory)
                                .transpose()?,
                        }
                    }
                    Action::RepairDirectory(v) => {
                        if v.directory.is_some() || v.retry_same == v.directory_value.is_some() {
                            return Err(ProtocolError::Malformed);
                        }
                        let pane = decode_pane(required(&v.pane)?)?;
                        let expected_version = version(v.expected_directory_version)?;
                        match &v.directory_value {
                            Some(v) => Command::RepairPaneDirectory {
                                pane,
                                expected_version,
                                value: organization::$p::decode_directory(v)?,
                            },
                            None => Command::RetryPaneDirectory {
                                pane,
                                expected_version,
                            },
                        }
                    }
                    Action::ChangeSettings(v) => {
                        let settings = required(&v.settings)?;
                        if settings.version != v.expected_version
                            || settings.worktree_location_version != v.expected_version
                            || settings.worktree_location.is_some()
                            || settings.remote_access.is_some()
                            || settings.remote_access_enabled
                            || !settings.bind_scope.is_empty()
                            || settings.scrollback_rows != 0
                        {
                            return Err(ProtocolError::Malformed);
                        }
                        Command::SetWorktreeLocation {
                            server,
                            expected_version: version(v.expected_version)?,
                            template: WorktreeLocationTemplate::new(
                                settings.worktree_location_template.clone(),
                            )
                            .map_err(|_| ProtocolError::Malformed)?,
                        }
                    }
                    Action::AssessClose(v) => {
                        if v.scope.is_some() {
                            return Err(ProtocolError::Malformed);
                        }
                        Command::AssessClose {
                            target: decode_close_target(required(&v.target)?)?,
                        }
                    }
                    Action::ConfirmClose(v) => {
                        if !v.assessment.is_empty() {
                            return Err(ProtocolError::Malformed);
                        }
                        Command::CloseConfirmed {
                            target: decode_close_target(required(&v.target)?)?,
                            assessment: decode_assessment_reference(required(&v.reference)?)?,
                        }
                    }
                    Action::AssessKillWorkspace(v) => Command::AssessKillWorkspace {
                        workspace: decode_workspace(required(&v.workspace)?)?,
                    },
                    Action::KillWorkspace(v) => Command::KillWorkspaceConfirmed {
                        workspace: decode_workspace(required(&v.workspace)?)?,
                        assessment: decode_assessment_reference(required(&v.assessment)?)?,
                    },
                    Action::AssessEndAllSessions(v) => Command::AssessEndAllSessions {
                        server: decode_server(required(&v.server)?)?,
                    },
                    Action::EndAllSessions(v) => {
                        if !v.assessment.is_empty() {
                            return Err(ProtocolError::Malformed);
                        }
                        Command::EndAllSessions {
                            server: decode_server(required(&v.server)?)?,
                            assessment: decode_assessment_reference(required(&v.reference)?)?,
                        }
                    }
                    Action::IdentifiedControl(v) => control::$p::decode_request(v, client)?,
                    Action::ReleaseControl(v) => control::$p::decode_release(v)?,
                    Action::IdentifiedStartup(v) => startup::$p::decode_request(v, id)?,
                    Action::CancelStartup(v) => Command::CancelStartup {
                        intent: decode_operation(required(&v.intent)?)?,
                    },
                    Action::SetRemoteAccess(v) => {
                        let policy = management::$p::decode_policy(required(&v.value)?)?;
                        if policy.version.get() != v.expected_version {
                            return Err(ProtocolError::Malformed);
                        }
                        Command::SetRemoteAccess {
                            server: decode_server(required(&v.server)?)?,
                            expected_version: version(v.expected_version)?,
                            value: policy.value,
                        }
                    }
                    Action::GitConfirmed(v) => {
                        Command::Worktree(super::super::git::$p::decode_command(v, server)?)
                    }
                    // Pairing owns its identified extension and the frozen legacy
                    // WorktreeWrite does not convey an exact canonical command.
                    Action::IdentifiedPairing(_) | Action::WorktreeWrite(_) => {
                        return Err(ProtocolError::UnexpectedMessage)
                    }
                };
                command
                    .validate_scope(id.stream())
                    .map_err(|_| ProtocolError::Malformed)?;
                CanonicalOperationRequest::new(id, command, codec)
                    .map_err(|_| ProtocolError::Malformed)
            }
            pub fn encode_request(
                value: &CanonicalOperationRequest,
            ) -> Result<wire::Operation, ProtocolError> {
                let id = value.binding().id;
                value
                    .command()
                    .validate_scope(id.stream())
                    .map_err(|_| ProtocolError::Malformed)?;
                let action = match value.command() {
                    Command::CreateWorkspace {
                        server,
                        source,
                        name,
                    } => {
                        let (draft_mode, name) = encode_name(name);
                        let (source_directory, source_pane) = match source {
                            WorkspaceCreationSource::ServerHome { .. } => (None, None),
                            WorkspaceCreationSource::Directory(v) => {
                                (Some(organization::$p::encode_directory(v)), None)
                            }
                            WorkspaceCreationSource::Pane(v) => (None, Some(encode_pane(*v))),
                        };
                        Action::CreateWorkspace(wire::CreateWorkspace {
                            server: Some(encode_global(server.bytes())),
                            source_directory,
                            source_pane,
                            draft_mode,
                            name,
                            pin: None,
                        })
                    }
                    Command::CreateTab { workspace, source } => {
                        let (source_directory, source_pane, worktree) = match source {
                            TabCreationSource::Directory(v) => {
                                (Some(organization::$p::encode_directory(v)), None, None)
                            }
                            TabCreationSource::Pane(v) => (None, Some(encode_pane(*v)), None),
                            TabCreationSource::Worktree(v) => {
                                (None, None, Some(encode_worktree(*v)))
                            }
                        };
                        Action::CreateTab(wire::CreateTab {
                            workspace: Some(encode_workspace(*workspace)),
                            source_directory,
                            source_pane,
                            worktree,
                        })
                    }
                    Command::CreatePane {
                        tab,
                        source,
                        axis,
                        edge,
                    } => Action::CreatePane(wire::CreatePane {
                        tab: Some(encode_tab(*tab)),
                        source: Some(encode_pane(*source)),
                        edge: organization::edge_number(*axis, *edge),
                    }),
                    Command::RenameWorkspace {
                        workspace,
                        expected_version,
                        draft,
                    } => {
                        let (mode, name) = encode_name(draft);
                        Action::RenameWorkspace(wire::RenameWorkspace {
                            workspace: Some(encode_workspace(*workspace)),
                            expected_name_version: expected_version.get(),
                            mode: if mode == 3 { 0 } else { mode as i32 },
                            name,
                            blank: mode == 3,
                        })
                    }
                    Command::SetPinnedDirectory {
                        workspace,
                        expected_version,
                        value,
                    } => Action::PinWorkspace(wire::PinWorkspace {
                        workspace: Some(encode_workspace(*workspace)),
                        expected_pin_version: expected_version.get(),
                        pin: None,
                        pin_value: value.as_ref().map(organization::$p::encode_directory),
                        clear_pin: value.is_none(),
                    }),
                    Command::RepairPaneDirectory {
                        pane,
                        expected_version,
                        value,
                    } => Action::RepairDirectory(wire::RepairDirectory {
                        pane: Some(encode_pane(*pane)),
                        expected_directory_version: expected_version.get(),
                        directory_value: Some(organization::$p::encode_directory(value)),
                        directory: None,
                        retry_same: false,
                    }),
                    Command::RetryPaneDirectory {
                        pane,
                        expected_version,
                    } => Action::RepairDirectory(wire::RepairDirectory {
                        pane: Some(encode_pane(*pane)),
                        expected_directory_version: expected_version.get(),
                        directory_value: None,
                        directory: None,
                        retry_same: true,
                    }),
                    Command::SetWorktreeLocation {
                        expected_version,
                        template,
                        ..
                    } => Action::ChangeSettings(wire::ChangeSettings {
                        expected_version: expected_version.get(),
                        settings: Some(wire::ServerSettings {
                            version: expected_version.get(),
                            worktree_location_version: expected_version.get(),
                            worktree_location_template: template.as_str().into(),
                            ..Default::default()
                        }),
                    }),
                    Command::AssessClose { target } => Action::AssessClose(wire::AssessClose {
                        target: Some(encode_close_target(*target)),
                        scope: None,
                    }),
                    Command::CloseConfirmed { target, assessment } => {
                        Action::ConfirmClose(wire::ConfirmClose {
                            target: Some(encode_close_target(*target)),
                            reference: Some(encode_assessment_reference(*assessment)),
                            assessment: vec![],
                        })
                    }
                    Command::AssessKillWorkspace { workspace } => {
                        Action::AssessKillWorkspace(wire::AssessKillWorkspace {
                            workspace: Some(encode_workspace(*workspace)),
                        })
                    }
                    Command::KillWorkspaceConfirmed {
                        workspace,
                        assessment,
                    } => Action::KillWorkspace(wire::KillWorkspaceConfirmed {
                        workspace: Some(encode_workspace(*workspace)),
                        assessment: Some(encode_assessment_reference(*assessment)),
                    }),
                    Command::AssessEndAllSessions { server } => {
                        Action::AssessEndAllSessions(wire::AssessEndAllSessions {
                            server: Some(encode_global(server.bytes())),
                        })
                    }
                    Command::EndAllSessions { server, assessment } => {
                        Action::EndAllSessions(wire::EndAllSessions {
                            server: Some(encode_global(server.bytes())),
                            reference: Some(encode_assessment_reference(*assessment)),
                            assessment: vec![],
                        })
                    }
                    Command::CompareAcquire(_) | Command::TakeOver(_) => {
                        Action::IdentifiedControl(control::$p::encode_request(
                            value.command(),
                            NonZeroU64::new(id.sequence().get()).ok_or(ProtocolError::Malformed)?,
                        )?)
                    }
                    Command::ReleaseControl(v) => {
                        Action::ReleaseControl(control::$p::encode_release(*v))
                    }
                    Command::BeginStartup(_)
                    | Command::CompleteStartup(_)
                    | Command::CompleteMeasuredStartup(_) => {
                        Action::IdentifiedStartup(startup::$p::encode_request(value.command())?)
                    }
                    Command::CancelStartup { intent } => {
                        Action::CancelStartup(wire::CancelStartup {
                            intent: Some(encode_operation(*intent)),
                        })
                    }
                    Command::SetRemoteAccess {
                        server,
                        expected_version,
                        value,
                    } => Action::SetRemoteAccess(wire::SetRemoteAccess {
                        server: Some(encode_global(server.bytes())),
                        expected_version: expected_version.get(),
                        value: Some(management::$p::encode_policy(&Versioned {
                            version: *expected_version,
                            value: value.clone(),
                        })),
                    }),
                    Command::Worktree(v) => {
                        Action::GitConfirmed(super::super::git::$p::encode_command(v)?)
                    }
                    Command::RevokePairing { .. }
                    | Command::RefreshWorktrees { .. }
                    | Command::CreateWorktree { .. }
                    | Command::AssessRemoveWorktree { .. }
                    | Command::RemoveWorktreeConfirmed { .. }
                    | Command::Pairing(_) => return Err(ProtocolError::UnexpectedMessage),
                };
                let result = wire::Operation {
                    id: Some(encode_operation(id)),
                    action: Some(action),
                };
                if prost::Message::encoded_len(&result) > MAX_MANAGEMENT_BYTES {
                    return Err(ProtocolError::LimitExceeded);
                }
                Ok(result)
            }
            pub fn encode_conflict(value: &FieldConflict) -> wire::FieldConflict {
                use wire::field_conflict::CurrentValue;
                let mut result = wire::FieldConflict::default();
                match value {
                    FieldConflict::WorkspaceName {
                        workspace,
                        current_value,
                        current_version,
                    } => {
                        result.field = 1;
                        result.target = Some(encode_workspace(*workspace));
                        result.current_version = current_version.get();
                        result.current_value = Some(CurrentValue::Name(wire::NameValue {
                            mode: match current_value.mode {
                                NameMode::Automatic => 1,
                                NameMode::Custom => 2,
                            },
                            name: current_value.display.as_str().into(),
                        }));
                    }
                    FieldConflict::PinnedDirectory {
                        workspace,
                        current_value,
                        current_version,
                    } => {
                        result.field = 2;
                        result.target = Some(encode_workspace(*workspace));
                        result.current_version = current_version.get();
                        result.directory_value = current_value
                            .as_ref()
                            .map(organization::$p::encode_directory);
                        if current_value.is_none() {
                            result.current_value = Some(CurrentValue::Absent(true));
                        }
                    }
                    FieldConflict::PaneDirectory {
                        pane,
                        current_value,
                        current_version,
                    } => {
                        result.field = 3;
                        result.target = Some(encode_pane(*pane));
                        result.current_version = current_version.get();
                        result.directory_value =
                            Some(organization::$p::encode_directory(current_value));
                    }
                    FieldConflict::WorktreeLocation {
                        server,
                        current_value,
                        current_version,
                    } => {
                        result.field = 4;
                        result.server = Some(encode_global(server.bytes()));
                        result.current_version = current_version.get();
                        result.current_value = Some(CurrentValue::Settings(wire::ServerSettings {
                            version: current_version.get(),
                            worktree_location_version: current_version.get(),
                            worktree_location_template: current_value.as_str().into(),
                            ..Default::default()
                        }));
                    }
                    FieldConflict::RemoteAccess {
                        server,
                        current_value,
                        current_version,
                    } => {
                        result.field = 4;
                        result.server = Some(encode_global(server.bytes()));
                        result.current_version = current_version.get();
                        result.remote_access = Some(management::$p::encode_policy(&Versioned {
                            version: *current_version,
                            value: current_value.clone(),
                        }));
                    }
                }
                result
            }
            pub fn decode_conflict(
                value: &wire::FieldConflict,
            ) -> Result<FieldConflict, ProtocolError> {
                use wire::field_conflict::CurrentValue;
                let current_version = version(value.current_version)?;
                let result = match value.field {
                    1 => {
                        let Some(CurrentValue::Name(v)) = &value.current_value else {
                            return Err(ProtocolError::Malformed);
                        };
                        FieldConflict::WorkspaceName {
                            workspace: decode_workspace(required(&value.target)?)?,
                            current_version,
                            current_value: WorkspaceName {
                                mode: match v.mode {
                                    1 => NameMode::Automatic,
                                    2 => NameMode::Custom,
                                    _ => return Err(ProtocolError::Malformed),
                                },
                                display: NameText::new(v.name.clone())
                                    .map_err(|_| ProtocolError::Malformed)?,
                            },
                        }
                    }
                    2 => FieldConflict::PinnedDirectory {
                        workspace: decode_workspace(required(&value.target)?)?,
                        current_version,
                        current_value: value
                            .directory_value
                            .as_ref()
                            .map(organization::$p::decode_directory)
                            .transpose()?,
                    },
                    3 => FieldConflict::PaneDirectory {
                        pane: decode_pane(required(&value.target)?)?,
                        current_version,
                        current_value: organization::$p::decode_directory(required(
                            &value.directory_value,
                        )?)?,
                    },
                    4 => {
                        let server = decode_server(required(&value.server)?)?;
                        match (&value.current_value, &value.remote_access) {
                            (Some(CurrentValue::Settings(v)), None) => {
                                FieldConflict::WorktreeLocation {
                                    server,
                                    current_version,
                                    current_value: WorktreeLocationTemplate::new(
                                        v.worktree_location_template.clone(),
                                    )
                                    .map_err(|_| ProtocolError::Malformed)?,
                                }
                            }
                            (None, Some(v)) => FieldConflict::RemoteAccess {
                                server,
                                current_version,
                                current_value: management::$p::decode_policy(v)?.value,
                            },
                            _ => return Err(ProtocolError::Malformed),
                        }
                    }
                    _ => return Err(ProtocolError::Malformed),
                };
                if encode_conflict(&result) != *value {
                    return Err(ProtocolError::Malformed);
                }
                Ok(result)
            }
            pub fn encode_assessment(
                value: CloseAssessment,
            ) -> Result<wire::CloseAssessment, ProtocolError> {
                let summary = value.summary();
                let (close, kill_workspace, end_all_server) = match summary.target() {
                    AssessmentTarget::Close(v) => (Some(encode_close_target(v)), None, None),
                    AssessmentTarget::KillWorkspace(v) => (None, Some(encode_workspace(v)), None),
                    AssessmentTarget::EndAllSessions(v) => {
                        (None, None, Some(encode_global(v.bytes())))
                    }
                    AssessmentTarget::RemoveWorktree { .. } => {
                        return Err(ProtocolError::UnexpectedMessage)
                    }
                };
                Ok(wire::CloseAssessment {
                    reference: Some(encode_assessment_reference(value.reference())),
                    epoch: value.epoch().get(),
                    close,
                    kill_workspace,
                    end_all_server,
                    workspace_count: summary.workspace_count(),
                    tab_count: summary.tab_count(),
                    pane_count: summary.pane_count(),
                    live_session_count: summary.live_session_count(),
                    requires_confirmation: summary.requires_confirmation(),
                })
            }
            pub fn decode_assessment(
                value: &wire::CloseAssessment,
            ) -> Result<CloseAssessment, ProtocolError> {
                let target = match (&value.close, &value.kill_workspace, &value.end_all_server) {
                    (Some(v), None, None) => AssessmentTarget::Close(decode_close_target(v)?),
                    (None, Some(v), None) => AssessmentTarget::KillWorkspace(decode_workspace(v)?),
                    (None, None, Some(v)) => AssessmentTarget::EndAllSessions(decode_server(v)?),
                    _ => return Err(ProtocolError::Malformed),
                };
                let summary = AssessmentSummary::new(
                    target,
                    value.workspace_count,
                    value.tab_count,
                    value.pane_count,
                    value.live_session_count,
                    value.requires_confirmation,
                )
                .map_err(|_| ProtocolError::Malformed)?;
                CloseAssessment::new(
                    decode_assessment_reference(required(&value.reference)?)?,
                    ServerEpoch::new(value.epoch),
                    summary,
                )
                .map_err(|_| ProtocolError::Malformed)
            }
            pub fn encode_failure(value: OperationFailure) -> wire::OperationFailureResult {
                use OperationFailure::*;
                let (kind, runtime_class) = match value {
                    TargetDeleted => (1, None),
                    StaleAuthority => (2, None),
                    StaleAssessment => (3, None),
                    NotControlled => (4, None),
                    Capacity => (5, None),
                    Exhausted => (6, None),
                    DirectoryUnavailable => (7, None),
                    StorageUnavailable => (8, None),
                    Unsupported => (9, None),
                    Runtime(v) => (10, Some(organization::failure_number(v))),
                    InvalidValue => (11, None),
                    WrongMembership => (12, None),
                    StaleValidation => (13, None),
                    AssessmentExpired => (14, None),
                    BusyRetiring => (15, None),
                };
                wire::OperationFailureResult {
                    kind,
                    runtime_class,
                }
            }
            pub fn decode_failure(
                value: &wire::OperationFailureResult,
            ) -> Result<OperationFailure, ProtocolError> {
                use OperationFailure::*;
                if value.kind != 10 && value.runtime_class.is_some() {
                    return Err(ProtocolError::Malformed);
                }
                Ok(match value.kind {
                    1 => TargetDeleted,
                    2 => StaleAuthority,
                    3 => StaleAssessment,
                    4 => NotControlled,
                    5 => Capacity,
                    6 => Exhausted,
                    7 => DirectoryUnavailable,
                    8 => StorageUnavailable,
                    9 => Unsupported,
                    10 => Runtime(organization::decode_failure(
                        value.runtime_class.ok_or(ProtocolError::Malformed)?,
                    )?),
                    11 => InvalidValue,
                    12 => WrongMembership,
                    13 => StaleValidation,
                    14 => AssessmentExpired,
                    15 => BusyRetiring,
                    _ => return Err(ProtocolError::Malformed),
                })
            }
            pub fn encode_outcome(
                operation: OperationId,
                revision: OrganizationRevision,
                value: &Outcome,
            ) -> Result<wire::Outcome, ProtocolError> {
                use wire::outcome::Result as R;
                value
                    .validate_scope(operation.stream())
                    .map_err(|_| ProtocolError::Malformed)?;
                let result = match value {
                    Outcome::Applied => R::Committed(true),
                    Outcome::CreatedWorkspace {
                        workspace,
                        tab,
                        pane,
                    } => R::LegacyCreatedWorkspace(wire::LegacyCreatedWorkspaceResult {
                        workspace: Some(encode_workspace(*workspace)),
                        tab: Some(encode_tab(*tab)),
                        pane: Some(encode_pane(*pane)),
                    }),
                    Outcome::CreatedTab { tab, pane } => {
                        R::LegacyCreatedTab(wire::LegacyCreatedTabResult {
                            tab: Some(encode_tab(*tab)),
                            pane: Some(encode_pane(*pane)),
                        })
                    }
                    Outcome::CreatedPane { pane } => R::LegacyCreatedPane(encode_pane(*pane)),
                    Outcome::CreatedWorktree { worktree } => {
                        R::LegacyCreatedWorktree(encode_worktree(*worktree))
                    }
                    Outcome::CreatedWithControl(v) => {
                        let (workspace, tab, pane) = match v.entities() {
                            CreatedEntities::Workspace {
                                workspace,
                                tab,
                                pane,
                            } => (
                                Some(encode_workspace(workspace)),
                                Some(encode_tab(tab)),
                                pane,
                            ),
                            CreatedEntities::Tab { tab, pane } => {
                                (None, Some(encode_tab(tab)), pane)
                            }
                            CreatedEntities::Pane(pane) => (None, None, pane),
                        };
                        R::CreatedWithControl(wire::CreatedOrganization {
                            workspace,
                            tab,
                            pane: Some(encode_pane(pane)),
                            control: Some(control::$p::encode_grant(v.control())),
                        })
                    }
                    Outcome::ControlGranted(v) => R::ControlGranted(control::$p::encode_grant(*v)),
                    Outcome::ControlAcquired(v) => {
                        R::LegacyControlAcquired(wire::LegacyControlAcquiredResult {
                            pane: Some(encode_pane(v.pane())),
                            client: Some(encode_client(v.client())),
                            generation: v.generation().get(),
                        })
                    }
                    Outcome::ControlUnavailable(v) => {
                        R::ControlUnavailable(wire::ControlUnavailableResult {
                            control: Some(match v {
                                ControlRecord::Unclaimed => {
                                    wire::control_unavailable_result::Control::Unclaimed(true)
                                }
                                ControlRecord::Held { client, generation } => {
                                    wire::control_unavailable_result::Control::Held(
                                        wire::HeldControl {
                                            client: Some(encode_client(*client)),
                                            generation: generation.get(),
                                        },
                                    )
                                }
                            }),
                        })
                    }
                    Outcome::StartupAwaitingMeasurement { intent } => {
                        R::StartupAwaitingMeasurement(wire::StartupAwaitingMeasurementResult {
                            intent: Some(encode_operation(*intent)),
                        })
                    }
                    Outcome::SessionStarted(v) => R::SessionStarted(encode_session(*v)),
                    Outcome::Assessed {
                        assessment,
                        affected_panes,
                        running_sessions,
                    } => R::LegacyAssessed(wire::LegacyAssessedResult {
                        assessment: Some(encode_assessment_reference(*assessment)),
                        affected_panes: *affected_panes,
                        running_sessions: *running_sessions,
                    }),
                    Outcome::Assessment(v) => R::ExactAssessment(encode_assessment(**v)?),
                    Outcome::Conflict(v) => R::Conflict(encode_conflict(v)),
                    Outcome::Failed(v) => R::OperationFailure(encode_failure(*v)),
                    Outcome::Git(v) => R::GitResult(super::super::git::$p::encode_outcome(v)?),
                    Outcome::Pairing(_) => return Err(ProtocolError::UnexpectedMessage),
                };
                let result = wire::Outcome {
                    operation: Some(encode_operation(operation)),
                    publication_revision: revision.get(),
                    result: Some(result),
                };
                if prost::Message::encoded_len(&result) > MAX_MANAGEMENT_BYTES {
                    return Err(ProtocolError::LimitExceeded);
                }
                Ok(result)
            }
            pub fn decode_outcome(
                value: &wire::Outcome,
                operation: OperationId,
            ) -> Result<(OrganizationRevision, Outcome), ProtocolError> {
                use wire::outcome::Result as R;
                if prost::Message::encoded_len(value) > MAX_MANAGEMENT_BYTES {
                    return Err(ProtocolError::LimitExceeded);
                }
                if decode_operation(required(&value.operation)?)? != operation {
                    return Err(ProtocolError::Authentication);
                }
                let outcome = match required(&value.result)? {
                    R::Committed(true) => Outcome::Applied,
                    R::LegacyCreatedWorkspace(v) => Outcome::CreatedWorkspace {
                        workspace: decode_workspace(required(&v.workspace)?)?,
                        tab: decode_tab(required(&v.tab)?)?,
                        pane: decode_pane(required(&v.pane)?)?,
                    },
                    R::LegacyCreatedTab(v) => Outcome::CreatedTab {
                        tab: decode_tab(required(&v.tab)?)?,
                        pane: decode_pane(required(&v.pane)?)?,
                    },
                    R::LegacyCreatedPane(v) => Outcome::CreatedPane {
                        pane: decode_pane(v)?,
                    },
                    R::LegacyCreatedWorktree(v) => Outcome::CreatedWorktree {
                        worktree: decode_worktree(v)?,
                    },
                    R::CreatedWithControl(v) => {
                        let pane = decode_pane(required(&v.pane)?)?;
                        let entities = match (&v.workspace, &v.tab) {
                            (Some(w), Some(t)) => CreatedEntities::Workspace {
                                workspace: decode_workspace(w)?,
                                tab: decode_tab(t)?,
                                pane,
                            },
                            (None, Some(t)) => CreatedEntities::Tab {
                                tab: decode_tab(t)?,
                                pane,
                            },
                            (None, None) => CreatedEntities::Pane(pane),
                            _ => return Err(ProtocolError::Malformed),
                        };
                        Outcome::CreatedWithControl(
                            CreatedOrganization::new(
                                entities,
                                control::$p::decode_grant(required(&v.control)?)?,
                            )
                            .map_err(|_| ProtocolError::Malformed)?,
                        )
                    }
                    R::ControlGranted(v) => Outcome::ControlGranted(control::$p::decode_grant(v)?),
                    R::LegacyControlAcquired(v) => Outcome::ControlAcquired(
                        ControlGrant::new(
                            decode_pane(required(&v.pane)?)?,
                            decode_client(required(&v.client)?)?,
                            ControlGeneration::new(v.generation),
                        )
                        .map_err(|_| ProtocolError::Malformed)?,
                    ),
                    R::ControlUnavailable(v) => {
                        Outcome::ControlUnavailable(match required(&v.control)? {
                            wire::control_unavailable_result::Control::Unclaimed(true) => {
                                ControlRecord::Unclaimed
                            }
                            wire::control_unavailable_result::Control::Held(v)
                                if v.generation != 0 =>
                            {
                                ControlRecord::Held {
                                    client: decode_client(required(&v.client)?)?,
                                    generation: ControlGeneration::new(v.generation),
                                }
                            }
                            _ => return Err(ProtocolError::Malformed),
                        })
                    }
                    R::StartupAwaitingMeasurement(v) => Outcome::StartupAwaitingMeasurement {
                        intent: decode_operation(required(&v.intent)?)?,
                    },
                    R::SessionStarted(v) => {
                        let session = decode_session(v)?;
                        if session.generation.get() == 0 {
                            return Err(ProtocolError::Malformed);
                        }
                        Outcome::SessionStarted(session)
                    }
                    R::LegacyAssessed(v) => Outcome::Assessed {
                        assessment: decode_assessment_reference(required(&v.assessment)?)?,
                        affected_panes: v.affected_panes,
                        running_sessions: v.running_sessions,
                    },
                    R::ExactAssessment(v) => Outcome::Assessment(Box::new(decode_assessment(v)?)),
                    R::Conflict(v) => Outcome::Conflict(decode_conflict(v)?),
                    R::OperationFailure(v) => Outcome::Failed(decode_failure(v)?),
                    R::GitResult(v) => Outcome::Git(super::super::git::$p::decode_outcome(
                        v,
                        operation.stream().server(),
                    )?),
                    R::PairingResult(_) => return Err(ProtocolError::UnexpectedMessage),
                    // Ambiguous frozen results remain readable at the wire layer;
                    // they cannot acquire an invented entity kind or exact failure.
                    _ => return Err(ProtocolError::Malformed),
                };
                outcome
                    .validate_scope(operation.stream())
                    .map_err(|_| ProtocolError::Malformed)?;
                Ok((
                    OrganizationRevision::new(value.publication_revision),
                    outcome,
                ))
            }
            pub fn encode_lookup_request(
                binding: OperationBinding,
            ) -> wire::OperationLookupRequest {
                wire::OperationLookupRequest {
                    operation: Some(encode_operation(binding.id)),
                    request_digest: binding.digest.bytes().to_vec(),
                }
            }
            pub fn decode_lookup_request(
                value: &wire::OperationLookupRequest,
                pairing: PairingId,
                client: ClientId,
            ) -> Result<OperationBinding, ProtocolError> {
                let id = decode_operation(required(&value.operation)?)?;
                if id.stream().pairing() != pairing || id.stream().client() != client {
                    return Err(ProtocolError::Authentication);
                }
                Ok(OperationBinding {
                    id,
                    digest: RequestDigest::from_bytes(
                        value
                            .request_digest
                            .as_slice()
                            .try_into()
                            .map_err(|_| ProtocolError::Malformed)?,
                    ),
                })
            }
            pub fn encode_lookup_result(
                binding: OperationBinding,
                value: &OperationLookup,
            ) -> Result<wire::OperationLookupResult, ProtocolError> {
                use wire::operation_lookup_result::Disposition as D;
                let disposition = match value {
                    OperationLookup::NeverAdmitted => D::NeverAdmitted(true),
                    OperationLookup::Pending => D::Pending(true),
                    OperationLookup::AcknowledgedRetired => D::AcknowledgedRetired(true),
                    OperationLookup::SequenceGap { expected } => D::SequenceGap(expected.get()),
                    OperationLookup::IdentityReusedWithDifferentContents => D::IdentityReused(true),
                    OperationLookup::Completed {
                        result,
                        publication_revision,
                    } => D::Completed(Box::new(encode_outcome(
                        binding.id,
                        *publication_revision,
                        result,
                    )?)),
                };
                let result = wire::OperationLookupResult {
                    operation: Some(encode_operation(binding.id)),
                    request_digest: binding.digest.bytes().to_vec(),
                    disposition: Some(disposition),
                };
                if prost::Message::encoded_len(&result) > MAX_MANAGEMENT_BYTES {
                    return Err(ProtocolError::LimitExceeded);
                }
                Ok(result)
            }
            pub fn decode_lookup_result(
                value: &wire::OperationLookupResult,
                binding: OperationBinding,
            ) -> Result<OperationLookup, ProtocolError> {
                use wire::operation_lookup_result::Disposition as D;
                if prost::Message::encoded_len(value) > MAX_MANAGEMENT_BYTES {
                    return Err(ProtocolError::LimitExceeded);
                }
                if decode_lookup_request(
                    &wire::OperationLookupRequest {
                        operation: value.operation.clone(),
                        request_digest: value.request_digest.clone(),
                    },
                    binding.id.stream().pairing(),
                    binding.id.stream().client(),
                )? != binding
                {
                    return Err(ProtocolError::Authentication);
                }
                Ok(match required(&value.disposition)? {
                    D::NeverAdmitted(true) => OperationLookup::NeverAdmitted,
                    D::Pending(true) => OperationLookup::Pending,
                    D::AcknowledgedRetired(true) => OperationLookup::AcknowledgedRetired,
                    D::SequenceGap(v) => OperationLookup::SequenceGap {
                        expected: OperationSequence::new(*v)
                            .map_err(|_| ProtocolError::Malformed)?,
                    },
                    D::IdentityReused(true) => OperationLookup::IdentityReusedWithDifferentContents,
                    D::Completed(v) => {
                        let (publication_revision, result) = decode_outcome(v, binding.id)?;
                        OperationLookup::Completed {
                            result,
                            publication_revision,
                        }
                    }
                    _ => return Err(ProtocolError::Malformed),
                })
            }
            pub fn decode_acknowledgement(
                value: &wire::Acknowledgement,
                pairing: PairingId,
                client: ClientId,
            ) -> Result<(OperationStreamId, OperationSequence), ProtocolError> {
                if decode_local(required(&value.pairing)?)? != pairing.local()
                    || decode_local(required(&value.client)?)? != client.local()
                    || pairing.server() != client.server()
                {
                    return Err(ProtocolError::Authentication);
                }
                let stream = OperationStreamId::new(
                    OperationStreamKey::new(
                        client.server(),
                        decode_local(required(&value.stream)?)?,
                    )
                    .map_err(|_| ProtocolError::Malformed)?,
                    pairing,
                    client,
                )
                .map_err(|_| ProtocolError::Malformed)?;
                Ok((
                    stream,
                    OperationSequence::new(value.contiguous_sequence)
                        .map_err(|_| ProtocolError::Malformed)?,
                ))
            }
            pub fn encode_acknowledgement(
                stream: OperationStreamId,
                through: OperationSequence,
            ) -> wire::Acknowledgement {
                wire::Acknowledgement {
                    pairing: Some(encode_local(stream.pairing().local())),
                    client: Some(encode_local(stream.client().local())),
                    stream: Some(encode_local(stream.key().local())),
                    contiguous_sequence: through.get(),
                }
            }
        }
    };
}
profile!(v1);
profile!(v2);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{codec::Payload, wire};
    use spaceterm_core::operation_ledger::canonical::CanonicalCodec;

    macro_rules! cases {
        ($name:ident, $p:ident, $major:ident) => {
            #[test]
            fn $name() {
                let server = ServerId::new([1; 16]);
                let client = ClientId::new(server, 2).unwrap();
                let stream = OperationStreamId::new(
                    OperationStreamKey::new(server, 3).unwrap(),
                    PairingId::new(server, 1).unwrap(),
                    client,
                )
                .unwrap();
                let workspace = WorkspaceId::new(server, 4).unwrap();
                let tab = TabId::new(server, 5).unwrap();
                let pane = PaneId::new(server, 6).unwrap();
                let directory = ServerDirectory::new(server, b"/owned".to_vec()).unwrap();
                let assessment = AssessmentRef {
                    operation: OperationId::new(stream, OperationSequence::new(40).unwrap()),
                    digest: RequestDigest::from_bytes([12; 32]),
                };
                let grant = ControlGrant::new(pane, client, ControlGeneration::new(7)).unwrap();
                let commands = [
                    Command::CreateWorkspace {
                        server,
                        source: WorkspaceCreationSource::ServerHome { server },
                        name: NameDraft::Automatic,
                    },
                    Command::CreateWorkspace {
                        server,
                        source: WorkspaceCreationSource::Directory(directory.clone()),
                        name: NameDraft::Blank,
                    },
                    Command::CreateWorkspace {
                        server,
                        source: WorkspaceCreationSource::Pane(pane),
                        name: NameDraft::Custom(NameText::new("named".into()).unwrap()),
                    },
                    Command::CreateTab {
                        workspace,
                        source: TabCreationSource::Pane(pane),
                    },
                    Command::CreateTab {
                        workspace,
                        source: TabCreationSource::Directory(directory.clone()),
                    },
                    Command::CreateTab {
                        workspace,
                        source: TabCreationSource::Worktree(WorktreeId::new(server, 7).unwrap()),
                    },
                    Command::CreatePane {
                        tab,
                        source: pane,
                        axis: SplitAxis::Horizontal,
                        edge: SplitEdge::Before,
                    },
                    Command::RenameWorkspace {
                        workspace,
                        expected_version: FieldVersion::new(8),
                        draft: NameDraft::Blank,
                    },
                    Command::SetPinnedDirectory {
                        workspace,
                        expected_version: FieldVersion::new(8),
                        value: Some(directory.clone()),
                    },
                    Command::SetPinnedDirectory {
                        workspace,
                        expected_version: FieldVersion::new(8),
                        value: None,
                    },
                    Command::RepairPaneDirectory {
                        pane,
                        expected_version: FieldVersion::new(8),
                        value: directory,
                    },
                    Command::RetryPaneDirectory {
                        pane,
                        expected_version: FieldVersion::new(8),
                    },
                    Command::AssessClose {
                        target: CloseTarget::Pane(pane),
                    },
                    Command::AssessKillWorkspace { workspace },
                    Command::AssessEndAllSessions { server },
                    Command::CloseConfirmed {
                        target: CloseTarget::Tab(tab),
                        assessment,
                    },
                    Command::CloseConfirmed {
                        target: CloseTarget::WorktreeTabs {
                            workspace,
                            worktree: WorktreeId::new(server, 7).unwrap(),
                        },
                        assessment,
                    },
                    Command::KillWorkspaceConfirmed {
                        workspace,
                        assessment,
                    },
                    Command::EndAllSessions { server, assessment },
                    Command::SetWorktreeLocation {
                        server,
                        expected_version: FieldVersion::new(8),
                        template: WorktreeLocationTemplate::new(
                            "/worktrees/{repository}/{branch}".into(),
                        )
                        .unwrap(),
                    },
                    Command::CompareAcquire(spaceterm_core::control::CompareAcquireRequest {
                        epoch: ServerEpoch::new(8),
                        pane,
                        client,
                        expected_generation: ControlGeneration::new(0),
                    }),
                    Command::TakeOver(spaceterm_core::control::TakeOverRequest {
                        epoch: ServerEpoch::new(8),
                        pane,
                        client,
                    }),
                    Command::ReleaseControl(spaceterm_core::control::ReleaseControlRequest {
                        epoch: ServerEpoch::new(8),
                        grant,
                    }),
                    Command::CancelStartup {
                        intent: assessment.operation,
                    },
                    Command::SetRemoteAccess {
                        server,
                        expected_version: FieldVersion::new(8),
                        value: spaceterm_core::remote_access::RemoteAccessPolicy::disabled(server),
                    },
                ];
                for (index, command) in commands.into_iter().enumerate() {
                    let id =
                        OperationId::new(stream, OperationSequence::new(index as u64 + 1).unwrap());
                    let original =
                        CanonicalOperationRequest::new(id, command, &CanonicalCodec).unwrap();
                    let encoded = $p::encode_request(&original).unwrap();
                    Payload::$major(wire::$p::Message {
                        kind: Some(wire::$p::message::Kind::Operation(encoded.clone())),
                    })
                    .encode()
                    .expect("converted request passes wire admission");
                    let decoded =
                        $p::decode_request(&encoded, stream.pairing(), client, &CanonicalCodec)
                            .unwrap();
                    assert_eq!(decoded, original);
                    assert!(
                        $p::decode_request(
                            &encoded,
                            stream.pairing(),
                            ClientId::new(server, 9).unwrap(),
                            &CanonicalCodec
                        )
                        .is_err()
                    );
                }
                let id = OperationId::new(stream, OperationSequence::new(20).unwrap());
                let original = CanonicalOperationRequest::new(
                    id,
                    Command::CreateWorkspace {
                        server,
                        source: WorkspaceCreationSource::ServerHome { server },
                        name: NameDraft::Automatic,
                    },
                    &CanonicalCodec,
                )
                .unwrap();
                let mut ambiguous = $p::encode_request(&original).unwrap();
                let Some(wire::$p::operation::Action::CreateWorkspace(value)) =
                    &mut ambiguous.action
                else {
                    panic!("create");
                };
                value.source_pane = Some(crate::convert::identity::encode_pane(pane));
                value.source_directory = Some($crate::convert::organization::$p::encode_directory(
                    &ServerDirectory::new(server, b"/another".to_vec()).unwrap(),
                ));
                assert!(
                    $p::decode_request(&ambiguous, stream.pairing(), client, &CanonicalCodec)
                        .is_err()
                );
            }
        };
    }
    cases!(ordinary_operation_round_trip_major_1, v1, Major1);
    cases!(ordinary_operation_round_trip_major_2, v2, Major2);
    macro_rules! outcomes {
        ($name:ident, $p:ident, $major:ident) => {
            #[test]
            fn $name() {
                let server = ServerId::new([1; 16]);
                let client = ClientId::new(server, 2).unwrap();
                let stream = OperationStreamId::new(
                    OperationStreamKey::new(server, 3).unwrap(),
                    PairingId::new(server, 1).unwrap(),
                    client,
                )
                .unwrap();
                let id = OperationId::new(stream, OperationSequence::new(1).unwrap());
                let pane = PaneId::new(server, 6).unwrap();
                let grant = ControlGrant::new(pane, client, ControlGeneration::new(7)).unwrap();
                let provenance = GrantedControl::new(ServerEpoch::new(8), grant).unwrap();
                let revision = OrganizationRevision::new(9);
                let mut outcomes = vec![
                    Outcome::Applied,
                    Outcome::CreatedPane { pane },
                    Outcome::ControlGranted(provenance),
                    Outcome::CreatedWithControl(
                        CreatedOrganization::new(CreatedEntities::Pane(pane), provenance).unwrap(),
                    ),
                    Outcome::ControlAcquired(grant),
                    Outcome::ControlUnavailable(ControlRecord::Unclaimed),
                    Outcome::ControlUnavailable(ControlRecord::Held {
                        client,
                        generation: ControlGeneration::new(7),
                    }),
                    Outcome::StartupAwaitingMeasurement { intent: id },
                    Outcome::SessionStarted(SessionKey {
                        pane,
                        epoch: ServerEpoch::new(8),
                        generation: SessionGeneration::new(10),
                    }),
                ];
                for failure in [
                    OperationFailure::TargetDeleted,
                    OperationFailure::StaleAuthority,
                    OperationFailure::StaleAssessment,
                    OperationFailure::NotControlled,
                    OperationFailure::Capacity,
                    OperationFailure::Exhausted,
                    OperationFailure::DirectoryUnavailable,
                    OperationFailure::StorageUnavailable,
                    OperationFailure::Unsupported,
                    OperationFailure::InvalidValue,
                    OperationFailure::WrongMembership,
                    OperationFailure::StaleValidation,
                    OperationFailure::AssessmentExpired,
                    OperationFailure::BusyRetiring,
                    OperationFailure::Runtime(FailureClass::RuntimeFault),
                ] {
                    outcomes.push(Outcome::Failed(failure));
                }
                let workspace = WorkspaceId::new(server, 4).unwrap();
                let tab = TabId::new(server, 5).unwrap();
                let directory = ServerDirectory::new(server, b"/owned".to_vec()).unwrap();
                let version = FieldVersion::new(8);
                let assessment = AssessmentRef {
                    operation: id,
                    digest: RequestDigest::from_bytes([11; 32]),
                };
                outcomes.extend([
                    Outcome::CreatedWorkspace {
                        workspace,
                        tab,
                        pane,
                    },
                    Outcome::CreatedTab { tab, pane },
                    Outcome::CreatedWorktree {
                        worktree: WorktreeId::new(server, 7).unwrap(),
                    },
                    Outcome::CreatedWithControl(
                        CreatedOrganization::new(
                            CreatedEntities::Workspace {
                                workspace,
                                tab,
                                pane,
                            },
                            provenance,
                        )
                        .unwrap(),
                    ),
                    Outcome::CreatedWithControl(
                        CreatedOrganization::new(CreatedEntities::Tab { tab, pane }, provenance)
                            .unwrap(),
                    ),
                    Outcome::Assessed {
                        assessment,
                        affected_panes: 3,
                        running_sessions: 2,
                    },
                    Outcome::Assessment(Box::new(
                        CloseAssessment::new(
                            assessment,
                            ServerEpoch::new(8),
                            AssessmentSummary::new(
                                AssessmentTarget::KillWorkspace(workspace),
                                1,
                                2,
                                3,
                                2,
                                true,
                            )
                            .unwrap(),
                        )
                        .unwrap(),
                    )),
                    Outcome::Conflict(FieldConflict::WorkspaceName {
                        workspace,
                        current_value: WorkspaceName {
                            mode: NameMode::Custom,
                            display: NameText::new("retained draft".into()).unwrap(),
                        },
                        current_version: version,
                    }),
                    Outcome::Conflict(FieldConflict::PinnedDirectory {
                        workspace,
                        current_value: Some(directory.clone()),
                        current_version: version,
                    }),
                    Outcome::Conflict(FieldConflict::PinnedDirectory {
                        workspace,
                        current_value: None,
                        current_version: version,
                    }),
                    Outcome::Conflict(FieldConflict::PaneDirectory {
                        pane,
                        current_value: directory,
                        current_version: version,
                    }),
                    Outcome::Conflict(FieldConflict::WorktreeLocation {
                        server,
                        current_value: WorktreeLocationTemplate::new(
                            "/worktrees/{repository}/{branch}".into(),
                        )
                        .unwrap(),
                        current_version: version,
                    }),
                    Outcome::Conflict(FieldConflict::RemoteAccess {
                        server,
                        current_value: spaceterm_core::remote_access::RemoteAccessPolicy::disabled(
                            server,
                        ),
                        current_version: version,
                    }),
                ]);
                for (index, outcome) in outcomes.into_iter().enumerate() {
                    let encoded = $p::encode_outcome(id, revision, &outcome).unwrap();
                    let admission = Payload::$major(wire::$p::Message {
                        kind: Some(wire::$p::message::Kind::Outcome(encoded.clone())),
                    })
                    .encode();
                    assert!(
                        admission.is_ok(),
                        "outcome index {index} failed wire admission"
                    );
                    assert_eq!(
                        $p::decode_outcome(&encoded, id).unwrap(),
                        (revision, outcome.clone())
                    );
                    assert!(
                        $p::decode_outcome(
                            &encoded,
                            OperationId::new(stream, OperationSequence::new(2).unwrap())
                        )
                        .is_err()
                    );
                    let completed = OperationLookup::Completed {
                        result: outcome,
                        publication_revision: revision,
                    };
                    let binding = OperationBinding {
                        id,
                        digest: RequestDigest::from_bytes([11; 32]),
                    };
                    let encoded = $p::encode_lookup_result(binding, &completed).unwrap();
                    assert_eq!(
                        $p::decode_lookup_result(&encoded, binding).unwrap(),
                        completed
                    );
                }
                let binding = OperationBinding {
                    id,
                    digest: RequestDigest::from_bytes([11; 32]),
                };
                for value in [
                    OperationLookup::NeverAdmitted,
                    OperationLookup::Pending,
                    OperationLookup::AcknowledgedRetired,
                    OperationLookup::SequenceGap {
                        expected: OperationSequence::new(2).unwrap(),
                    },
                    OperationLookup::IdentityReusedWithDifferentContents,
                ] {
                    assert_eq!(
                        $p::decode_lookup_result(
                            &$p::encode_lookup_result(binding, &value).unwrap(),
                            binding
                        )
                        .unwrap(),
                        value
                    );
                }
            }
        };
    }
    outcomes!(retained_outcomes_and_lookup_round_trip_major_1, v1, Major1);
    outcomes!(retained_outcomes_and_lookup_round_trip_major_2, v2, Major2);
    #[test]
    fn nested_retained_results_require_the_same_authenticated_capability_as_direct_results() {
        use crate::negotiation::CAPABILITY_CANONICAL_DOMAIN;
        macro_rules! check {
            ($p:ident, $major:ident, $number:literal) => {{
                let server = ServerId::new([1; 16]);
                let stream = OperationStreamId::new(
                    OperationStreamKey::new(server, 3).unwrap(),
                    PairingId::new(server, 1).unwrap(),
                    ClientId::new(server, 2).unwrap(),
                )
                .unwrap();
                let id = OperationId::new(stream, OperationSequence::new(1).unwrap());
                let binding = OperationBinding {
                    id,
                    digest: RequestDigest::from_bytes([11; 32]),
                };
                for outcome in [
                    Outcome::Failed(OperationFailure::StaleAuthority),
                    Outcome::Conflict(FieldConflict::PinnedDirectory {
                        workspace: WorkspaceId::new(server, 1).unwrap(),
                        current_value: Some(
                            ServerDirectory::new(server, b"/owned".to_vec()).unwrap(),
                        ),
                        current_version: FieldVersion::new(1),
                    }),
                ] {
                    let direct = Payload::$major(wire::$p::Message {
                        kind: Some(wire::$p::message::Kind::Outcome(
                            $p::encode_outcome(id, OrganizationRevision::new(1), &outcome).unwrap(),
                        )),
                    });
                    let nested = Payload::$major(wire::$p::Message {
                        kind: Some(wire::$p::message::Kind::OperationLookupResult(
                            $p::encode_lookup_result(
                                binding,
                                &OperationLookup::Completed {
                                    result: outcome,
                                    publication_revision: OrganizationRevision::new(1),
                                },
                            )
                            .unwrap(),
                        )),
                    });
                    for nested in [direct, nested] {
                        for capabilities in [vec![], vec![CAPABILITY_CANONICAL_DOMAIN]] {
                            let (mut client, mut server) =
                                super::super::organization::tests::connections(
                                    $number,
                                    capabilities.clone(),
                                );
                            if capabilities.is_empty() {
                                assert!(matches!(
                                    server.encode(&nested),
                                    Err(crate::ProtocolError::Negotiation)
                                ));
                                let fallback = Payload::$major(wire::$p::Message {
                                    kind: Some(wire::$p::message::Kind::Outcome(
                                        wire::$p::Outcome {
                                            operation: Some(
                                                crate::convert::identity::encode_operation(id),
                                            ),
                                            publication_revision: 1,
                                            result: Some(wire::$p::outcome::Result::Committed(
                                                true,
                                            )),
                                        },
                                    )),
                                });
                                assert_eq!(
                                    client.decode(&server.encode(&fallback).unwrap()).unwrap(),
                                    fallback
                                );
                            } else {
                                assert_eq!(
                                    client.decode(&server.encode(&nested).unwrap()).unwrap(),
                                    nested
                                );
                            }
                        }
                    }
                }
            }};
        }
        check!(v1, Major1, 1);
        check!(v2, Major2, 2);
    }
}
