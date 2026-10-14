//! Public runtime bodies use one bound at encoding and generated predecode admission.

use prost::Message;
use spaceterm_protocol::{
    ProtocolError,
    codec::Payload,
    limits::MAX_ACCOUNT_RUNTIME_BYTES,
    wire::{common, v1, v2},
};

fn payload(major: u32, reply: bool, request: u64, generation: u64, length: usize) -> Payload {
    let value = common::AccountRuntimeMessage {
        request,
        request_generation: generation,
        body: vec![b'x'; length],
    };
    match (major, reply) {
        (1, false) => Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::AccountRuntimeRequest(value)),
        }),
        (1, true) => Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::AccountRuntimeReply(value)),
        }),
        (2, false) => Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::AccountRuntimeRequest(value)),
        }),
        (2, true) => Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::AccountRuntimeReply(value)),
        }),
        _ => panic!("supported protocol major required"),
    }
}

fn raw_bytes(value: &Payload) -> Vec<u8> {
    match value {
        Payload::Major1(message) => message.encode_to_vec(),
        Payload::Major2(message) => message.encode_to_vec(),
    }
}

#[test]
fn runtime_body_at_the_named_bound_preserves_request_generation_in_both_majors() {
    for major in [1, 2] {
        for reply in [false, true] {
            for generation in [0, 17, u64::MAX] {
                let value = payload(major, reply, 3, generation, MAX_ACCOUNT_RUNTIME_BYTES);
                let encoded = value.encode().unwrap();
                assert_eq!(Payload::decode(major, &encoded), Ok(value));
            }
        }
    }
}

#[test]
fn runtime_body_above_the_named_bound_is_rejected_before_generated_decode() {
    for major in [1, 2] {
        for reply in [false, true] {
            let value = payload(major, reply, 3, 17, MAX_ACCOUNT_RUNTIME_BYTES + 1);
            assert_eq!(value.encode(), Err(ProtocolError::LimitExceeded));
            // Prost bypasses admission so this checks the real hostile-input boundary.
            assert_eq!(
                Payload::decode(major, &raw_bytes(&value)),
                Err(ProtocolError::LimitExceeded)
            );
        }
    }
}

#[test]
fn runtime_body_requires_a_nonzero_request_and_nonempty_body() {
    for major in [1, 2] {
        for reply in [false, true] {
            for value in [
                payload(major, reply, 0, 17, 1),
                payload(major, reply, 3, 17, 0),
            ] {
                assert_eq!(value.encode(), Err(ProtocolError::Malformed));
                assert_eq!(
                    Payload::decode(major, &raw_bytes(&value)),
                    Err(ProtocolError::Malformed)
                );
            }
        }
    }
}
