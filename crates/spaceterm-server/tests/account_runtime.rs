#[path = "support/job_a.rs"]
mod support;

use spaceterm_core::{authority::*, operations::*, organization::*};
use spaceterm_server::{contracts::ServerFailureClass, service::RecoveredServer};
use support::*;

#[tokio::test]
async fn native_runtime_inventory_observes_sessions_and_reserves_exact_all_client_scope() {
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let grant = acquire(&handle, principal, &codec).await;
    start(&handle, principal, &codec, grant).await;
    let assessment = handle.assess_account_runtime().await.unwrap();
    assert_eq!(assessment.server, support::server());
    assert_eq!(assessment.epoch, Some(handle.snapshot().epoch()));
    assert_eq!(assessment.sessions.len(), 1);
    assert_eq!(assessment.sessions[0].pane, pane());
    handle
        .reserve_account_runtime(assessment.clone())
        .await
        .unwrap();
    // Replaying this exact reservation succeeds, but work from any Client is blocked.
    handle
        .reserve_account_runtime(assessment.clone())
        .await
        .unwrap();
    let rename = request(
        &codec,
        1,
        2,
        Command::RenameWorkspace {
            workspace: workspace(),
            expected_version: FieldVersion::new(1),
            draft: NameDraft::Custom(NameText::new("blocked".into()).unwrap()),
        },
    );
    assert!(handle.submit(principal, rename).await.is_err());
    handle
        .abort_account_runtime(assessment.clone())
        .await
        .unwrap();
    assert_eq!(
        handle
            .reserve_account_runtime(assessment)
            .await
            .unwrap_err()
            .class,
        ServerFailureClass::StaleAuthority
    );
    let fresh = handle.assess_account_runtime().await.unwrap();
    assert_eq!(fresh.sessions.len(), 1);
    handle.reserve_account_runtime(fresh).await.unwrap();
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn native_runtime_reservation_rejects_scope_changed_after_assessment() {
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let assessment = handle.assess_account_runtime().await.unwrap();
    let grant = acquire(&handle, principal, &codec).await;
    start(&handle, principal, &codec, grant).await;
    assert_eq!(
        handle
            .reserve_account_runtime(assessment)
            .await
            .unwrap_err()
            .class,
        ServerFailureClass::StaleAuthority
    );
    assert_eq!(
        handle
            .assess_account_runtime()
            .await
            .unwrap()
            .sessions
            .len(),
        1
    );
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

fn retain_native_pairing(fixture: &Fixture) {
    use spaceterm_core::{identity::*, pairing::*};
    use spaceterm_protocol::authentication::SigningKey;
    let pairing = PairingId::new(support::server(), 1).unwrap();
    let authorization = PairingAuthorization::new(PairingAuthorizationParts {
        record: PairingRecord {
            id: pairing,
            device: DeviceId::new([2; 16]),
            state: PairingState::Authorized,
        },
        authorization_version: FieldVersion::new(1),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: fixture.key.public_key(),
            pairing_authentication_public_key: fixture.pairings.key.public_key(),
        })
        .unwrap(),
        clients: (1..=2)
            .map(|number| {
                PairingClientStreams::new(PairingClientStreamsParts {
                    client: ClientRecord {
                        id: client(number),
                        pairing,
                    },
                    ordinary: stream(number),
                    self_revocation: OperationStreamId::new(
                        OperationStreamKey::new(support::server(), number + 2).unwrap(),
                        pairing,
                        client(number),
                    )
                    .unwrap(),
                })
                .unwrap()
            })
            .collect(),
        revocation_receipt: None,
    })
    .unwrap();
    let mut db = fixture.db.lock().unwrap();
    db.recovered.high_water.operation_stream = 4;
    db.recovered.pairing = Some(
        PairingRecovery::new(PairingRecoveryParts {
            authorizations: vec![authorization],
            enrollments: vec![],
        })
        .unwrap(),
    );
}

#[tokio::test]
async fn native_paired_admission_requires_durable_authorization_not_legacy_pairing_lookup() {
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let host = fixture.host.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    assert_eq!(
        handle
            .authorize_account_lifecycle(principal)
            .await
            .unwrap_err()
            .class,
        ServerFailureClass::Authentication
    );
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn native_paired_reservation_rechecks_original_connection_binding_and_authorization_version()
{
    let fixture = Fixture::new();
    retain_native_pairing(&fixture);
    let original = fixture.principal(1, 1);
    let replacement = fixture.principal(1, 2);
    let host = fixture.host.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, original).await;
    let (authorized, version) = handle.authorize_account_lifecycle(original).await.unwrap();
    assert_eq!(authorized.client(), original.client());
    assert_eq!(authorized.device(), original.device());
    let scope = handle.assess_account_runtime().await.unwrap();
    connect(&handle, replacement).await;
    assert_eq!(
        handle
            .reserve_paired_account_runtime(original, version, scope.clone())
            .await
            .unwrap_err()
            .class,
        ServerFailureClass::StaleAuthority
    );
    assert_eq!(
        handle
            .reserve_paired_account_runtime(replacement, FieldVersion::new(2), scope.clone())
            .await
            .unwrap_err()
            .class,
        ServerFailureClass::Authentication
    );
    handle
        .reserve_paired_account_runtime(replacement, version, scope.clone())
        .await
        .unwrap();
    handle.abort_account_runtime(scope).await.unwrap();
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn native_paired_reservation_fences_pairing_revocation_until_original_non_admission_is_resolved()
 {
    let fixture = Fixture::new();
    retain_native_pairing(&fixture);
    let principal = fixture.principal(1, 1);
    let other = fixture.principal(2, 1);
    let host = fixture.host.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    connect(&handle, other).await;
    let (_, version) = handle.authorize_account_lifecycle(principal).await.unwrap();
    let scope = handle.assess_account_runtime().await.unwrap();
    handle
        .reserve_paired_account_runtime(principal, version, scope.clone())
        .await
        .unwrap();
    assert_eq!(
        handle
            .revoke_clients(vec![principal.client()])
            .await
            .unwrap_err()
            .class,
        ServerFailureClass::StaleAuthority
    );
    assert!(handle.authorize_account_lifecycle(principal).await.is_ok());
    handle.abort_account_runtime(scope).await.unwrap();
    handle
        .revoke_clients(vec![principal.client()])
        .await
        .unwrap();
    assert_eq!(
        handle
            .authorize_account_lifecycle(principal)
            .await
            .unwrap_err()
            .class,
        ServerFailureClass::Authentication
    );
    assert!(handle.authorize_account_lifecycle(other).await.is_ok());
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}
