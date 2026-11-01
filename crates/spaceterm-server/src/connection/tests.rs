//! The connection contract end to end: a `ServerLink` and an `AdmittedConnection` over a loopback
//! carrier, pumped on one thread in place of the two I/O threads.
use super::carrier::{Carrier, Offer};
use super::link::{ChannelEvent, Health, LinkError, LinkEvent, RequestError, ServerLink};
use super::loopback::Loopback;
use super::outbox::{FrameTicket, Outbox, Presentation, Presented};
use super::registry::{PairedConnections, Sever};
use super::server::AdmittedConnection;
use super::service::{DirectManagement, Dispatch, Pairing, PaneService, Peer};
use super::sync::SyncWorker;
use crate::ServerError;
use crate::direct::{DirectError, Management, ManagementReply};
use crate::pane::control::ClientName;
use crate::pane::{AttachmentClaim, Lease, PaneSize};
use crate::protocol::{
    Channel, ClaimReply, EffectDone, Fatal, Input, InputEvent, InventoryRevision, Operation,
    PaneEvent, Refusal, ReplyBody, RequestId, ScreenUpdate, View,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Recorded {
    outbox: Option<Outbox>,
    deferred: Vec<RequestId>,
    inputs: Vec<(u32, usize)>,
    views: Vec<View>,
    released: Vec<Channel>,
    sync_threads: Vec<std::thread::ThreadId>,
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
        let outbox = recorded.outbox.clone().unwrap();
        match operation {
            Operation::Claim(claim) => {
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
            Operation::Release { channel } => {
                outbox.close(channel);
                recorded.released.push(channel);
                Dispatch::reply(Ok(ReplyBody::Done))
            }
            Operation::Mutate { .. } => {
                recorded.deferred.push(id);
                Dispatch::Deferred
            }
            Operation::WorktreeRead(_) => {
                let recorded = Arc::clone(&self.0);
                Dispatch::After(Box::new(move || {
                    let thread = std::thread::current().id();
                    recorded.lock().unwrap().sync_threads.push(thread);
                    Err(Refusal::StorageUnavailable)
                }))
            }
            _ => Dispatch::reply(Ok(ReplyBody::Done)),
        }
    }
    fn input(&mut self, _peer: &Peer, input: Input) {
        let mut recorded = self.0.lock().unwrap();
        recorded.inputs.push((input.serial, input.events.len()));
        let outbox = recorded.outbox.clone().unwrap();
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

struct Direct(Mutex<Vec<bool>>);
impl DirectManagement for Direct {
    fn manage(&self, peer: &Peer, _request: Management) -> Result<ManagementReply, DirectError> {
        self.0.lock().unwrap().push(peer.paired());
        Err(DirectError::Disabled)
    }
}

struct Harness {
    link: ServerLink,
    client: Loopback,
    server: Loopback,
    connection: AdmittedConnection,
    service: Mutex<Box<dyn PaneService>>,
    sync: SyncWorker,
    recorded: Arc<Mutex<Recorded>>,
    events: Arc<Mutex<Vec<LinkEvent>>>,
    direct: Arc<Direct>,
}

impl Harness {
    fn new(pairing: Option<Pairing>) -> Self {
        let now = Instant::now();
        let (client, server) = Loopback::pair(4).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let link = ServerLink::new(
            Arc::new(move |event| sink.lock().unwrap().push(event)),
            Arc::new(|| {}),
            now,
        );
        let peer = Peer {
            client: [9; 32],
            connection: [8; 32],
            pairing,
        };
        let outbox = Outbox::new(Arc::new(|| {}));
        let recorded = Arc::new(Mutex::new(Recorded::default()));
        let mut service: Box<dyn PaneService> = Box::new(Toy(Arc::clone(&recorded)));
        service.admitted(&peer, "Build Mac".into(), outbox.clone());
        let (sync, _) = SyncWorker::spawn().unwrap();
        let direct = Arc::new(Direct(Mutex::new(Vec::new())));
        let handler: Arc<dyn DirectManagement> = direct.clone();
        Self {
            link,
            client,
            server,
            connection: AdmittedConnection::new(peer, outbox, sync.clone(), Some(handler), now),
            service: Mutex::new(service),
            sync,
            recorded,
            events,
            direct,
        }
    }

    /// Moves frames both ways until neither side makes progress and the sync worker is idle, as
    /// the two I/O threads would.
    fn pump(&mut self) -> Result<(), Fatal> {
        loop {
            self.pump_once()?;
            self.sync.flush().unwrap();
            if !self.pump_once()? {
                return Ok(());
            }
        }
    }

    fn pump_once(&mut self) -> Result<bool, Fatal> {
        let now = Instant::now();
        let mut any = false;
        loop {
            let mut progress = false;
            if let Some((frame, priority)) = self.link.next_frame() {
                assert_eq!(
                    self.client.offer(&frame, priority).unwrap(),
                    Offer::Accepted
                );
                progress = true;
            }
            while let Some(frame) = self.server.receive().unwrap() {
                progress = true;
                if let Some(inbound) = self.connection.receive(frame, now)? {
                    let mut service = self.service.lock().unwrap();
                    self.connection.dispatch(inbound, service.as_mut())?;
                }
            }
            if let Some((frame, priority)) = self.connection.outbox().next_frame() {
                assert_eq!(
                    self.server.offer(&frame, priority).unwrap(),
                    Offer::Accepted
                );
                self.connection.wrote(now, true, false);
                progress = true;
            }
            while let Some(frame) = self.client.receive().unwrap() {
                progress = true;
                let _ = self.link.receive(frame, now);
            }
            if !progress {
                return Ok(any);
            }
            any = true;
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

fn attachment() -> crate::attachments::Attachment {
    crate::attachments::Attachment {
        workspace: [1; 32],
        generation: 1,
    }
}

type Events = Arc<Mutex<Vec<ChannelEvent>>>;

fn claimed(harness: &mut Harness) -> (super::link::ChannelHandle, Events) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let channel = harness
        .link
        .open_channel(Arc::new(move |event| sink.lock().unwrap().push(event)))
        .unwrap();
    let claim = channel
        .claim(
            attachment(),
            [2; 32],
            AttachmentClaim::Select { explicit: true },
            size(),
            None,
        )
        .unwrap();
    harness.pump().unwrap();
    assert!(matches!(
        claim.wait(),
        Ok(ReplyBody::Claim(ClaimReply::Attached { .. }))
    ));
    (channel, events)
}

/// An edit the toy service defers.
fn edit() -> Operation {
    Operation::Mutate {
        sequence: 1,
        mutation: crate::hierarchy::Mutation::CreateTab {
            workspace: [1; 32],
            source: None,
            control: crate::hierarchy::CreateControl::Creator,
        },
    }
}

fn frames(events: &Events) -> Vec<(u64, Option<u64>, u32)> {
    std::mem::take(&mut *events.lock().unwrap())
        .into_iter()
        .map(|event| match event {
            ChannelEvent::Frame(frame) => (frame.frame, frame.base, frame.input),
            ChannelEvent::Pane(_) | ChannelEvent::Accessibility(_) | ChannelEvent::Closed(_) => {
                panic!("unexpected channel event")
            }
        })
        .collect()
}

#[test]
fn requests_are_concurrent_and_sync_work_runs_off_the_io_thread() {
    let mut harness = Harness::new(None);
    let mutate = harness.link.request(edit()).unwrap();
    let release = harness
        .link
        .request(Operation::ReleaseAttachment {
            attachment: attachment(),
        })
        .unwrap();
    let after = harness
        .link
        .request(Operation::WorktreeRead(crate::worktree::Read::Settings))
        .unwrap();
    let direct = harness
        .link
        .request(Operation::Direct(Management::Status))
        .unwrap();
    harness.pump().unwrap();
    assert!(
        mutate.try_take().is_none(),
        "the deferred reply is still owed"
    );
    assert!(matches!(release.wait(), Ok(ReplyBody::Done)));
    assert_eq!(
        after.wait().unwrap_err(),
        RequestError::Refused(Refusal::StorageUnavailable)
    );
    assert!(matches!(
        direct.wait(),
        Ok(ReplyBody::Direct(Err(DirectError::Disabled)))
    ));
    assert_eq!(*harness.direct.0.lock().unwrap(), [false]);
    let threads = harness.recorded.lock().unwrap().sync_threads.clone();
    assert_eq!(threads.len(), 1);
    assert_ne!(threads[0], std::thread::current().id());

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
        harness.events.lock().unwrap().as_slice(),
        [LinkEvent::Inventory(_)]
    ));
    assert!(matches!(mutate.wait(), Ok(ReplyBody::Done)));
}

#[test]
fn claimed_channels_receive_frames_under_one_frame_of_credit() {
    let mut harness = Harness::new(None);
    let (channel, events) = claimed(&mut harness);
    channel.view(true, 120).unwrap();
    harness.pump().unwrap();
    assert_eq!(harness.recorded.lock().unwrap().views.len(), 1);
    assert_eq!(frames(&events), [(1, None, 0)]);

    let key = |modifiers| vec![InputEvent::CopyOrForward { modifiers }];
    assert_eq!(channel.input(1, key(1)).unwrap(), 1);
    assert_eq!(channel.input(1, key(2)).unwrap(), 2);
    harness.pump().unwrap();
    assert_eq!(harness.recorded.lock().unwrap().inputs, [(1, 1), (2, 1)]);
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

    // The final state follows the acknowledgement, then the end event closes the channel.
    harness
        .connection
        .outbox()
        .present(channel.channel().unwrap(), Box::new(Screen(9)))
        .unwrap();
    harness
        .connection
        .outbox()
        .end(channel.channel().unwrap(), PaneEvent::Ended)
        .unwrap();
    harness.pump().unwrap();
    assert!(events.lock().unwrap().is_empty());
    channel.acknowledge(2).unwrap();
    harness.pump().unwrap();
    assert!(matches!(
        events.lock().unwrap().as_slice(),
        [ChannelEvent::Frame(last), ChannelEvent::Pane(PaneEvent::Ended)] if last.input == 9
    ));
    assert_eq!(channel.input(1, key(3)), Err(LinkError::Unclaimed));
    drop(channel);
    harness.pump().unwrap();
    assert!(
        harness.recorded.lock().unwrap().released.is_empty(),
        "an ended channel needs no release"
    );
}

#[test]
fn dropping_a_channel_releases_it_and_acknowledges_late_frames() {
    let mut harness = Harness::new(None);
    let (channel, _) = claimed(&mut harness);
    let id = channel.channel().unwrap();
    channel.acknowledge(1).unwrap();
    harness.pump().unwrap();
    let outbox = harness.connection.outbox().clone();
    outbox.present(id, Box::new(Screen(1))).unwrap();
    let (frame, priority) = outbox.next_frame().unwrap();
    harness.server.offer(&frame, priority).unwrap();
    drop(channel);
    let frame = harness.client.receive().unwrap().unwrap();
    harness.link.receive(frame, Instant::now()).unwrap();

    let mut assembler = crate::protocol::frame::Assembler::default();
    let mut sent = Vec::new();
    while let Some((frame, priority)) = harness.link.next_frame() {
        let raw = crate::protocol::RawFrame::parse(&frame).unwrap();
        sent.push(assembler.client(raw).unwrap().unwrap());
        harness.client.offer(&frame, priority).unwrap();
    }
    assert!(matches!(
        sent.as_slice(),
        [
            crate::protocol::ClientMessage::Request(release),
            crate::protocol::ClientMessage::FrameAck(ack),
        ] if matches!(release.operation, Operation::Release { channel } if channel == id)
            && ack.frame == 2
    ));
    harness.pump().unwrap();
    assert_eq!(harness.recorded.lock().unwrap().released, [id]);
    assert_eq!(outbox.unacknowledged_since(), None);

    let unclaimed = harness.link.open_channel(Arc::new(|_| {})).unwrap();
    assert_eq!(
        unclaimed.input(1, vec![InputEvent::Resize(size())]),
        Err(LinkError::Unclaimed)
    );
    assert_eq!(unclaimed.view(true, 60), Err(LinkError::Unclaimed));
}

#[test]
fn a_paste_is_sent_whole_in_bounded_messages() {
    let mut harness = Harness::new(None);
    let (channel, _) = claimed(&mut harness);
    let text = "界".repeat(crate::protocol::limits::MAX_PASTE_BYTES / 3);
    let last = channel.paste(1, &text).unwrap();
    harness.pump().unwrap();
    let inputs = harness.recorded.lock().unwrap().inputs.clone();
    assert_eq!(inputs.len(), last as usize);
    assert!(inputs.len() > 1);
    assert_eq!(
        channel.paste(1, &"x".repeat(crate::protocol::limits::MAX_PASTE_BYTES + 1)),
        Err(LinkError::OutOfBounds)
    );
    assert_eq!(
        channel.input(1, vec![]).unwrap_err(),
        LinkError::OutOfBounds
    );
    assert_eq!(
        channel.input(1, vec![InputEvent::Resize(size())]).unwrap(),
        last + 1,
        "refused messages consume no serial"
    );
}

#[test]
fn protocol_order_violations_end_the_connection() {
    let mut harness = Harness::new(None);
    let (channel, _) = claimed(&mut harness);
    drop(channel);
    // A misbehaving client: input on a channel number no claim introduced.
    harness
        .client
        .offer(
            &crate::protocol::frame::encode_client(&crate::protocol::ClientMessage::Input(Input {
                channel: Channel(7),
                serial: 1,
                generation: 1,
                events: vec![InputEvent::Resize(size())],
            }))
            .unwrap()
            .frames[0],
            super::carrier::Priority::L0,
        )
        .unwrap();
    let frame = harness.server.receive().unwrap().unwrap();
    assert_eq!(
        harness
            .connection
            .receive(frame, Instant::now())
            .unwrap_err(),
        Fatal::ProtocolViolation
    );
}

#[test]
fn revocation_ends_a_live_paired_connection() {
    struct Severed(Arc<Mutex<bool>>);
    impl Sever for Severed {
        fn sever(&self) {
            *self.0.lock().unwrap() = true;
        }
    }
    let pairing = Pairing {
        pairing: [4; 32],
        generation: [5; 32],
    };
    let mut harness = Harness::new(Some(pairing));
    let shutdown = harness.link.request(Operation::Shutdown).unwrap();
    harness.pump().unwrap();
    assert_eq!(
        shutdown.wait().unwrap_err(),
        RequestError::Refused(Refusal::Unsupported),
        "paired peers cannot administer the account"
    );

    let severed = Arc::new(Mutex::new(false));
    let mut registry = PairedConnections::default();
    registry
        .register(
            harness.connection.peer().connection,
            harness.connection.peer().client,
            pairing,
            None,
            Some(harness.connection.outbox().clone()),
            Box::new(Severed(Arc::clone(&severed))),
        )
        .unwrap();
    let pending = harness.link.request(edit());
    registry.revoke([4; 32]);
    harness.pump().unwrap();
    assert!(*severed.lock().unwrap());
    assert!(harness.recorded.lock().unwrap().deferred.is_empty());
    assert_eq!(
        harness.link.health(),
        Health::Closed(LinkError::Goodbye(Fatal::Revoked))
    );
    assert_eq!(
        pending.unwrap().wait().unwrap_err(),
        RequestError::Link(LinkError::Goodbye(Fatal::Revoked))
    );
}

#[test]
fn health_comes_from_traffic_and_failure_settles_every_pending_request() {
    let mut harness = Harness::new(None);
    let start = Instant::now();
    let next = harness.link.check(start + Duration::from_secs(5)).unwrap();
    assert!(next > start + Duration::from_secs(5));
    harness.pump().unwrap();
    assert_eq!(harness.link.health(), Health::Live);

    let pending = harness.link.request(edit()).unwrap();
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
        harness.events.lock().unwrap().as_slice(),
        [LinkEvent::Health(Health::Closed(_))]
    ));
    assert_eq!(
        harness.link.request(Operation::Shutdown).err(),
        Some(closed)
    );
    assert_eq!(harness.link.check(start + Duration::from_secs(60)), None);
}

#[test]
fn a_claim_past_the_channel_bound_is_refused_and_the_connection_continues() {
    use crate::protocol::frame::encode_client;
    use crate::protocol::{Claim, ClientMessage, Request};
    let mut harness = Harness::new(None);
    let mut channels = Vec::new();
    for _ in 0..crate::protocol::limits::MAX_CHANNELS {
        channels.push(claimed(&mut harness).0);
    }
    // The link never opens more channels, so a claim past the bound comes from another client.
    let message = ClientMessage::Request(Box::new(Request {
        id: RequestId(9999),
        operation: Operation::Claim(Claim {
            channel: Channel(10_000),
            attachment: attachment(),
            pane: [2; 32],
            claim: AttachmentClaim::Select { explicit: true },
            size: Some(size()),
            style: None,
        }),
    }));
    let frame = encode_client(&message).unwrap().frames.remove(0);
    let received = harness.connection.receive(
        crate::protocol::RawFrame::parse(&frame).unwrap(),
        Instant::now(),
    );
    assert!(
        matches!(received, Ok(None)),
        "the claim is refused, not fatal"
    );
    let outbox = harness.connection.outbox().clone();
    let mut assembler = crate::protocol::frame::Assembler::default();
    let mut refused = false;
    while let Some((frame, _)) = outbox.next_frame() {
        if let Some(crate::protocol::ServerMessage::Reply(reply)) = assembler
            .server(crate::protocol::RawFrame::parse(&frame).unwrap())
            .unwrap()
        {
            refused |=
                reply.id == RequestId(9999) && matches!(reply.result, Err(Refusal::Capacity));
        }
    }
    assert!(refused);
    assert!(!outbox.is_closing());
    drop(channels);
}

/// Pane workers can open handles in one order and reach claim submission in another.
#[test]
fn channels_can_be_claimed_in_a_different_order_than_their_handles_were_opened() {
    let mut harness = Harness::new(None);
    let first = harness.link.open_channel(Arc::new(|_| {})).unwrap();
    let second = harness.link.open_channel(Arc::new(|_| {})).unwrap();
    let claim = |channel: &super::link::ChannelHandle| {
        channel
            .claim(
                attachment(),
                [2; 32],
                AttachmentClaim::Select { explicit: false },
                size(),
                None,
            )
            .unwrap()
    };
    let second_reply = claim(&second);
    let first_reply = claim(&first);
    assert_eq!(
        harness.pump(),
        Ok(()),
        "concurrent Pane claims must preserve wire channel order"
    );
    for reply in [second_reply, first_reply] {
        assert!(matches!(
            reply.wait(),
            Ok(ReplyBody::Claim(ClaimReply::Attached { .. }))
        ));
    }
    assert_eq!(harness.link.health(), Health::Live);
}
