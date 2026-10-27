use super::*;
use spaceterm_server::protocol::frame::Assembler;
use spaceterm_server::protocol::{RawFrame, ServerMessage};
use std::time::Duration;

/// Encodes a screen's wire generation and remembers every screen it encoded. A screen equal to
/// the acknowledged base encodes as nothing.
#[derive(Default)]
struct Recording {
    encoded: Arc<Mutex<Vec<Captured>>>,
    baseline: Option<u64>,
}

impl ScreenEncoding for Recording {
    fn encode(&mut self, screen: &Captured, ticket: FrameTicket) -> Option<ScreenUpdate> {
        if ticket.base.is_some() && self.baseline == Some(screen.generation) {
            return None;
        }
        self.baseline = Some(screen.generation);
        self.encoded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(screen.clone());
        Some(ScreenUpdate {
            generation: screen.generation,
            accessibility_revision: screen.accessibility_revision,
            ..ScreenUpdate::default()
        })
    }
}

fn outbox() -> Outbox {
    Outbox::new(Arc::new(|| {}))
}

fn captured(generation: u64, input_serial: u32) -> Captured {
    let mut screen = (*ScreenSnapshot::empty(crate::local_path::LocalPathSemantics::Posix)).clone();
    screen.input_serial = input_serial;
    Captured {
        screen: Arc::new(screen),
        generation,
        accessibility_revision: 0,
    }
}

/// Writes everything the outbox holds, acknowledging each frame as a client does.
fn drain(outbox: &Outbox, channel: Channel) -> Vec<ServerMessage> {
    let mut assembler = Assembler::default();
    let mut messages = Vec::new();
    while let Some((frame, _)) = outbox.next_frame() {
        let Some(message) = assembler.server(RawFrame::parse(&frame).unwrap()).unwrap() else {
            continue;
        };
        if let ServerMessage::Frame(frame) = &message {
            outbox.acknowledge(channel, frame.frame).unwrap();
        }
        messages.push(message);
    }
    messages
}

fn bound(outbox: &Outbox) -> Publication {
    let mut publication = Publication::default();
    publication
        .bind(outbox.clone(), Channel(1), Box::<Recording>::default())
        .unwrap();
    publication
}

#[test]
fn a_new_screen_is_pushed_once_with_its_input_serial() {
    let outbox = outbox();
    let mut publication = bound(&outbox);

    assert!(publication.screen(&captured(1, 4)));
    assert!(!publication.screen(&captured(1, 4)));

    let sent = drain(&outbox, Channel(1));
    let [ServerMessage::Frame(frame)] = sent.as_slice() else {
        panic!("expected one frame, got {sent:?}");
    };
    assert_eq!(
        (frame.base, frame.input, frame.update.generation),
        (None, 4, 1)
    );
}

#[test]
fn a_hidden_channel_receives_nothing_and_revealing_presents_the_current_screen() {
    let outbox = outbox();
    let mut publication = bound(&outbox);
    publication.screen(&captured(1, 0));
    drain(&outbox, Channel(1));

    outbox.view(Channel(1), false);
    publication.view(false, Some(&captured(1, 0)));
    assert!(!publication.screen(&captured(2, 0)));
    assert!(drain(&outbox, Channel(1)).is_empty());

    outbox.view(Channel(1), true);
    publication.view(true, Some(&captured(2, 0)));
    let sent = drain(&outbox, Channel(1));
    let [ServerMessage::Frame(frame)] = sent.as_slice() else {
        panic!("revealing must present the current screen, got {sent:?}");
    };
    assert_eq!((frame.base, frame.update.generation), (Some(1), 2));
}

#[test]
fn revealing_an_unchanged_screen_sends_nothing() {
    let outbox = outbox();
    let mut publication = bound(&outbox);
    publication.screen(&captured(1, 0));
    drain(&outbox, Channel(1));

    outbox.view(Channel(1), false);
    publication.view(false, Some(&captured(1, 0)));
    outbox.view(Channel(1), true);
    publication.view(true, Some(&captured(1, 0)));

    assert!(drain(&outbox, Channel(1)).is_empty());
}

#[test]
fn ending_presents_the_final_screen_before_the_last_event() {
    let outbox = outbox();
    let mut publication = bound(&outbox);
    publication.screen(&captured(1, 0));

    publication.end(Some(&captured(2, 3)), PaneEvent::Ended);
    publication.event(PaneEvent::HiddenInput(true));

    // The final screen replaced the one not yet written.
    let sent = drain(&outbox, Channel(1));
    let [ServerMessage::Frame(last), ServerMessage::Pane(ended)] = sent.as_slice() else {
        panic!("expected the final frame, then the end, got {sent:?}");
    };
    assert_eq!((last.update.generation, last.input), (2, 3));
    assert!(matches!(ended.event, PaneEvent::Ended));
    assert!(!publication.is_bound());
}

#[test]
fn hidden_input_is_sent_only_when_it_changes() {
    let outbox = outbox();
    let mut publication = bound(&outbox);

    publication.hidden_input(false);
    publication.hidden_input(false);
    publication.hidden_input(true);

    let events: Vec<_> = drain(&outbox, Channel(1))
        .into_iter()
        .map(|message| match message {
            ServerMessage::Pane(pane) => pane.event,
            other => panic!("expected Pane events, got {other:?}"),
        })
        .collect();
    assert!(matches!(
        events.as_slice(),
        [PaneEvent::HiddenInput(false), PaneEvent::HiddenInput(true)]
    ));
}

#[test]
fn a_released_channel_receives_nothing() {
    let outbox = outbox();
    let mut publication = bound(&outbox);
    publication.unbind();
    outbox.close(Channel(1));

    assert!(!publication.screen(&captured(1, 0)));
    publication.event(PaneEvent::HiddenInput(true));
    assert!(drain(&outbox, Channel(1)).is_empty());
}

#[test]
fn a_closing_connection_stops_publication() {
    let outbox = outbox();
    let mut publication = bound(&outbox);
    outbox.goodbye(Fatal::Stopping).unwrap();

    assert!(!publication.screen(&captured(1, 0)));
    assert!(!publication.is_bound());
}

#[test]
fn a_waiting_publisher_wakes_once_for_notifications_before_the_wait() {
    let signal = PublishSignal::default();
    let notifier = signal.notifier();
    notifier();
    notifier();
    let started = Instant::now();
    signal.wait(Some(started + Duration::from_secs(5)));
    assert!(started.elapsed() < Duration::from_secs(1));

    let started = Instant::now();
    signal.wait(Some(started + Duration::from_millis(20)));
    assert!(started.elapsed() >= Duration::from_millis(20));
}

#[test]
fn the_publisher_runs_at_its_deadline_and_on_notification_until_stopped() {
    let signal = PublishSignal::default();
    let running = Arc::new(AtomicBool::new(true));
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let publisher = {
        let (signal, running, runs) = (signal.clone(), Arc::clone(&running), Arc::clone(&runs));
        std::thread::spawn(move || {
            run_publisher(&signal, &running, |now| {
                // The first run asks to run again soon; later runs wait for a notification.
                (runs.fetch_add(1, Ordering::AcqRel) == 0).then(|| now + Duration::from_millis(5))
            })
        })
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while runs.load(Ordering::Acquire) < 2 {
        assert!(Instant::now() < deadline, "the deadline run did not happen");
        std::thread::yield_now();
    }
    signal.notify();
    while runs.load(Ordering::Acquire) < 3 {
        assert!(
            Instant::now() < deadline,
            "the notification did not run the publisher"
        );
        std::thread::yield_now();
    }
    running.store(false, Ordering::Release);
    signal.notify();
    publisher.join().unwrap();
}

/// A real shell's echo reaches the controlling channel as a pushed frame. The publisher only
/// wakes for the Terminal Session's notifications; nothing polls.
#[test]
fn a_shell_echo_is_pushed_with_the_input_serial_that_caused_it() {
    let mut host = super::super::tests::host();
    let lease = super::super::tests::attach(&mut host, 1, true);
    let signal = PublishSignal::default();
    let outbox = outbox();
    let encoded = Arc::new(Mutex::new(Vec::new()));
    {
        let pane = host.pane.as_mut().unwrap();
        pane.subscription = pane.runtime.subscribe_notifying(Some(signal.notifier()));
        pane.publication
            .bind(
                outbox.clone(),
                Channel(1),
                Box::new(Recording {
                    encoded: Arc::clone(&encoded),
                    baseline: None,
                }),
            )
            .unwrap();
    }
    assert!(matches!(
        super::super::tests::command(&mut host, 1, lease, "echo pushed"),
        Reply::Unchanged
    ));
    host.pane.as_ref().unwrap().runtime.note_input_serial(2);

    let frame = await_row(&mut host, &signal, &outbox, &encoded, "pushed");
    assert_eq!(frame.input, 2);

    // A hidden Pane pushes nothing; revealing it pushes the screen it missed.
    outbox.view(Channel(1), false);
    host.pane.as_mut().unwrap().view(false, 120);
    assert!(matches!(
        super::super::tests::command(&mut host, 1, lease, "echo hidden"),
        Reply::Unchanged
    ));
    let quiet = Instant::now() + Duration::from_millis(300);
    while Instant::now() < quiet {
        signal.wait(Some(quiet));
        host.pane.as_mut().unwrap().publish();
        assert!(drain(&outbox, Channel(1)).is_empty());
    }
    outbox.view(Channel(1), true);
    host.pane.as_mut().unwrap().view(true, 120);
    await_row(&mut host, &signal, &outbox, &encoded, "hidden");
}

/// Publishes after each notification until a pushed frame shows a row equal to `text`. Fails
/// if no notification arrives for 10 seconds.
fn await_row(
    host: &mut DevelopmentPaneHost,
    signal: &PublishSignal,
    outbox: &Outbox,
    encoded: &Mutex<Vec<Captured>>,
    text: &str,
) -> Box<spaceterm_server::protocol::Frame> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        signal.wait(Some(deadline));
        assert!(
            Instant::now() < deadline,
            "the session never notified a screen showing {text}"
        );
        assert_eq!(host.pane.as_mut().unwrap().publish(), Lifecycle::Live);
        let frames: Vec<_> = drain(outbox, Channel(1))
            .into_iter()
            .filter_map(|message| match message {
                ServerMessage::Frame(frame) => Some(frame),
                _ => None,
            })
            .collect();
        let encoded = std::mem::take(&mut *encoded.lock().unwrap());
        assert_eq!(frames.len(), encoded.len());
        let shown = frames.into_iter().zip(encoded).find(|(_, captured)| {
            encode_screen(&captured.screen).rows.iter().any(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .trim_end()
                    == text
            })
        });
        if let Some((frame, _)) = shown {
            return frame;
        }
    }
}
