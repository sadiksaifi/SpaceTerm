//! Terminal input through the protocol, on a real headless PTY.
use super::dispatch::Served;
use super::tests::{create_workspace, host, size, workspace_inventory};
use super::*;
use spaceterm_server::attachments::Attachment;
use spaceterm_server::connection::outbox::Outbox;
use spaceterm_server::connection::service::{Dispatch, PaneService as _, Peer};
use spaceterm_server::protocol::{
    self, Channel, ClaimReply, InputEvent, InputOutcome, Operation, PaneEvent, ReplyBody,
    RequestId, ServerMessage, frame::Assembler, screen::ScreenUpdate,
};
use std::time::{Duration, Instant};

const PEER: Peer = Peer {
    client: [1; 32],
    connection: [1; 32],
    pairing: None,
};
const CHANNEL: Channel = Channel(1);

fn reply(served: &mut Served<WorkspaceHost>, operation: Operation) -> ReplyBody {
    match served.request(&PEER, RequestId(1), operation) {
        Dispatch::Reply(result) => (*result).expect("the operation succeeds"),
        _ => panic!("the operation replies at once"),
    }
}

fn text(text: &str) -> InputEvent {
    let InteractiveInput::Key(key) = super::tests::text_key(text, false) else {
        unreachable!()
    };
    InputEvent::Key(key)
}

fn send(served: &mut Served<WorkspaceHost>, serial: u32, events: Vec<InputEvent>) {
    served.input(
        &PEER,
        protocol::Input {
            channel: CHANNEL,
            serial,
            generation: 1,
            events,
        },
    );
}

/// What the client received on its channel: the screen's rows and the Input outcomes.
#[derive(Default)]
struct Received {
    assembler: Assembler,
    rows: Vec<String>,
    outcomes: Vec<(u32, InputOutcome)>,
}

impl Received {
    /// Applies every message the outbox holds and acknowledges each frame.
    fn drain(&mut self, outbox: &Outbox) {
        while let Some((frame, _)) = outbox.next_frame() {
            let frame = protocol::RawFrame::parse(&frame).unwrap();
            match self.assembler.server(frame).unwrap() {
                Some(ServerMessage::Frame(frame)) => {
                    self.apply(&frame.update, frame.base.is_none());
                    outbox.acknowledge(frame.channel, frame.frame).unwrap();
                }
                Some(ServerMessage::Pane(message)) => {
                    if let PaneEvent::Input { serial, outcome } = message.event {
                        self.outcomes.push((serial, outcome));
                    }
                }
                _ => {}
            }
        }
    }

    /// Applies a keyframe, or a delta's `shift` and then its `rows`.
    fn apply(&mut self, update: &ScreenUpdate, keyframe: bool) {
        if let Some(geometry) = update.geometry
            && (keyframe || self.rows.len() != usize::from(geometry.rows))
        {
            self.rows = vec![String::new(); usize::from(geometry.rows)];
        }
        if let Some(shift) = update.shift {
            shift.apply(&mut self.rows, String::new).unwrap();
        }
        for row in &update.rows {
            self.rows[usize::from(row.index)] =
                row.runs.iter().map(|run| run.text.as_str()).collect();
        }
    }
}

/// Publishes until a row of the received screen ends with `line`. Output can follow a prompt on
/// its row when input arrives before the shell prints that prompt, as dash does.
fn await_line(
    served: &mut Served<WorkspaceHost>,
    outbox: &Outbox,
    received: &mut Received,
    line: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        // As the connection's I/O thread does when a Terminal Session asks it to publish.
        if outbox.take_publication() {
            served.publish(&PEER);
        }
        received.drain(outbox);
        if received
            .rows
            .iter()
            .any(|row| row.trim_end().ends_with(line))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected {line:?}; screen: {:?}",
            received.rows
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Text input made of control characters reaches the PTY as the keys that type them, and text
/// with a control character no key produces is refused while the Terminal Session keeps running.
#[test]
fn control_characters_in_text_input_reach_the_pty_and_unknown_ones_are_refused() {
    let mut binding = WorkspaceHost::new(host());
    let workspace = create_workspace(&mut binding, 1);
    let inventory = workspace_inventory(&mut binding);
    let pane = inventory
        .workspaces
        .iter()
        .find(|candidate| candidate.id == workspace)
        .and_then(|workspace| workspace.tabs.first())
        .and_then(|tab| tab.panes.first())
        .unwrap()
        .id;
    let mut served = Served::new(binding);
    // Terminal Sessions ask the connection to publish.
    served.ticker(Arc::new(|| {}));
    let outbox = Outbox::new(Arc::new(|| {}));
    served.admitted(&PEER, "Test".into(), outbox.clone());
    let claim = reply(
        &mut served,
        Operation::Claim(protocol::Claim {
            channel: CHANNEL,
            attachment: Attachment {
                workspace,
                generation: 1,
            },
            pane,
            claim: AttachmentClaim::Select { explicit: true },
            size: Some(size()),
            style: None,
        }),
    );
    assert!(matches!(
        claim,
        ReplyBody::Claim(ClaimReply::Attached { .. })
    ));
    // A channel starts hidden; showing it pushes the current screen.
    outbox.view(CHANNEL, true);
    served.view(
        &PEER,
        protocol::View {
            channel: CHANNEL,
            visible: true,
            max_hz: 60,
        },
    );
    send(
        &mut served,
        1,
        vec![InputEvent::Focus(FocusTarget::Terminal)],
    );
    let mut received = Received::default();
    // One message with printable text, a carriage return and more text: both lines run.
    send(
        &mut served,
        2,
        vec![
            text("printf 'CR-%s\\n' one\rprintf 'CR-%s\\n' two"),
            text("\r"),
        ],
    );
    await_line(&mut served, &outbox, &mut received, "CR-one");
    await_line(&mut served, &outbox, &mut received, "CR-two");
    // FS (0x1c) has no key. The message is refused as a whole and nothing reaches the PTY.
    send(
        &mut served,
        3,
        vec![text("printf 'FS-%s\\n' typed\u{1c}\r")],
    );
    send(&mut served, 4, vec![text("printf 'CR-%s\\n' three\r")]);
    await_line(&mut served, &outbox, &mut received, "CR-three");
    assert_eq!(
        received.outcomes,
        [(3, InputOutcome::Discarded(Refusal::Unsupported))],
        "only the refused message is reported"
    );
    served.shutdown().unwrap();
}
