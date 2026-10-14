//! The real canonical native store retains original remote native comparison
//! facts across physical reopening. No credential access or native action runs.

#[path = "../../spaceterm-client/tests/operation_recovery/support.rs"]
mod support;

use spaceterm_client::journal::{
    ClientJournalChange, JournalController, JournalLimits, RemoteNativeRequest,
};
use spaceterm_client_native::journal::NativeClientJournal;
use spaceterm_core::operation_ledger::canonical::CanonicalCodec;
use std::{os::unix::fs::PermissionsExt, sync::Arc};
use support::{Verifier, binding, saved};

async fn open(directory: &std::path::Path, create: bool) -> JournalController {
    let codec = Arc::new(CanonicalCodec);
    let verifier = Arc::new(Verifier);
    let limits = JournalLimits::default();
    let native = if create {
        NativeClientJournal::create(
            directory.to_owned(),
            codec.clone(),
            verifier.clone(),
            limits,
        )
        .await
    } else {
        NativeClientJournal::open_existing(
            directory.to_owned(),
            codec.clone(),
            verifier.clone(),
            limits,
        )
        .await
    }
    .unwrap();
    JournalController::recover(Arc::new(native), codec, verifier, limits)
        .await
        .unwrap()
}

#[tokio::test]
async fn native_physical_reopen_preserves_original_assessment_submission_and_terminal_result() {
    // Provision only a fresh private fixture directory. The production adapter
    // still validates the canonical temporary root and every storage ancestor.
    let root = std::fs::canonicalize("/var/tmp").unwrap();
    let directory = tempfile::Builder::new()
        .prefix("remote-native-journal-")
        .tempdir_in(root)
        .unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let binding = binding(1);
    let pairing = binding.target().local_pairing().unwrap();
    let client = binding.target().client();
    let request = [19; 16];
    let scope = b"original-public-update-scope-and-capture".to_vec();
    let intent = b"original-public-update-action".to_vec();
    let assessment = b"original-public-assessment-expiry-23".to_vec();
    let result = b"original-public-terminal-native-progress".to_vec();
    let journal = open(directory.path(), true).await;
    journal
        .transact(vec![ClientJournalChange::SaveBinding(Box::new(saved(
            binding.clone(),
        )))])
        .await
        .unwrap();
    journal
        .retain_remote_native_request(
            RemoteNativeRequest::new(pairing.clone(), client, request, scope.clone()).unwrap(),
        )
        .await
        .unwrap();
    journal
        .retain_remote_native_intent(
            pairing.clone(),
            client,
            request,
            scope.clone(),
            intent.clone(),
        )
        .await
        .unwrap();
    drop(journal);

    // Action intent must survive physical reopening before native Capture.
    let journal = open(directory.path(), false).await;
    assert_eq!(
        journal
            .recover_remote_native_requests(&pairing, client)
            .unwrap()[0]
            .intent(),
        Some(intent.as_slice())
    );
    journal
        .retain_remote_native_assessment(
            pairing.clone(),
            client,
            request,
            scope.clone(),
            assessment.clone(),
        )
        .await
        .unwrap();
    journal
        .mark_remote_native_submitted(
            pairing.clone(),
            client,
            request,
            scope.clone(),
            Some(assessment.clone()),
        )
        .await
        .unwrap();
    drop(journal);

    let journal = open(directory.path(), false).await;
    let records = journal
        .recover_remote_native_requests(&pairing, client)
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].request_id(), request);
    assert_eq!(records[0].original_scope(), scope);
    assert_eq!(records[0].intent(), Some(intent.as_slice()));
    assert_eq!(records[0].assessment(), Some(assessment.as_slice()));
    assert!(records[0].submitted());
    journal
        .retain_remote_native_result(
            pairing.clone(),
            client,
            request,
            scope.clone(),
            result.clone(),
        )
        .await
        .unwrap();
    drop(journal);

    let journal = open(directory.path(), false).await;
    let records = journal
        .recover_remote_native_requests(&pairing, client)
        .unwrap();
    assert_eq!(records[0].result(), Some(result.as_slice()));
    assert_eq!(records[0].assessment(), Some(assessment.as_slice()));
    assert_eq!(records[0].original_scope(), scope);
    journal
        .retire_remote_native_request(pairing.clone(), client, request, scope, result)
        .await
        .unwrap();
    drop(journal);

    assert!(
        open(directory.path(), false)
            .await
            .recover_remote_native_requests(&pairing, client)
            .unwrap()
            .is_empty()
    );
}
