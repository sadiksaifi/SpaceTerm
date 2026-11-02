//! Presentation client for `mise run bench:server`.
//!
//! server_bench IDENTITY [--accessibility] [--columns N] [--rows N] [--max-hz N]
//!
//! The client creates a Workspace, claims its Pane at a fixed size, presents it and acknowledges
//! every pushed frame at once, as a desktop client that keeps up does. With `--accessibility` it
//! also demands semantics and acknowledges every pushed semantic revision at its tail, as a
//! desktop client does while an assistive application reads the Pane. It never applies what it
//! receives.
//!
//! When the Pane is attached, it prints one JSON line with the Workspace and Pane ids. Then it
//! reads one command per line on standard input:
//!
//! - `type TEXT` types TEXT and Enter into the Pane.
//! - `stats` prints one JSON line with the messages received so far and every byte the server
//!   wrote on this connection.
//!
//! When its input ends, it closes the Workspace and exits. It prints no terminal contents.
use spaceterm::runtime::PhysicalKey;
use spaceterm_server::ServerConnection;
use spaceterm_server::attachments::Attachment;
use spaceterm_server::connection::link::{ChannelEvent, ChannelHandle, LinkEvent, ServerLink};
use spaceterm_server::hierarchy::{CloseTarget, CreateControl, Id, Mutation, Outcome};
use spaceterm_server::pane::accessibility::Demand;
use spaceterm_server::pane::{AttachmentClaim, FocusTarget, Key, PaneSize};
use spaceterm_server::protocol::{
    ClaimReply, InputEvent, InputOutcome, Operation, PaneEvent, ReplyBody, limits,
};
use std::io::{BufRead, Write};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Duration;

/// A failed stage. Server errors are typed classifications without terminal contents or paths.
#[derive(Debug)]
struct Failure(String);

type Result<T> = std::result::Result<T, Failure>;

fn fail<T>(stage: &str) -> Result<T> {
    Err(Failure(stage.to_owned()))
}

fn at<T, E: std::fmt::Debug>(stage: &str, result: std::result::Result<T, E>) -> Result<T> {
    result.map_err(|error| Failure(format!("{stage}: {error:?}")))
}

/// Every claim names attachment generation 1. The client releases no attachment, so the server
/// never holds a newer generation for its Workspace.
const GENERATION: u64 = 1;

struct Options {
    identity: String,
    accessibility: bool,
    columns: u16,
    rows: u16,
    max_hz: u8,
}

fn options(arguments: &[String]) -> Result<Options> {
    let [identity, flags @ ..] = arguments else {
        return fail(
            "usage: server_bench IDENTITY [--accessibility] [--columns N] [--rows N] [--max-hz N]",
        );
    };
    let mut options = Options {
        identity: identity.clone(),
        accessibility: false,
        columns: 120,
        rows: 40,
        max_hz: limits::FRAME_RATES[0],
    };
    let mut flags = flags.iter();
    while let Some(flag) = flags.next() {
        if flag == "--accessibility" {
            options.accessibility = true;
            continue;
        }
        let value = flags.next().map(String::as_str);
        match (flag.as_str(), value.map(str::parse::<u16>)) {
            ("--columns", Some(Ok(columns))) if columns > 0 => options.columns = columns,
            ("--rows", Some(Ok(rows))) if rows > 0 => options.rows = rows,
            ("--max-hz", Some(Ok(hz)))
                if limits::FRAME_RATES.iter().any(|r| u16::from(*r) == hz) =>
            {
                options.max_hz = hz as u8;
            }
            _ => return fail("unknown option or invalid value"),
        }
    }
    Ok(options)
}

/// What reaches the main loop: pushed channel events and commands from standard input.
enum Message {
    Channel(ChannelEvent),
    Type(String),
    Stats,
    End,
}

/// Messages received, and the semantic bytes among them. The byte count the task compares is
/// the link's: every frame the server wrote on this connection, headers included.
#[derive(Default)]
struct Received {
    frames: u64,
    events: u64,
    accessibility_messages: u64,
    accessibility_revisions: u64,
    accessibility_payload_bytes: u64,
}

impl Received {
    fn json(&self, link: &ServerLink) -> serde_json::Value {
        serde_json::json!({
            "event": "stats",
            "frames": self.frames,
            "events": self.events,
            "accessibility_messages": self.accessibility_messages,
            "accessibility_revisions": self.accessibility_revisions,
            "accessibility_payload_bytes": self.accessibility_payload_bytes,
            "bytes": link.received_bytes(),
        })
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    // A separate account-local client identity keeps the desktop's attachments and control
    // untouched. Set it before any other thread starts.
    // SAFETY: main has a single thread here.
    unsafe { std::env::set_var("SPACETERM_DEVELOPMENT_CLIENT", "server-bench") };
    if let Err(Failure(stage)) = options(&arguments).and_then(|options| run(&options)) {
        eprintln!("server_bench: {stage}");
        std::process::exit(1);
    }
}

fn run(options: &Options) -> Result<()> {
    let server = at("connect", ServerConnection::for_identity(&options.identity))?;
    let link = at("connect", connect(&server))?;
    let (inventory, sequence) = inventory(&link)?;
    let Some(boot) = inventory.close.as_ref().map(|close| close.boot) else {
        return fail("server boot");
    };
    let mutation = Mutation::CreateWorkspace {
        name: format!("Server bench {}", std::process::id()),
        directory: None,
        control: CreateControl::Creator,
    };
    let Outcome::Accepted { target: workspace } = mutate(&link, sequence, mutation)? else {
        return fail("Workspace creation refused");
    };
    let (sender, messages) = mpsc::channel();
    let result = present(options, &link, workspace, &sender, &messages);
    // A fresh connection closes the Workspace even when the presentation's link failed.
    let closed = close(&server, boot, workspace);
    result.and(closed)
}

fn present(
    options: &Options,
    link: &ServerLink,
    workspace: Id,
    sender: &Sender<Message>,
    messages: &mpsc::Receiver<Message>,
) -> Result<()> {
    let (inventory, _) = inventory(link)?;
    let Some(pane) = inventory
        .workspaces
        .iter()
        .find(|candidate| candidate.id == workspace)
        .and_then(|workspace| workspace.tabs.first())
        .map(|tab| tab.root_pane)
    else {
        return fail("bench Pane missing");
    };
    let sink = sender.clone();
    let channel = at(
        "attach",
        link.open_channel(Arc::new(move |event: ChannelEvent| {
            let _ = sink.send(Message::Channel(event));
        })),
    )?;
    let size = PaneSize {
        columns: options.columns,
        rows: options.rows,
        cell_width: 8,
        cell_height: 18,
    };
    let attachment = Attachment {
        workspace,
        generation: GENERATION,
    };
    let claim = AttachmentClaim::Select { explicit: true };
    let pending = at("attach", channel.claim(attachment, pane, claim, size, None))?;
    let ReplyBody::Claim(ClaimReply::Attached { .. }) = at("attach", pending.wait())? else {
        return fail("attach refused");
    };
    at(
        "focus",
        channel.input(GENERATION, vec![InputEvent::Focus(FocusTarget::Terminal)]),
    )?;
    at("view", channel.view(true, options.max_hz))?;
    if options.accessibility {
        let demand = Operation::AccessibilityDemand {
            channel: at("channel", channel.channel())?,
            demand: Demand::Start { generation: 1 },
        };
        let pending = at("accessibility", channel.request(demand))?;
        let ReplyBody::Done = at("accessibility", pending.wait())? else {
            return fail("accessibility demand refused");
        };
    }
    let input = sender.clone();
    std::thread::spawn(move || read_commands(&input));
    println!(
        "{}",
        serde_json::json!({
            "event": "ready",
            "workspace": hex(&workspace),
            "pane": hex(&pane),
            "accessibility": options.accessibility,
        })
    );
    flush();

    let mut received = Received::default();
    loop {
        match messages.recv_timeout(Duration::from_secs(1)) {
            Ok(Message::Channel(event)) => {
                if !deliver(&channel, event, &mut received)? {
                    return fail("the bench Pane ended");
                }
            }
            Ok(Message::Type(text)) => {
                at(
                    "input",
                    channel.input(
                        GENERATION,
                        vec![
                            InputEvent::Key(text_key(&text)),
                            InputEvent::Key(enter_key()),
                        ],
                    ),
                )?;
            }
            Ok(Message::Stats) => {
                println!("{}", received.json(link));
                flush();
            }
            Ok(Message::End) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
    channel.detach();
    Ok(())
}

/// Acknowledges a frame, and a semantic revision at its tail, at once and counts what arrived.
/// Returns false when the Pane ended.
fn deliver(channel: &ChannelHandle, event: ChannelEvent, received: &mut Received) -> Result<bool> {
    match event {
        ChannelEvent::Frame(frame) => {
            received.frames += 1;
            at("acknowledge", channel.acknowledge(frame.frame))?;
        }
        ChannelEvent::Accessibility(update) => {
            received.accessibility_messages += 1;
            received.accessibility_payload_bytes += update.encoded_len() as u64;
            if update.tail.is_some() {
                received.accessibility_revisions += 1;
                let header = update.header;
                at(
                    "acknowledge",
                    channel.acknowledge_accessibility(header.demand_generation, header.revision),
                )?;
            }
        }
        ChannelEvent::Pane(event) => {
            received.events += 1;
            match &event {
                PaneEvent::Input {
                    outcome: InputOutcome::Discarded(refusal),
                    ..
                } => return Err(Failure(format!("input discarded: {refusal:?}"))),
                event if event.last() => return Ok(false),
                _ => {}
            }
        }
        ChannelEvent::Closed(_) => return Ok(false),
    }
    Ok(true)
}

fn read_commands(sender: &Sender<Message>) {
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let message = if let Some(text) = line.strip_prefix("type ") {
            Message::Type(text.to_owned())
        } else if line == "stats" {
            Message::Stats
        } else {
            eprintln!("server_bench: unknown command");
            continue;
        };
        if sender.send(message).is_err() {
            return;
        }
    }
    let _ = sender.send(Message::End);
}

fn flush() {
    let _ = std::io::stdout().flush();
}

fn hex(id: &Id) -> String {
    id.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn connect(
    server: &ServerConnection,
) -> std::result::Result<ServerLink, spaceterm_server::ServerError> {
    server.connect(Arc::new(|_: LinkEvent| {}))
}

fn request(link: &ServerLink, stage: &str, operation: Operation) -> Result<ReplyBody> {
    let pending = at(stage, link.request(operation))?;
    at(stage, pending.wait())
}

/// The newest pushed inventory. The server pushes one when it admits the link.
fn inventory(link: &ServerLink) -> Result<(spaceterm_server::hierarchy::Inventory, u64)> {
    match link.next_inventory(0) {
        Ok((_, revision)) => Ok((revision.inventory.clone(), revision.next_sequence)),
        Err(_) => fail("inventory"),
    }
}

/// Applies one Workspace edit and retires its outcome, so retained outcomes never reach the
/// server's per-client capacity.
fn mutate(link: &ServerLink, sequence: u64, mutation: Mutation) -> Result<Outcome> {
    let reply = request(link, "mutate", Operation::Mutate { sequence, mutation })?;
    let ReplyBody::Outcome(result) = reply else {
        return fail("mutation outcome");
    };
    if result.outcome != Outcome::Pending {
        at("mutate", link.acknowledge(sequence))?;
        // The retirement leaves with everything queued before this request's reply arrives.
        request(link, "mutate", nothing())?;
    }
    Ok(result.outcome)
}

/// A request that changes nothing: it releases an attachment of a Workspace that never existed.
fn nothing() -> Operation {
    Operation::ReleaseAttachment {
        attachment: Attachment {
            workspace: [0; 32],
            generation: 0,
        },
    }
}

/// Kills the bench Workspace on a fresh connection, unless the server restarted.
fn close(server: &ServerConnection, boot: Id, workspace: Id) -> Result<()> {
    let link = at("cleanup connect", connect(server))?;
    let (inventory, sequence) = inventory(&link)?;
    if inventory.close.as_ref().map(|close| close.boot) != Some(boot) {
        return fail("the server restarted; nothing to close");
    }
    let target = CloseTarget::KillWorkspace { workspace };
    let Some(confirmation) = inventory.close_confirmation(target) else {
        return Ok(());
    };
    match mutate(
        &link,
        sequence,
        Mutation::Close {
            target,
            confirmation,
        },
    )? {
        Outcome::Accepted { .. } => Ok(()),
        _ => fail("bench Workspace close refused"),
    }
}

fn text_key(text: &str) -> Key {
    Key {
        physical: PhysicalKey::Unidentified as u32,
        action: 0,
        logical: "text-input".into(),
        text: Some(text.into()),
        unshifted: None,
        modifiers: 0,
        consumed: 0,
        option_as_alt: false,
    }
}

fn enter_key() -> Key {
    Key {
        physical: PhysicalKey::Enter as u32,
        action: 0,
        logical: "enter".into(),
        text: None,
        unshifted: None,
        modifiers: 0,
        consumed: 0,
        option_as_alt: false,
    }
}
