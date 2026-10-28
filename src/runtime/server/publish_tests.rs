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
        line: pane::LineState::default(),
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

    /// Whether a row ends with `output`. A shell such as dash prints its first prompt after
    /// input that arrived early, so output can follow that prompt on its row.
    fn shows_output(&self, output: &str) -> bool {
        self.0.iter().any(|row| row.trim_end().ends_with(output))
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
        ServerMessage::Pane(_line),
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
fn line_states_and_effects_are_sent_as_they_change() {
    let outbox = outbox();
    let mut publication = bound(&outbox);
    let effect = pane::effects::Request {
        id: 1,
        generation: 2,
        remaining_ms: 1000,
        effect: pane::effects::Effect::Bell,
    };

    let prompt = pane::LineState::default();
    let password = pane::LineState {
        epoch: 1,
        mode: pane::LineMode {
            canonical: true,
            ..pane::LineMode::UNKNOWN
        },
    };
    for (line, effects) in [
        (prompt, vec![effect]),
        (prompt, Vec::new()),
        (password, Vec::new()),
    ] {
        publication.publish(Observed::Live(Observation {
            screen: None,
            effects,
            line,
            accessibility: None,
        }));
    }

    let sent = events(drain(&outbox));
    let [
        PaneEvent::Effect(_),
        PaneEvent::Line(first),
        PaneEvent::Line(second),
    ] = sent.as_slice()
    else {
        panic!("expected an effect and two line states, got {sent:?}");
    };
    assert_eq!((*first, *second), (prompt, password));
}

#[test]
fn an_accessibility_revision_travels_as_an_event_only_without_a_new_screen() {
    let outbox = outbox();
    let mut publication = bound(&outbox);
    let observed = |screen, revision| {
        Observed::Live(Observation {
            screen,
            effects: Vec::new(),
            line: pane::LineState::default(),
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
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                panic!(
                    "the session never notified the expected screen; screen: {:?}",
                    rows.0
                );
            };
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
        Arc::new(move |_| {
            let wake = Arc::clone(&wake);
            Arc::new(move || {
                *wake.0.lock().unwrap() = true;
                wake.1.notify_all();
            })
        })
    });
    let lease = super::super::tests::attach(&mut host, 1, true);
    let outbox = outbox();
    let mut publication = bound(&outbox);
    let mut rows = Rows::default();

    assert!(matches!(
        // The typed command never contains the output it prints.
        super::super::tests::command(&mut host, 1, lease, "printf 'PUSH%s\\n' ED"),
        Reply::Unchanged
    ));
    host.input_written(&lease, 2);
    let input = await_frame(
        &mut host,
        lease,
        &wake,
        (&outbox, &mut publication, &mut rows),
        |rows| rows.shows_output("PUSHED"),
    );
    assert_eq!(input, 2);

    // A hidden Pane publishes nothing; revealing it pushes the screen it missed.
    outbox.view(CHANNEL, false);
    publication.view(false);
    HostCommands::view(&mut host, &lease, false, 120);
    assert!(matches!(
        super::super::tests::command(&mut host, 1, lease, "printf 'HIDD%s\\n' EN"),
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
        |rows| rows.shows_output("HIDDEN"),
    );
    host.shutdown().unwrap();
}

/// What wakes the first client's connection and what wakes the host. A test publishes or ticks
/// only after the matching wake, so a lost wake stalls it.
mod wakes {
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

    /// Which wakes rang since they were last served.
    #[derive(Default)]
    struct Rang {
        connection: bool,
        host: bool,
    }

    #[derive(Default)]
    struct Rings {
        rang: Mutex<Rang>,
        ready: Condvar,
    }

    impl Rings {
        fn ring(&self, ring: impl FnOnce(&mut Rang)) {
            ring(&mut self.rang.lock().unwrap());
            self.ready.notify_all();
        }
    }

    /// Which threads a test plays.
    #[derive(Clone, Copy, PartialEq)]
    enum Serve {
        /// Only the first connection's I/O thread, which publishes when its outbox asks.
        Connection,
        /// That thread and the host's, which ticks when its wake rings.
        Both,
    }

    struct Fixture {
        served: Served<SharedWorkspaceHost>,
        rings: Arc<Rings>,
        outbox: Outbox,
        rows: Rows,
        /// The latest frame's grid generation and history epoch, and the newest accessibility
        /// revision a frame or a Pane event carried.
        generation: u64,
        epoch: u64,
        accessibility: u64,
        /// Whether the host's wake rang since the fixture started.
        host_rang: bool,
        /// When the host's last tick asked to tick again.
        host_due: Option<Instant>,
        /// How often the connection published, and after which of those publications the host's
        /// wake had rung, so a test knows which publication rang it.
        publications: usize,
        rung_by_publication: Vec<usize>,
        /// The publication that delivered the message `await_message` last returned.
        found_in: usize,
        workspace: [u8; 32],
        pane: [u8; 32],
    }

    impl Fixture {
        fn new() -> Self {
            let mut fixture = Self::hidden();
            fixture.input(1, vec![InputEvent::Focus(FocusTarget::Terminal)]);
            fixture.show();
            // The claim rang the host. Tests count only rings that follow the setup.
            fixture.rings.rang.lock().unwrap().host = false;
            fixture.host_rang = false;
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
                move || rings.ring(|rang| rang.host = true)
            }));
            let outbox = Outbox::new(Arc::new({
                let rings = Arc::clone(&rings);
                move || rings.ring(|rang| rang.connection = true)
            }));
            served.admitted(&FIRST, "First".into(), outbox.clone());
            let mut fixture = Self {
                served,
                rings,
                outbox,
                rows: Rows::default(),
                generation: 0,
                epoch: 0,
                accessibility: 0,
                host_rang: false,
                host_due: None,
                publications: 0,
                rung_by_publication: Vec::new(),
                found_in: 0,
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

        /// Waits until a wake `serve` plays rings, and serves it as its thread would: the
        /// connection publishes, the host ticks, also at the deadline its last tick returned.
        /// Fails if nothing rings by `deadline`.
        fn serve(&mut self, serve: Serve, deadline: Instant) {
            let both = serve == Serve::Both;
            let (rang, due) = {
                let mut rang = self.rings.rang.lock().unwrap();
                loop {
                    self.host_rang |= rang.host;
                    let due = both && self.host_due.is_some_and(|due| Instant::now() >= due);
                    if rang.connection || (both && rang.host) || due {
                        break (std::mem::take(&mut *rang), due);
                    }
                    let until = [Some(deadline), self.host_due.filter(|_| both)]
                        .into_iter()
                        .flatten()
                        .min()
                        .unwrap_or(deadline);
                    let Some(remaining) = until.checked_duration_since(Instant::now()) else {
                        panic!("nothing woke the connection");
                    };
                    rang = self.rings.ready.wait_timeout(rang, remaining).unwrap().0;
                }
            };
            if rang.connection && self.outbox.take_publication() {
                // The host's wake also serves its own timed work, so only a ring during this
                // publication counts as this publication's.
                let earlier = std::mem::take(&mut self.rings.rang.lock().unwrap().host);
                self.served.publish(&FIRST);
                self.publications += 1;
                let mut rang = self.rings.rang.lock().unwrap();
                if rang.host {
                    self.rung_by_publication.push(self.publications);
                }
                rang.host |= earlier;
            }
            if both && (rang.host || due) {
                self.host_due = self.served.tick(Instant::now());
            }
        }

        /// Serves wakes until the connection writes a message that satisfies `found`, applying
        /// each frame to the client's rows first. Fails after 10 seconds.
        fn await_message(
            &mut self,
            serve: Serve,
            found: impl Fn(&ServerMessage, &Rows) -> bool,
        ) -> ServerMessage {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                self.serve(serve, deadline);
                let mut first = None;
                for message in self.receive() {
                    if first.is_none() && found(&message, &self.rows) {
                        first = Some(message);
                    }
                }
                if let Some(message) = first {
                    self.found_in = self.publications;
                    return message;
                }
            }
        }

        /// Drains what the connection wrote, as its client reads it: each frame applies to the
        /// rows the client holds.
        fn receive(&mut self) -> Vec<ServerMessage> {
            let messages = drain(&self.outbox);
            for message in &messages {
                if let ServerMessage::Frame(frame) = message {
                    self.rows.apply(frame);
                    self.generation = frame.update.generation;
                    self.epoch = frame.update.history.epoch;
                }
                if let Some(revision) = accessibility_revision(message) {
                    self.accessibility = self.accessibility.max(revision);
                }
            }
            messages
        }

        /// Waits until the host's wake has rung since the fixture started, without ticking.
        fn await_host(&mut self) {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut rang = self.rings.rang.lock().unwrap();
            while !self.host_rang && !rang.host {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .expect("nothing woke the host");
                rang = self.rings.ready.wait_timeout(rang, remaining).unwrap().0;
            }
            self.host_rang = true;
        }

        fn await_event(&mut self, serve: Serve, found: impl Fn(&PaneEvent) -> bool) -> PaneEvent {
            let message = self.await_message(
                serve,
                |message, _| matches!(message, ServerMessage::Pane(pane) if found(&pane.event)),
            );
            let ServerMessage::Pane(pane) = message else {
                unreachable!()
            };
            pane.event
        }

        /// Serves only the connection until a frame shows a row that ends with `output`.
        fn await_output(&mut self, output: &str) {
            self.await_message(Serve::Connection, |message, rows| {
                matches!(message, ServerMessage::Frame(_)) && rows.shows_output(output)
            });
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.served.shutdown();
        }
    }

    /// A shell's echo reaches its connection while the host's thread sleeps.
    #[test]
    fn an_echo_reaches_the_connection_without_the_host() {
        let mut fixture = Fixture::new();
        fixture.command(2, "printf 'ECH%s\\n' OED");
        fixture.await_output("ECHOED");
    }

    #[test]
    fn a_bell_and_a_clipboard_request_reach_the_connection_without_the_host() {
        let mut fixture = Fixture::new();
        fixture.command(2, "printf '\\007'");
        let bell = fixture.await_event(Serve::Connection, |event| {
            matches!(event, PaneEvent::Effect(_))
        });
        assert!(matches!(
            bell,
            PaneEvent::Effect(pane::effects::Request {
                effect: pane::effects::Effect::Bell,
                ..
            })
        ));

        fixture.command(3, "printf '\\033]52;c;?\\007'; sleep 5");
        fixture.await_event(Serve::Connection, |event| {
            matches!(
                event,
                PaneEvent::Effect(pane::effects::Request {
                    effect: pane::effects::Effect::ClipboardRead { .. },
                    ..
                })
            )
        });
    }

    /// An exit reaches the connection after its final screen, and wakes the host, whose tick
    /// pushes the inventory without the Pane. Two things ring it in that publication: the
    /// observed exit, and the session losing its last route. The shared host's tests pin the
    /// first.
    #[test]
    fn an_exit_reaches_the_connection_without_the_host_and_then_wakes_it() {
        let mut fixture = Fixture::new();
        fixture.command(2, "printf 'BY%s\\n' E; exit");
        fixture.await_message(Serve::Connection, |message, rows| {
            let ended =
                matches!(message, ServerMessage::Pane(pane) if matches!(pane.event, PaneEvent::Ended));
            assert!(!ended || rows.shows_output("BYE"), "the final screen comes first");
            ended
        });
        assert!(
            fixture.rung_by_publication.contains(&fixture.found_in),
            "the publication that delivered the final screen rang the host"
        );
    }

    /// A presented Terminal Session carries a new title with its screen, which only the
    /// connection observes. The observation rings the host, whose tick pushes the inventory.
    #[test]
    fn a_title_observed_by_the_connection_reaches_the_inventory_through_the_host() {
        let mut fixture = Fixture::new();
        fixture.command(2, "printf '\\033]0;RETITLED\\007'");
        // Only the connection runs, so nothing but its observation can ring the host.
        let deadline = Instant::now() + Duration::from_secs(10);
        while fixture.rung_by_publication.is_empty() {
            fixture.serve(Serve::Connection, deadline);
            fixture.receive();
        }
        let pane = fixture.pane;
        let titled = move |message: &ServerMessage| {
            matches!(message, ServerMessage::Inventory(revision) if revision
                .inventory
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.tabs)
                .flat_map(|tab| &tab.panes)
                .any(|candidate| candidate.id == pane
                    && candidate
                        .presentation
                        .as_ref()
                        .is_some_and(|presentation| presentation.title == "RETITLED")))
        };
        fixture.await_message(Serve::Both, |message, _| titled(message));
    }

    #[test]
    fn a_takeover_ends_the_former_channel() {
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
        fixture.await_event(Serve::Both, |event| matches!(event, PaneEvent::Takeover(_)));
    }

    /// The accessibility revision `message` carries, if any.
    fn accessibility_revision(message: &ServerMessage) -> Option<u64> {
        match message {
            ServerMessage::Pane(pane) => match pane.event {
                PaneEvent::Accessibility { revision } => Some(revision),
                _ => None,
            },
            ServerMessage::Frame(frame) => Some(frame.update.accessibility_revision),
            _ => None,
        }
    }

    /// A client's accessibility request makes the Terminal Session publish a model of its
    /// current screen, behind its presentation barrier. The model reaches the connection without
    /// the host.
    #[test]
    fn an_accessibility_model_reaches_the_connection_without_the_host() {
        let mut fixture = Fixture::new();
        let request = |fixture: &mut Fixture, id| {
            let operation = Operation::Accessibility {
                channel: CHANNEL,
                revision: None,
                offset: 0,
                base: None,
            };
            let _ = fixture.served.request(&FIRST, RequestId(id), operation);
        };
        // The first request's model, or the seed model the session publishes on its own.
        request(&mut fixture, 2);
        fixture.await_message(Serve::Connection, |message, _| {
            accessibility_revision(message).is_some_and(|revision| revision > 0)
        });
        let modeled = fixture.accessibility;
        // New text makes the current screen's model differ from every model published so far,
        // so the next request publishes a newer one.
        fixture.command(2, "printf 'READ%s\\n' ABLE");
        fixture.await_output("READABLE");
        request(&mut fixture, 3);
        fixture.await_message(Serve::Connection, |message, _| {
            accessibility_revision(message).is_some_and(|revision| revision > modeled)
        });
    }

    /// A Scrollback page is read behind the commands before it on the Pane's terminal worker,
    /// which replies to the connection itself.
    #[test]
    fn a_scrollback_read_replies_without_the_host() {
        let mut fixture = Fixture::new();
        fixture.command(2, "seq 1 200");
        fixture.await_output("200");
        let id = RequestId(2);
        fixture.outbox.begin(id).unwrap();
        let operation = Operation::ScrollbackPage {
            channel: CHANNEL,
            epoch: fixture.epoch,
            start: 0,
            rows: 20,
        };
        assert!(matches!(
            fixture.served.request(&FIRST, id, operation),
            Dispatch::Deferred
        ));
        fixture.await_message(
            Serve::Connection,
            |message, _| matches!(message, ServerMessage::Reply(reply) if reply.id == id),
        );
    }

    /// Whether `outbox` was asked to publish within 10 seconds. Clears the request.
    fn asked_to_publish(outbox: &Outbox) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if outbox.take_publication() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// After a takeover, the former controller's channel stays bound until its connection
    /// publishes, so the session's next output wakes both connections.
    #[test]
    fn a_session_bound_on_two_connections_wakes_both() {
        let mut fixture = Fixture::new();
        fixture.command(2, "sleep 1; printf 'BO%s\\n' TH");
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
        assert!(
            asked_to_publish(&fixture.outbox),
            "the first connection woke"
        );
        assert!(asked_to_publish(&second), "the second connection woke");
    }

    /// A connection that ends takes its routes with it. The session it presented rings the host
    /// at once, and its later output wakes the host instead of the ended connection.
    #[test]
    fn a_disconnected_connection_routes_its_sessions_to_the_host() {
        let mut fixture = Fixture::new();
        fixture.command(2, "sleep 1; printf 'GO%s\\n' NE");
        fixture.served.disconnected(&FIRST);
        assert!(
            std::mem::take(&mut fixture.rings.rang.lock().unwrap().host),
            "losing its last route rang the host"
        );
        fixture.await_host();
        assert!(
            !fixture.outbox.take_publication(),
            "nothing asks the ended connection to publish"
        );
    }

    /// A notification taken while the connection closes cannot reach the peer, so it rings the
    /// host instead of being dropped.
    #[test]
    fn a_closing_connection_hands_its_publication_to_the_host() {
        let mut fixture = Fixture::new();
        fixture.outbox.goodbye(protocol::Fatal::Stopping).unwrap();
        fixture.served.publish(&FIRST);
        assert!(fixture.rings.rang.lock().unwrap().host, "the host rang");
    }

    /// A Pane claimed while hidden receives no screen until its client shows it.
    #[test]
    fn a_claimed_pane_receives_no_screen_until_it_is_shown() {
        let mut fixture = Fixture::hidden();
        std::thread::sleep(Duration::from_millis(200));
        if fixture.outbox.take_publication() {
            fixture.served.publish(&FIRST);
        }
        fixture.served.tick(Instant::now());
        assert!(frames(&drain(&fixture.outbox)).is_empty());

        // Showing presents the screen the host holds. A shell that has not drawn its first
        // screen yet presents it when it does.
        fixture.show();
        if frames(&fixture.receive()).is_empty() {
            fixture.await_message(Serve::Connection, |message, _| {
                matches!(message, ServerMessage::Frame(_))
            });
        }
    }
}
