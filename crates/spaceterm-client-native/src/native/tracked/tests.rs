use super::*;
use spaceterm_core::{
    authority::{FieldVersion, OrganizationRevision},
    identity::*,
    operations::AssessmentRef,
    pairing::*,
};
use spaceterm_protocol::pairing_adapter;

#[test]
fn lost_unpair_assessment_lookup_recovers_original_id_in_both_majors() {
    let server = ServerId::new([1; 16]);
    let pairing_id = PairingId::new(server, 2).unwrap();
    let client = ClientId::new(server, 3).unwrap();
    let stream = OperationStreamId::new(
        OperationStreamKey::new(server, 4).unwrap(),
        pairing_id,
        client,
    )
    .unwrap();
    let operation = OperationId::new(stream, OperationSequence::new(1).unwrap());
    let target = PairingTarget::new(PairingTargetParts {
        server,
        device: DeviceId::new([5; 16]),
        pairing: pairing_id,
    })
    .unwrap();
    let request = CanonicalOperationRequest::new(
        operation,
        Command::Pairing(PairingCommand::AssessUnpair {
            target: target.clone(),
        }),
        &spaceterm_core::operation_ledger::canonical::CanonicalCodec,
    )
    .unwrap();
    let binding = request.binding();
    let expected = UnpairAssessment::new(UnpairAssessmentParts {
        reference: AssessmentRef {
            operation,
            digest: binding.digest,
        },
        target: target.clone(),
        authorization_version: FieldVersion::new(7),
        clients: vec![ClientRecord {
            id: client,
            pairing: pairing_id,
        }],
    })
    .unwrap();
    macro_rules! check {
        ($p:ident) => {{
            let result = $p::Outcome {
                operation: Some(identity::encode_operation(operation)),
                publication_revision: 9,
                result: Some($p::outcome::Result::PairingResult(Box::new(
                    $p::PairingOutcome {
                        operation: Some(Box::new(pairing_adapter::operation_to_wire(operation))),
                        disposition: 4,
                        unpair_assessment: Some(Box::new(
                            spaceterm_protocol::wire::pairing::UnpairAssessment {
                                assessment_operation: Some(pairing_adapter::operation_to_wire(
                                    operation,
                                )),
                                assessment_digest: binding.digest.bytes().to_vec(),
                                target: Some(pairing_adapter::target_to_wire(&target)),
                                authorization_version: 7,
                                clients: vec![identity::encode_client(client)],
                            },
                        )),
                        ..Default::default()
                    },
                ))),
            };
            let mut lookup = $p::OperationLookupResult {
                operation: Some(identity::encode_operation(operation)),
                request_digest: binding.digest.bytes().to_vec(),
                disposition: Some($p::operation_lookup_result::Disposition::Completed(
                    Box::new(result),
                )),
            };
            assert_eq!(
                pairing::$p::lookup(&lookup, binding).unwrap(),
                OperationLookup::Completed {
                    result: Outcome::Pairing(Box::new(PairingOutcome::UnpairAssessed(
                        expected.clone()
                    ))),
                    publication_revision: OrganizationRevision::new(9),
                }
            );
            let mut oversized = lookup.clone();
            let Some($p::operation_lookup_result::Disposition::Completed(completed)) =
                &mut oversized.disposition
            else {
                unreachable!()
            };
            let Some($p::outcome::Result::PairingResult(result)) = &mut completed.result else {
                unreachable!()
            };
            result.devices.push($p::PairedDevice {
                name: "x".repeat(spaceterm_core::operations::MAX_MANAGEMENT_BYTES),
                ..Default::default()
            });
            assert!(
                pairing::$p::lookup(&oversized, binding).is_err(),
                "pairing extension preserves the generic byte ceiling"
            );
            lookup.request_digest[0] ^= 1;
            assert!(
                pairing::$p::lookup(&lookup, binding).is_err(),
                "outer digest is authenticated"
            );
            lookup.request_digest = binding.digest.bytes().to_vec();
            let Some($p::operation_lookup_result::Disposition::Completed(outcome)) =
                &mut lookup.disposition
            else {
                unreachable!()
            };
            outcome.operation = Some(identity::encode_operation(OperationId::new(
                stream,
                OperationSequence::new(2).unwrap(),
            )));
            assert!(
                pairing::$p::lookup(&lookup, binding).is_err(),
                "nested outcome keeps the original identity"
            );
            lookup.disposition = Some($p::operation_lookup_result::Disposition::Pending(true));
            assert_eq!(
                pairing::$p::lookup(&lookup, binding).unwrap(),
                OperationLookup::Pending
            );
        }};
    }
    check!(v1);
    check!(v2);
}
