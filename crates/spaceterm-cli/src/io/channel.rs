//! One channel on a link, as a terminal command uses it: a claim or an observation, the screen
//! its frames build, and the events it receives.
//!
//! Frames are applied and acknowledged as they arrive. Clipboard and Permission Setup requests
//! are denied, a Bell is ignored, and Command Finished is consumed, so a command never acts on a
//! terminal's effects.
use super::{Deadline, link_failure};
use crate::contract::{Failure, Format};
use spaceterm_server::connection::link::{
    ChannelEvent, ChannelHandle, ChannelSink, LinkError, PendingReply, ServerLink,
};
use spaceterm_server::pane::{Color, effects};
use spaceterm_server::protocol::screen::{
    History, Row, ScreenUpdate, Style, StyleDefinition, StyleRef, flags,
};
use spaceterm_server::protocol::{PaneEvent, RunReceipt};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

/// The frame rate a command declares: the lowest a client may.
const FRAME_RATE: u8 = spaceterm_server::protocol::limits::FRAME_RATES[0];

/// Events the link delivered, waiting for the command's thread.
#[derive(Default)]
struct Inbox {
    events: Mutex<VecDeque<ChannelEvent>>,
    ready: Condvar,
}

impl ChannelSink for Inbox {
    fn deliver(&self, event: ChannelEvent) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(event);
        self.ready.notify_all();
    }
}

/// What one received event changed.
#[derive(Debug)]
pub(crate) enum Event {
    /// A frame was applied.
    Frame,
    /// Progress arrived without a frame.
    Progress,
    Run(RunReceipt),
    Input {
        serial: u32,
        outcome: spaceterm_server::protocol::InputOutcome,
    },
    /// The channel's last event.
    Ended(PaneEvent),
    Closed(LinkError),
}

/// The visible screen as the channel's frames built it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Screen {
    pub(crate) rows: Vec<Row>,
    styles: BTreeMap<u16, Style>,
    pub(crate) history: History,
    pub(crate) columns: u16,
    pub(crate) cursor: Option<(u16, u16)>,
    pub(crate) alternate: bool,
    /// Whether a keyframe arrived.
    pub(crate) keyed: bool,
}

impl Screen {
    fn apply(&mut self, update: ScreenUpdate, keyframe: bool) {
        if keyframe {
            self.styles.clear();
            self.rows.clear();
        }
        if let Some(geometry) = update.geometry {
            self.columns = geometry.columns;
            self.rows.resize_with(usize::from(geometry.rows), blank);
        }
        if let Some(modes) = update.modes {
            self.alternate = modes.alternate_screen;
        }
        if let Some(shift) = update.shift {
            let _ = shift.apply(&mut self.rows, blank);
        }
        for definition in update.styles {
            self.styles.insert(definition.id, definition.style);
        }
        for row in update.rows {
            let index = usize::from(row.index);
            if let Some(slot) = self.rows.get_mut(index) {
                *slot = row;
            }
        }
        self.history = update.history;
        self.cursor = update
            .cursor
            .position
            .map(|position| (position.row, position.column));
        self.keyed |= keyframe;
    }

    /// The visible rows, rendered. Text mode trims trailing spaces.
    pub(crate) fn render(&self, format: Format) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| render(row, &self.styles, format))
            .collect()
    }
}

fn blank() -> Row {
    Row {
        index: 0,
        wrapped: false,
        runs: Vec::new(),
    }
}

/// A page's own style table.
pub(crate) fn page_styles(definitions: &[StyleDefinition]) -> BTreeMap<u16, Style> {
    definitions
        .iter()
        .map(|definition| (definition.id, definition.style))
        .collect()
}

/// One row as text. `ansi` adds only synthesized SGR, never what the program wrote.
pub(crate) fn render(row: &Row, styles: &BTreeMap<u16, Style>, format: Format) -> String {
    let mut text = String::new();
    let mut styled = false;
    for run in &row.runs {
        if format == Format::Ansi {
            let style = match run.style {
                StyleRef::Default => None,
                StyleRef::Id(id) => styles.get(&id).copied(),
                StyleRef::Inline(style) => Some(style),
            };
            match style.map(sgr).filter(|codes| !codes.is_empty()) {
                Some(codes) => {
                    text.push_str(&format!("\u{1b}[0;{codes}m"));
                    styled = true;
                }
                None if styled => {
                    text.push_str("\u{1b}[0m");
                    styled = false;
                }
                None => {}
            }
        }
        text.push_str(&run.text);
    }
    if styled {
        text.push_str("\u{1b}[0m");
    }
    if format == Format::Text {
        text.truncate(text.trim_end_matches(' ').len());
    }
    text
}

/// The SGR parameters of a style, without the leading reset.
fn sgr(style: Style) -> String {
    let mut codes = Vec::new();
    for (flag, code) in [
        (flags::BOLD, "1"),
        (flags::FAINT, "2"),
        (flags::ITALIC, "3"),
        (flags::BLINKING, "5"),
        (flags::INVERSE, "7"),
        (flags::INVISIBLE, "8"),
        (flags::STRIKETHROUGH, "9"),
        (flags::OVERLINE, "53"),
    ] {
        if style.flags & flag != 0 {
            codes.push(code.to_owned());
        }
    }
    if style.underline != 0 {
        codes.push("4".to_owned());
    }
    for (color, base) in [(style.foreground, 38), (style.background, 48)] {
        match color {
            Color::Default => {}
            Color::Palette(index) => codes.push(format!("{base};5;{index}")),
            Color::Rgb([red, green, blue]) => codes.push(format!("{base};2;{red};{green};{blue}")),
        }
    }
    codes.join(";")
}

/// One channel and what it received.
pub(crate) struct Channel {
    link: ServerLink,
    handle: Option<ChannelHandle>,
    inbox: Arc<Inbox>,
    pub(crate) screen: Screen,
    /// The highest Input serial a frame or progress reported.
    pub(crate) input: u32,
    /// The PTY output count progress last reported.
    pub(crate) output_bytes: Option<u64>,
    /// When the Terminal Session last showed output: a frame or new output bytes.
    pub(crate) activity: Instant,
}

impl Channel {
    pub(crate) fn open(link: &ServerLink) -> Result<Self, Failure> {
        let inbox = Arc::new(Inbox::default());
        let handle = link
            .open_channel(Arc::clone(&inbox) as Arc<dyn ChannelSink>)
            .map_err(link_failure)?;
        Ok(Self {
            link: link.clone(),
            handle: Some(handle),
            inbox,
            screen: Screen::default(),
            input: 0,
            output_bytes: None,
            activity: Instant::now(),
        })
    }

    pub(crate) fn handle(&self) -> &ChannelHandle {
        self.handle.as_ref().expect("the channel is open")
    }

    pub(crate) fn link(&self) -> &ServerLink {
        &self.link
    }

    pub(crate) fn number(&self) -> spaceterm_server::protocol::Channel {
        self.handle().channel()
    }

    pub(crate) fn observe(
        &self,
        pane: [u8; 32],
        session: Option<[u8; 32]>,
    ) -> Result<PendingReply, Failure> {
        self.handle().observe(pane, session).map_err(link_failure)
    }

    /// Shows the channel, so its Terminal Session publishes frames to it.
    pub(crate) fn show(&self) -> Result<(), Failure> {
        self.handle().view(true, FRAME_RATE).map_err(link_failure)
    }

    /// Keeps the claim's control when the command ends: the process exits without releasing.
    pub(crate) fn keep(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.detach();
        }
    }

    /// Receives the next event until `deadline`. `None` means the deadline passed.
    pub(crate) fn next(&mut self, deadline: Deadline) -> Result<Option<Event>, Failure> {
        let event = {
            let mut events = self.inbox.events.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(event) = events.pop_front() {
                    break event;
                }
                let Some(slice) = deadline.slice() else {
                    return Ok(None);
                };
                events = self
                    .inbox
                    .ready
                    .wait_timeout(events, slice)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        };
        Ok(Some(match event {
            ChannelEvent::Frame(frame) => {
                let keyframe = frame.base.is_none();
                let number = frame.frame;
                self.input = self.input.max(frame.input);
                self.screen.apply(frame.update, keyframe);
                self.activity = Instant::now();
                // A lost acknowledgement only pauses frames; the link reports its end.
                let _ = self.handle().acknowledge(number);
                Event::Frame
            }
            ChannelEvent::Pane(PaneEvent::Effect(request)) => {
                self.complete(&request);
                return self.next(deadline);
            }
            ChannelEvent::Pane(PaneEvent::Progress {
                input,
                output_bytes,
            }) => {
                self.input = self.input.max(input);
                if self.output_bytes != Some(output_bytes) {
                    if self.output_bytes.is_some() {
                        self.activity = Instant::now();
                    }
                    self.output_bytes = Some(output_bytes);
                }
                Event::Progress
            }
            ChannelEvent::Pane(PaneEvent::RunFinished(receipt)) => Event::Run(*receipt),
            ChannelEvent::Pane(PaneEvent::Input { serial, outcome }) => {
                Event::Input { serial, outcome }
            }
            ChannelEvent::Pane(event) if event.last() => Event::Ended(event),
            ChannelEvent::Pane(_) => return self.next(deadline),
            ChannelEvent::Closed(error) => Event::Closed(error),
        }))
    }

    /// Denies what would act on this machine for the terminal, and consumes the rest.
    fn complete(&self, request: &effects::Request) {
        if matches!(
            request.effect,
            effects::Effect::ClipboardRead { .. }
                | effects::Effect::ClipboardWrite { .. }
                | effects::Effect::PermissionSetup { .. }
        ) {
            let _ = self.handle().effect_done(
                request.generation,
                request.id,
                effects::Completion::Denied,
            );
        }
    }

    /// Receives events until a keyframe built the screen.
    pub(crate) fn await_screen(&mut self, deadline: Deadline) -> Result<(), Failure> {
        while !self.screen.keyed {
            match self.next(deadline)? {
                None => return Err(deadline.timeout()),
                Some(Event::Ended(event)) => return Err(ended(&event)),
                Some(Event::Closed(error)) => return Err(link_failure(error)),
                Some(_) => {}
            }
        }
        Ok(())
    }
}

impl Drop for Channel {
    /// An observation or a claim that is not kept ends with the command.
    fn drop(&mut self) {
        drop(self.handle.take());
    }
}

/// The failure a channel's last event means.
pub(crate) fn ended(event: &PaneEvent) -> Failure {
    use crate::contract::{Code, ControlledReason, SessionStateName};
    match event {
        PaneEvent::Takeover(_) => super::controlled(ControlledReason::Other),
        PaneEvent::DirectoryUnavailable => Failure::new(Code::DirectoryUnavailable),
        PaneEvent::Failed => Failure::new(Code::StartupFailed),
        _ => super::session_failure(Code::SessionEnded, SessionStateName::Ended),
    }
}

/// What a claim reached.
#[derive(Debug)]
pub(crate) enum Claimed {
    Attached(spaceterm_server::pane::Lease),
    Takeover(spaceterm_server::protocol::Takeover),
    Restored(Box<spaceterm_server::protocol::Restored>),
}

/// The cell size of a Session a command sizes. Commands read text, so any size serves.
const CELL: (u16, u16) = (8, 16);

/// A `--size` as a Pane size.
pub(crate) fn pane_size(size: crate::args::TerminalSize) -> spaceterm_server::pane::PaneSize {
    spaceterm_server::pane::PaneSize {
        columns: size.columns,
        rows: size.rows,
        cell_width: CELL.0,
        cell_height: CELL.1,
    }
}

/// A new attachment of `workspace`. Its generation grows with the clock, so each command's
/// claim follows the last one this identity made.
fn attachment(workspace: [u8; 32]) -> spaceterm_server::attachments::Attachment {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    spaceterm_server::attachments::Attachment {
        workspace,
        generation: u64::try_from(now.as_nanos()).unwrap_or(u64::MAX).max(1),
    }
}

impl Channel {
    /// Claims the target Pane on this channel. A `size` of `None` keeps the running size.
    pub(crate) fn claim(
        &self,
        target: &super::PaneTarget,
        claim: spaceterm_server::pane::AttachmentClaim,
        size: Option<spaceterm_server::pane::PaneSize>,
        deadline: Deadline,
    ) -> Result<Claimed, Failure> {
        use spaceterm_server::protocol::{ClaimReply, ReplyBody};
        let pending = self
            .handle()
            .claim(attachment(target.workspace), target.pane, claim, size, None)
            .map_err(link_failure)?;
        match super::wait(pending, deadline)? {
            ReplyBody::Claim(ClaimReply::Attached { lease }) => Ok(Claimed::Attached(lease)),
            ReplyBody::Claim(ClaimReply::Takeover(takeover)) => Ok(Claimed::Takeover(takeover)),
            ReplyBody::Claim(ClaimReply::Restored(restored)) => Ok(Claimed::Restored(restored)),
            _ => Err(Failure::new(crate::contract::Code::ProtocolError)),
        }
    }
}

/// Whether this identity holds the grant a Take Over offer names, through an earlier
/// attachment. Resuming it takes nothing from anyone.
pub(crate) fn own_grant(takeover: &spaceterm_server::protocol::Takeover) -> bool {
    matches!(
        takeover.controller,
        spaceterm_server::pane::control::Controller::Client {
            this_client: true,
            ..
        }
    )
}
