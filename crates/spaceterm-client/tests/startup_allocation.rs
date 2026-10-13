#![cfg(feature = "test-support")]
#[path = "projection/support.rs"]
mod support;

use spaceterm_client::operations::OperationTracker;
use spaceterm_core::{
    control::StartupSource, operation_ledger_contracts::CanonicalOperationRequest,
    operations::Command,
};

#[tokio::test]
async fn concurrent_startup_and_shared_edits_allocate_matching_durable_intent_identities() {
    let fixture = support::Fixture::new(true).await;
    let tracker = OperationTracker::new(fixture.peer.journal().clone());
    let binding = fixture.server.binding(1).unwrap();
    let fence = support::pane_fence(&fixture.pane);
    let first = tracker.begin_startup(
        &binding,
        fence,
        StartupSource::StartSession,
        fixture.workspace,
        fixture.server.as_ref(),
    );
    let second = tracker.submit(
        &binding,
        Command::SetWorktreeLocation {
            server: fixture.server.server(),
            expected_version: fixture
                .server
                .snapshot()
                .unwrap()
                .state()
                .settings
                .worktree_location
                .version,
            template: spaceterm_core::organization::WorktreeLocationTemplate::new(
                "~/.worktrees/{repository}/{branch}".into(),
            )
            .unwrap(),
        },
        None,
        fixture.server.as_ref(),
    );
    let third = tracker.begin_startup(
        &binding,
        fence,
        StartupSource::StartSession,
        fixture.workspace,
        fixture.server.as_ref(),
    );
    let (first, second, third) = tokio::join!(first, second, third);
    let (first_intent, first_ticket) = first.unwrap();
    let second_ticket = second.unwrap();
    let (third_intent, third_ticket) = third.unwrap();
    assert_eq!(first_intent.id(), first_ticket.id());
    assert_eq!(third_intent.id(), third_ticket.id());
    let mut allocated = [
        first_ticket.id().sequence().get(),
        second_ticket.id().sequence().get(),
        third_ticket.id().sequence().get(),
    ];
    allocated.sort();
    assert_eq!(allocated[1], allocated[0] + 1);
    assert_eq!(allocated[2], allocated[1] + 1);
    let saved = tracker.journal().snapshot().unwrap();
    for intent in [first_intent, third_intent] {
        let record = saved.operation(intent.id()).unwrap();
        let request =
            CanonicalOperationRequest::decode(&record.request, tracker.journal().codec()).unwrap();
        assert!(
            matches!(request.command(), Command::BeginStartup(retained) if retained.id() == request.binding().id && *retained == intent)
        );
    }
}
