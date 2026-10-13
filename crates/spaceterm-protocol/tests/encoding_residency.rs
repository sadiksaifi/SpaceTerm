#[path = "support/allocation_probe.rs"]
mod allocation_probe;
#[expect(dead_code, reason = "shared fixture supports other protocol tests")]
mod support;

use spaceterm_protocol::{
    codec::Payload,
    limits::MAX_CLIPBOARD_BYTES,
    wire::{v1, v2},
};

#[test]
fn maximum_native_text_encoding_uses_at_most_two_scratch_copies() {
    for major in [1, 2] {
        let (mut client, mut server) = support::Fixture::new().connect(&[major]);
        macro_rules! payload {
            ($p:ident, $variant:ident) => {
                Payload::$variant($p::Message {
                    kind: Some($p::message::Kind::NativeRequest($p::NativeRequest {
                        fence: Some(support::fence()),
                        request: 1,
                        action: Some($p::native_request::Action::Clipboard(
                            $p::ClipboardRequest {
                                read: false,
                                text: vec![b'x'; MAX_CLIPBOARD_BYTES],
                                deadline_ms: 0,
                            },
                        )),
                        not_before_delay_ms: 0,
                        remaining_ms: Some(1000),
                    })),
                })
            };
        }
        let payload = match major {
            1 => payload!(v1, Major1),
            _ => payload!(v2, Major2),
        };
        let (encoded, peak) = allocation_probe::measure(|| server.encode(&payload).unwrap());
        assert!(
            peak <= 2 * MAX_CLIPBOARD_BYTES + 16 * 1024,
            "encoding scratch retained {peak} bytes for a maximum native request"
        );
        assert!(
            client.decode(&encoded).unwrap() == payload,
            "native request round trip must preserve the payload"
        );
    }
}
