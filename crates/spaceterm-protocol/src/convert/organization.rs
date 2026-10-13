//! Revision-bound organization records. Directories are server data, not native
//! authority. Multipart parts remain uninstalled until the complete graph validates.

use super::identity::*;
use crate::{ProtocolError, codec::Payload};
use spaceterm_core::{authority::*, identity::*, organization::*, organization_retained::*};
use std::ops::Range;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationPart {
    pub request: u64,
    pub generation: u64,
    pub transfer: u64,
    pub index: u32,
    pub total: u32,
    pub epoch: ServerEpoch,
    pub base: OrganizationRevision,
    pub revision: OrganizationRevision,
}
impl PublicationPart {
    pub fn validate(self) -> Result<Self, ProtocolError> {
        if self.request == 0
            || self.transfer == 0
            || self.epoch.get() == 0
            || self.total == 0
            || self.total > spaceterm_core::management::MAX_PUBLICATION_PARTS
            || self.index >= self.total
            || self.base > self.revision
        {
            return Err(ProtocolError::Malformed);
        }
        Ok(self)
    }
}
/// A partial graph. Only complete multipart assembly can construct a Snapshot.
pub struct SnapshotPart {
    pub part: PublicationPart,
    pub state: OrganizationState,
    pub runtime: Vec<PaneRuntimeRecord>,
}

pub fn record_count(value: &OrganizationSnapshot) -> usize {
    let s = value.state();
    s.workspaces.len() + s.tabs.len() + s.panes.len() + s.repositories.len() + s.worktrees.len()
}
/// Borrowed wire-size bounds for page planning. The fixed allowances cover tags,
/// length prefixes, 64-bit varints and qualified identities in either profile.
/// No generated DTO, directory copy or string copy is constructed here.
pub fn snapshot_record_bounds(
    value: &OrganizationSnapshot,
    mut record: impl FnMut(usize) -> Result<(), ProtocolError>,
) -> Result<(), ProtocolError> {
    let state = value.state();
    let facts = state.retained.as_ref().ok_or(ProtocolError::Malformed)?;
    let directory = |value: &ServerDirectory| value.bytes().len() + 64;
    for workspace in &state.workspaces {
        let basis = facts
            .workspace_names()
            .iter()
            .find(|basis| basis.workspace() == workspace.id)
            .ok_or(ProtocolError::Malformed)?;
        record(
            256 + workspace.name.value.display.as_str().len()
                + basis.assigned_automatic().as_str().len()
                + basis.automatic_base().as_str().len()
                + workspace
                    .pinned_directory
                    .value
                    .as_ref()
                    .map_or(0, directory),
        )?;
    }
    for tab in &state.tabs {
        let default = facts
            .creation_defaults()
            .iter()
            .find(|default| default.tab() == tab.id)
            .ok_or(ProtocolError::Malformed)?;
        let bytes = default
            .replay_order()
            .len()
            .checked_mul(64)
            .and_then(|bytes| bytes.checked_add(256))
            .ok_or(ProtocolError::LimitExceeded)?;
        record(bytes)?;
    }
    for pane in &state.panes {
        record(
            512 + directory(&pane.starting_directory)
                + pane
                    .confirmed_restoration_directory
                    .value
                    .as_ref()
                    .map_or(0, directory),
        )?;
    }
    for repository in &state.repositories {
        record(256 + directory(&repository.directory))?;
    }
    for worktree in &state.worktrees {
        record(256 + directory(&worktree.directory))?;
    }
    Ok(())
}

/// Includes the worst-case multipart header and the repeated settings record.
pub fn snapshot_header_bound(value: &OrganizationSnapshot) -> usize {
    1024 + value
        .state()
        .settings
        .worktree_location
        .value
        .as_str()
        .len()
}

fn version(value: u64) -> Result<FieldVersion, ProtocolError> {
    if value == 0 {
        Err(ProtocolError::Malformed)
    } else {
        Ok(FieldVersion::new(value))
    }
}
fn ordinal(value: u64) -> Result<CreationOrdinal, ProtocolError> {
    if value == 0 {
        Err(ProtocolError::Malformed)
    } else {
        Ok(CreationOrdinal::new(value))
    }
}
pub fn failure_number(value: FailureClass) -> i32 {
    match value {
        FailureClass::LaunchUnavailable => 1,
        FailureClass::RuntimeUnavailable => 2,
        FailureClass::RuntimeFault => 3,
        FailureClass::ResourceLimit => 4,
        FailureClass::StorageUnavailable => 5,
        FailureClass::UnsafeStorage => 6,
        FailureClass::AuthorizationDenied => 7,
        FailureClass::DirectoryUnavailable => 8,
        FailureClass::InvalidRequest => 9,
        FailureClass::StaleAuthority => 10,
        FailureClass::Unsupported => 11,
        FailureClass::Internal => 12,
    }
}
pub fn decode_failure(value: i32) -> Result<FailureClass, ProtocolError> {
    Ok(match value {
        1 => FailureClass::LaunchUnavailable,
        2 => FailureClass::RuntimeUnavailable,
        3 => FailureClass::RuntimeFault,
        4 => FailureClass::ResourceLimit,
        5 => FailureClass::StorageUnavailable,
        6 => FailureClass::UnsafeStorage,
        7 => FailureClass::AuthorizationDenied,
        8 => FailureClass::DirectoryUnavailable,
        9 => FailureClass::InvalidRequest,
        10 => FailureClass::StaleAuthority,
        11 => FailureClass::Unsupported,
        12 => FailureClass::Internal,
        _ => return Err(ProtocolError::Malformed),
    })
}
pub fn edge_number(axis: SplitAxis, edge: SplitEdge) -> i32 {
    match (axis, edge) {
        (SplitAxis::Horizontal, SplitEdge::Before) => 1,
        (SplitAxis::Horizontal, SplitEdge::After) => 2,
        (SplitAxis::Vertical, SplitEdge::Before) => 3,
        (SplitAxis::Vertical, SplitEdge::After) => 4,
    }
}
pub fn decode_edge(value: i32) -> Result<(SplitAxis, SplitEdge), ProtocolError> {
    Ok(match value {
        1 => (SplitAxis::Horizontal, SplitEdge::Before),
        2 => (SplitAxis::Horizontal, SplitEdge::After),
        3 => (SplitAxis::Vertical, SplitEdge::Before),
        4 => (SplitAxis::Vertical, SplitEdge::After),
        _ => return Err(ProtocolError::Malformed),
    })
}
macro_rules! profile {
    ($p:ident, $variant:ident) => {
        pub mod $p {
            use super::*;
            use crate::wire::$p as wire;
            pub fn encode_directory(v: &ServerDirectory) -> wire::ServerDirectoryValue {
                wire::ServerDirectoryValue {
                    server: Some(encode_global(v.server().bytes())),
                    value: v.bytes().to_vec(),
                }
            }
            pub fn decode_directory(
                v: &wire::ServerDirectoryValue,
            ) -> Result<ServerDirectory, ProtocolError> {
                ServerDirectory::new(decode_server(required(&v.server)?)?, v.value.clone())
                    .map_err(|_| ProtocolError::Malformed)
            }
            pub fn encode_part(v: PublicationPart) -> wire::PublicationPart {
                wire::PublicationPart {
                    request: v.request,
                    request_generation: v.generation,
                    transfer: v.transfer,
                    index: v.index,
                    total: v.total,
                    epoch: v.epoch.get(),
                    base_revision: v.base.get(),
                    revision: v.revision.get(),
                }
            }
            pub fn decode_part(
                v: &wire::PublicationPart,
            ) -> Result<PublicationPart, ProtocolError> {
                PublicationPart {
                    request: v.request,
                    generation: v.request_generation,
                    transfer: v.transfer,
                    index: v.index,
                    total: v.total,
                    epoch: ServerEpoch::new(v.epoch),
                    base: OrganizationRevision::new(v.base_revision),
                    revision: OrganizationRevision::new(v.revision),
                }
                .validate()
            }
            pub fn encode_settings(
                v: &ServerSettingsRecord,
                remote: &Versioned<spaceterm_core::remote_access::RemoteAccessPolicy>,
            ) -> wire::ServerSettings {
                wire::ServerSettings {
                    version: v.worktree_location.version.get(),
                    worktree_location_template: v.worktree_location.value.as_str().into(),
                    worktree_location_version: v.worktree_location.version.get(),
                    remote_access_enabled: remote.value.is_enabled(),
                    remote_access: Some(super::super::management::$p::encode_policy(remote)),
                    ..Default::default()
                }
            }
            pub fn decode_settings(
                v: &wire::ServerSettings,
            ) -> Result<
                (
                    ServerSettingsRecord,
                    Versioned<spaceterm_core::remote_access::RemoteAccessPolicy>,
                ),
                ProtocolError,
            > {
                if v.worktree_location.is_some()
                    || !v.bind_scope.is_empty()
                    || v.scrollback_rows != 0
                    || v.version != v.worktree_location_version
                {
                    return Err(ProtocolError::Malformed);
                }
                let remote =
                    super::super::management::$p::decode_policy(required(&v.remote_access)?)?;
                if v.remote_access_enabled != remote.value.is_enabled() {
                    return Err(ProtocolError::Malformed);
                }
                Ok((
                    ServerSettingsRecord {
                        worktree_location: Versioned {
                            version: version(v.worktree_location_version)?,
                            value: WorktreeLocationTemplate::new(
                                v.worktree_location_template.clone(),
                            )
                            .map_err(|_| ProtocolError::Malformed)?,
                        },
                    },
                    remote,
                ))
            }
            pub fn encode_workspace_record(
                v: &WorkspaceRecord,
                basis: &WorkspaceNameBasis,
            ) -> wire::Workspace {
                wire::Workspace {
                    id: Some(encode_workspace(v.id)),
                    creation_ordinal: v.creation_ordinal.get(),
                    name: v.name.value.display.as_str().into(),
                    name_mode: match v.name.value.mode {
                        NameMode::Automatic => 1,
                        NameMode::Custom => 2,
                    },
                    name_version: v.name.version.get(),
                    pin_version: v.pinned_directory.version.get(),
                    root_tab: Some(encode_tab(v.root_tab)),
                    assigned_automatic_name: basis.assigned_automatic().as_str().into(),
                    automatic_name_base: basis.automatic_base().as_str().into(),
                    pin_value: v.pinned_directory.value.as_ref().map(encode_directory),
                    pin: None,
                }
            }
            pub fn decode_workspace_record(
                v: &wire::Workspace,
            ) -> Result<(WorkspaceRecord, WorkspaceNameBasis), ProtocolError> {
                if v.pin.is_some() {
                    return Err(ProtocolError::Malformed);
                }
                let id = super::super::identity::decode_workspace(required(&v.id)?)?;
                Ok((
                    WorkspaceRecord {
                        id,
                        name: Versioned {
                            version: version(v.name_version)?,
                            value: WorkspaceName {
                                mode: match v.name_mode {
                                    1 => NameMode::Automatic,
                                    2 => NameMode::Custom,
                                    _ => return Err(ProtocolError::Malformed),
                                },
                                display: NameText::new(v.name.clone())
                                    .map_err(|_| ProtocolError::Malformed)?,
                            },
                        },
                        pinned_directory: Versioned {
                            version: version(v.pin_version)?,
                            value: v.pin_value.as_ref().map(decode_directory).transpose()?,
                        },
                        root_tab: decode_tab(required(&v.root_tab)?)?,
                        creation_ordinal: ordinal(v.creation_ordinal)?,
                    },
                    WorkspaceNameBasis::new(
                        id,
                        NameText::new(v.automatic_name_base.clone())
                            .map_err(|_| ProtocolError::Malformed)?,
                        NameText::new(v.assigned_automatic_name.clone())
                            .map_err(|_| ProtocolError::Malformed)?,
                    ),
                ))
            }
            pub fn encode_tab_record(
                v: &TabRecord,
                default: &TabCreationDefault,
                membership: TabMembershipVersion,
            ) -> wire::Tab {
                wire::Tab {
                    id: Some(encode_tab(v.id)),
                    workspace: Some(encode_workspace(v.workspace)),
                    root_pane: Some(encode_pane(v.root_pane)),
                    worktree: v.worktree.map(encode_worktree),
                    creation_ordinal: v.creation_ordinal.get(),
                    creation_replay_order: default
                        .replay_order()
                        .iter()
                        .copied()
                        .map(encode_pane)
                        .collect(),
                    membership_version: membership.version().get(),
                }
            }
            pub fn decode_tab_record(
                v: &wire::Tab,
            ) -> Result<(TabRecord, TabCreationDefault, TabMembershipVersion), ProtocolError> {
                let id = decode_tab(required(&v.id)?)?;
                Ok((
                    TabRecord {
                        id,
                        workspace: super::super::identity::decode_workspace(required(
                            &v.workspace,
                        )?)?,
                        root_pane: decode_pane(required(&v.root_pane)?)?,
                        worktree: v.worktree.as_ref().map(decode_worktree).transpose()?,
                        creation_ordinal: ordinal(v.creation_ordinal)?,
                    },
                    TabCreationDefault::new(
                        id,
                        v.creation_replay_order
                            .iter()
                            .map(decode_pane)
                            .collect::<Result<_, _>>()?,
                    )
                    .map_err(|_| ProtocolError::Malformed)?,
                    TabMembershipVersion::new(id, version(v.membership_version)?),
                ))
            }
            pub fn encode_pane_record(v: &PaneRecord, rt: PaneRuntimeRecord) -> wire::Pane {
                let (phase, end_reason, runtime_failure_class) = match rt.phase {
                    SessionPhase::NotStarted => (1, 0, None),
                    SessionPhase::Running { .. } => (2, 0, None),
                    SessionPhase::Starting { .. } => (6, 0, None),
                    SessionPhase::DirectoryUnavailable => (5, 0, None),
                    SessionPhase::Failed { class } => (4, 0, Some(failure_number(class))),
                    SessionPhase::Ended { reason } => (
                        3,
                        match reason {
                            EndReason::ServerRestart => 1,
                            EndReason::EndAllSessions => 2,
                            EndReason::Stop => 3,
                            EndReason::OrdinaryExit => 4,
                            EndReason::RuntimeFailure => 5,
                            EndReason::WorktreeRemoval => 6,
                            EndReason::ServerUpdate => 7,
                        },
                        None,
                    ),
                };
                wire::Pane {
                    id: Some(encode_pane(v.id)),
                    tab: Some(encode_tab(v.tab)),
                    creation_ordinal: v.creation_ordinal.get(),
                    creation: match v.creation {
                        PaneCreation::Initial => None,
                        PaneCreation::SplitOf { source, axis, edge } => Some(wire::Creation {
                            split_source: Some(encode_pane(source)),
                            edge: edge_number(axis, edge),
                        }),
                    },
                    starting_directory_value: Some(encode_directory(&v.starting_directory)),
                    restoration_directory_value: v
                        .confirmed_restoration_directory
                        .value
                        .as_ref()
                        .map(encode_directory),
                    directory_version: v.confirmed_restoration_directory.version.get(),
                    session_generation: rt.session_generation.get(),
                    control_generation: rt.control_generation.get(),
                    control: match rt.control {
                        ControlRecord::Unclaimed => None,
                        ControlRecord::Held { client, generation } => Some(wire::Control {
                            client: Some(encode_local(client.local())),
                            generation: generation.get(),
                        }),
                    },
                    phase,
                    end_reason,
                    runtime_failure_class,
                    ..Default::default()
                }
            }
            pub fn decode_pane_record(
                v: &wire::Pane,
            ) -> Result<(PaneRecord, PaneRuntimeRecord), ProtocolError> {
                if v.starting_directory.is_some()
                    || v.restoration_directory.is_some()
                    || v.failure.is_some()
                {
                    return Err(ProtocolError::Malformed);
                }
                let id = decode_pane(required(&v.id)?)?;
                let generation = SessionGeneration::new(v.session_generation);
                let phase = match (v.phase, v.end_reason, v.runtime_failure_class) {
                    (1, 0, None) => SessionPhase::NotStarted,
                    (2, 0, None) if generation.get() != 0 => SessionPhase::Running { generation },
                    (6, 0, None) if generation.get() != 0 => SessionPhase::Starting { generation },
                    (5, 0, None) => SessionPhase::DirectoryUnavailable,
                    (4, 0, Some(class)) => SessionPhase::Failed {
                        class: decode_failure(class)?,
                    },
                    (3, reason, None) => SessionPhase::Ended {
                        reason: match reason {
                            1 => EndReason::ServerRestart,
                            2 => EndReason::EndAllSessions,
                            3 => EndReason::Stop,
                            4 => EndReason::OrdinaryExit,
                            5 => EndReason::RuntimeFailure,
                            6 => EndReason::WorktreeRemoval,
                            7 => EndReason::ServerUpdate,
                            _ => return Err(ProtocolError::Malformed),
                        },
                    },
                    _ => return Err(ProtocolError::Malformed),
                };
                let control = match &v.control {
                    None => ControlRecord::Unclaimed,
                    Some(c) if c.generation != 0 && c.generation == v.control_generation => {
                        ControlRecord::Held {
                            client: ClientId::new(id.server(), decode_local(required(&c.client)?)?)
                                .map_err(|_| ProtocolError::Malformed)?,
                            generation: ControlGeneration::new(c.generation),
                        }
                    }
                    _ => return Err(ProtocolError::Malformed),
                };
                Ok((
                    PaneRecord {
                        id,
                        tab: decode_tab(required(&v.tab)?)?,
                        creation_ordinal: ordinal(v.creation_ordinal)?,
                        creation: match &v.creation {
                            None => PaneCreation::Initial,
                            Some(c) => {
                                let (axis, edge) = decode_edge(c.edge)?;
                                PaneCreation::SplitOf {
                                    source: decode_pane(required(&c.split_source)?)?,
                                    axis,
                                    edge,
                                }
                            }
                        },
                        starting_directory: decode_directory(required(
                            &v.starting_directory_value,
                        )?)?,
                        confirmed_restoration_directory: Versioned {
                            version: version(v.directory_version)?,
                            value: v
                                .restoration_directory_value
                                .as_ref()
                                .map(decode_directory)
                                .transpose()?,
                        },
                    },
                    PaneRuntimeRecord {
                        pane: id,
                        session_generation: generation,
                        phase,
                        control_generation: ControlGeneration::new(v.control_generation),
                        control,
                    },
                ))
            }
            pub fn snapshot_range(
                value: &OrganizationSnapshot,
                range: Range<usize>,
                part: PublicationPart,
            ) -> Result<wire::OrganizationSnapshot, ProtocolError> {
                part.validate()?;
                if part.base.get() != 0
                    || part.epoch != value.epoch()
                    || part.revision != value.state().revision
                    || range.start > range.end
                    || range.end > record_count(value)
                {
                    return Err(ProtocolError::Malformed);
                }
                let s = value.state();
                let facts = s.retained.as_ref().ok_or(ProtocolError::Malformed)?;
                let mut result = wire::OrganizationSnapshot {
                    server: Some(encode_global(s.server.bytes())),
                    epoch: value.epoch().get(),
                    revision: s.revision.get(),
                    settings: Some(encode_settings(&s.settings, facts.remote_access())),
                    complete: part.index + 1 == part.total,
                    part: Some(encode_part(part)),
                    ..Default::default()
                };
                let mut n = 0;
                for v in &s.workspaces {
                    if range.contains(&n) {
                        let basis = facts
                            .workspace_names()
                            .iter()
                            .find(|b| b.workspace() == v.id)
                            .ok_or(ProtocolError::Malformed)?;
                        result.workspaces.push(encode_workspace_record(v, basis));
                    }
                    n += 1;
                }
                for v in &s.tabs {
                    if range.contains(&n) {
                        let default = facts
                            .creation_defaults()
                            .iter()
                            .find(|d| d.tab() == v.id)
                            .ok_or(ProtocolError::Malformed)?;
                        let membership = facts
                            .tab_membership_versions()
                            .iter()
                            .find(|m| m.id() == v.id)
                            .ok_or(ProtocolError::Malformed)?;
                        result.tabs.push(encode_tab_record(v, default, *membership));
                    }
                    n += 1;
                }
                for v in &s.panes {
                    if range.contains(&n) {
                        let rt = value
                            .runtime()
                            .iter()
                            .find(|r| r.pane == v.id)
                            .ok_or(ProtocolError::Malformed)?;
                        result.panes.push(encode_pane_record(v, *rt));
                    }
                    n += 1;
                }
                for v in &s.repositories {
                    if range.contains(&n) {
                        let version = facts
                            .repository_inventory_versions()
                            .iter()
                            .find(|r| r.id() == v.id)
                            .ok_or(ProtocolError::Malformed)?;
                        result.repositories.push(wire::RepositoryRecord {
                            id: Some(encode_repository(v.id)),
                            directory: Some(encode_directory(&v.directory)),
                            inventory_version: version.version().get(),
                        });
                    }
                    n += 1;
                }
                for v in &s.worktrees {
                    if range.contains(&n) {
                        let inventory = facts
                            .worktree_inventory()
                            .iter()
                            .find(|w| w.worktree() == v.id)
                            .ok_or(ProtocolError::Malformed)?;
                        result.worktrees.push(wire::Worktree {
                            id: Some(encode_worktree(v.id)),
                            repository: Some(encode_repository(v.repository)),
                            directory_value: Some(encode_directory(&v.directory)),
                            listed: inventory.listed(),
                            version: inventory.version().get(),
                            main: v.main,
                            missing: v.missing,
                            ..Default::default()
                        });
                    }
                    n += 1;
                }
                Ok(result)
            }
            pub fn decode_snapshot_part(
                v: &wire::OrganizationSnapshot,
            ) -> Result<SnapshotPart, ProtocolError> {
                if !v.continuation.is_empty() {
                    return Err(ProtocolError::Malformed);
                }
                let server = decode_server(required(&v.server)?)?;
                let part = decode_part(required(&v.part)?)?;
                if part.base.get() != 0
                    || part.epoch.get() != v.epoch
                    || part.revision.get() != v.revision
                    || v.complete != (part.index + 1 == part.total)
                {
                    return Err(ProtocolError::Malformed);
                }
                let (settings, remote_access) = decode_settings(required(&v.settings)?)?;
                if remote_access.value.server() != server {
                    return Err(ProtocolError::Malformed);
                }
                let mut facts = OrganizationRetainedParts {
                    server,
                    workspace_names: vec![],
                    creation_defaults: vec![],
                    pane_generations: vec![],
                    tab_membership_versions: vec![],
                    repository_inventory_versions: vec![],
                    worktree_inventory: vec![],
                    remote_access,
                };
                let mut state = OrganizationState {
                    server,
                    revision: part.revision,
                    settings,
                    workspaces: vec![],
                    tabs: vec![],
                    panes: vec![],
                    repositories: vec![],
                    worktrees: vec![],
                    retained: None,
                };
                let mut runtime = vec![];
                for w in &v.workspaces {
                    let (record, basis) = decode_workspace_record(w)?;
                    state.workspaces.push(record);
                    facts.workspace_names.push(basis);
                }
                for t in &v.tabs {
                    let (record, default, membership) = decode_tab_record(t)?;
                    state.tabs.push(record);
                    facts.creation_defaults.push(default);
                    facts.tab_membership_versions.push(membership);
                }
                for p in &v.panes {
                    let (record, rt) = decode_pane_record(p)?;
                    state.panes.push(record);
                    facts.pane_generations.push(PaneGenerationHighWater::new(
                        rt.pane,
                        rt.session_generation,
                        rt.control_generation,
                    ));
                    runtime.push(rt);
                }
                for r in &v.repositories {
                    let id = decode_repository(required(&r.id)?)?;
                    state.repositories.push(RepositoryRecord {
                        id,
                        directory: decode_directory(required(&r.directory)?)?,
                    });
                    facts
                        .repository_inventory_versions
                        .push(RepositoryInventoryVersion::new(
                            id,
                            version(r.inventory_version)?,
                        ));
                }
                for w in &v.worktrees {
                    if w.directory.is_some() || !w.display_name.is_empty() {
                        return Err(ProtocolError::Malformed);
                    }
                    let id = decode_worktree(required(&w.id)?)?;
                    state.worktrees.push(WorktreeRecord {
                        id,
                        repository: decode_repository(required(&w.repository)?)?,
                        directory: decode_directory(required(&w.directory_value)?)?,
                        main: w.main,
                        missing: w.missing,
                    });
                    facts.worktree_inventory.push(WorktreeInventoryFacts::new(
                        id,
                        w.listed,
                        version(w.version)?,
                    ));
                }
                facts.workspace_names.sort_by_key(|v| v.workspace());
                facts.creation_defaults.sort_by_key(|v| v.tab());
                facts.pane_generations.sort_by_key(|v| v.pane());
                facts.tab_membership_versions.sort_by_key(|v| v.id());
                facts.repository_inventory_versions.sort_by_key(|v| v.id());
                facts.worktree_inventory.sort_by_key(|v| v.worktree());
                state.retained = Some(
                    OrganizationRetainedFacts::new(facts).map_err(|_| ProtocolError::Malformed)?,
                );
                Ok(SnapshotPart {
                    part,
                    state,
                    runtime,
                })
            }
            pub fn snapshot_payload(
                value: &OrganizationSnapshot,
                range: Range<usize>,
                part: PublicationPart,
            ) -> Result<Payload, ProtocolError> {
                Ok(Payload::$variant(wire::Message {
                    kind: Some(wire::message::Kind::OrganizationSnapshot(snapshot_range(
                        value, range, part,
                    )?)),
                }))
            }
        }
    };
}
profile!(v1, Major1);
profile!(v2, Major2);

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    struct TestKey(ring::signature::Ed25519KeyPair);
    impl crate::authentication::SigningKey for TestKey {
        fn public_key(&self) -> [u8; 32] {
            use ring::signature::KeyPair;
            self.0.public_key().as_ref().try_into().unwrap()
        }
        fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
            Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
        }
    }
    struct Pairing(crate::authentication::PairingRecord);
    impl crate::authentication::PairingLookup for Pairing {
        fn lookup(
            &self,
            server: [u8; 16],
            pairing: [u8; 16],
            client: [u8; 16],
        ) -> Result<crate::authentication::PairingRecord, ProtocolError> {
            if (server, pairing, client)
                == (
                    self.0.principal.server,
                    self.0.principal.pairing,
                    self.0.principal.client,
                )
            {
                Ok(self.0)
            } else {
                Err(ProtocolError::PairingDenied)
            }
        }
    }
    pub(crate) fn connections(
        major: u32,
        capabilities: Vec<u32>,
    ) -> (
        crate::connection::AuthenticatedConnection,
        crate::connection::AuthenticatedConnection,
    ) {
        use crate::authentication::{
            ClientHandshake, Established, HandshakeConfig, Purpose, ServerHandshake,
        };
        use crate::authentication::{Principal, ServerPin, SigningKey};
        let server_key = std::sync::Arc::new(TestKey(
            ring::signature::Ed25519KeyPair::from_seed_unchecked(&[41; 32]).unwrap(),
        ));
        let device_key = std::sync::Arc::new(TestKey(
            ring::signature::Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap(),
        ));
        let principal = Principal {
            server: [1; 16],
            device: [2; 16],
            pairing: [3; 16],
            client: [4; 16],
        };
        let pin = ServerPin {
            identity: principal.server,
            public_key: server_key.public_key(),
        };
        let pairings = Pairing(crate::authentication::PairingRecord {
            principal,
            device_public_key: device_key.public_key(),
            active: true,
        });
        let config = HandshakeConfig {
            majors: vec![major],
            capabilities,
            ..Default::default()
        };
        let (client, hello) = ClientHandshake::begin(
            pin,
            principal,
            Purpose::Ordinary,
            config.clone(),
            device_key,
        )
        .unwrap();
        let (server, reply) =
            ServerHandshake::accept(principal.server, 7, config, server_key, &pairings, &hello)
                .unwrap();
        let (client, proof) = client.receive_server(&reply).unwrap();
        let (server, ready) = server.receive_device(&pairings, &proof).unwrap();
        let Established::Ordinary(client) = client.receive_ready(&ready).unwrap() else {
            panic!("ordinary client")
        };
        let Established::Ordinary(server) = server else {
            panic!("ordinary server")
        };
        (client, server)
    }

    #[test]
    fn organization_comparison_additions_require_their_own_authenticated_capability() {
        macro_rules! check {
            ($p:ident, $variant:ident, $major:literal) => {{
                use crate::wire::$p as wire;
                let tab = wire::Tab {
                    id: Some(encode_tab(TabId::new(ServerId::new([1; 16]), 1).unwrap())),
                    workspace: Some(encode_workspace(
                        WorkspaceId::new(ServerId::new([1; 16]), 1).unwrap(),
                    )),
                    root_pane: Some(encode_pane(pane().id)),
                    creation_ordinal: 1,
                    membership_version: 8,
                    ..Default::default()
                };
                let pane = $p::encode_pane_record(
                    &pane(),
                    PaneRuntimeRecord {
                        pane: pane().id,
                        session_generation: SessionGeneration::new(1),
                        phase: SessionPhase::Failed {
                            class: FailureClass::ResourceLimit,
                        },
                        control_generation: ControlGeneration::new(0),
                        control: ControlRecord::Unclaimed,
                    },
                );
                for (tabs, panes) in [(vec![tab.clone()], vec![]), (vec![], vec![pane.clone()])] {
                    for delta in [false, true] {
                        let extended = if delta {
                            Payload::$variant(wire::Message {
                                kind: Some(wire::message::Kind::OrganizationDelta(
                                    wire::OrganizationDelta {
                                        server: Some(encode_global([1; 16])),
                                        epoch: 7,
                                        base_revision: 1,
                                        revision: 2,
                                        tabs: tabs.clone(),
                                        panes: panes.clone(),
                                        ..Default::default()
                                    },
                                )),
                            })
                        } else {
                            Payload::$variant(wire::Message {
                                kind: Some(wire::message::Kind::OrganizationSnapshot(
                                    wire::OrganizationSnapshot {
                                        server: Some(encode_global([1; 16])),
                                        epoch: 7,
                                        revision: 2,
                                        tabs: tabs.clone(),
                                        panes: panes.clone(),
                                        ..Default::default()
                                    },
                                )),
                            })
                        };
                        assert!(extended.encode().is_ok());
                        let (mut client, mut server) = connections(
                            $major,
                            vec![crate::negotiation::CAPABILITY_CANONICAL_DOMAIN],
                        );
                        assert_eq!(server.encode(&extended), Err(ProtocolError::Negotiation));
                        let frozen = Payload::$variant(wire::Message {
                            kind: Some(wire::message::Kind::OrganizationSnapshot(
                                wire::OrganizationSnapshot {
                                    server: Some(encode_global([1; 16])),
                                    epoch: 7,
                                    revision: 2,
                                    ..Default::default()
                                },
                            )),
                        });
                        assert_eq!(client.decode(&server.encode(&frozen).unwrap()), Ok(frozen));
                        let (mut client, mut server) = connections(
                            $major,
                            vec![crate::negotiation::CAPABILITY_CANONICAL_DOMAIN, 520_004],
                        );
                        assert_eq!(
                            client.decode(&server.encode(&extended).unwrap()),
                            Ok(extended)
                        );
                    }
                }
            }};
        }
        check!(v1, Major1, 1);
        check!(v2, Major2, 2);
    }
    fn pane() -> PaneRecord {
        let server = ServerId::new([1; 16]);
        PaneRecord {
            id: PaneId::new(server, 1).unwrap(),
            tab: TabId::new(server, 2).unwrap(),
            creation: PaneCreation::Initial,
            creation_ordinal: CreationOrdinal::new(3),
            starting_directory: ServerDirectory::new(server, b"server-only-directory".to_vec())
                .unwrap(),
            confirmed_restoration_directory: Versioned {
                version: FieldVersion::new(4),
                value: Some(ServerDirectory::new(server, b"confirmed".to_vec()).unwrap()),
            },
        }
    }
    macro_rules! cases {
        ($test:ident,$profile:ident) => {
            #[test]
            fn $test() {
                let pane = pane();
                let client = ClientId::new(pane.id.server(), 5).unwrap();
                let mut runtime = PaneRuntimeRecord {
                    pane: pane.id,
                    session_generation: SessionGeneration::new(0),
                    phase: SessionPhase::NotStarted,
                    control_generation: ControlGeneration::new(0),
                    control: ControlRecord::Unclaimed,
                };
                assert_eq!(
                    $profile::decode_pane_record(&$profile::encode_pane_record(&pane, runtime))
                        .unwrap(),
                    (pane.clone(), runtime)
                );
                runtime.session_generation = SessionGeneration::new(6);
                runtime.control_generation = ControlGeneration::new(7);
                runtime.control = ControlRecord::Held {
                    client,
                    generation: ControlGeneration::new(7),
                };
                for class in [
                    FailureClass::LaunchUnavailable,
                    FailureClass::RuntimeUnavailable,
                    FailureClass::RuntimeFault,
                    FailureClass::ResourceLimit,
                    FailureClass::StorageUnavailable,
                    FailureClass::UnsafeStorage,
                    FailureClass::AuthorizationDenied,
                    FailureClass::DirectoryUnavailable,
                    FailureClass::InvalidRequest,
                    FailureClass::StaleAuthority,
                    FailureClass::Unsupported,
                    FailureClass::Internal,
                ] {
                    runtime.phase = SessionPhase::Failed { class };
                    assert_eq!(
                        $profile::decode_pane_record(&$profile::encode_pane_record(&pane, runtime))
                            .unwrap(),
                        (pane.clone(), runtime)
                    );
                }
                for reason in [
                    EndReason::ServerRestart,
                    EndReason::EndAllSessions,
                    EndReason::Stop,
                    EndReason::OrdinaryExit,
                    EndReason::RuntimeFailure,
                    EndReason::WorktreeRemoval,
                    EndReason::ServerUpdate,
                ] {
                    runtime.phase = SessionPhase::Ended { reason };
                    assert_eq!(
                        $profile::decode_pane_record(&$profile::encode_pane_record(&pane, runtime))
                            .unwrap(),
                        (pane.clone(), runtime)
                    );
                }
                runtime.phase = SessionPhase::Running {
                    generation: runtime.session_generation,
                };
                let mut wire = $profile::encode_pane_record(&pane, runtime);
                wire.session_generation = 0;
                assert_eq!(
                    $profile::decode_pane_record(&wire),
                    Err(ProtocolError::Malformed)
                );
                wire.session_generation = 6;
                wire.control.as_mut().unwrap().generation = 8;
                assert_eq!(
                    $profile::decode_pane_record(&wire),
                    Err(ProtocolError::Malformed)
                );
                wire.control.as_mut().unwrap().generation = 7;
                wire.runtime_failure_class = Some(1);
                assert_eq!(
                    $profile::decode_pane_record(&wire),
                    Err(ProtocolError::Malformed)
                );
                runtime.phase = SessionPhase::Failed {
                    class: FailureClass::Internal,
                };
                let mut wire = $profile::encode_pane_record(&pane, runtime);
                wire.runtime_failure_class = Some(99);
                assert_eq!(
                    $profile::decode_pane_record(&wire),
                    Err(ProtocolError::Malformed)
                );
                let tab = TabRecord {
                    id: pane.tab,
                    workspace: WorkspaceId::new(pane.id.server(), 8).unwrap(),
                    root_pane: pane.id,
                    worktree: None,
                    creation_ordinal: CreationOrdinal::new(9),
                };
                let default = TabCreationDefault::new(tab.id, vec![pane.id]).unwrap();
                let membership = TabMembershipVersion::new(tab.id, FieldVersion::new(10));
                let wire = $profile::encode_tab_record(&tab, &default, membership);
                assert_eq!(
                    $profile::decode_tab_record(&wire).unwrap(),
                    (tab, default, membership)
                );
            }
        };
    }
    cases!(
        major_1_organization_preserves_exact_runtime_and_retained_comparisons,
        v1
    );
    cases!(
        major_2_organization_preserves_exact_runtime_and_retained_comparisons,
        v2
    );
}
