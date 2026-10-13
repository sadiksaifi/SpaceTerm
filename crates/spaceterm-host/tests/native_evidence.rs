#![cfg(any(target_os = "macos", target_os = "linux"))]

use spaceterm_host::{
    native_deployment::contracts::NativeFailure,
    native_evidence::{
        EvidenceContext, EvidenceIdentity, EvidenceOutcome, JsonlEvidenceEmitter,
        NativeEvidenceEmitter, NativeEvidenceRecord, NativeHostObservation,
    },
    session_capabilities::{CapabilityObservation, LocalEnvironmentComparison},
};
use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
};

fn identity() -> EvidenceIdentity {
    EvidenceIdentity::new([1; 16], [2; 20], false, [3; 32]).unwrap()
}
fn host() -> NativeHostObservation {
    NativeHostObservation::macos([26, 6, 0], [25, 6, 0])
}
fn capability(outcome: u32, effect: u32, cleanup: u32) -> CapabilityObservation {
    let fields = [1u32, 3, 1, outcome, 3, 1, effect, cleanup];
    let bytes: Vec<_> = fields.into_iter().flat_map(u32::to_be_bytes).collect();
    CapabilityObservation::decode(&bytes).unwrap()
}

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn local_environment_comparison_jsonl_preserves_reference_and_context() {
    let output = Output::default();
    let emitter = JsonlEvidenceEmitter::new(output.clone());
    for context in [1u32, 2] {
        let bytes: Vec<_> = [2u32, context, 1, 3, 3]
            .into_iter()
            .flat_map(u32::to_be_bytes)
            .collect();
        let comparison = LocalEnvironmentComparison::decode(&bytes).unwrap();
        let record = NativeEvidenceRecord::environment(
            identity(),
            host(),
            comparison,
            true,
            true,
            true,
            EvidenceOutcome::passed(),
        )
        .unwrap();
        emitter.emit(&record).unwrap();
    }
    assert_eq!(emitter.finish().unwrap().records, 2);
    let bytes = output.0.lock().unwrap();
    let records: Vec<serde_json::Value> = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(records[0]["facts"]["reference"], "sanitized-launch-v1");
    assert_eq!(records[0]["facts"]["path_difference"], "components-changed");
    assert_eq!(records[0]["facts"]["locale_difference"], "values-changed");
    assert_eq!(records[0]["context"], "gui-parented");
    assert_eq!(records[1]["context"], "daemon-parented");
    assert_eq!(records[1]["sequence"], 2);
}

#[test]
fn native_capability_credit_requires_observed_effect_attribution_and_cleanup() {
    let record = NativeEvidenceRecord::capability(identity(), host(), capability(1, 1, 1)).unwrap();
    let bytes = record.encode_jsonl(1).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["case"], "microphone");
    assert_eq!(value["facts"]["effect"], "passed");
    let wrong = capability(2, 0, 0); // A denied result with no attempted denial.
    assert!(matches!(
        NativeEvidenceRecord::capability(identity(), host(), wrong),
        Err(NativeFailure::InvalidRecord)
    ));
    let unresolved =
        NativeEvidenceRecord::capability(identity(), host(), capability(10, 1, 2)).unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&unresolved.encode_jsonl(1).unwrap()).unwrap();
    assert_eq!(value["result"], "cleanup-unresolved");
    assert_eq!(value["failure"], "cleanup-unresolved");
    assert_eq!(value["facts"]["effect"], "passed");
}

#[test]
fn duplicate_and_candidate_substitution_latch_emitter_failure() {
    let emitter = JsonlEvidenceEmitter::new(Output::default());
    let record = NativeEvidenceRecord::capability(identity(), host(), capability(1, 1, 1)).unwrap();
    emitter.emit(&record).unwrap();
    assert_eq!(emitter.emit(&record), Err(NativeFailure::InvalidRecord));
    assert_eq!(emitter.finish(), Err(NativeFailure::InvalidRecord));
    let emitter = JsonlEvidenceEmitter::new(Output::default());
    emitter.emit(&record).unwrap();
    let substituted = NativeEvidenceRecord::capability(
        EvidenceIdentity::new([1; 16], [2; 20], false, [4; 32]).unwrap(),
        host(),
        capability(1, 1, 1),
    )
    .unwrap();
    assert_eq!(
        emitter.emit(&substituted),
        Err(NativeFailure::IdentityMismatch)
    );
    assert_eq!(emitter.finish(), Err(NativeFailure::IdentityMismatch));
}

#[test]
fn short_write_and_flush_loss_cannot_be_retried_as_success() {
    struct LostOutput {
        bytes: usize,
        flush_fails: bool,
    }
    impl Write for LostOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.bytes == 0 {
                return Err(io::Error::other("private native detail"));
            }
            let count = bytes.len().min(self.bytes);
            self.bytes -= count;
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            if self.flush_fails {
                Err(io::Error::other("private native detail"))
            } else {
                Ok(())
            }
        }
    }
    let record = NativeEvidenceRecord::capability(identity(), host(), capability(1, 1, 1)).unwrap();
    let emitter = JsonlEvidenceEmitter::new(LostOutput {
        bytes: 7,
        flush_fails: false,
    });
    assert_eq!(emitter.emit(&record), Err(NativeFailure::IoUnavailable));
    assert_eq!(emitter.emit(&record), Err(NativeFailure::IoUnavailable));
    assert_eq!(emitter.finish(), Err(NativeFailure::IoUnavailable));
    let emitter = JsonlEvidenceEmitter::new(LostOutput {
        bytes: 8192,
        flush_fails: true,
    });
    emitter.emit(&record).unwrap();
    assert_eq!(emitter.finish(), Err(NativeFailure::IoUnavailable));
    assert_eq!(emitter.finish(), Err(NativeFailure::IoUnavailable));
}

#[test]
fn missing_shell_observations_remain_pending_not_passed() {
    let bytes: Vec<_> = [2u32, 1, 1, 1, 1]
        .into_iter()
        .flat_map(u32::to_be_bytes)
        .collect();
    let comparison = LocalEnvironmentComparison::decode(&bytes).unwrap();
    assert!(matches!(
        NativeEvidenceRecord::environment(
            identity(),
            host(),
            comparison,
            false,
            true,
            true,
            EvidenceOutcome::passed(),
        ),
        Err(NativeFailure::InvalidRecord)
    ));
    let pending = NativeEvidenceRecord::environment(
        identity(),
        host(),
        comparison,
        false,
        true,
        false,
        EvidenceOutcome::pending(NativeFailure::OutcomePending),
    )
    .unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&pending.encode_jsonl(1).unwrap()).unwrap();
    assert_eq!(value["result"], "pending");
    assert_eq!(value["facts"]["login"], false);
    assert_eq!(EvidenceContext::GuiParented.as_str(), "gui-parented");
}

// The composition task consumes output from the actual public emitter. This is
// test composition only and carries no native-proof credit or cleanup authority.
#[test]
fn native_jsonl_round_trip() {
    let temporary = tempfile::tempdir().unwrap();
    let path = std::env::var_os("SPACETERM_TEST_EVIDENCE_OUTPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().join("observations.jsonl"));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    let emitter = JsonlEvidenceEmitter::new(file);
    for context in [1u32, 2] {
        let comparison: Vec<_> = [2u32, context, 1, 3, 3]
            .into_iter()
            .flat_map(u32::to_be_bytes)
            .collect();
        emitter
            .emit(
                &NativeEvidenceRecord::environment(
                    identity(),
                    host(),
                    LocalEnvironmentComparison::decode(&comparison).unwrap(),
                    true,
                    true,
                    true,
                    EvidenceOutcome::passed(),
                )
                .unwrap(),
            )
            .unwrap();
    }
    for record in scenario_records() {
        emitter.emit(&record).unwrap();
    }
    assert_eq!(emitter.finish().unwrap().records, 49);
    use spaceterm_host::native_evidence::{EvidenceResult, NativeScenario};
    let failed = NativeEvidenceRecord::observed(
        identity(),
        host(),
        EvidenceContext::DaemonParented,
        NativeScenario::Privilege {
            observation: privilege_observation(),
            primary_gid: 20,
            supplementary_groups: vec![20, 0],
        },
        EvidenceOutcome::new(
            EvidenceResult::Failed,
            Some(NativeFailure::PrivilegeDropUnproven),
        )
        .unwrap(),
    )
    .unwrap();
    let mut rejected = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path.with_extension("root-group.jsonl"))
        .unwrap();
    rejected
        .write_all(&failed.encode_jsonl(1).unwrap())
        .unwrap();
}

fn privilege_observation() -> spaceterm_host::native_evidence::PrivilegeObservation {
    use spaceterm_host::native_evidence::PrivilegeObservation;
    PrivilegeObservation {
        real_ids_verified: true,
        effective_ids_verified: true,
        saved_ids_verified: true,
        supplementary_groups_verified: true,
        execution_bound: true,
        hardened_runtime_verified: true,
        descriptor_scope_verified: true,
        secret_channels_closed: true,
        custody_descriptors_absent: true,
        retriever_absent: true,
        root_process_absent: true,
        root_listener_absent: true,
    }
}

fn scenario_records() -> Vec<NativeEvidenceRecord> {
    use spaceterm_host::native_evidence::*;
    let mut records = Vec::new();
    let mut add = |context, scenario, outcome| {
        records.push(
            NativeEvidenceRecord::observed(identity(), host(), context, scenario, outcome).unwrap(),
        );
    };
    add(
        EvidenceContext::DaemonParented,
        NativeScenario::Stopped {
            desired_state: ObservedDesiredState::Stopped,
            credential_calls: 0,
            installation_retention_calls: 0,
            process_absent: true,
            endpoint_absent: true,
        },
        EvidenceOutcome::passed(),
    );
    for (case, trigger) in [
        (StartupCase::ApprovedSelection, ObservedTrigger::None),
        (StartupCase::Crash, ObservedTrigger::Crash),
        (StartupCase::Start, ObservedTrigger::Start),
        (StartupCase::Restart, ObservedTrigger::Restart),
    ] {
        add(
            EvidenceContext::DaemonParented,
            NativeScenario::Startup {
                case,
                desired_state: ObservedDesiredState::Running,
                selection: ObservedSelection::Approved,
                trigger,
                observation: StartupObservation {
                    readiness_authenticated: true,
                    original_process_retired: true,
                    endpoint_bound: true,
                },
            },
            EvidenceOutcome::passed(),
        );
    }
    add(
        EvidenceContext::DaemonParented,
        NativeScenario::Credential(CredentialObservation {
            policy_accepted: true,
            interaction_disabled: true,
            retrieved: true,
            secret_wiped: true,
            release_rechecked: true,
            retriever_absent: true,
        }),
        EvidenceOutcome::passed(),
    );
    for case in [
        CredentialDenialCase::UnprivilegedCopy,
        CredentialDenialCase::UnsignedCopy,
        CredentialDenialCase::WrongIdentifier,
        CredentialDenialCase::OtherAccount,
        CredentialDenialCase::Stopped,
        CredentialDenialCase::StaleGeneration,
    ] {
        add(
            EvidenceContext::DaemonParented,
            NativeScenario::CredentialDenial {
                case,
                retrieved: false,
                observation: CredentialDenialObservation {
                    attempt_observed: true,
                    release_denied: true,
                    secret_wiped: true,
                    retriever_absent: true,
                },
            },
            EvidenceOutcome::new(
                EvidenceResult::Denied,
                Some(NativeFailure::CredentialPolicyRejected),
            )
            .unwrap(),
        );
    }
    add(
        EvidenceContext::DaemonParented,
        NativeScenario::Privilege {
            observation: privilege_observation(),
            primary_gid: 20,
            supplementary_groups: (20..40).collect(),
        },
        EvidenceOutcome::passed(),
    );
    add(
        EvidenceContext::DaemonParented,
        NativeScenario::Drain {
            started_writes: 1,
            settled_writes: 1,
            retriever_absent: true,
            phase_order: vec![
                StopPhase::StoppedDurable,
                StopPhase::AdmissionsClosed,
                StopPhase::WritesSettled,
                StopPhase::SessionsRetired,
                StopPhase::ProcessAbsent,
                StopPhase::EndpointAbsent,
                StopPhase::SupervisorRemoved,
            ],
        },
        EvidenceOutcome::passed(),
    );
    add(
        EvidenceContext::DaemonParented,
        NativeScenario::Update(UpdateObservation {
            fresh_version_authenticated: true,
            approval_durable: true,
            candidate_running: true,
            identity_retained: true,
            pairings_retained: true,
            organization_retained: true,
            roots_retained: true,
            remote_access_retained: true,
            previous_process_absent: true,
            sessions_retired: true,
            started_writes_settled: true,
        }),
        EvidenceOutcome::passed(),
    );
    add(
        EvidenceContext::GuiParented,
        NativeScenario::Bootstrap(BootstrapObservation {
            candidate_manifest_verified: true,
            candidate_signatures_verified: true,
            protected_admin_digest_verified: true,
            protected_admin_signature_verified: true,
            protected_admin_policy_verified: true,
            verified_before_root_exec: true,
            receipt_durable_before_effects: true,
            root_execution_observed: true,
            fresh_copy: true,
            collision_check_complete: true,
        }),
        EvidenceOutcome::passed(),
    );
    add(
        EvidenceContext::GuiParented,
        NativeScenario::Cleanup {
            observation: CleanupObservation {
                receipt_retained: true,
                scope_verified: true,
                process_absent: true,
                retriever_absent: true,
                sessions_absent: true,
                endpoint_absent: true,
            },
            remaining_objects: 0,
        },
        EvidenceOutcome::passed(),
    );
    for (case, result, failure) in [
        (
            AuthorizationCase::Denied,
            EvidenceResult::Denied,
            Some(NativeFailure::AuthorizationDenied),
        ),
        (
            AuthorizationCase::Cancelled,
            EvidenceResult::Cancelled,
            Some(NativeFailure::AuthorizationCancelled),
        ),
        (
            AuthorizationCase::Unavailable,
            EvidenceResult::Unavailable,
            Some(NativeFailure::AuthorizationUnavailable),
        ),
        (AuthorizationCase::Granted, EvidenceResult::Passed, None),
    ] {
        add(
            EvidenceContext::GuiParented,
            NativeScenario::Authorization {
                case,
                operation: EvidenceOperation::Start,
                attempt_observed: true,
                granted: result == EvidenceResult::Passed,
                caller_bound: result == EvidenceResult::Passed,
                helper_absent: true,
            },
            EvidenceOutcome::new(result, failure).unwrap(),
        );
    }
    for context in [1u32, 2] {
        for capability in 1..=13 {
            let bytes: Vec<_> = [1u32, capability, context, 1, 3, 1, 1, 1]
                .into_iter()
                .flat_map(u32::to_be_bytes)
                .collect();
            records.push(
                NativeEvidenceRecord::capability(
                    identity(),
                    host(),
                    CapabilityObservation::decode(&bytes).unwrap(),
                )
                .unwrap(),
            );
        }
    }
    records
}

#[test]
fn non_capability_passes_require_effects_and_keep_actual_failed_selection() {
    use spaceterm_host::native_evidence::*;
    assert!(matches!(
        NativeEvidenceRecord::observed(
            identity(),
            host(),
            EvidenceContext::DaemonParented,
            NativeScenario::Credential(CredentialObservation::default()),
            EvidenceOutcome::passed()
        ),
        Err(NativeFailure::InvalidRecord)
    ));
    assert!(matches!(
        NativeEvidenceRecord::observed(
            identity(),
            host(),
            EvidenceContext::DaemonParented,
            NativeScenario::Privilege {
                observation: PrivilegeObservation::default(),
                primary_gid: 20,
                supplementary_groups: vec![20]
            },
            EvidenceOutcome::passed()
        ),
        Err(NativeFailure::InvalidRecord)
    ));
    let scenario = || NativeScenario::Startup {
        case: StartupCase::Start,
        desired_state: ObservedDesiredState::Running,
        selection: ObservedSelection::Staged,
        trigger: ObservedTrigger::Start,
        observation: StartupObservation {
            readiness_authenticated: true,
            original_process_retired: true,
            endpoint_bound: true,
        },
    };
    assert!(matches!(
        NativeEvidenceRecord::observed(
            identity(),
            host(),
            EvidenceContext::DaemonParented,
            scenario(),
            EvidenceOutcome::passed()
        ),
        Err(NativeFailure::InvalidRecord)
    ));
    let failed = NativeEvidenceRecord::observed(
        identity(),
        host(),
        EvidenceContext::DaemonParented,
        scenario(),
        EvidenceOutcome::new(
            EvidenceResult::Failed,
            Some(NativeFailure::UnsafeNativeState),
        )
        .unwrap(),
    )
    .unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&failed.encode_jsonl(1).unwrap()).unwrap();
    assert_eq!(value["facts"]["selection"], "staged");
    assert_eq!(value["result"], "failed");
}

#[test]
fn passed_darwin_privilege_proof_rejects_root_in_the_full_group_list() {
    use spaceterm_host::native_evidence::*;
    let observation = privilege_observation();
    for supplementary_groups in [vec![20, 0], (20..40).chain([0]).collect()] {
        let scenario = || NativeScenario::Privilege {
            observation,
            primary_gid: 20,
            supplementary_groups: supplementary_groups.clone(),
        };
        assert!(matches!(
            NativeEvidenceRecord::observed(
                identity(),
                host(),
                EvidenceContext::DaemonParented,
                scenario(),
                EvidenceOutcome::passed(),
            ),
            Err(NativeFailure::InvalidRecord)
        ));
        // Keep the actual root-group observation when reporting the failed drop.
        let failed = NativeEvidenceRecord::observed(
            identity(),
            host(),
            EvidenceContext::DaemonParented,
            scenario(),
            EvidenceOutcome::new(
                EvidenceResult::Failed,
                Some(NativeFailure::PrivilegeDropUnproven),
            )
            .unwrap(),
        )
        .unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&failed.encode_jsonl(1).unwrap()).unwrap();
        assert_eq!(
            value["facts"]["supplementary_groups"],
            serde_json::json!(supplementary_groups)
        );
    }
}

#[test]
fn a_released_secret_cannot_be_reported_as_unavailable_or_unrun() {
    use spaceterm_host::native_evidence::*;
    for result in [EvidenceResult::Unavailable, EvidenceResult::Unrun] {
        assert!(matches!(
            NativeEvidenceRecord::observed(
                identity(),
                host(),
                EvidenceContext::DaemonParented,
                NativeScenario::Credential(CredentialObservation {
                    retrieved: true,
                    ..CredentialObservation::default()
                }),
                EvidenceOutcome::new(result, Some(NativeFailure::CredentialUnavailable)).unwrap()
            ),
            Err(NativeFailure::InvalidRecord)
        ));
    }
}
