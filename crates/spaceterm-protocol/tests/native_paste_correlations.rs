use prost::Message;
use spaceterm_core::{authority::*, identity::*, native_requests::*, observations::MonotonicTick};
use spaceterm_protocol::{
    convert::{identity, native_requests},
    wire::{v1, v2},
};

fn request() -> NativeRequest {
    let server = ServerId::new([7; 16]);
    let pane = PaneId::new(server, 3).unwrap();
    let client = ClientId::new(server, 2).unwrap();
    let fence = EffectFence::new(
        SessionKey {
            pane,
            epoch: ServerEpoch::new(4),
            generation: SessionGeneration::new(5),
        },
        ControlGrant::new(pane, client, ControlGeneration::new(6)).unwrap(),
        ConnectionBinding::new(client, ConnectionGeneration::new(7)),
        ContextGeneration::new(8),
        None,
    )
    .unwrap();
    NativeRequest::new(
        fence,
        NativeRequestId::new(41).unwrap(),
        MonotonicTick::new(100),
        MonotonicTick::new(100),
        Some(MonotonicTick::new(30100)),
        NativeRequestKind::UnsafePaste {
            approval: PasteApprovalId::new(19).unwrap(),
            text: NativeText::new("one\ntwo".into()).unwrap(),
        },
    )
    .unwrap()
}

#[test]
fn native_unsafe_paste_roundtrip_retains_distinct_runtime_request_in_both_majors() {
    macro_rules! check { ($p:ident) => {{
        let request = request();
        let mut bytes = native_requests::$p::encode_request(&request, MonotonicTick::new(100)).unwrap().encode_to_vec();
        // Native request 41 originated from runtime request 127, field 9.
        bytes.extend_from_slice(&[0x48, 127]);
        let wire = $p::NativeRequest::decode(bytes.as_slice()).unwrap();
        let decoded = native_requests::$p::decode_request(&wire, MonotonicTick::new(100)).unwrap();
        assert_eq!(native_requests::$p::encode_request(&decoded, MonotonicTick::new(100)).unwrap().encode_to_vec(), bytes);
        let completion = NativeCompletion::new(&decoded, NativeCompletionKind::Paste { approval: PasteApprovalId::new(19).unwrap(), decision: PasteDecision::Confirm }).unwrap();
        let completion = native_requests::$p::encode_completion(&completion).unwrap();
        assert_eq!(completion.request, 41);
        assert_eq!(completion.fence, Some(identity::encode_fence(request.fence())));
        let bytes = completion.encode_to_vec();
        assert!(bytes.ends_with(&[0x50, 127]), "completion must retain originating runtime request independently of native identity");
    }}; }
    check!(v1);
    check!(v2);
}

#[test]
fn native_paste_completion_rejects_missing_or_changed_origin_in_both_majors() {
    macro_rules! check {
        ($p:ident) => {{
            let request = request().with_runtime_request(127).unwrap();
            let completion = NativeCompletion::new(
                &request,
                NativeCompletionKind::Paste {
                    approval: PasteApprovalId::new(19).unwrap(),
                    decision: PasteDecision::Confirm,
                },
            )
            .unwrap();
            let mut wire = native_requests::$p::encode_completion(&completion).unwrap();
            wire.effect_request = 999;
            assert_eq!(
                native_requests::$p::decode_completion(&wire, &request).unwrap(),
                completion
            );
            wire.originating_runtime_request = 128;
            assert_eq!(
                native_requests::$p::decode_completion(&wire, &request),
                Err(spaceterm_protocol::ProtocolError::Malformed)
            );
            wire.originating_runtime_request = 0;
            assert_eq!(
                native_requests::$p::decode_completion(&wire, &request),
                Err(spaceterm_protocol::ProtocolError::Malformed)
            );
            let mut wire =
                native_requests::$p::encode_request(&request, MonotonicTick::new(100)).unwrap();
            wire.action = Some($p::native_request::Action::Notification(
                $p::NotificationRequest {
                    kind: 1,
                    exit_status: None,
                    duration_millis: 0,
                },
            ));
            assert_eq!(
                native_requests::$p::decode_request(&wire, MonotonicTick::new(100)),
                Err(spaceterm_protocol::ProtocolError::Malformed)
            );
        }};
    }
    check!(v1);
    check!(v2);
}
