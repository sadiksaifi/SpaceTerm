//! Both-major publication assembly over the shared canonical record converters.
use super::*;
use spaceterm_client::replica::{OrganizationCursor, PublicationStamp, SnapshotPart};
use spaceterm_core::{organization::*, organization_retained::*};
use spaceterm_protocol::convert::organization;

pub(super) fn decode(payload: Payload) -> Result<OrganizationPublication, ClientFailure> {
    macro_rules! profile {
        ($message:expr, $p:ident) => {{
            match $message.kind.ok_or_else(invalid)? {
                $p::message::Kind::OrganizationSnapshot(value) => {
                    let decoded = organization::$p::decode_snapshot_part(&value)
                        .map_err(ClientFailure::from)?;
                    Ok(OrganizationPublication::Snapshot(Box::new(SnapshotPart {
                        stamp: stamp(decoded.state.server, decoded.part),
                        state: decoded.state,
                        runtime: decoded.runtime,
                    })))
                }
                $p::message::Kind::OrganizationDelta(value) => {
                    let server =
                        identity::decode_server(value.server.as_ref().ok_or_else(invalid)?)
                            .map_err(ClientFailure::from)?;
                    let part =
                        organization::$p::decode_part(value.part.as_ref().ok_or_else(invalid)?)
                            .map_err(ClientFailure::from)?;
                    if !value.continuation.is_empty()
                        || part.epoch.get() != value.epoch
                        || part.base.get() != value.base_revision
                        || part.revision.get() != value.revision
                        || value.complete != (part.index + 1 == part.total)
                    {
                        return Err(invalid());
                    }
                    let mut writes = vec![];
                    let mut runtime = vec![];
                    for item in value.workspaces {
                        let (record, basis) = organization::$p::decode_workspace_record(&item)
                            .map_err(ClientFailure::from)?;
                        writes.push(OrganizationWrite::PutWorkspace(record));
                        writes.push(OrganizationWrite::Retained(
                            RetainedOrganizationWrite::PutWorkspaceNameBasis(basis),
                        ));
                    }
                    for item in value.tabs {
                        let (record, default, membership) =
                            organization::$p::decode_tab_record(&item)
                                .map_err(ClientFailure::from)?;
                        writes.push(OrganizationWrite::PutTab(record));
                        writes.push(OrganizationWrite::Retained(
                            RetainedOrganizationWrite::PutCreationDefault(default),
                        ));
                        writes.push(OrganizationWrite::Retained(
                            RetainedOrganizationWrite::SetTabMembershipVersion(membership),
                        ));
                    }
                    for item in value.panes {
                        let (record, live) = organization::$p::decode_pane_record(&item)
                            .map_err(ClientFailure::from)?;
                        writes.push(OrganizationWrite::PutPane(record));
                        writes.push(OrganizationWrite::Retained(
                            RetainedOrganizationWrite::SetPaneGenerationHighWater(
                                PaneGenerationHighWater::new(
                                    live.pane,
                                    live.session_generation,
                                    live.control_generation,
                                ),
                            ),
                        ));
                        runtime.push(live);
                    }
                    for item in value.repositories {
                        let id = identity::decode_repository(item.id.as_ref().ok_or_else(invalid)?)
                            .map_err(ClientFailure::from)?;
                        if item.inventory_version == 0 {
                            return Err(invalid());
                        }
                        writes.push(OrganizationWrite::PutRepository(RepositoryRecord {
                            id,
                            directory: organization::$p::decode_directory(
                                item.directory.as_ref().ok_or_else(invalid)?,
                            )
                            .map_err(ClientFailure::from)?,
                        }));
                        writes.push(OrganizationWrite::Retained(
                            RetainedOrganizationWrite::SetRepositoryInventoryVersion(
                                RepositoryInventoryVersion::new(
                                    id,
                                    FieldVersion::new(item.inventory_version),
                                ),
                            ),
                        ));
                    }
                    for item in value.worktrees {
                        if item.directory.is_some()
                            || !item.display_name.is_empty()
                            || item.version == 0
                        {
                            return Err(invalid());
                        }
                        let id = identity::decode_worktree(item.id.as_ref().ok_or_else(invalid)?)
                            .map_err(ClientFailure::from)?;
                        writes.push(OrganizationWrite::PutWorktree(WorktreeRecord {
                            id,
                            repository: identity::decode_repository(
                                item.repository.as_ref().ok_or_else(invalid)?,
                            )
                            .map_err(ClientFailure::from)?,
                            directory: organization::$p::decode_directory(
                                item.directory_value.as_ref().ok_or_else(invalid)?,
                            )
                            .map_err(ClientFailure::from)?,
                            main: item.main,
                            missing: item.missing,
                        }));
                        writes.push(OrganizationWrite::Retained(
                            RetainedOrganizationWrite::SetWorktreeInventoryFacts(
                                WorktreeInventoryFacts::new(
                                    id,
                                    item.listed,
                                    FieldVersion::new(item.version),
                                ),
                            ),
                        ));
                    }
                    for item in value.deleted_workspaces {
                        let id = identity::decode_workspace(&item).map_err(ClientFailure::from)?;
                        writes.push(OrganizationWrite::DeleteWorkspace(id));
                        writes.push(OrganizationWrite::Retained(
                            RetainedOrganizationWrite::DeleteWorkspaceNameBasis(id),
                        ));
                    }
                    for item in value.deleted_tabs {
                        let id = identity::decode_tab(&item).map_err(ClientFailure::from)?;
                        writes.push(OrganizationWrite::DeleteTab(id));
                        writes.push(OrganizationWrite::Retained(
                            RetainedOrganizationWrite::DeleteCreationDefault(id),
                        ));
                        writes.push(OrganizationWrite::Retained(
                            RetainedOrganizationWrite::DeleteTabMembershipVersion(id),
                        ));
                    }
                    for item in value.deleted_panes {
                        let id = identity::decode_pane(&item).map_err(ClientFailure::from)?;
                        writes.push(OrganizationWrite::DeletePane(id));
                        writes.push(OrganizationWrite::Retained(
                            RetainedOrganizationWrite::DeletePaneGenerationHighWater(id),
                        ));
                    }
                    for item in value.deleted_repositories {
                        let id = identity::decode_repository(&item).map_err(ClientFailure::from)?;
                        writes.push(OrganizationWrite::DeleteRepository(id));
                        writes.push(OrganizationWrite::Retained(
                            RetainedOrganizationWrite::DeleteRepositoryInventoryVersion(id),
                        ));
                    }
                    for item in value.deleted_worktrees {
                        let id = identity::decode_worktree(&item).map_err(ClientFailure::from)?;
                        writes.push(OrganizationWrite::DeleteWorktree(id));
                        writes.push(OrganizationWrite::Retained(
                            RetainedOrganizationWrite::DeleteWorktreeInventoryFacts(id),
                        ));
                    }
                    if let Some(item) = value.settings {
                        let (settings, remote) = organization::$p::decode_settings(&item)
                            .map_err(ClientFailure::from)?;
                        writes.push(OrganizationWrite::SetServerSettings(settings));
                        writes.push(OrganizationWrite::Retained(
                            RetainedOrganizationWrite::SetRemoteAccess(remote),
                        ));
                    }
                    let delta = OrganizationDelta::new(
                        server,
                        part.epoch,
                        part.base,
                        part.revision,
                        writes,
                        runtime,
                    )
                    .map_err(|_| invalid())?;
                    let mut creations = vec![];
                    for item in value.accepted_pane_creations {
                        let (axis, edge) =
                            organization::decode_edge(item.edge).map_err(ClientFailure::from)?;
                        if item.creation_ordinal == 0 {
                            return Err(invalid());
                        }
                        creations.push(
                            AcceptedPaneCreation::new(
                                &delta,
                                identity::decode_tab(item.tab.as_ref().ok_or_else(invalid)?)
                                    .map_err(ClientFailure::from)?,
                                identity::decode_pane(item.pane.as_ref().ok_or_else(invalid)?)
                                    .map_err(ClientFailure::from)?,
                                CreationOrdinal::new(item.creation_ordinal),
                                PaneCreation::SplitOf {
                                    source: identity::decode_pane(
                                        item.source.as_ref().ok_or_else(invalid)?,
                                    )
                                    .map_err(ClientFailure::from)?,
                                    axis,
                                    edge,
                                },
                            )
                            .map_err(|_| invalid())?,
                        );
                    }
                    let delta = delta
                        .with_accepted_creations(creations)
                        .map_err(|_| invalid())?;
                    Ok(OrganizationPublication::DeltaPart {
                        stamp: stamp(server, part),
                        delta,
                    })
                }
                _ => Err(invalid()),
            }
        }};
    }
    match payload {
        Payload::Major1(message) => profile!(message, v1),
        Payload::Major2(message) => profile!(message, v2),
    }
}
fn stamp(server: ServerId, part: organization::PublicationPart) -> PublicationStamp {
    PublicationStamp {
        request: part.request,
        request_generation: part.generation,
        transfer: part.transfer,
        index: part.index,
        total: part.total,
        cursor: OrganizationCursor {
            server,
            epoch: part.epoch,
            revision: part.revision,
        },
    }
}
