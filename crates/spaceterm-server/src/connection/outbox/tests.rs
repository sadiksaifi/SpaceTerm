use super::*;
use crate::protocol::frame::Assembler;
use crate::protocol::{RawFrame, ScreenUpdate};
use std::sync::atomic::{AtomicUsize, Ordering};

struct State(u64);
impl Presentation for State {
    fn encode(self: Box<Self>, _ticket: FrameTicket) -> Option<Presented> {
        Some(Presented {
            input: self.0 as u32,
            update: ScreenUpdate {
                generation: self.0,
                ..ScreenUpdate::default()
            },
        })
    }
}
struct Unchanged;
impl Presentation for Unchanged {
    fn encode(self: Box<Self>, _ticket: FrameTicket) -> Option<Presented> {
        None
    }
}
/// A state whose encoding exceeds the update bound.
struct Oversized;
impl Presentation for Oversized {
    fn encode(self: Box<Self>, _ticket: FrameTicket) -> Option<Presented> {
        let rows = (0..256)
            .map(|index| crate::protocol::screen::Row {
                index,
                wrapped: false,
                runs: vec![crate::protocol::screen::Run {
                    style: crate::protocol::screen::StyleRef::Default,
                    link: None,
                    text: "x".repeat(70 * 1024),
                    cells: crate::protocol::screen::Cells::Narrow,
                }],
            })
            .collect();
        Some(Presented {
            input: 0,
            update: ScreenUpdate {
                rows,
                ..ScreenUpdate::default()
            },
        })
    }
}

fn fresh() -> (Outbox, Arc<AtomicUsize>) {
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    (
        Outbox::new(Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        })),
        wakes,
    )
}

fn drain(outbox: &Outbox) -> Vec<ServerMessage> {
    let mut assembler = Assembler::default();
    let mut messages = Vec::new();
    while let Some((frame, _)) = outbox.next_frame() {
        if let Some(message) = assembler.server(RawFrame::parse(&frame).unwrap()).unwrap() {
            messages.push(message);
        }
    }
    messages
}

fn frame(message: &ServerMessage) -> (u64, Option<u64>, u32) {
    match message {
        ServerMessage::Frame(frame) => (frame.frame, frame.base, frame.input),
        _ => panic!("expected a frame"),
    }
}

#[test]
fn one_frame_of_credit_and_newer_state_replaces_pending_state() {
    let (outbox, wakes) = fresh();
    let channel = Channel(1);
    outbox.open(channel).unwrap();
    outbox.present(channel, Box::new(State(1))).unwrap();
    outbox.present(channel, Box::new(State(2))).unwrap();
    assert!(wakes.load(Ordering::Relaxed) >= 2);
    let sent = drain(&outbox);
    assert_eq!(sent.iter().map(frame).collect::<Vec<_>>(), [(1, None, 2)]);
    assert_eq!(
        outbox.unacknowledged_since(),
        None,
        "the timer starts when the frame is written"
    );
    let written = Instant::now();
    outbox.written(written);
    assert_eq!(outbox.unacknowledged_since(), Some(written));

    outbox.present(channel, Box::new(State(3))).unwrap();
    outbox.present(channel, Box::new(State(4))).unwrap();
    assert!(
        drain(&outbox).is_empty(),
        "no credit before the acknowledgement"
    );
    assert_eq!(outbox.acknowledge(channel, 2), Err(OutboxError::Violation));
    outbox.acknowledge(channel, 1).unwrap();
    let sent = drain(&outbox);
    assert_eq!(
        sent.iter().map(frame).collect::<Vec<_>>(),
        [(2, Some(1), 4)]
    );

    outbox.acknowledge(channel, 2).unwrap();
    outbox.require_keyframe(channel);
    outbox.present(channel, Box::new(Unchanged)).unwrap();
    assert!(drain(&outbox).is_empty());
    assert_eq!(
        outbox.unacknowledged_since(),
        None,
        "an unchanged state keeps the credit"
    );
    outbox.present(channel, Box::new(State(5))).unwrap();
    let sent = drain(&outbox);
    assert_eq!(
        frame(&sent[0]).1,
        None,
        "the required keyframe survives an unchanged state"
    );
}

#[test]
fn hidden_channels_receive_no_frames() {
    let (outbox, _) = fresh();
    let channel = Channel(4);
    outbox.open(channel).unwrap();
    outbox.present(channel, Box::new(State(1))).unwrap();
    outbox.view(channel, false);
    outbox.present(channel, Box::new(State(2))).unwrap();
    assert!(drain(&outbox).is_empty());
    outbox.view(channel, true);
    outbox.present(channel, Box::new(State(3))).unwrap();
    assert_eq!(drain(&outbox).len(), 1);
}

#[test]
fn ending_a_channel_writes_the_final_state_before_the_event() {
    let (outbox, _) = fresh();
    let channel = Channel(1);
    outbox.open(channel).unwrap();
    outbox.present(channel, Box::new(State(1))).unwrap();
    assert_eq!(drain(&outbox).len(), 1);
    outbox.present(channel, Box::new(State(2))).unwrap();
    outbox.end(channel, PaneEvent::Ended).unwrap();
    outbox.present(channel, Box::new(State(3))).unwrap();
    assert!(
        drain(&outbox).is_empty(),
        "the final state waits for the in-flight frame's acknowledgement"
    );
    assert!(outbox.take_closed().is_empty());
    outbox.acknowledge(channel, 1).unwrap();
    let sent = drain(&outbox);
    assert!(matches!(
        sent.as_slice(),
        [ServerMessage::Frame(last), ServerMessage::Pane(PaneMessage { event: PaneEvent::Ended, .. })]
            if last.input == 2
    ));
    assert_eq!(outbox.take_closed(), [channel]);
    outbox
        .acknowledge(channel, 2)
        .expect("an acknowledgement after the end is ignored");

    let other = Channel(2);
    outbox.open(other).unwrap();
    outbox.present(other, Box::new(State(1))).unwrap();
    assert_eq!(drain(&outbox).len(), 1);
    outbox.end(other, PaneEvent::Failed).unwrap();
    assert!(drain(&outbox).is_empty());
    outbox.acknowledge(other, 1).unwrap();
    assert!(matches!(
        drain(&outbox).as_slice(),
        [ServerMessage::Pane(PaneMessage {
            event: PaneEvent::Failed,
            ..
        })]
    ));
}

#[test]
fn encoding_failures_are_the_servers_own_and_keep_reply_ids() {
    let (outbox, _) = fresh();
    outbox.begin(RequestId(1)).unwrap();
    let huge = crate::protocol::Page {
        epoch: 1,
        start: 0,
        rows: vec![crate::protocol::screen::Row {
            index: 0,
            wrapped: false,
            runs: vec![crate::protocol::screen::Run {
                style: crate::protocol::screen::StyleRef::Default,
                link: None,
                text: "x".repeat(limits::MAX_REPLY_BYTES),
                cells: crate::protocol::screen::Cells::Narrow,
            }],
        }],
        styles: vec![],
        links: vec![],
    };
    let error = outbox
        .reply(RequestId(1), Ok(ReplyBody::Page(huge.clone())))
        .unwrap_err();
    assert_eq!(error, OutboxError::Encode(EncodeError::TooLarge));
    assert_eq!(Fatal::from(error), Fatal::Internal);
    outbox
        .reply_or_refuse(RequestId(1), Ok(ReplyBody::Page(huge)))
        .unwrap();
    assert!(matches!(
        drain(&outbox).as_slice(),
        [ServerMessage::Reply(reply)] if reply.result.as_ref().err() == Some(&Refusal::Capacity)
    ));
    assert_eq!(
        outbox.reply(RequestId(1), Ok(ReplyBody::Done)),
        Err(OutboxError::Misuse),
        "exactly one reply per request"
    );

    let channel = Channel(3);
    outbox.open(channel).unwrap();
    outbox.present(channel, Box::new(Oversized)).unwrap();
    assert!(matches!(
        drain(&outbox).as_slice(),
        [ServerMessage::Pane(PaneMessage {
            event: PaneEvent::Failed,
            ..
        })]
    ));
    assert_eq!(outbox.take_closed(), [channel]);
}

#[test]
fn publication_order_holds_within_l1_and_l0_preempts_l2_chunks() {
    let (outbox, _) = fresh();
    let channel = Channel(1);
    outbox.open(channel).unwrap();
    for id in 1..=3 {
        outbox.begin(RequestId(id)).unwrap();
    }
    let page = crate::protocol::Page {
        epoch: 1,
        start: 0,
        // 300 rows of 512 cells: about 150 KiB.
        rows: (0..300)
            .map(|index| crate::protocol::screen::Row {
                index,
                wrapped: false,
                runs: vec![crate::protocol::screen::Run {
                    style: crate::protocol::screen::StyleRef::Default,
                    link: None,
                    text: "x".repeat(512),
                    cells: crate::protocol::screen::Cells::Narrow,
                }],
            })
            .collect(),
        styles: vec![],
        links: vec![],
    };
    outbox
        .reply(RequestId(1), Ok(ReplyBody::Page(page)))
        .unwrap();
    outbox
        .inventory(InventoryRevision {
            inventory: crate::hierarchy::State::empty().inventory,
            next_sequence: 1,
        })
        .unwrap();
    outbox.reply(RequestId(2), Ok(ReplyBody::Done)).unwrap();
    outbox.present(channel, Box::new(State(1))).unwrap();
    outbox
        .pane(channel, PaneEvent::Line(Default::default()))
        .unwrap();

    let mut assembler = Assembler::default();
    let mut order = Vec::new();
    let mut frames = 0;
    while let Some((frame, priority)) = outbox.next_frame() {
        frames += 1;
        if frames == 6 {
            outbox.ping(9).unwrap();
        }
        let frame = RawFrame::parse(&frame).unwrap();
        assert!(frame.body.len() <= limits::MAX_FRAME_BODY);
        if let Some(message) = assembler.server(frame).unwrap() {
            order.push(match message {
                ServerMessage::Inventory(_) => "inventory",
                ServerMessage::Reply(reply) if reply.id == RequestId(2) => "reply",
                ServerMessage::Frame(_) => "frame",
                ServerMessage::Pane(_) => "pane",
                ServerMessage::Ping(_) => {
                    assert_eq!(priority, Priority::L0);
                    "ping"
                }
                ServerMessage::Reply(reply) if reply.id == RequestId(1) => {
                    assert_eq!(priority, Priority::L2);
                    "page"
                }
                _ => "other",
            });
        }
    }
    assert_eq!(
        order,
        ["inventory", "reply", "frame", "pane", "ping", "page"]
    );
}

#[test]
fn goodbye_is_last_and_bounds_stall_the_connection() {
    let (outbox, _) = fresh();
    outbox.ping(1).unwrap();
    outbox.goodbye(Fatal::Revoked).unwrap();
    assert!(outbox.is_closing());
    assert_eq!(
        outbox.pane(Channel(1), PaneEvent::Ended),
        Err(OutboxError::Closed)
    );
    assert!(matches!(
        drain(&outbox).as_slice(),
        [
            ServerMessage::Ping(_),
            ServerMessage::Goodbye(Fatal::Revoked)
        ]
    ));

    let (outbox, _) = fresh();
    let result = (0..=limits::MAX_L0_FRAMES as u64).try_for_each(|nonce| outbox.pong(nonce));
    assert_eq!(result, Err(OutboxError::Stalled));
    // The outbox closes itself: the queued pongs leave, then `Goodbye(Stalled)`.
    assert!(outbox.is_closing());
    assert_eq!(outbox.pong(0), Err(OutboxError::Closed));
    let written = drain(&outbox);
    assert_eq!(written.len(), limits::MAX_L0_FRAMES + 1);
    assert!(matches!(
        written.last(),
        Some(ServerMessage::Goodbye(Fatal::Stalled))
    ));

    let (outbox, _) = fresh();
    for channel in 0..limits::MAX_CHANNELS as u32 {
        outbox.open(Channel(channel)).unwrap();
    }
    assert_eq!(outbox.open(Channel(999)), Err(OutboxError::Capacity));
}

fn graphics(bytes: usize) -> Result<ReplyBody, Refusal> {
    Ok(ReplyBody::Graphics(Ok(crate::pane::graphics::Chunk {
        image: crate::pane::graphics::ImageKey {
            image_id: 1,
            generation: 1,
        },
        offset: 0,
        bytes: vec![7; bytes],
    })))
}

/// A reply's id stays in flight until its last frame is taken for writing, so the in-flight
/// bound also bounds replies a peer leaves unread.
#[test]
fn a_reply_holds_its_request_id_until_its_last_frame_is_taken() {
    let (outbox, _) = fresh();
    for id in 0..limits::MAX_REQUESTS_IN_FLIGHT as u32 {
        outbox.begin(RequestId(id)).unwrap();
        outbox.reply(RequestId(id), Ok(ReplyBody::Done)).unwrap();
    }
    let extra = RequestId(limits::MAX_REQUESTS_IN_FLIGHT as u32);
    assert_eq!(outbox.begin(extra), Err(OutboxError::Violation));
    assert_eq!(drain(&outbox).len(), limits::MAX_REQUESTS_IN_FLIGHT);
    outbox.begin(extra).unwrap();
    // A reply of several frames frees its id with the last one.
    outbox
        .reply(extra, graphics(3 * limits::BULK_CHUNK_BYTES))
        .unwrap();
    assert!(outbox.owes_replies());
    let (_, priority) = outbox.next_frame().unwrap();
    assert_eq!(priority, Priority::L2);
    assert!(
        outbox.owes_replies(),
        "frames of the reply are still queued"
    );
    while outbox.next_frame().is_some() {}
    assert!(!outbox.owes_replies());
    // A reply already queued cannot be sent twice.
    outbox.begin(RequestId(1)).unwrap();
    outbox.reply(RequestId(1), Ok(ReplyBody::Done)).unwrap();
    assert_eq!(
        outbox.reply(RequestId(1), Ok(ReplyBody::Done)),
        Err(OutboxError::Misuse)
    );
}

/// Queued replies and Pane events are bounded by bytes at L1 and L2, and passing a bound closes
/// the connection with `Goodbye(Stalled)`.
#[test]
fn queued_bytes_are_bounded_at_l1_and_l2() {
    let (outbox, _) = fresh();
    let chunk = limits::MAX_REPLY_BYTES / 2;
    let mut result = Ok(());
    for id in 0..8 {
        outbox.begin(RequestId(id)).unwrap();
        result = outbox.reply(RequestId(id), graphics(chunk));
        if result.is_err() {
            assert!(
                (id as usize + 1) * chunk >= limits::MAX_L2_BYTES,
                "refused reply {id}: {result:?}"
            );
            break;
        }
    }
    assert_eq!(result, Err(OutboxError::Stalled));
    assert!(outbox.is_closing());

    let (outbox, _) = fresh();
    let write = |text: String| {
        outbox.pane(
            Channel(1),
            PaneEvent::Effect(crate::pane::effects::Request {
                id: 1,
                generation: 1,
                remaining_ms: 1,
                effect: crate::pane::effects::Effect::ClipboardWrite {
                    target: crate::pane::effects::ClipboardTarget::Standard,
                    text,
                },
            }),
        )
    };
    let text = "x".repeat(crate::pane::effects::MAX_CLIPBOARD_BYTES);
    let events = (0..).find(|_| write(text.clone()).is_err()).unwrap();
    assert!(events * crate::pane::effects::MAX_CLIPBOARD_BYTES <= limits::MAX_L1_BYTES);
    assert!(outbox.is_closing());
}

/// A model that encodes as `messages` messages of one row each, the last with the tail, or as
/// nothing when it equals its base.
struct Model {
    revision: u64,
    messages: u32,
}

impl Semantics for Model {
    fn revision(&self) -> u64 {
        self.revision
    }

    fn encode(
        self: Arc<Self>,
        ticket: SemanticTicket,
        base: Option<Arc<dyn Semantics>>,
    ) -> Option<Box<dyn SemanticEncoding>> {
        assert_eq!(ticket.revision, self.revision);
        assert_eq!(ticket.base, base.as_ref().map(|base| base.revision()));
        (self.messages > 0).then(|| {
            Box::new(Messages {
                ticket,
                sent: 0,
                total: self.messages,
            }) as Box<dyn SemanticEncoding>
        })
    }
}

struct Messages {
    ticket: SemanticTicket,
    sent: u32,
    total: u32,
}

impl SemanticEncoding for Messages {
    fn next(&mut self) -> wire::Update {
        let line = self.sent;
        self.sent += 1;
        wire::Update {
            header: wire::Header {
                demand_generation: self.ticket.demand_generation,
                revision: self.ticket.revision,
                base: self.ticket.base,
                screen: wire::Screen::Primary,
                screen_generation: 0,
            },
            splices: Vec::new(),
            rows: vec![wire::RowUpdate::new(
                line,
                1,
                false,
                "x".into(),
                [wire::CellSpan {
                    column: 0,
                    width: 1,
                    bytes: 1,
                }],
            )],
            tail: (self.sent == self.total).then_some(wire::Tail {
                visible_lines: 0..0,
                cursor: None,
                selection: None,
            }),
        }
    }
}

use crate::pane::accessibility as wire;

fn model(revision: u64, messages: u32) -> Arc<dyn Semantics> {
    Arc::new(Model { revision, messages })
}

/// A written semantic message: its channel, generation, revision, base, line and whether it is the
/// revision's last.
#[derive(Debug, PartialEq, Eq)]
struct Sent {
    channel: u32,
    generation: u64,
    revision: u64,
    base: Option<u64>,
    line: u32,
    last: bool,
}

fn sent(message: &ServerMessage) -> Option<Sent> {
    let ServerMessage::Accessibility(message) = message else {
        return None;
    };
    let header = message.update.header;
    Some(Sent {
        channel: message.channel.0,
        generation: header.demand_generation,
        revision: header.revision,
        base: header.base,
        line: message.update.rows[0].line,
        last: message.update.tail.is_some(),
    })
}

fn semantic(outbox: &Outbox) -> Vec<(u64, u64, Option<u64>, u32, bool)> {
    drain(outbox)
        .iter()
        .map(|message| {
            let sent = sent(message).expect("only semantic messages");
            (
                sent.generation,
                sent.revision,
                sent.base,
                sent.line,
                sent.last,
            )
        })
        .collect()
}

/// Takes the next frame and returns it decoded.
fn take(outbox: &Outbox) -> Option<ServerMessage> {
    let (frame, _) = outbox.next_frame()?;
    Assembler::default()
        .server(RawFrame::parse(&frame).unwrap())
        .unwrap()
}

fn demanding(channel: Channel) -> Outbox {
    let (outbox, _) = fresh();
    outbox.open(channel).unwrap();
    outbox.start_semantics(channel, 1).unwrap();
    outbox
}

/// One semantic credit per channel, separate from frame credit: a revision's messages share it,
/// a newer model replaces one not yet started, and the next revision is relative to the
/// acknowledged one.
#[test]
fn one_semantic_credit_covers_every_message_of_a_revision() {
    let channel = Channel(1);
    let outbox = demanding(channel);
    outbox.semantics(channel, model(1, 3)).unwrap();
    outbox.present(channel, Box::new(State(1))).unwrap();

    let order: Vec<_> = drain(&outbox)
        .iter()
        .map(|message| sent(message).map(|sent| (sent.line, sent.last)))
        .collect();
    assert_eq!(
        order,
        [Some((0, false)), None, Some((1, false)), Some((2, true))],
        "a frame interleaves with a revision's messages"
    );

    outbox.semantics(channel, model(2, 1)).unwrap();
    outbox.semantics(channel, model(3, 2)).unwrap();
    assert!(
        drain(&outbox).is_empty(),
        "no credit before the acknowledgement"
    );
    outbox.acknowledge(channel, 1).unwrap();
    assert!(
        drain(&outbox).is_empty(),
        "frame credit is not semantic credit"
    );

    outbox.acknowledge_semantics(channel, 1, 1).unwrap();
    assert_eq!(
        semantic(&outbox),
        [(1, 3, Some(1), 0, false), (1, 3, Some(1), 1, true)],
        "the newest model replaced the one not yet started"
    );

    outbox.acknowledge_semantics(channel, 1, 3).unwrap();
    outbox.semantics(channel, model(4, 0)).unwrap();
    assert!(
        drain(&outbox).is_empty(),
        "a model equal to its base sends nothing"
    );
    outbox.semantics(channel, model(5, 1)).unwrap();
    assert_eq!(
        semantic(&outbox),
        [(1, 5, Some(3), 0, true)],
        "an unsent model keeps the credit and the base"
    );
}

/// The acknowledgement rule of ADR 0062, case by case.
#[test]
fn semantic_acknowledgements_follow_the_rule_in_order() {
    let channel = Channel(1);
    let (outbox, _) = fresh();
    outbox
        .acknowledge_semantics(Channel(9), 1, 1)
        .expect("a channel never opened is ignored");
    outbox.open(channel).unwrap();
    assert_eq!(
        outbox.acknowledge_semantics(channel, 1, 1),
        Err(OutboxError::Violation),
        "a channel that never started demand"
    );

    outbox.start_semantics(channel, 1).unwrap();
    outbox.semantics(channel, model(1, 2)).unwrap();
    assert!(take(&outbox).is_some());
    assert_eq!(
        outbox.acknowledge_semantics(channel, 1, 1),
        Err(OutboxError::Violation),
        "the revision in flight before its tail was written"
    );
    assert_eq!(semantic(&outbox), [(1, 1, None, 1, true)]);
    assert_eq!(
        outbox.acknowledge_semantics(channel, 2, 1),
        Err(OutboxError::Violation),
        "a generation after the last start"
    );
    assert_eq!(
        outbox.acknowledge_semantics(channel, 1, 2),
        Err(OutboxError::Violation),
        "a revision above any written"
    );
    outbox.acknowledge_semantics(channel, 1, 1).unwrap();
    outbox
        .acknowledge_semantics(channel, 1, 1)
        .expect("a duplicate is ignored");

    outbox.semantics(channel, model(2, 1)).unwrap();
    outbox.semantics(channel, model(3, 1)).unwrap();
    assert_eq!(semantic(&outbox), [(1, 3, Some(1), 0, true)]);
    outbox
        .acknowledge_semantics(channel, 1, 2)
        .expect("a revision coalesced away is ignored");
    outbox.semantics(channel, model(4, 1)).unwrap();
    assert!(
        drain(&outbox).is_empty(),
        "an ignored acknowledgement returns no credit"
    );
    outbox.acknowledge_semantics(channel, 1, 3).unwrap();
    assert_eq!(semantic(&outbox), [(1, 4, Some(3), 0, true)]);

    outbox.start_semantics(channel, 2).unwrap();
    outbox
        .acknowledge_semantics(channel, 1, 4)
        .expect("an earlier generation is ignored");
    outbox.semantics(channel, model(4, 1)).unwrap();
    assert_eq!(semantic(&outbox), [(2, 4, None, 0, true)]);
    outbox.semantics(channel, model(5, 1)).unwrap();
    outbox
        .acknowledge_semantics(channel, 1, 4)
        .expect("an earlier generation delayed past a restart is ignored");
    assert!(drain(&outbox).is_empty(), "and returns no credit");
    outbox.acknowledge_semantics(channel, 2, 4).unwrap();
    assert_eq!(semantic(&outbox), [(2, 5, Some(4), 0, true)]);

    outbox.close(channel);
    outbox
        .acknowledge_semantics(channel, 2, 5)
        .expect("a closed channel is ignored");
}

/// A start abandons whatever the previous generation had in flight, at every message boundary
/// of a three-message revision, and sends a complete document without waiting for the old
/// credit. With no terminal change, it reuses the revision number.
#[test]
fn a_restart_at_each_message_boundary_sends_a_complete_document_at_once() {
    for taken in 0..=3 {
        let channel = Channel(1);
        let outbox = demanding(channel);
        outbox.semantics(channel, model(7, 3)).unwrap();
        for _ in 0..taken {
            assert!(take(&outbox).is_some());
        }
        outbox.start_semantics(channel, 2).unwrap();
        outbox.semantics(channel, model(7, 3)).unwrap();
        assert_eq!(
            semantic(&outbox),
            [
                (2, 7, None, 0, false),
                (2, 7, None, 1, false),
                (2, 7, None, 2, true)
            ],
            "after {taken} messages of the old generation"
        );
        outbox
            .acknowledge_semantics(channel, 1, 7)
            .expect("the old revision's acknowledgement is ignored");
        outbox.acknowledge_semantics(channel, 2, 7).unwrap();

        for stale in [2, 1, 0] {
            assert_eq!(
                outbox.start_semantics(channel, stale),
                Err(OutboxError::Violation),
                "a start must name a greater generation"
            );
        }
    }
}

/// Hide suspends a revision whose messages started, keeping its model and credit; show sends the
/// rest through its tail, then the newest model against it. A revision not yet started is
/// replaced by the newest model.
#[test]
fn hide_suspends_a_started_revision_and_show_finishes_it() {
    let channel = Channel(1);
    for taken in 0..=3 {
        let outbox = demanding(channel);
        outbox.semantics(channel, model(1, 3)).unwrap();
        let mut written: Vec<_> = (0..taken).filter_map(|_| take(&outbox)).collect();
        outbox.view(channel, false);
        outbox.semantics(channel, model(2, 1)).unwrap();
        assert!(drain(&outbox).is_empty(), "after {taken}: hidden");
        if taken == 3 {
            outbox.acknowledge_semantics(channel, 1, 1).unwrap();
            assert!(drain(&outbox).is_empty(), "credit returned while hidden");
        }
        outbox.view(channel, true);
        written.extend(drain(&outbox));
        if taken < 3 {
            outbox.acknowledge_semantics(channel, 1, 1).unwrap();
            written.extend(drain(&outbox));
        }
        let written: Vec<_> = written
            .iter()
            .map(|message| {
                let sent = sent(message).unwrap();
                (sent.revision, sent.base, sent.line, sent.last)
            })
            .collect();
        let expected: &[_] = if taken == 0 {
            // A revision not yet started is replaced by the newer model.
            &[(2, None, 0, true)]
        } else {
            &[
                (1, None, 0, false),
                (1, None, 1, false),
                (1, None, 2, true),
                (2, Some(1), 0, true),
            ]
        };
        assert_eq!(written, expected, "after {taken} messages before hiding");
    }
}

/// End, Release, Close and the end of the Terminal Session abandon an unfinished revision. The
/// channel's last event waits for frame credit only.
#[test]
fn channel_termination_never_waits_for_semantic_credit() {
    let channel = Channel(1);
    for taken in 0..=3 {
        // The Terminal Session ends with a frame in flight.
        let outbox = demanding(channel);
        outbox.present(channel, Box::new(State(1))).unwrap();
        assert!(matches!(take(&outbox), Some(ServerMessage::Frame(_))));
        outbox.semantics(channel, model(1, 3)).unwrap();
        for _ in 0..taken {
            assert!(take(&outbox).is_some());
        }
        outbox.end(channel, PaneEvent::Ended).unwrap();
        assert!(
            drain(&outbox).is_empty(),
            "after {taken}: waits for the frame"
        );
        outbox.acknowledge(channel, 1).unwrap();
        assert!(
            matches!(
                drain(&outbox).as_slice(),
                [ServerMessage::Pane(PaneMessage {
                    event: PaneEvent::Ended,
                    ..
                })]
            ),
            "after {taken}: the end follows the frame's acknowledgement alone"
        );
        outbox
            .acknowledge_semantics(channel, 1, 1)
            .expect("a semantic acknowledgement after the end is ignored");

        // Demand ends, or the client releases or closes the channel.
        for terminate in [Outbox::end_semantics, Outbox::close] {
            let outbox = demanding(channel);
            outbox.semantics(channel, model(1, 3)).unwrap();
            for _ in 0..taken {
                assert!(take(&outbox).is_some());
            }
            terminate(&outbox, channel);
            outbox.semantics(channel, model(2, 1)).unwrap();
            assert!(drain(&outbox).is_empty(), "after {taken}: nothing more");
            outbox
                .acknowledge_semantics(channel, 1, 1)
                .expect("the abandoned revision's acknowledgement is ignored");
        }
    }
}

/// The encoder runs only when the I/O thread takes the channel's turn, one message per turn: a
/// revision is never encoded ahead of its writing.
#[test]
fn semantic_messages_are_encoded_one_per_turn() {
    struct Counted(Arc<AtomicUsize>, Messages);
    impl SemanticEncoding for Counted {
        fn next(&mut self) -> wire::Update {
            self.0.fetch_add(1, Ordering::Relaxed);
            self.1.next()
        }
    }
    struct CountedModel(Arc<AtomicUsize>);
    impl Semantics for CountedModel {
        fn revision(&self) -> u64 {
            1
        }
        fn encode(
            self: Arc<Self>,
            ticket: SemanticTicket,
            _base: Option<Arc<dyn Semantics>>,
        ) -> Option<Box<dyn SemanticEncoding>> {
            Some(Box::new(Counted(
                Arc::clone(&self.0),
                Messages {
                    ticket,
                    sent: 0,
                    total: 4,
                },
            )))
        }
    }
    let channel = Channel(1);
    let outbox = demanding(channel);
    let encoded = Arc::new(AtomicUsize::new(0));
    outbox
        .semantics(channel, Arc::new(CountedModel(Arc::clone(&encoded))))
        .unwrap();
    assert_eq!(encoded.load(Ordering::Relaxed), 0, "queued, not encoded");
    for turn in 1..=4 {
        assert!(take(&outbox).is_some());
        assert_eq!(encoded.load(Ordering::Relaxed), turn);
    }
    assert!(take(&outbox).is_none());
}
