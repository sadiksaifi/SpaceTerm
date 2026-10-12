use spaceterm_core::authority::*;
use spaceterm_core::identity::*;
use spaceterm_core::native_management::*;
use spaceterm_core::operations::RequestDigest;
use spaceterm_core::organization::RetirementReason;
use spaceterm_core::retirement::*;

fn descriptor(action: NativeRetirementAction) -> NativeRetirementDescriptor {
    let server = ServerId::new([7; 16]);
    let pane = PaneId::new(server, 1).unwrap();
    NativeRetirementDescriptor::new(NativeRetirementDescriptorParts {
        request: NativeManagementRequestRef::new(
            server,
            [9; 16],
            RequestDigest::from_bytes([8; 32]),
        )
        .unwrap(),
        batch: RetirementBatchId::new(server, ServerEpoch::new(3), 1).unwrap(),
        action,
        acceptance_revision: OrganizationRevision::new(4),
        panes: vec![
            RetirementPane::new(
                WorkspaceId::new(server, 1).unwrap(),
                TabId::new(server, 1).unwrap(),
                pane,
                None,
                SessionGeneration::new(3),
            )
            .unwrap(),
        ],
        sessions: vec![
            RetiredSession::new(
                SessionKey {
                    pane,
                    epoch: ServerEpoch::new(3),
                    generation: SessionGeneration::new(2),
                },
                RetirementReason::EndAllSessions,
                RetirementCleanupState::AwaitingEvidence,
            )
            .unwrap(),
        ],
    })
    .unwrap()
}
#[test]
fn native_stop_restart_and_activation_keep_their_native_request_and_exact_reason() {
    for (action, expected) in [
        (NativeRetirementAction::Stop, RetirementReason::Stop),
        (
            NativeRetirementAction::RestartApproved,
            RetirementReason::Stop,
        ),
        (
            NativeRetirementAction::ActivateUpdate,
            RetirementReason::ServerUpdate,
        ),
    ] {
        let value = descriptor(action);
        assert_eq!(value.reason(), expected);
        assert_eq!(value.request().id_bytes(), [9; 16]);
        let recovered: NativeRetirementDescriptor =
            serde_json::from_slice(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(recovered, value);
    }
}
#[test]
fn overlapping_native_retirement_does_not_retag_the_first_reason() {
    let value = descriptor(NativeRetirementAction::Stop);
    assert_eq!(
        value.sessions()[0].first_reason(),
        RetirementReason::EndAllSessions
    );
}
#[test]
fn native_retirement_rejects_foreign_request_and_unlisted_session() {
    let mut parts = descriptor(NativeRetirementAction::Stop).parts().clone();
    parts.request = NativeManagementRequestRef::new(
        ServerId::new([6; 16]),
        [9; 16],
        RequestDigest::from_bytes([8; 32]),
    )
    .unwrap();
    assert_eq!(
        NativeRetirementDescriptor::new(parts),
        Err(RetirementRecordError::InvalidBinding)
    );
    let mut parts = descriptor(NativeRetirementAction::Stop).parts().clone();
    parts.panes.clear();
    assert_eq!(
        NativeRetirementDescriptor::new(parts),
        Err(RetirementRecordError::InvalidBinding)
    );
}
#[test]
fn native_request_reference_rejects_zero_identity_in_constructor_and_deserialization() {
    let server = ServerId::new([7; 16]);
    assert!(
        NativeManagementRequestRef::new(server, [0; 16], RequestDigest::from_bytes([8; 32]))
            .is_err()
    );
    let reference = descriptor(NativeRetirementAction::Stop).request();
    let mut json = serde_json::to_value(reference).unwrap();
    json["id"] = serde_json::to_value([0u8; 16]).unwrap();
    assert!(serde_json::from_value::<NativeManagementRequestRef>(json).is_err());
}

#[test]
fn native_retirement_progress_cannot_rebind_the_request_or_retag_the_first_reason() {
    let original = descriptor(NativeRetirementAction::Stop);
    let mut parts = original.parts().clone();
    let record = parts.sessions[0];
    parts.sessions[0] =
        RetiredSession::new(record.session(), RetirementReason::Stop, record.cleanup()).unwrap();
    let changed = NativeRetirementDescriptor::new(parts).unwrap();
    assert_eq!(
        NativeRetirementMutation::Progress {
            expected: Box::new(original),
            next: Box::new(changed)
        }
        .validate(),
        Err(RetirementRecordError::InvalidBinding)
    );
}
