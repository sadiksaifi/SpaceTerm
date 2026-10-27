//! Channel bindings of `Served` over a host whose Sessions the test ends.
use super::dispatch::{HostCommands, Served};
use super::publish::{Observation, Observed};
use super::*;
use spaceterm_server::attachments::Attachment;
use spaceterm_server::connection::outbox::Outbox;
use spaceterm_server::connection::service::{Dispatch, PaneService as _, Peer};
use spaceterm_server::protocol::{self, Channel, ClaimReply, Operation, ReplyBody, RequestId};
use std::collections::BTreeSet;
use std::sync::Mutex;

const PEER: Peer = Peer {
    client: [1; 32],
    connection: [1; 32],
    pairing: None,
};

#[derive(Default)]
struct Sessions {
    started: u32,
    live: BTreeSet<[u8; 32]>,
    unbound: Vec<Lease>,
    /// Input serials the host was told reached the PTY.
    written: Vec<u32>,
}

/// Attaches every claim to a new Session. The test ends Sessions by removing them.
struct Host(Arc<Mutex<Sessions>>);

impl HostCommands for Host {
    fn request(&mut self, _: [u8; 32], connection: [u8; 32], request: Request) -> Reply {
        if let Request::Input {
            input: InteractiveInput::Paste { text },
            ..
        } = &request
        {
            return Reply::Paste(if text.contains("confirm") {
                PasteOutcome::ConfirmationRequired(pane::PasteConfirmation {
                    id: 1,
                    byte_len: text.len() as u32,
                    line_count: 1,
                    multiline: false,
                    control_bytes: false,
                    closing_fence: false,
                })
            } else {
                PasteOutcome::Written
            });
        }
        let Request::ClaimPane { .. } = request else {
            return Reply::Unchanged;
        };
        let mut sessions = self.0.lock().unwrap();
        sessions.started += 1;
        let mut session = [0; 32];
        session[..4].copy_from_slice(&sessions.started.to_be_bytes());
        sessions.live.insert(session);
        Reply::Attached {
            lease: Lease {
                server: [2; 32],
                session,
                control: [3; 32],
                connection,
            },
        }
    }
    fn lease_live(&self, lease: &Lease) -> bool {
        self.0.lock().unwrap().live.contains(&lease.session)
    }
    fn unbound(&mut self, _: [u8; 32], lease: &Lease) {
        self.0.lock().unwrap().unbound.push(*lease);
    }
    fn observe(&mut self, _: [u8; 32], lease: &Lease) -> Observed {
        if self.lease_live(lease) {
            Observed::Live(Observation {
                screen: None,
                effects: Vec::new(),
                hidden_input: false,
                accessibility: None,
            })
        } else {
            Observed::Gone {
                last: None,
                event: protocol::PaneEvent::Ended,
            }
        }
    }
    fn view(&mut self, _: &Lease, _: bool, _: u8) {}
    fn input_written(&mut self, _: &Lease, serial: u32) {
        self.0.lock().unwrap().written.push(serial);
    }
    fn disconnected(&mut self, _: [u8; 32]) {}
    fn shutdown(&mut self) -> Result<(), ServerError> {
        Ok(())
    }
}

fn served() -> (Served<Host>, Arc<Mutex<Sessions>>, Outbox) {
    let sessions = Arc::new(Mutex::new(Sessions::default()));
    let mut served = Served::new(Host(Arc::clone(&sessions)));
    let outbox = Outbox::new(Arc::new(|| {}));
    served.admitted(&PEER, "Test".into(), outbox.clone());
    (served, sessions, outbox)
}

fn claim(served: &mut Served<Host>, channel: u32, pane: u8) -> Lease {
    let operation = Operation::Claim(protocol::Claim {
        channel: Channel(channel),
        attachment: Attachment {
            workspace: [4; 32],
            generation: 1,
        },
        pane: [pane; 32],
        claim: AttachmentClaim::Select { explicit: true },
        size: super::tests::size(),
        style: None,
    });
    match served.request(&PEER, RequestId(channel), operation) {
        Dispatch::Reply(reply) => match *reply {
            Ok(ReplyBody::Claim(ClaimReply::Attached { lease })) => lease,
            other => panic!("claim {channel} was not attached: {other:?}"),
        },
        _ => panic!("a claim replies at once"),
    }
}

/// More Sessions than a connection has channels start and end on one connection. Each ended
/// Session frees its channel, so no claim is refused.
#[test]
fn ended_sessions_free_their_channels_on_one_connection() {
    let (mut served, sessions, outbox) = served();
    let channels = 3 * protocol::limits::MAX_CHANNELS as u32;
    for channel in 1..=channels {
        claim(&mut served, channel, (channel % 7) as u8 + 1);
        sessions.lock().unwrap().live.clear();
        served.tick(std::time::Instant::now());
    }
    assert_eq!(outbox.take_closed().len(), channels as usize);
}

/// Claiming a Pane this connection holds again resets the host's input state for the old lease
/// and closes the old channel.
#[test]
fn a_reclaim_on_the_same_connection_resets_the_previous_binding() {
    let (mut served, sessions, outbox) = served();
    let first = claim(&mut served, 1, 9);
    claim(&mut served, 2, 9);
    assert_eq!(sessions.lock().unwrap().unbound, [first]);
    assert_eq!(outbox.take_closed(), [Channel(1)]);
    claim(&mut served, 3, 8);
    assert_eq!(
        sessions.lock().unwrap().unbound.len(),
        1,
        "another Pane is unaffected"
    );
}

/// Only a message that wrote to the Terminal Session marks its serial: not a paste chunk that
/// waits for more, nor a paste that waits for confirmation.
#[test]
fn an_input_serial_is_written_only_once_its_bytes_reach_the_session() {
    let (mut served, sessions, _outbox) = served();
    claim(&mut served, 1, 9);
    let paste = |text: &str, last| protocol::InputEvent::Paste {
        text: text.into(),
        last,
    };
    let mut send = |serial, events| {
        served.input(
            &PEER,
            protocol::Input {
                channel: Channel(1),
                serial,
                generation: 1,
                events,
            },
        );
    };

    send(1, vec![paste("first ", false)]);
    send(2, vec![paste("half", true)]);
    send(3, vec![paste("needs confirm", true)]);
    send(4, vec![protocol::InputEvent::Resize(super::tests::size())]);

    assert_eq!(sessions.lock().unwrap().written, [2, 4]);
}
