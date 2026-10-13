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
