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
        history: screen_encoder::HistoryTracker::default().observe(&screen),
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

fn bound(outbox: &Outbox) -> Publication {
    Publication::bind(outbox, CHANNEL).unwrap()
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
