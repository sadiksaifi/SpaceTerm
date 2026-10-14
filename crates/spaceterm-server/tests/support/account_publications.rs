//! Publication validation for the account fixture's authenticated connections.

use super::*;
use spaceterm_client::{
    limits::ClientLimits,
    replica::{OrganizationResidency, Replica, ReplicaUpdate, ResyncToken},
};
use spaceterm_core::organization_retained::{PaneGenerationHighWater, RetainedOrganizationWrite};
use spaceterm_protocol::{connection::AuthenticatedConnection, convert::organization};
use std::ops::Deref;

pub(super) struct PublicationCodec {
    inner: AuthenticatedConnection,
    replica: Replica,
    token: ResyncToken,
    active: Option<(u64, u64)>,
    pending: Option<(u64, u64)>,
}
impl PublicationCodec {
    fn new(inner: AuthenticatedConnection) -> Self {
        let mut replica = Replica::new(
            ServerId::new(inner.principal().server),
            Arc::new(OrganizationResidency::new(ClientLimits::default()).unwrap()),
        );
        let token = replica
            .begin_resync(ServerEpoch::new(inner.epoch()))
            .unwrap();
        Self {
            inner,
            replica,
            token,
            active: None,
            pending: None,
        }
    }
    pub(super) fn request_snapshot(&mut self, generation: u64) {
        assert!(self.pending.is_none());
        assert!(generation > 0);
        if let Some((_, previous)) = self.active {
            assert!(generation > previous);
        }
        // Keep the installed replica ready while queued publications from the
        // previous query arrive before the new query's complete snapshot.
        self.pending = Some((generation, generation));
    }
    pub(super) fn encode(&mut self, payload: &Payload) -> Result<Vec<u8>, ProtocolError> {
        self.inner.encode(payload)
    }
    pub(super) fn decode(&mut self, bytes: &[u8]) -> Result<Payload, ProtocolError> {
        let payload = self.inner.decode(bytes)?;
        match &payload {
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::OrganizationSnapshot(value)),
            }) => {
                let decoded = organization::v2::decode_snapshot_part(value).unwrap();
                assert_eq!(
                    self.pending,
                    Some((decoded.part.request, decoded.part.generation))
                );
                assert_eq!(decoded.part.epoch.get(), self.inner.epoch());
                assert_eq!((decoded.part.index, decoded.part.total), (0, 1));
                assert!(value.complete);
                let snapshot = spaceterm_core::organization::OrganizationSnapshot::new(
                    decoded.part.epoch,
                    decoded.state,
                    decoded.runtime,
                )
                .unwrap();
                assert_eq!(
                    self.replica.install_snapshot(self.token, snapshot).unwrap(),
                    ReplicaUpdate::Installed
                );
                self.active = self.pending.take();
            }
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::OrganizationDelta(value)),
            }) => {
                let part = organization::v2::decode_part(value.part.as_ref().unwrap()).unwrap();
                assert_eq!(self.active, Some((part.request, part.generation)));
                assert_eq!(part.epoch.get(), self.inner.epoch());
                assert_eq!((part.index, part.total), (0, 1));
                assert_eq!(part.epoch.get(), value.epoch);
                assert_eq!(part.base.get(), value.base_revision);
                assert_eq!(part.revision.get(), value.revision);
                assert!(value.complete && value.continuation.is_empty());
                let delta = decode_delta(value);
                assert_eq!(
                    self.replica.apply_delta(&delta).unwrap(),
                    ReplicaUpdate::Installed
                );
            }
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::OrganizationResync(value)),
            }) => {
                let comparison = Some((value.request, value.request_generation));
                assert!(comparison == self.active || comparison == self.pending);
                assert_eq!(value.epoch, self.inner.epoch());
                panic!("unexpected organization resync in bounded account fixture");
            }
            Payload::Major1(v1::Message {
                kind:
                    Some(
                        v1::message::Kind::OrganizationSnapshot(_)
                        | v1::message::Kind::OrganizationDelta(_)
                        | v1::message::Kind::OrganizationResync(_),
                    ),
            }) => panic!("account fixture organization queries use major 2"),
            _ => {}
        }
        Ok(payload)
    }
}
impl Deref for PublicationCodec {
    type Target = AuthenticatedConnection;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// No public complete wire-delta decoder exists. Reuse the canonical record
// converters and Replica; fail if this fixture receives fields it does not exercise.
fn decode_delta(value: &v2::OrganizationDelta) -> spaceterm_core::organization::OrganizationDelta {
    assert!(value.repositories.is_empty() && value.worktrees.is_empty());
    assert!(
        value.deleted_workspaces.is_empty()
            && value.deleted_tabs.is_empty()
            && value.deleted_panes.is_empty()
    );
    assert!(value.deleted_repositories.is_empty() && value.deleted_worktrees.is_empty());
    assert!(value.accepted_pane_creations.is_empty());
    let server =
        spaceterm_protocol::convert::identity::decode_server(value.server.as_ref().unwrap())
            .unwrap();
    let mut writes = Vec::new();
    let mut runtime = Vec::new();
    for item in &value.workspaces {
        let (record, basis) = organization::v2::decode_workspace_record(item).unwrap();
        writes.push(OrganizationWrite::PutWorkspace(record));
        writes.push(OrganizationWrite::Retained(
            RetainedOrganizationWrite::PutWorkspaceNameBasis(basis),
        ));
    }
    for item in &value.tabs {
        let (record, default, membership) = organization::v2::decode_tab_record(item).unwrap();
        writes.push(OrganizationWrite::PutTab(record));
        writes.push(OrganizationWrite::Retained(
            RetainedOrganizationWrite::PutCreationDefault(default),
        ));
        writes.push(OrganizationWrite::Retained(
            RetainedOrganizationWrite::SetTabMembershipVersion(membership),
        ));
    }
    for item in &value.panes {
        let (record, live) = organization::v2::decode_pane_record(item).unwrap();
        writes.push(OrganizationWrite::PutPane(record));
        writes.push(OrganizationWrite::Retained(
            RetainedOrganizationWrite::SetPaneGenerationHighWater(PaneGenerationHighWater::new(
                live.pane,
                live.session_generation,
                live.control_generation,
            )),
        ));
        runtime.push(live);
    }
    if let Some(item) = &value.settings {
        let (settings, remote) = organization::v2::decode_settings(item).unwrap();
        writes.push(OrganizationWrite::SetServerSettings(settings));
        writes.push(OrganizationWrite::Retained(
            RetainedOrganizationWrite::SetRemoteAccess(remote),
        ));
    }
    spaceterm_core::organization::OrganizationDelta::new(
        server,
        ServerEpoch::new(value.epoch),
        OrganizationRevision::new(value.base_revision),
        OrganizationRevision::new(value.revision),
        writes,
        runtime,
    )
    .unwrap()
}

pub(super) async fn handshake(
    transport: &dyn MessageTransport,
    pin: authentication::ServerPin,
    principal: authentication::Principal,
    key: Arc<DeviceKey>,
    major: u32,
) -> PublicationCodec {
    PublicationCodec::new(super::super::handshake(transport, pin, principal, key, major).await)
}

pub(super) async fn reconcile(
    transport: &dyn MessageTransport,
    codec: &mut PublicationCodec,
    handle: &ServerHandle,
) -> u64 {
    assert!(codec.active.is_none() && codec.pending.is_none());
    super::super::reconcile(transport, &mut codec.inner, handle).await
}

pub(super) async fn round_trip(
    transport: &dyn MessageTransport,
    codec: &mut PublicationCodec,
    payload: Payload,
) -> Payload {
    transport
        .send(&codec.encode(&payload).unwrap())
        .await
        .unwrap();
    loop {
        let reply = codec
            .decode(&transport.recv().await.unwrap().unwrap())
            .unwrap();
        if matches!(
            &reply,
            Payload::Major2(v2::Message {
                kind: Some(
                    v2::message::Kind::OrganizationSnapshot(_)
                        | v2::message::Kind::OrganizationDelta(_)
                )
            })
        ) {
            continue;
        }
        return reply;
    }
}

pub(super) async fn query_status(
    transport: &dyn MessageTransport,
    codec: &mut PublicationCodec,
) -> ServerStatus {
    macro_rules! profile {
        ($p:ident, $variant:ident) => {
            Payload::$variant($p::Message {
                kind: Some($p::message::Kind::StatusQuery($p::StatusQuery {
                    request: 1,
                    request_generation: 1,
                    ..Default::default()
                })),
            })
        };
    }
    let query = if codec.major() == 1 {
        profile!(v1, Major1)
    } else {
        profile!(v2, Major2)
    };
    transport
        .send(&codec.encode(&query).unwrap())
        .await
        .unwrap();
    loop {
        let payload = codec
            .decode(&transport.recv().await.unwrap().unwrap())
            .unwrap();
        let publication = match payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::StatusPublication(value)),
            }) => management::v1::decode_publication(&value, &|_| {
                Err(ProtocolError::UnexpectedMessage)
            })
            .unwrap(),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::StatusPublication(value)),
            }) => management::v2::decode_publication(&value, &|_| {
                Err(ProtocolError::UnexpectedMessage)
            })
            .unwrap(),
            Payload::Major2(v2::Message {
                kind:
                    Some(
                        v2::message::Kind::OrganizationSnapshot(_)
                        | v2::message::Kind::OrganizationDelta(_),
                    ),
            }) => continue,
            _ => panic!("status publication"),
        };
        let StatusPublication::Snapshot { status, .. } = publication else {
            panic!("snapshot");
        };
        return status;
    }
}
