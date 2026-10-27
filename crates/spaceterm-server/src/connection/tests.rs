//! The connection contract end to end: a `ServerLink` and a `AdmittedConnection` over a loopback
//! carrier, pumped on one thread in place of the I/O threads.
use super::carrier::{Carrier, Offer};
use super::link::{ChannelEvent, Health, LinkError, LinkEvent, RequestError, ServerLink};
use super::loopback::Loopback;
use super::outbox::{FrameTicket, Outbox, Presentation, Presented};
use super::server::AdmittedConnection;
use super::service::{Dispatch, PaneService, Peer};
use crate::ServerError;
use crate::pane::control::ClientName;
use crate::pane::{AttachmentClaim, Lease, PaneSize};
use crate::protocol::interim::Interim;
use crate::protocol::{
    ClaimReply, EffectDone, Fatal, Input, InputEvent, InventoryRevision, Operation, PaneEvent,
    Refusal, ReplyBody, RequestId, ScreenUpdate, View,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Recorded {
    outbox: Option<Outbox>,
    deferred: Vec<RequestId>,
    inputs: Vec<u32>,
    views: Vec<View>,
}

struct Toy(Arc<Mutex<Recorded>>);

struct Screen(u32);
impl Presentation for Screen {
    fn encode(self: Box<Self>, _ticket: FrameTicket) -> Option<Presented> {
        Some(Presented {
            input: self.0,
            update: ScreenUpdate::default(),
        })
    }
}

impl PaneService for Toy {
    fn admitted(&mut self, _peer: &Peer, _name: ClientName, outbox: Outbox) {
        self.0.lock().unwrap().outbox = Some(outbox);
    }
    fn request(&mut self, peer: &Peer, id: RequestId, operation: Operation) -> Dispatch {
        let mut recorded = self.0.lock().unwrap();
        match operation {
            Operation::Claim(claim) => {
                let outbox = recorded.outbox.as_ref().unwrap();
                outbox.open(claim.channel).unwrap();
                outbox.present(claim.channel, Box::new(Screen(0))).unwrap();
                Dispatch::reply(Ok(ReplyBody::Claim(ClaimReply::Attached {
                    lease: Lease {
                        server: [1; 32],
                        session: [2; 32],
                        control: [3; 32],
                        connection: peer.connection,
                    },
                })))
            }
            Operation::Resolve { .. } => {
                recorded.deferred.push(id);
                Dispatch::Deferred
            }
            Operation::Interim(Interim::Inventory) => {
                Dispatch::After(Box::new(|| Err(Refusal::StorageUnavailable)))
            }
            _ => Dispatch::reply(Ok(ReplyBody::Done)),
        }
    }
    fn input(&mut self, _peer: &Peer, input: Input) {
        let mut recorded = self.0.lock().unwrap();
        recorded.inputs.push(input.serial);
        let outbox = recorded.outbox.as_ref().unwrap();
        outbox
            .present(input.channel, Box::new(Screen(input.serial)))
            .unwrap();
    }
    fn view(&mut self, _peer: &Peer, view: View) {
        self.0.lock().unwrap().views.push(view);
    }
    fn effect_done(&mut self, _peer: &Peer, _done: EffectDone) {}
    fn acknowledge(&mut self, _peer: &Peer, _sequence: u64) {}
    fn disconnected(&mut self, _peer: &Peer) {}
    fn shutdown(&mut self) -> Result<(), ServerError> {
        Ok(())
    }
}

struct Harness {
    link: ServerLink,
    client: Loopback,
    server: Loopback,
    connection: AdmittedConnection,
    service: Mutex<Box<dyn PaneService>>,
    recorded: Arc<Mutex<Recorded>>,
    inventory: Arc<Mutex<Vec<LinkEvent>>>,
}

impl Harness {
    fn new(paired: bool) -> Self {
        let now = Instant::now();
        let (client, server) = Loopback::pair(4).unwrap();
        let inventory = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::clone(&inventory);
        let link = ServerLink::new(
            Arc::new(move |event| events.lock().unwrap().push(event)),
            Arc::new(|| {}),
            now,
        );
        let peer = Peer {
            client: [9; 32],
            connection: [8; 32],
            paired,
        };
        let outbox = Outbox::new(Arc::new(|| {}));
        let recorded = Arc::new(Mutex::new(Recorded::default()));
        let mut service: Box<dyn PaneService> = Box::new(Toy(Arc::clone(&recorded)));
        service.admitted(&peer, "Build Mac".into(), outbox.clone());
        Self {
            link,
            client,
            server,
            connection: AdmittedConnection::new(peer, outbox, now),
            service: Mutex::new(service),
            recorded,
            inventory,
        }
    }

    /// Moves frames both ways until neither side makes progress, as the two I/O threads would.
    fn pump(&mut self) -> Result<(), Fatal> {
        let now = Instant::now();
        let mut held_client = None;
        let mut held_server = None;
        loop {
            let mut progress = false;
            if held_client.is_none() {
                held_client = self.link.next_frame();
            }
            if let Some(frame) = held_client.take() {
                match self.client.offer(&frame).unwrap() {
                    Offer::Accepted => progress = true,
                    Offer::Full => held_client = Some(frame),
                }
            }
            while let Some(frame) = self.server.receive().unwrap() {
                progress = true;
                if let Some(inbound) = self.connection.receive(frame, now)? {
                    let deferred = {
                        let mut service = self.service.lock().unwrap();
                        self.connection.dispatch(inbound, service.as_mut())?
                    };
                    if let Some(deferred) = deferred {
                        deferred.run(&self.connection)?;
                    }
                }
            }
            if held_server.is_none() {
                held_server = self.connection.outbox().next_frame(now).unwrap();
            }
            if let Some(frame) = held_server.take() {
                match self.server.offer(&frame).unwrap() {
                    Offer::Accepted => progress = true,
                    Offer::Full => held_server = Some(frame),
                }
            }
            while let Some(frame) = self.client.receive().unwrap() {
                progress = true;
                let _ = self.link.receive(frame, now);
            }
            if !progress {
                return Ok(());
            }
        }
    }
}

fn size() -> PaneSize {
    PaneSize {
        columns: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
    }
}

#[test]
fn requests_are_concurrent_and_deferred_replies_arrive_later() {
    let mut harness = Harness::new(false);
    let resolve = harness
        .link
        .request(Operation::Resolve { sequence: 1 })
        .unwrap();
    let release = harness
        .link
        .request(Operation::ReleaseAttachment {
            attachment: crate::attachments::Attachment {
                workspace: [1; 32],
                generation: 1,
            },
        })
        .unwrap();
    let after = harness
        .link
        .request(Operation::Interim(Interim::Inventory))
        .unwrap();
    harness.pump().unwrap();
    assert!(
        resolve.try_take().is_none(),
        "the deferred reply is still owed"
    );
    assert!(matches!(release.wait(), Ok(ReplyBody::Done)));
    assert_eq!(
        after.wait().unwrap_err(),
        RequestError::Refused(Refusal::StorageUnavailable)
    );

    let id = harness.recorded.lock().unwrap().deferred[0];
    let outbox = harness.connection.outbox().clone();
    outbox
        .inventory(InventoryRevision {
            inventory: crate::hierarchy::State::empty().inventory,
            next_sequence: 2,
        })
        .unwrap();
    outbox.reply(id, Ok(ReplyBody::Done)).unwrap();
    harness.pump().unwrap();
    assert!(matches!(
        harness.inventory.lock().unwrap().as_slice(),
        [LinkEvent::Inventory(_)]
    ));
    assert!(matches!(resolve.wait(), Ok(ReplyBody::Done)));
}

#[test]
fn claimed_channels_receive_frames_under_one_frame_of_credit() {
    let mut harness = Harness::new(false);
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let channel = harness
        .link
        .open_channel(Arc::new(move |event| sink.lock().unwrap().push(event)))
        .unwrap();
    let claim = channel
        .claim(
            crate::attachments::Attachment {
                workspace: [1; 32],
                generation: 1,
            },
            [2; 32],
            AttachmentClaim::Select { explicit: true },
            size(),
            None,
        )
        .unwrap();
    channel.view(true, 120).unwrap();
    harness.pump().unwrap();
    assert!(matches!(
        claim.wait(),
        Ok(ReplyBody::Claim(ClaimReply::Attached { .. }))
    ));
    assert_eq!(harness.recorded.lock().unwrap().views.len(), 1);

    let frames = |events: &Arc<Mutex<Vec<ChannelEvent>>>| {
        std::mem::take(&mut *events.lock().unwrap())
            .into_iter()
            .map(|event| match event {
                ChannelEvent::Frame(frame) => (frame.frame, frame.base, frame.input),
                ChannelEvent::Pane(_) => panic!("unexpected Pane event"),
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(frames(&events), [(1, None, 0)]);

    let key = |modifiers| vec![InputEvent::CopyOrForward { modifiers }];
    assert_eq!(channel.input(1, key(1)).unwrap(), 1);
    assert_eq!(channel.input(1, key(2)).unwrap(), 2);
    harness.pump().unwrap();
    assert_eq!(harness.recorded.lock().unwrap().inputs, [1, 2]);
    assert!(
        frames(&events).is_empty(),
        "no credit until frame 1 is acknowledged"
    );

    channel.acknowledge(1).unwrap();
    harness.pump().unwrap();
    assert_eq!(
        frames(&events),
        [(2, Some(1), 2)],
        "only the latest state is sent"
    );

    harness
        .connection
        .outbox()
        .pane(channel.channel(), PaneEvent::Ended)
        .unwrap();
    harness.pump().unwrap();
    assert!(matches!(
        events.lock().unwrap().as_slice(),
        [ChannelEvent::Pane(PaneEvent::Ended)]
    ));
}

#[test]
fn protocol_order_violations_end_the_connection() {
    let mut harness = Harness::new(false);
    let channel = harness.link.open_channel(Arc::new(|_| {})).unwrap();
    channel.input(1, vec![InputEvent::Resize(size())]).unwrap();
    assert_eq!(
        harness.pump(),
        Err(Fatal::ProtocolViolation),
        "input on a channel that was never claimed"
    );

    let mut harness = Harness::new(false);
    let unacknowledged = harness.link.open_channel(Arc::new(|_| {})).unwrap();
    unacknowledged.acknowledge(1).unwrap();
    assert_eq!(harness.pump(), Err(Fatal::ProtocolViolation));
}

#[test]
fn paired_peers_cannot_administer_the_account() {
    let mut harness = Harness::new(true);
    let shutdown = harness.link.request(Operation::Shutdown).unwrap();
    harness.pump().unwrap();
    assert_eq!(
        shutdown.wait().unwrap_err(),
        RequestError::Refused(Refusal::Unsupported)
    );
}

#[test]
fn health_comes_from_traffic_and_failure_settles_every_pending_request() {
    let mut harness = Harness::new(false);
    let start = Instant::now();
    let next = harness.link.check(start + Duration::from_secs(5)).unwrap();
    assert!(next > start + Duration::from_secs(5));
    harness.pump().unwrap();
    assert_eq!(harness.link.health(), Health::Live);

    let pending = harness
        .link
        .request(Operation::Resolve { sequence: 1 })
        .unwrap();
    harness.pump().unwrap();
    harness
        .connection
        .outbox()
        .goodbye(Fatal::Stopping)
        .unwrap();
    harness.pump().unwrap();
    let closed = LinkError::Goodbye(Fatal::Stopping);
    assert_eq!(harness.link.health(), Health::Closed(closed));
    assert_eq!(pending.wait().unwrap_err(), RequestError::Link(closed));
    assert!(matches!(
        harness.inventory.lock().unwrap().as_slice(),
        [LinkEvent::Health(Health::Closed(_))]
    ));
    assert_eq!(
        harness.link.request(Operation::Shutdown).err(),
        Some(closed)
    );
    assert_eq!(harness.link.check(start + Duration::from_secs(60)), None);
}
