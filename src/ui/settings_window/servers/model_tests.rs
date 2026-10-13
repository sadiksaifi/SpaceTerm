use super::super::{
    copy,
    tests::{local, operation, pairing, staged_server},
};
use super::*;
use spaceterm_core::{identity::DeviceId, pairing::PairingTargetParts};

fn snapshot(server: ServerSummary) -> ServersSnapshot {
    ServersSnapshot {
        servers: vec![server],
        pending_unpairs: vec![],
    }
}

#[test]
fn verified_protocol_profiles_identify_the_side_that_needs_updating() {
    assert_eq!(
        VerifiedCompatibility::from_verified_advertisements(&[2, 1], &[4, 3]).unwrap(),
        VerifiedCompatibility::Incompatible(IncompatibleSide::ThisDevice)
    );
    assert_eq!(
        VerifiedCompatibility::from_verified_advertisements(&[4, 3], &[2, 1]).unwrap(),
        VerifiedCompatibility::Incompatible(IncompatibleSide::Server)
    );
    assert_eq!(
        VerifiedCompatibility::from_verified_advertisements(&[3, 1], &[4, 2]).unwrap(),
        VerifiedCompatibility::UnsupportedProfiles
    );
    assert!(VerifiedCompatibility::from_verified_advertisements(&[], &[2, 1]).is_err());
    assert!(VerifiedCompatibility::from_verified_advertisements(&[1, 2], &[2, 1]).is_err());
    assert_eq!(
        VerifiedCompatibility::from_verified_advertisements(&[2, 1], &[1]).unwrap(),
        VerifiedCompatibility::Compatible { major: 1 }
    );
}

#[test]
fn a_bound_action_survives_selection_changes_but_not_pairing_or_runtime_replacement() {
    let server = local();
    let original = snapshot(server.clone());
    let request =
        BoundServerRequest::bind(ServerRequest::Stop(server.key.clone()), &original).unwrap();
    let mut reordered = original.clone();
    reordered.servers.insert(
        0,
        ServerSummary {
            key: ServerKey::new("other"),
            pairing: pairing(2, 1),
            ..local()
        },
    );
    assert!(request.still_matches(&reordered));
    assert!(!request.still_matches(&snapshot(ServerSummary {
        pairing: pairing(1, 2),
        ..server.clone()
    })));
    assert!(!request.still_matches(&snapshot(ServerSummary {
        runtime_epoch: Some(ServerEpoch::new(2)),
        ..server
    })));
}

#[test]
fn activation_compares_the_original_server_candidate_digest_and_assessment() {
    let server = staged_server(local());
    let assessment = server.installations.activation.clone().unwrap();
    let request = BoundServerRequest::bind(
        ServerRequest::InstallUpdate {
            server: server.key.clone(),
            version: assessment.candidate.version.clone(),
            assessment,
        },
        &snapshot(server.clone()),
    )
    .unwrap();
    for change in 0..9 {
        let mut changed = server.clone();
        match change {
            0 => changed.pairing = pairing(1, 2),
            1 => {
                changed
                    .installations
                    .staged
                    .as_mut()
                    .unwrap()
                    .artifact_sha256 = [3; 32]
            }
            2 => {
                changed.installations.staged.as_mut().unwrap().installation =
                    InstallationId(NonZeroU64::new(3).unwrap())
            }
            3 => changed.installations.selection_generation = NonZeroU64::new(2),
            4 => {
                changed.installations.activation.as_mut().unwrap().reference =
                    AssessmentToken::new("replacement".into()).unwrap()
            }
            5 => changed.status = ServerStatus::Stopped,
            6 => changed.installations.approved = changed.installations.staged.clone(),
            7 => changed.installations.running = changed.installations.staged.clone(),
            _ => {
                changed
                    .installations
                    .activation
                    .as_mut()
                    .unwrap()
                    .native_request = NativeRequestId::new([2; 16]).unwrap()
            }
        }
        assert!(
            !request.still_matches(&snapshot(changed)),
            "changed comparison {change}"
        );
    }
    // Displaying the same version can never make an unassessed staged candidate executable.
    let mut labels_only = local();
    labels_only.staged_version = Some("1.5.0".into());
    assert!(labels_only.installations.activation.is_none());
    assert_ne!(
        super::super::version_action(&labels_only),
        super::super::VersionAction::Update {
            version: "1.5.0".into(),
            enabled: true
        }
    );
}

#[test]
fn pending_notice_dismissal_keeps_its_original_pairing_and_operation() {
    let mut snapshot = ServersSnapshot {
        servers: vec![],
        pending_unpairs: vec![PendingUnpair {
            id: "old-task".into(),
            server_name: "Old server".into(),
            original_pairing: pairing(1, 1),
            operation: operation(1),
            phase: PendingUnpairPhase::AwaitingConfirmation(PendingUnpairReason::Unreachable),
        }],
    };
    let request = BoundServerRequest::bind(
        ServerRequest::DismissUnpairNotice("old-task".into()),
        &snapshot,
    )
    .unwrap();
    assert!(matches!(
        request.target,
        ServerRequestTarget::PendingUnpair { .. }
    ));
    assert!(
        BoundServerRequest::bind(ServerRequest::Start(ServerKey::new("old-task")), &snapshot)
            .is_none()
    );
    snapshot.pending_unpairs[0].original_pairing = pairing(1, 2);
    assert!(!request.still_matches(&snapshot));
}

#[test]
fn device_inventory_rejects_cross_server_duplicate_and_oversized_pages() {
    let key = pairing(1, 1);
    let device = PairedDevice {
        target: PairingTarget::new(PairingTargetParts {
            server: key.parts().server,
            pairing: key.parts().pairing,
            device: DeviceId::new([1; 16]),
        })
        .unwrap(),
        name: "This device".into(),
    };
    let mut page = PairedDevices {
        version: FieldVersion::new(1),
        this_device: DeviceId::new([1; 16]),
        total: 1,
        devices: vec![device.clone()],
        next: None,
    };
    assert!(page.valid_for(&key));
    assert!(!page.valid_for(&pairing(2, 1)));
    page.devices.push(device.clone());
    page.total = 2;
    assert!(!page.valid_for(&key));
    page.devices = vec![device; 257];
    page.total = 257;
    assert!(!page.valid_for(&key));
}

#[test]
fn compatibility_copy_distinguishes_client_restart_from_server_session_loss() {
    let old_client = ServerSummary {
        status: ServerStatus::Incompatible(IncompatibleSide::ThisDevice),
        ..local()
    };
    let old_server = ServerSummary {
        status: ServerStatus::Incompatible(IncompatibleSide::Server),
        ..local()
    };
    assert!(copy::status_explanation(&old_client).contains("Terminal Sessions keep running"));
    assert!(
        copy::status_explanation(&old_server).contains("Terminal Sessions end for every client")
    );
}

#[test]
fn pending_notices_explain_the_failure_and_do_not_offer_host_management() {
    for (reason, expected) in [
        (PendingUnpairReason::Unreachable, "can't be reached"),
        (PendingUnpairReason::Incompatible, "cannot use one protocol"),
        (PendingUnpairReason::RouteChanged, "original server"),
        (
            PendingUnpairReason::CredentialUnavailable,
            "credential is unavailable",
        ),
        (
            PendingUnpairReason::StorageUnavailable,
            "record is unavailable",
        ),
        (
            PendingUnpairReason::OutcomeUnknown,
            "reply could not be verified",
        ),
    ] {
        let pending = PendingUnpair {
            id: "pending".into(),
            server_name: "Old server".into(),
            original_pairing: pairing(1, 1),
            operation: operation(1),
            phase: PendingUnpairPhase::AwaitingConfirmation(reason),
        };
        let (_, text) = copy::pending_unpair_text(&pending);
        assert!(text.contains(expected), "reason {reason:?}: {text}");
        assert!(text.contains("including after you dismiss this"));
        for forbidden in [
            "Start Server",
            "Restart Server",
            "Install Update",
            "pair again",
        ] {
            assert!(!text.contains(forbidden));
        }
        let cleanup = PendingUnpair {
            phase: PendingUnpairPhase::CredentialCleanup,
            ..pending
        };
        let (_, text) = copy::pending_unpair_text(&cleanup);
        assert!(text.contains("confirmed revocation"));
        assert!(text.contains("only this pairing's retained credential"));
        assert!(!text.contains("keeps asking"));
    }
}

#[test]
fn account_service_copy_only_requires_authorization_to_enable_lingering() {
    let login_session = copy::status_explanation(&local());
    assert!(login_session.contains("starts at login and stops at logout"));
    assert!(login_session.contains("does not require a password"));
    let boot_service = copy::status_explanation(&ServerSummary {
        service_lifecycle: Some(ServerServiceLifecycle::BootWithLingering),
        ..local()
    });
    assert!(boot_service.contains("starts at boot and keeps running after logout"));
    assert!(boot_service.contains("administrator authorization once"));
    assert!(boot_service.contains("Start, Stop, Restart, and updates do not require a password"));
}

#[test]
fn permission_copy_identifies_the_selected_machine_without_transferring_client_grants() {
    let local_text = copy::permissions_explanation(&local());
    assert!(local_text.contains("Studio"));
    assert!(local_text.contains("don't carry over"));
    let remote_text = copy::permissions_explanation(&ServerSummary {
        route: ServerRoute::Wss {
            authority: "example.invalid".into(),
        },
        ..local()
    });
    assert!(remote_text.contains("on its machine"));
    assert!(remote_text.contains("Permissions on this device don't carry over"));
}

#[test]
fn paired_device_actions_carry_the_exact_assessed_pairing_not_the_device_name() {
    use spaceterm_client::management_cli::confirmation::{AssessmentEvidence, ManagementAction};
    use spaceterm_core::{
        identity::ClientId,
        observations::MonotonicTick,
        operations::{AssessmentRef, RequestDigest},
        pairing::{ClientRecord, UnpairAssessment, UnpairAssessmentParts},
    };
    let server = local();
    let key = server.pairing.parts();
    let target = PairingTarget::new(PairingTargetParts {
        server: key.server,
        pairing: key.pairing,
        device: DeviceId::new([1; 16]),
    })
    .unwrap();
    let evidence = UnpairAssessment::new(UnpairAssessmentParts {
        reference: AssessmentRef {
            operation: operation(1),
            digest: RequestDigest::from_bytes([1; 32]),
        },
        target: target.clone(),
        authorization_version: FieldVersion::new(1),
        clients: vec![ClientRecord {
            id: ClientId::new(key.server, 1).unwrap(),
            pairing: key.pairing,
        }],
    })
    .unwrap();
    let assessment = ManagementAssessment::new(
        AssessmentToken::new("unpair-1".into()).unwrap(),
        ManagementAction::Unpair {
            target: target.clone(),
        },
        AssessmentEvidence::Unpair(evidence),
        MonotonicTick::new(1),
        MonotonicTick::new(20),
    )
    .unwrap();
    let server = ServerSummary {
        paired_devices: Some(PairedDevices {
            version: FieldVersion::new(3),
            this_device: DeviceId::new([1; 16]),
            total: 1,
            devices: vec![PairedDevice {
                target: target.clone(),
                name: "This device".into(),
            }],
            next: None,
        }),
        unpair_assessment: Some(assessment.clone()),
        ..server
    };
    let state = snapshot(server.clone());
    let request = ServerRequest::UnpairDevice {
        server: server.key.clone(),
        target: target.clone(),
        assessment: Box::new(assessment.clone()),
    };
    let bound = BoundServerRequest::bind(request, &state).unwrap();
    assert!(bound.still_matches(&state));
    let mut revised = state.clone();
    revised.servers[0].paired_devices.as_mut().unwrap().version = FieldVersion::new(4);
    assert!(!bound.still_matches(&revised));
    revised = state.clone();
    revised.servers[0].unpair_assessment = None;
    assert!(!bound.still_matches(&revised));
    revised = state.clone();
    revised.servers[0].status = ServerStatus::Offline;
    assert!(!bound.still_matches(&revised));
    let wrong_device = PairingTarget::new(PairingTargetParts {
        device: DeviceId::new([2; 16]),
        ..target.parts().clone()
    })
    .unwrap();
    assert!(
        BoundServerRequest::bind(
            ServerRequest::UnpairDevice {
                server: server.key.clone(),
                target: wrong_device,
                assessment: Box::new(assessment),
            },
            &state
        )
        .is_none()
    );
    assert!(
        BoundServerRequest::bind(
            ServerRequest::ReadPairedDevices {
                server: server.key,
                after: None,
                version: None
            },
            &state
        )
        .is_some()
    );
}

#[test]
fn verified_profile_projection_does_not_guess_an_update_for_interleaved_majors() {
    let compatibility =
        VerifiedCompatibility::from_verified_advertisements(&[3, 1], &[4, 2]).unwrap();
    let server = ServerSummary {
        status: compatibility.connection_status(),
        compatibility: Some(compatibility),
        ..local()
    };
    let text = copy::status_explanation(&server);
    assert!(text.contains("do not establish which side needs updating"));
    assert!(!server.connected());
    assert_ne!(
        super::super::version_action(&server),
        super::super::VersionAction::Restart { enabled: true }
    );
}
