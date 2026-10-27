use super::*;
use spaceterm_server::protocol::frame::Assembler;
use spaceterm_server::protocol::{Frame, RawFrame, ServerMessage};
use std::time::{Duration, Instant};

const CHANNEL: Channel = Channel(1);

fn outbox() -> Outbox {
    Outbox::new(Arc::new(|| {}))
}

/// A screen whose rows read `text`, as wire generation `generation`.
fn capture(generation: u64, text: &str, input_serial: u32) -> ScreenCapture {
    let mut emulator =
        crate::terminal::emulator::TerminalEmulator::new(geometry(super::super::tests::size()))
            .expect("a test terminal starts");
    emulator.feed(text.as_bytes());
    emulator.set_input_serial(input_serial);
    let screen = emulator
        .snapshot()
        .expect("a snapshot is captured")
        .expect("new output makes a snapshot");
    ScreenCapture {
        screen,
        generation,
        accessibility_revision: 0,
    }
}

fn live(screen: Option<ScreenCapture>) -> Observed {
    Observed::Live(Observation {
        screen,
        effects: Vec::new(),
        hidden_input: false,
        accessibility: None,
    })
}

/// Writes everything the outbox holds, acknowledging each frame as a client does.
fn drain(outbox: &Outbox) -> Vec<ServerMessage> {
    let mut assembler = Assembler::default();
    let mut messages = Vec::new();
    while let Some((frame, _)) = outbox.next_frame() {
        let Some(message) = assembler.server(RawFrame::parse(&frame).unwrap()).unwrap() else {
            continue;
        };
        if let ServerMessage::Frame(frame) = &message {
            outbox.acknowledge(CHANNEL, frame.frame).unwrap();
        }
        messages.push(message);
    }
    messages
}

fn frames(messages: &[ServerMessage]) -> Vec<&Frame> {
    messages
        .iter()
        .filter_map(|message| match message {
            ServerMessage::Frame(frame) => Some(frame.as_ref()),
            _ => None,
        })
        .collect()
}

fn events(messages: Vec<ServerMessage>) -> Vec<PaneEvent> {
    messages
        .into_iter()
        .filter_map(|message| match message {
            ServerMessage::Pane(pane) => Some(pane.event),
            _ => None,
        })
        .collect()
}

/// The client's text rows: a keyframe replaces them, and an update applies its shift, then its
/// rows.
#[derive(Default)]
struct Rows(Vec<String>);

impl Rows {
    fn apply(&mut self, frame: &Frame) {
        let update = &frame.update;
        if frame.base.is_none() {
            let rows = update.geometry.map_or(0, |geometry| geometry.rows);
            self.0 = vec![String::new(); usize::from(rows)];
        } else if let Some(geometry) = update.geometry {
            self.0.resize(usize::from(geometry.rows), String::new());
        }
        if let Some(shift) = update.shift {
            shift
                .apply(&mut self.0, String::new)
                .expect("a valid shift");
        }
        for row in &update.rows {
            self.0[usize::from(row.index)] = row.runs.iter().map(|run| run.text.as_str()).collect();
        }
    }

    fn shows(&self, text: &str) -> bool {
        self.0.iter().any(|row| row.trim_end() == text)
    }
}

/// A bound channel the client shows.
fn bound(outbox: &Outbox) -> Publication {
    let mut publication = Publication::bind(outbox, CHANNEL).unwrap();
    outbox.view(CHANNEL, true);
    publication.view(true);
    publication
}

#[test]
fn a_new_screen_is_pushed_once_with_its_input_serial() {
    let outbox = outbox();
    let mut publication = bound(&outbox);

    assert!(publication.publish(live(Some(capture(1, "one", 4)))));
    assert!(publication.publish(live(Some(capture(1, "one", 4)))));

    let sent = drain(&outbox);
    let [frame] = frames(&sent)[..] else {
        panic!("expected one frame, got {sent:?}");
    };
    let mut rows = Rows::default();
    rows.apply(frame);
    assert_eq!((frame.base, frame.input), (None, 4));
    assert!(rows.shows("one"));
}

#[test]
fn later_screens_apply_to_the_acknowledged_frame() {
    let outbox = outbox();
    let mut publication = bound(&outbox);
    let mut rows = Rows::default();
    publication.publish(live(Some(capture(1, "one", 0))));
    for frame in frames(&drain(&outbox)) {
        rows.apply(frame);
    }

    publication.publish(live(Some(capture(2, "one\r\ntwo", 0))));

    let sent = drain(&outbox);
    let [frame] = frames(&sent)[..] else {
        panic!("expected one frame, got {sent:?}");
    };
    assert_eq!(frame.base, Some(1));
    rows.apply(frame);
    assert!(rows.shows("one") && rows.shows("two"));
}

#[test]
fn a_hidden_channel_receives_nothing_and_revealing_presents_the_current_screen() {
    let outbox = outbox();
    let mut publication = bound(&outbox);
    publication.publish(live(Some(capture(1, "one", 0))));
    drain(&outbox);

    outbox.view(CHANNEL, false);
    publication.view(false);
    publication.publish(live(Some(capture(2, "two", 0))));
    assert!(frames(&drain(&outbox)).is_empty());

    outbox.view(CHANNEL, true);
    publication.view(true);
    publication.publish(live(Some(capture(2, "two", 0))));
    let sent = drain(&outbox);
    let [frame] = frames(&sent)[..] else {
        panic!("revealing must present the current screen, got {sent:?}");
    };
    assert_eq!(frame.base, Some(1));
}

#[test]
fn revealing_an_unchanged_screen_sends_nothing() {
    let outbox = outbox();
    let mut publication = bound(&outbox);
    publication.publish(live(Some(capture(1, "one", 0))));
    drain(&outbox);

    outbox.view(CHANNEL, false);
    publication.view(false);
    outbox.view(CHANNEL, true);
    publication.view(true);
    publication.publish(live(Some(capture(1, "one", 0))));

    assert!(drain(&outbox).is_empty());
}

#[test]
fn an_ended_pane_sends_its_final_screen_before_its_last_event() {
    let outbox = outbox();
    let mut publication = bound(&outbox);
    publication.publish(live(Some(capture(1, "one", 0))));

    assert!(!publication.publish(Observed::Gone {
        last: Some(capture(2, "bye", 3)),
        event: PaneEvent::Ended,
    }));

    // The final screen replaced the one not yet written.
    let sent = drain(&outbox);
    let [
        ServerMessage::Pane(_hidden_input),
        ServerMessage::Frame(last),
        ServerMessage::Pane(ended),
    ] = sent.as_slice()
    else {
        panic!("expected the final frame, then the end, got {sent:?}");
    };
    assert_eq!(last.input, 3);
    assert!(matches!(ended.event, PaneEvent::Ended));
    assert_eq!(outbox.take_closed(), [CHANNEL]);
}

#[test]
fn hidden_input_and_effects_are_sent_as_they_change() {
    let outbox = outbox();
    let mut publication = bound(&outbox);
    let effect = pane::effects::Request {
        id: 1,
        generation: 2,
        remaining_ms: 1000,
        effect: pane::effects::Effect::Bell,
    };

    for (hidden_input, effects) in [
        (false, vec![effect]),
        (false, Vec::new()),
        (true, Vec::new()),
    ] {
        publication.publish(Observed::Live(Observation {
            screen: None,
            effects,
            hidden_input,
            accessibility: None,
        }));
    }

    assert!(matches!(
        events(drain(&outbox)).as_slice(),
        [
            PaneEvent::Effect(_),
            PaneEvent::HiddenInput(false),
            PaneEvent::HiddenInput(true)
        ]
    ));
}

#[test]
fn an_accessibility_revision_travels_as_an_event_only_without_a_new_screen() {
    let outbox = outbox();
    let mut publication = bound(&outbox);
    let observed = |screen, revision| {
        Observed::Live(Observation {
            screen,
            effects: Vec::new(),
            hidden_input: false,
            accessibility: Some(revision),
        })
    };

    publication.publish(observed(Some(capture(1, "one", 0)), 1));
    publication.publish(observed(Some(capture(1, "one", 0)), 2));

    let accessibility: Vec<_> = events(drain(&outbox))
        .into_iter()
        .filter_map(|event| match event {
            PaneEvent::Accessibility { revision } => Some(revision),
            _ => None,
        })
        .collect();
    assert_eq!(accessibility, [2]);
}

#[test]
fn a_closing_connection_ends_publication() {
    let outbox = outbox();
    let mut publication = bound(&outbox);
    outbox.goodbye(Fatal::Stopping).unwrap();

    assert!(!publication.publish(live(Some(capture(1, "one", 0)))));
}

/// Publishes after each Terminal Session notification until a frame satisfies `until`, and
/// returns that frame's Input serial. Fails after 10 seconds.
fn await_frame(
    host: &mut DevelopmentPaneHost,
    lease: Lease,
    wake: &(std::sync::Mutex<bool>, std::sync::Condvar),
    (outbox, publication, rows): (&Outbox, &mut Publication, &mut Rows),
    until: impl Fn(&Rows) -> bool,
) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(publication.publish(HostCommands::observe(host, [1; 32], &lease)));
        for frame in frames(&drain(outbox)) {
            rows.apply(frame);
            if until(rows) {
                return frame.input;
            }
        }
        let mut notified = wake.0.lock().unwrap();
        while !*notified {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("the session never notified the expected screen");
            notified = wake.1.wait_timeout(notified, remaining).unwrap().0;
        }
        *notified = false;
    }
}

/// A real shell's echo reaches the controlling channel as a pushed frame that reports the Input
/// serial that caused it. The test waits only for the Terminal Session's notifications.
#[test]
fn a_shell_echo_is_pushed_with_the_input_serial_that_caused_it() {
    let mut host = super::super::tests::host();
    let wake = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    host.set_notifier({
        let wake = Arc::clone(&wake);
        Arc::new(move || {
            *wake.0.lock().unwrap() = true;
            wake.1.notify_all();
        })
    });
    let lease = super::super::tests::attach(&mut host, 1, true);
    let outbox = outbox();
    let mut publication = bound(&outbox);
    let mut rows = Rows::default();

    assert!(matches!(
        super::super::tests::command(&mut host, 1, lease, "echo pushed"),
        Reply::Unchanged
    ));
    host.input_written(&lease, 2);
    let input = await_frame(
        &mut host,
        lease,
        &wake,
        (&outbox, &mut publication, &mut rows),
        |rows| rows.shows("pushed"),
    );
    assert_eq!(input, 2);

    // A hidden Pane publishes nothing; revealing it pushes the screen it missed.
    outbox.view(CHANNEL, false);
    publication.view(false);
    HostCommands::view(&mut host, &lease, false, 120);
    assert!(matches!(
        super::super::tests::command(&mut host, 1, lease, "echo hidden"),
        Reply::Unchanged
    ));
    std::thread::sleep(Duration::from_millis(200));
    assert!(publication.publish(HostCommands::observe(&mut host, [1; 32], &lease)));
    assert!(frames(&drain(&outbox)).is_empty());
    outbox.view(CHANNEL, true);
    publication.view(true);
    HostCommands::view(&mut host, &lease, true, 120);
    await_frame(
        &mut host,
        lease,
        &wake,
        (&outbox, &mut publication, &mut rows),
        |rows| rows.shows("hidden"),
    );
    host.shutdown().unwrap();
}

/// What rings a `Served` host's wake. A test ticks only after a ring, so a lost ring stalls it.
mod bell {
    use super::super::super::dispatch::Served;
    use super::super::super::tests::{create_shared, host, shared_inventory, size};
    use super::*;
    use spaceterm_server::attachments::Attachment;
    use spaceterm_server::connection::service::{Dispatch, PaneService as _, Peer};
    use spaceterm_server::protocol::{
        self, ClaimReply, InputEvent, Operation, ReplyBody, RequestId,
    };
    use std::sync::{Condvar, Mutex};

    const FIRST: Peer = Peer {
        client: [1; 32],
        connection: [1; 32],
        pairing: None,
    };
    const SECOND: Peer = Peer {
        client: [2; 32],
        connection: [2; 32],
        pairing: None,
    };

    #[derive(Default)]
    struct Rings {
        rung: Mutex<bool>,
        ready: Condvar,
    }

    struct Fixture {
        served: Served<SharedWorkspaceHost>,
        rings: Arc<Rings>,
        outbox: Outbox,
        workspace: [u8; 32],
        pane: [u8; 32],
    }

    impl Fixture {
        fn new() -> Self {
            let mut fixture = Self::hidden();
            fixture.input(1, vec![InputEvent::Focus(FocusTarget::Terminal)]);
            fixture.show();
            fixture
        }

        /// A Pane the first client claimed and has not shown yet.
        fn hidden() -> Self {
            let mut shared = SharedWorkspaceHost::new(host());
            let workspace = create_shared(&mut shared, 1);
            let pane = shared_inventory(&mut shared)
                .workspaces
                .iter()
                .find(|candidate| candidate.id == workspace)
                .and_then(|workspace| workspace.tabs.first())
                .and_then(|tab| tab.panes.first())
                .unwrap()
                .id;
            let mut served = Served::new(shared);
            let rings = Arc::new(Rings::default());
            served.ticker(Arc::new({
                let rings = Arc::clone(&rings);
                move || {
                    *rings.rung.lock().unwrap() = true;
                    rings.ready.notify_all();
                }
            }));
            let outbox = super::outbox();
            served.admitted(&FIRST, "First".into(), outbox.clone());
            let mut fixture = Self {
                served,
                rings,
                outbox,
                workspace,
                pane,
            };
            let claim = fixture.claim(&FIRST, AttachmentClaim::Select { explicit: true });
            assert!(matches!(claim, ClaimReply::Attached { .. }));
            fixture
        }

        /// Shows the first client's channel, as its first `View` does.
        fn show(&mut self) {
            self.outbox.view(CHANNEL, true);
            self.served.view(
                &FIRST,
                protocol::View {
                    channel: CHANNEL,
                    visible: true,
                    max_hz: 60,
                },
            );
        }

        fn claim(&mut self, peer: &Peer, claim: AttachmentClaim) -> ClaimReply {
            let operation = Operation::Claim(protocol::Claim {
                channel: CHANNEL,
                attachment: Attachment {
                    workspace: self.workspace,
                    generation: 1,
                },
                pane: self.pane,
                claim,
                size: size(),
                style: None,
            });
            match self.served.request(peer, RequestId(1), operation) {
                Dispatch::Reply(reply) => match *reply {
                    Ok(ReplyBody::Claim(reply)) => reply,
                    other => panic!("the claim failed: {other:?}"),
                },
                _ => panic!("a claim replies at once"),
            }
        }

        fn input(&mut self, serial: u32, events: Vec<InputEvent>) {
            self.served.input(
                &FIRST,
                protocol::Input {
                    channel: CHANNEL,
                    serial,
                    generation: 1,
                    events,
                },
            );
        }

        /// Types `command` and Return.
        fn command(&mut self, serial: u32, command: &str) {
            let InteractiveInput::Key(key) = super::super::super::tests::text_key(command, false)
            else {
                unreachable!()
            };
            let InteractiveInput::Key(enter) = super::super::super::tests::text_key("\r", false)
            else {
                unreachable!()
            };
            self.input(serial, vec![InputEvent::Key(key), InputEvent::Key(enter)]);
        }

        /// Waits until the wake rings or `deadline` passes, and returns whether it rang.
        fn rang_by(&self, deadline: Instant) -> bool {
            let mut rung = self.rings.rung.lock().unwrap();
            while !*rung {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    return false;
                };
                rung = self.rings.ready.wait_timeout(rung, remaining).unwrap().0;
            }
            *rung = false;
            true
        }

        /// Ticks after each ring until a message satisfies `found`. Fails if nothing rings for
        /// 10 seconds.
        fn await_message(&mut self, found: impl Fn(&ServerMessage) -> bool) -> ServerMessage {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                assert!(self.rang_by(deadline), "nothing rang the host's wake");
                self.served.tick(Instant::now());
                if let Some(message) = drain(&self.outbox).into_iter().find(&found) {
                    return message;
                }
            }
        }

        fn await_event(&mut self, found: impl Fn(&PaneEvent) -> bool) -> PaneEvent {
            let message = self.await_message(
                |message| matches!(message, ServerMessage::Pane(pane) if found(&pane.event)),
            );
            let ServerMessage::Pane(pane) = message else {
                unreachable!()
            };
            pane.event
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.served.shutdown();
        }
    }

    #[test]
    fn a_bell_and_a_clipboard_request_ring_the_wake() {
        let mut fixture = Fixture::new();
        fixture.command(2, "printf '\\007'");
        let bell = fixture.await_event(|event| matches!(event, PaneEvent::Effect(_)));
        assert!(matches!(
            bell,
            PaneEvent::Effect(pane::effects::Request {
                effect: pane::effects::Effect::Bell,
                ..
            })
        ));

        fixture.command(3, "printf '\\033]52;c;?\\007'; sleep 5");
        let read = fixture.await_event(|event| {
            matches!(
                event,
                PaneEvent::Effect(pane::effects::Request {
                    effect: pane::effects::Effect::ClipboardRead { .. },
                    ..
                })
            )
        });
        assert!(matches!(read, PaneEvent::Effect(_)));
    }

    #[test]
    fn an_exit_rings_the_wake_and_ends_the_channel() {
        let mut fixture = Fixture::new();
        fixture.command(2, "exit");
        fixture.await_event(|event| matches!(event, PaneEvent::Ended));
    }

    #[test]
    fn a_takeover_rings_the_wake_and_ends_the_former_channel() {
        let mut fixture = Fixture::new();
        let second = super::outbox();
        fixture
            .served
            .admitted(&SECOND, "Second".into(), second.clone());
        let ClaimReply::Takeover(offer) =
            fixture.claim(&SECOND, AttachmentClaim::Select { explicit: true })
        else {
            panic!("the second client is offered Take Over");
        };
        let taken = fixture.claim(
            &SECOND,
            AttachmentClaim::TakeOver {
                target: offer.target,
            },
        );
        assert!(matches!(taken, ClaimReply::Attached { .. }));
        fixture.await_event(|event| matches!(event, PaneEvent::Takeover(_)));
    }

    /// A Pane claimed while hidden receives no screen until its client shows it.
    #[test]
    fn a_claimed_pane_receives_no_screen_until_it_is_shown() {
        let mut fixture = Fixture::hidden();
        std::thread::sleep(Duration::from_millis(200));
        fixture.served.tick(Instant::now());
        assert!(frames(&drain(&fixture.outbox)).is_empty());

        fixture.show();
        assert_eq!(frames(&drain(&fixture.outbox)).len(), 1);
    }
}
