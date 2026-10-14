//! Native requests retain their captured fence and expiry. Wire text is transient.

use super::identity::*;
use crate::ProtocolError;
use spaceterm_core::{native_requests::*, observations::MonotonicTick};

fn check_fence(fence: spaceterm_core::authority::EffectFence) -> Result<(), ProtocolError> {
    if fence.content().is_some() || fence.session().generation.get() == 0 {
        Err(ProtocolError::Malformed)
    } else {
        Ok(())
    }
}
fn text(bytes: &[u8]) -> Result<NativeText, ProtocolError> {
    if bytes.len() > MAX_NATIVE_TEXT_BYTES {
        return Err(ProtocolError::LimitExceeded);
    }
    NativeText::new(
        std::str::from_utf8(bytes)
            .map_err(|_| ProtocolError::Malformed)?
            .to_owned(),
    )
    .map_err(|_| ProtocolError::LimitExceeded)
}
fn add_tick(tick: MonotonicTick, milliseconds: u32) -> Result<MonotonicTick, ProtocolError> {
    tick.get()
        .checked_add(u64::from(milliseconds))
        .map(MonotonicTick::new)
        .ok_or(ProtocolError::Malformed)
}

macro_rules! profile {
    ($p:ident) => {
        pub mod $p {
            use super::*;
            use crate::wire::$p as wire;
            pub fn encode_request(
                request: &NativeRequest,
                now: MonotonicTick,
            ) -> Result<wire::NativeRequest, ProtocolError> {
                check_fence(request.fence())?;
                if now < request.issued_at() {
                    return Err(ProtocolError::Malformed);
                }
                let remaining = request
                    .deadline()
                    .map(|deadline| {
                        deadline
                            .checked_elapsed_since(now)
                            .filter(|remaining| *remaining != 0)
                            .and_then(|remaining| u32::try_from(remaining).ok())
                            .ok_or(ProtocolError::Malformed)
                    })
                    .transpose()?;
                // A Permission notice's response delay starts when the client
                // presents it. Queued delivery cannot shorten that delay.
                let delay_origin = if matches!(request.kind(), NativeRequestKind::Permission(_)) {
                    request.issued_at()
                } else {
                    now
                };
                let delay = u32::try_from(
                    request
                        .not_before()
                        .get()
                        .saturating_sub(delay_origin.get()),
                )
                .map_err(|_| ProtocolError::Malformed)?;
                if remaining.is_some_and(|remaining| remaining <= delay) {
                    return Err(ProtocolError::Malformed);
                }
                let action = match request.kind() {
                    NativeRequestKind::ClipboardRead => {
                        wire::native_request::Action::Clipboard(wire::ClipboardRequest {
                            read: true,
                            text: vec![],
                            deadline_ms: remaining.ok_or(ProtocolError::Malformed)?,
                        })
                    }
                    NativeRequestKind::ClipboardWrite(text) => {
                        wire::native_request::Action::Clipboard(wire::ClipboardRequest {
                            read: false,
                            text: text.as_str().as_bytes().to_vec(),
                            deadline_ms: 0,
                        })
                    }
                    NativeRequestKind::UnsafePaste { approval, text } => {
                        wire::native_request::Action::UnsafePaste(wire::UnsafePasteRequest {
                            text: text.as_str().as_bytes().to_vec(),
                            expires_after_ms: remaining.ok_or(ProtocolError::Malformed)?,
                            approval: approval.get(),
                        })
                    }
                    NativeRequestKind::Notification(notice) => {
                        let (kind, exit_status, duration_millis) = match notice {
                            NotificationNotice::Bell => (1, None, 0),
                            NotificationNotice::CommandFinished {
                                exit_status,
                                duration_millis,
                            } => (2, *exit_status, *duration_millis),
                        };
                        wire::native_request::Action::Notification(wire::NotificationRequest {
                            kind,
                            exit_status,
                            duration_millis,
                        })
                    }
                    NativeRequestKind::Permission(notice) => {
                        wire::native_request::Action::Permission(wire::PermissionRequest {
                            kind: 0,
                            screen_recording: notice.screen_recording(),
                            accessibility: notice.accessibility(),
                        })
                    }
                };
                Ok(wire::NativeRequest {
                    fence: Some(encode_fence(request.fence())),
                    request: request.id().get(),
                    action: Some(action),
                    not_before_delay_ms: delay,
                    remaining_ms: remaining,
                    originating_runtime_request: request
                        .runtime_request()
                        .map_or(0, |request| request.get()),
                })
            }
            pub fn decode_request(
                value: &wire::NativeRequest,
                received_at: MonotonicTick,
            ) -> Result<NativeRequest, ProtocolError> {
                let fence = decode_fence(required(&value.fence)?, None)?;
                check_fence(fence)?;
                let kind = match required(&value.action)? {
                    wire::native_request::Action::Clipboard(clipboard) => {
                        if clipboard.read {
                            if !clipboard.text.is_empty()
                                || Some(clipboard.deadline_ms) != value.remaining_ms
                            {
                                return Err(ProtocolError::Malformed);
                            }
                            NativeRequestKind::ClipboardRead
                        } else {
                            if clipboard.deadline_ms != 0 {
                                return Err(ProtocolError::Malformed);
                            }
                            NativeRequestKind::ClipboardWrite(text(&clipboard.text)?)
                        }
                    }
                    wire::native_request::Action::UnsafePaste(paste) => {
                        if Some(paste.expires_after_ms) != value.remaining_ms {
                            return Err(ProtocolError::Malformed);
                        }
                        NativeRequestKind::UnsafePaste {
                            approval: PasteApprovalId::new(paste.approval)
                                .map_err(|_| ProtocolError::Malformed)?,
                            text: text(&paste.text)?,
                        }
                    }
                    wire::native_request::Action::Notification(notice) => {
                        NativeRequestKind::Notification(match notice.kind {
                            1 if notice.exit_status.is_none() && notice.duration_millis == 0 => {
                                NotificationNotice::Bell
                            }
                            2 => NotificationNotice::CommandFinished {
                                exit_status: notice.exit_status,
                                duration_millis: notice.duration_millis,
                            },
                            _ => return Err(ProtocolError::Malformed),
                        })
                    }
                    wire::native_request::Action::Permission(permission) => {
                        if permission.kind != 0 {
                            return Err(ProtocolError::Malformed);
                        }
                        NativeRequestKind::Permission(
                            PermissionNotice::new(
                                permission.screen_recording,
                                permission.accessibility,
                            )
                            .map_err(|_| ProtocolError::Malformed)?,
                        )
                    }
                };
                let not_before = add_tick(received_at, value.not_before_delay_ms)?;
                let deadline = value
                    .remaining_ms
                    .map(|remaining| add_tick(received_at, remaining))
                    .transpose()?;
                let request = NativeRequest::new(
                    fence,
                    NativeRequestId::new(value.request).map_err(|_| ProtocolError::Malformed)?,
                    received_at,
                    not_before,
                    deadline,
                    kind,
                )
                .map_err(|_| ProtocolError::Malformed)?;
                if value.originating_runtime_request == 0 {
                    Ok(request)
                } else {
                    request
                        .with_runtime_request(value.originating_runtime_request)
                        .map_err(|_| ProtocolError::Malformed)
                }
            }
            pub fn encode_completion(
                completion: &NativeCompletion,
            ) -> Result<wire::NativeCompletion, ProtocolError> {
                check_fence(completion.fence())?;
                let mut value = wire::NativeCompletion {
                    fence: Some(encode_fence(completion.fence())),
                    request: completion.id().get(),
                    originating_runtime_request: completion
                        .runtime_request()
                        .map_or(0, |request| request.get()),
                    ..Default::default()
                };
                value.disposition = match completion.kind() {
                    NativeCompletionKind::Denied => 1,
                    NativeCompletionKind::Expired => 2,
                    NativeCompletionKind::Cancelled => 3,
                    NativeCompletionKind::ClipboardText(text) => {
                        value.clipboard_text = text.as_str().as_bytes().to_vec();
                        4
                    }
                    NativeCompletionKind::ClipboardWritten => 5,
                    NativeCompletionKind::Paste { approval, decision } => {
                        value.paste_approval = approval.get();
                        value.paste_decision = match decision {
                            PasteDecision::Confirm => 1,
                            PasteDecision::Cancel => 2,
                        };
                        6
                    }
                    NativeCompletionKind::PermissionAcknowledged => 7,
                    NativeCompletionKind::NotificationAcknowledged => 8,
                };
                value.allowed = matches!(value.disposition, 4 | 5 | 7 | 8)
                    || value.disposition == 6 && value.paste_decision == 1;
                Ok(value)
            }
            /// Decode against the retained request, never a reconstructed request
            /// from the completion's untrusted identity or native text.
            pub fn decode_completion(
                value: &wire::NativeCompletion,
                request: &NativeRequest,
            ) -> Result<NativeCompletion, ProtocolError> {
                if value.request != request.id().get()
                    || value.originating_runtime_request
                        != request.runtime_request().map_or(0, |request| request.get())
                    || decode_fence(required(&value.fence)?, None)? != request.fence()
                    || value.failure.is_some()
                    || value.allowed
                        != (matches!(value.disposition, 4 | 5 | 7 | 8)
                            || value.disposition == 6 && value.paste_decision == 1)
                    || (value.disposition != 4 && !value.clipboard_text.is_empty())
                    || (value.disposition != 6
                        && (value.paste_approval != 0 || value.paste_decision != 0))
                {
                    return Err(ProtocolError::Malformed);
                }
                let kind = match value.disposition {
                    1 => NativeCompletionKind::Denied,
                    2 => NativeCompletionKind::Expired,
                    3 => NativeCompletionKind::Cancelled,
                    4 => NativeCompletionKind::ClipboardText(text(&value.clipboard_text)?),
                    5 => NativeCompletionKind::ClipboardWritten,
                    6 => NativeCompletionKind::Paste {
                        approval: PasteApprovalId::new(value.paste_approval)
                            .map_err(|_| ProtocolError::Malformed)?,
                        decision: match value.paste_decision {
                            1 => PasteDecision::Confirm,
                            2 => PasteDecision::Cancel,
                            _ => return Err(ProtocolError::Malformed),
                        },
                    },
                    7 => NativeCompletionKind::PermissionAcknowledged,
                    8 => NativeCompletionKind::NotificationAcknowledged,
                    _ => return Err(ProtocolError::Malformed),
                };
                NativeCompletion::new(request, kind).map_err(|_| ProtocolError::Malformed)
            }
        }
    };
}
profile!(v1);
profile!(v2);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{codec::Payload, wire};
    use spaceterm_core::{authority::*, identity::*};

    fn request(kind: NativeRequestKind) -> NativeRequest {
        let server = ServerId::new([1; 16]);
        let client = ClientId::new(server, 1).unwrap();
        let pane = PaneId::new(server, 1).unwrap();
        let fence = EffectFence::new(
            SessionKey {
                epoch: ServerEpoch::new(1),
                pane,
                generation: SessionGeneration::new(1),
            },
            ControlGrant::new(pane, client, ControlGeneration::new(2)).unwrap(),
            ConnectionBinding::new(client, ConnectionGeneration::new(3)),
            ContextGeneration::new(4),
            None,
        )
        .unwrap();
        let delay = if matches!(kind, NativeRequestKind::Permission(_)) {
            500
        } else {
            0
        };
        NativeRequest::new(
            fence,
            NativeRequestId::new(5).unwrap(),
            MonotonicTick::new(100),
            MonotonicTick::new(100 + delay),
            Some(MonotonicTick::new(1100)),
            kind,
        )
        .unwrap()
    }
    macro_rules! cases {
        ($test:ident, $p:ident, $variant:ident) => {
            #[test]
            fn $test() {
                for kind in [
                    NativeRequestKind::ClipboardRead,
                    NativeRequestKind::ClipboardWrite(NativeText::new("clipboard".into()).unwrap()),
                    NativeRequestKind::UnsafePaste {
                        approval: PasteApprovalId::new(8).unwrap(),
                        text: NativeText::new("paste\n".into()).unwrap(),
                    },
                    NativeRequestKind::Permission(PermissionNotice::new(true, true).unwrap()),
                    NativeRequestKind::Notification(NotificationNotice::Bell),
                    NativeRequestKind::Notification(NotificationNotice::CommandFinished {
                        exit_status: Some(1),
                        duration_millis: 5000,
                    }),
                ] {
                    let original = request(kind);
                    let encoded = $p::encode_request(&original, MonotonicTick::new(350)).unwrap();
                    assert_eq!(encoded.remaining_ms, Some(750));
                    if matches!(original.kind(), NativeRequestKind::Permission(_)) {
                        assert_eq!(encoded.not_before_delay_ms, 500);
                    }
                    let bytes = Payload::$variant(wire::$p::Message {
                        kind: Some(wire::$p::message::Kind::NativeRequest(encoded.clone())),
                    })
                    .encode()
                    .unwrap();
                    let Payload::$variant(message) =
                        Payload::decode(if stringify!($p) == "v1" { 1 } else { 2 }, &bytes)
                            .unwrap()
                    else {
                        panic!("major")
                    };
                    let Some(wire::$p::message::Kind::NativeRequest(decoded)) = message.kind else {
                        panic!("native request")
                    };
                    let local = $p::decode_request(&decoded, MonotonicTick::new(2000)).unwrap();
                    assert_eq!(local.fence(), original.fence());
                    assert_eq!(local.id(), original.id());
                    assert_eq!(local.kind(), original.kind());
                    assert_eq!(local.deadline(), Some(MonotonicTick::new(2750)));
                    let accepted = match original.kind() {
                        NativeRequestKind::ClipboardRead => NativeCompletionKind::ClipboardText(
                            NativeText::new(String::new()).unwrap(),
                        ),
                        NativeRequestKind::ClipboardWrite(_) => {
                            NativeCompletionKind::ClipboardWritten
                        }
                        NativeRequestKind::UnsafePaste { approval, .. } => {
                            NativeCompletionKind::Paste {
                                approval: *approval,
                                decision: PasteDecision::Confirm,
                            }
                        }
                        NativeRequestKind::Notification(_) => {
                            NativeCompletionKind::NotificationAcknowledged
                        }
                        NativeRequestKind::Permission(_) => {
                            NativeCompletionKind::PermissionAcknowledged
                        }
                    };
                    let mut completions = vec![
                        NativeCompletionKind::Denied,
                        NativeCompletionKind::Expired,
                        NativeCompletionKind::Cancelled,
                        accepted,
                    ];
                    if let NativeRequestKind::UnsafePaste { approval, .. } = original.kind() {
                        completions.push(NativeCompletionKind::Paste {
                            approval: *approval,
                            decision: PasteDecision::Cancel,
                        });
                    }
                    for kind in completions {
                        let completion = NativeCompletion::new(&original, kind).unwrap();
                        let encoded = $p::encode_completion(&completion).unwrap();
                        if matches!(
                            completion.kind(),
                            NativeCompletionKind::Paste {
                                decision: PasteDecision::Cancel,
                                ..
                            }
                        ) {
                            assert!(
                                !encoded.allowed,
                                "paste cancellation must not authorize insertion"
                            );
                        }
                        assert_eq!(
                            $p::decode_completion(&encoded, &original).unwrap(),
                            completion
                        );
                        let mut forged = encoded;
                        forged.request += 1;
                        assert_eq!(
                            $p::decode_completion(&forged, &original),
                            Err(ProtocolError::Malformed)
                        );
                    }
                    let mut forged = encoded;
                    forged.remaining_ms = Some(0);
                    assert_eq!(
                        $p::decode_request(&forged, MonotonicTick::new(2000)),
                        Err(ProtocolError::Malformed)
                    );
                    assert_eq!(
                        $p::encode_request(&original, MonotonicTick::new(1100)),
                        Err(ProtocolError::Malformed)
                    );
                }
            }
        };
    }
    cases!(native_deadlines_and_exact_completions_major_1, v1, Major1);
    cases!(native_deadlines_and_exact_completions_major_2, v2, Major2);
}
