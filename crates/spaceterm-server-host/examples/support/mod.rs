//! The native checks' SpaceTerm Protocol client: requests on one admitted connection, and claimed
//! Panes with their input, reported input outcomes, and pushed screens and Pane events.
//!
//! The spaceterm crate's native tests also include this file, so it names physical keys by their
//! protocol codes instead of through the spaceterm crate.
use spaceterm_server::{
    ServerConnection, ServerError,
    attachments::Attachment,
    connection::link::{ChannelEvent, ChannelHandle, LinkError, LinkEvent, ServerLink},
    hierarchy::{Id, Inventory, Mutation, RetainedOutcome},
    pane::{
        AttachmentClaim, FocusTarget, Key, Lease, PaneSize, PasteOutcome, Refusal, effects,
        graphics,
    },
    protocol::{
        Channel, ClaimReply, Frame, InputEvent, InputOutcome, Operation, PaneEvent, ReplyBody,
        Restored, Takeover,
        interim::{Interim, InterimReply},
        limits::MAX_PAGE_ROWS,
        screen::{Colors, Cursor, Geometry, History, Modes},
    },
};
use std::{
    collections::{BTreeMap, VecDeque},
    error::Error,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

pub type Result<T> = std::result::Result<T, ServerError>;

/// How long a helper waits for the server to report on input, show terminal output or end a
/// channel.
const WAIT: Duration = Duration::from_secs(10);
/// The protocol's physical-key codes for Unidentified and Enter (libghostty-vt `Key`).
const UNIDENTIFIED: u32 = 0;
const ENTER: u32 = 58;
/// The attachment generation of every claim these checks make without an attachment permit. The
/// server refuses a generation older than one this client used or released for the Workspace, and
/// these checks release no attachment of such a Workspace.
pub const GENERATION: u64 = 1;

/// Opens a connection of its own, which closes when every clone of it drops.
pub fn connect(server: &ServerConnection) -> Result<ServerLink> {
    server.connect(Arc::new(|_: LinkEvent| {}))
}

pub fn request(link: &ServerLink, operation: Operation) -> Result<ReplyBody> {
    Ok(link.request(operation)?.wait()?)
}

/// Sends a request whose reply carries no value.
pub fn done(link: &ServerLink, operation: Operation) -> Result<()> {
    match request(link, operation)? {
        ReplyBody::Done => Ok(()),
        _ => Err(ServerError::Protocol),
    }
}

/// Sends a polled operation that awaits its push replacement.
pub fn interim(link: &ServerLink, operation: Interim) -> Result<InterimReply> {
    match request(link, Operation::Interim(operation))? {
        ReplyBody::Interim(reply) => Ok(reply),
        _ => Err(ServerError::Protocol),
    }
}

/// The Workspace inventory and the sequence of the next Workspace edit.
pub fn inventory(link: &ServerLink) -> Result<(Inventory, u64)> {
    match interim(link, Interim::Inventory)? {
        InterimReply::Inventory {
            inventory,
            next_sequence,
        } => Ok((inventory, next_sequence)),
        _ => Err(ServerError::Protocol),
    }
}

/// An operation that changes nothing: it releases an attachment of a Workspace that never
/// existed, which the server answers before it reads any attachment state.
pub fn no_op() -> Operation {
    Operation::ReleaseAttachment {
        attachment: Attachment {
            workspace: [0; 32],
            generation: 0,
        },
    }
}

/// One request that changes nothing; see [`no_op`].
pub fn round_trip(link: &ServerLink) -> Result<()> {
    done(link, no_op())
}

/// Submits a Workspace edit. `None` means its outcome is still pending; `resolve` settles it.
pub fn mutate(
    link: &ServerLink,
    sequence: u64,
    mutation: Mutation,
) -> Result<Option<RetainedOutcome>> {
    outcome(request(link, Operation::Mutate { sequence, mutation })?)
}

/// The retained outcome of the edit `sequence`, or `None` while it is pending.
pub fn resolve(link: &ServerLink, sequence: u64) -> Result<Option<RetainedOutcome>> {
    outcome(request(link, Operation::Resolve { sequence })?)
}

fn outcome(body: ReplyBody) -> Result<Option<RetainedOutcome>> {
    match body {
        ReplyBody::Outcome(outcome) => Ok(Some(outcome)),
        ReplyBody::Interim(InterimReply::MutationPending) => Ok(None),
        _ => Err(ServerError::Protocol),
    }
}

/// The Workspace that holds `pane`.
pub fn workspace_of(inventory: &Inventory, pane: Id) -> Option<Id> {
    inventory
        .workspaces
        .iter()
        .find(|workspace| {
            workspace
                .tabs
                .iter()
                .any(|tab| tab.panes.iter().any(|candidate| candidate.id == pane))
        })
        .map(|workspace| workspace.id)
}

/// The first Pane of the inventory, as the single-Pane `Attach` and `Select` requests of protocol
/// major 1 chose it. With `create`, an empty inventory first gains a Workspace, as the server's
/// own initial organization does. Without it, an empty inventory has no Pane to claim.
pub fn first_pane(link: &ServerLink, create: bool) -> Result<Id> {
    let (inventory, sequence) = inventory(link)?;
    let first = |inventory: &Inventory| {
        inventory
            .workspaces
            .first()
            .and_then(|workspace| workspace.tabs.first())
            .and_then(|tab| tab.panes.first())
            .map(|pane| pane.id)
    };
    if let Some(pane) = first(&inventory) {
        return Ok(pane);
    }
    if !create {
        return Err(ServerError::SessionUnavailable);
    }
    let mutation = Mutation::CreateWorkspace {
        name: "Workspace".into(),
        directory: None,
    };
    let mut outcome = mutate(link, sequence, mutation)?;
    let deadline = Instant::now() + WAIT;
    while outcome.is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        outcome = resolve(link, sequence)?;
    }
    if outcome.is_none() {
        return Err(ServerError::Protocol);
    }
    link.acknowledge(sequence)?;
    first(&self::inventory(link)?.0).ok_or(ServerError::SessionUnavailable)
}

/// The reply to a claim. Only `Attached` binds a channel.
pub enum Claimed {
    Attached(Pane),
    Takeover(Takeover),
    Restored(Box<Restored>),
}

impl Claimed {
    pub fn attached(self) -> Option<Pane> {
        match self {
            Self::Attached(pane) => Some(pane),
            _ => None,
        }
    }
}

/// Claims `pane` under `attachment` on a new channel of `link`. Each claim needs its own channel.
pub fn claim_in(
    link: &ServerLink,
    attachment: Attachment,
    pane: Id,
    claim: AttachmentClaim,
    size: PaneSize,
) -> Result<Claimed> {
    let events = Arc::new(Events::default());
    let sink = Arc::clone(&events);
    let channel = link.open_channel(Arc::new(move |event: ChannelEvent| sink.deliver(event)))?;
    let reply = channel
        .claim(attachment, pane, claim, size, None)
        .map_err(ServerError::from)
        .and_then(|pending| Ok(pending.wait()?));
    match reply {
        Ok(ReplyBody::Claim(ClaimReply::Attached { lease })) => Ok(Claimed::Attached(Pane {
            link: link.clone(),
            id: channel.channel(),
            channel: Some(channel),
            lease,
            events,
        })),
        // An unbound channel has nothing to release.
        Ok(ReplyBody::Claim(ClaimReply::Takeover(takeover))) => {
            channel.detach();
            Ok(Claimed::Takeover(takeover))
        }
        Ok(ReplyBody::Claim(ClaimReply::Restored(restored))) => {
            channel.detach();
            Ok(Claimed::Restored(restored))
        }
        Ok(_) => {
            channel.detach();
            Err(ServerError::Protocol)
        }
        Err(error) => {
            channel.detach();
            Err(error)
        }
    }
}

/// Claims `pane` under generation [`GENERATION`] of the Workspace that holds it. A Pane missing
/// from the inventory is unavailable, as the server reports it.
pub fn claim(
    link: &ServerLink,
    pane: Id,
    claim: AttachmentClaim,
    size: PaneSize,
) -> Result<Claimed> {
    let workspace =
        workspace_of(&inventory(link)?.0, pane).ok_or(ServerError::SessionUnavailable)?;
    let attachment = Attachment {
        workspace,
        generation: GENERATION,
    };
    claim_in(link, attachment, pane, claim, size)
}

/// Selects `pane`. Only an explicit selection may start or acquire work.
pub fn select(link: &ServerLink, pane: Id, explicit: bool, size: PaneSize) -> Result<Claimed> {
    claim(link, pane, AttachmentClaim::Select { explicit }, size)
}

/// Starts the shell of the inventory's first Pane, or resumes its control, as the single-Pane
/// `Attach` request of protocol major 1 did. That request refused a Takeover Screen as controlled.
pub fn attach(link: &ServerLink, create: bool, size: PaneSize) -> Result<Pane> {
    let pane = first_pane(link, create)?;
    match select(link, pane, create, size)? {
        Claimed::Attached(pane) => Ok(pane),
        Claimed::Takeover(_) => Err(ServerError::Controlled),
        Claimed::Restored(_) => Err(ServerError::Protocol),
    }
}

pub fn text_key(text: &str) -> Key {
    Key {
        physical: UNIDENTIFIED,
        action: 0,
        logical: "text-input".into(),
        text: Some(text.into()),
        unshifted: None,
        modifiers: 0,
        consumed: 0,
        option_as_alt: false,
    }
}

pub fn enter_key() -> Key {
    Key {
        physical: ENTER,
        action: 0,
        logical: "enter".into(),
        text: None,
        unshifted: None,
        modifiers: 0,
        consumed: 0,
        option_as_alt: false,
    }
}

/// Whether a row of `screen` reads exactly `marker`, ignoring trailing blanks.
pub fn has_row(screen: &Screen, marker: &str) -> bool {
    screen.rows.iter().any(|row| row.trim_end() == marker)
}

/// A channel's screen as of the last frame applied to it: the text of each visible row and the
/// state these checks assert on.
#[derive(Clone, Debug, Default)]
pub struct Screen {
    /// The last applied frame, or `None` before the first keyframe.
    pub frame: Option<u64>,
    /// The highest Input serial the server wrote to the PTY before it captured this screen.
    pub input: u32,
    pub geometry: Option<Geometry>,
    pub modes: Option<Modes>,
    pub colors: Option<Colors>,
    pub cursor: Cursor,
    /// Fences pointer and wheel coordinates.
    pub generation: u64,
    /// The Scrollback above the visible rows.
    pub history: History,
    pub graphics: Option<std::result::Result<graphics::Snapshot, graphics::Error>>,
    /// The text of each visible row. A wide character's spacer adds nothing.
    pub rows: Vec<String>,
}

impl Screen {
    /// Applies `frame`. Fails when the frame names a base other than the last applied frame or
    /// does not fit the screen.
    fn apply(&mut self, frame: &Frame) -> Result<()> {
        let update = &frame.update;
        update.validate().map_err(|_| ServerError::Protocol)?;
        let keyframe = frame.base.is_none();
        if !keyframe && frame.base != self.frame {
            return Err(ServerError::Protocol);
        }
        let geometry = update
            .geometry
            .or(self.geometry.filter(|_| !keyframe))
            .ok_or(ServerError::Protocol)?;
        // A keyframe and a new grid size start from blank rows.
        let grid = |geometry: Geometry| (geometry.columns, geometry.rows);
        if keyframe || self.geometry.map(grid) != Some(grid(geometry)) {
            self.rows = vec![String::new(); usize::from(geometry.rows)];
        }
        if let Some(shift) = update.shift {
            shift
                .apply(&mut self.rows, String::new)
                .ok_or(ServerError::Protocol)?;
        }
        for row in &update.rows {
            *self
                .rows
                .get_mut(usize::from(row.index))
                .ok_or(ServerError::Protocol)? =
                row.runs.iter().map(|run| run.text.as_str()).collect();
        }
        self.geometry = Some(geometry);
        if let Some(modes) = update.modes {
            self.modes = Some(modes);
        }
        if let Some(colors) = &update.colors {
            self.colors = Some(colors.clone());
        }
        self.history = update.history;
        if let Some(graphics) = &update.graphics {
            self.graphics = Some(graphics.clone());
        }
        self.cursor = update.cursor;
        self.generation = update.generation;
        self.input = frame.input;
        self.frame = Some(frame.frame);
        Ok(())
    }
}

/// How a channel ended: with its last Pane event, or with the end of its connection.
#[derive(Clone, Debug)]
pub enum Ending {
    Pane(PaneEvent),
    Link(LinkError),
}

/// What the server pushed on a channel. The connection's I/O thread records it; a waiting caller
/// applies the frames and acknowledges each.
#[derive(Default)]
struct Events {
    state: Mutex<Received>,
    ready: Condvar,
}

#[derive(Default)]
struct Received {
    /// The outcome of each Input message the server discarded or each paste it settled, by
    /// serial.
    outcomes: BTreeMap<u32, InputOutcome>,
    /// Frames not yet applied. The server sends the next frame only after an acknowledgement.
    frames: VecDeque<Box<Frame>>,
    /// Every frame the channel received.
    received: u64,
    screen: Screen,
    /// Native effect requests not yet taken, oldest first.
    effects: VecDeque<effects::Request>,
    ending: Option<Ending>,
}

impl Events {
    fn lock(&self) -> std::sync::MutexGuard<'_, Received> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs on the connection's I/O thread, so it only records the event.
    fn deliver(&self, event: ChannelEvent) {
        let mut received = self.lock();
        match event {
            ChannelEvent::Frame(frame) => {
                received.received += 1;
                received.frames.push_back(frame);
            }
            ChannelEvent::Pane(PaneEvent::Input { serial, outcome }) => {
                received.outcomes.insert(serial, outcome);
            }
            ChannelEvent::Pane(PaneEvent::Effect(request)) => {
                received.effects.push_back(request);
            }
            ChannelEvent::Pane(event) if event.last() => {
                received.ending.get_or_insert(Ending::Pane(event));
            }
            ChannelEvent::Closed(error) => {
                received.ending.get_or_insert(Ending::Link(error));
            }
            // These checks read neither hidden input nor accessibility revisions.
            ChannelEvent::Pane(_) => return,
        }
        drop(received);
        self.ready.notify_all();
    }
}

/// A Pane claimed on one channel. Dropping it keeps the Pane's control, as abandoning a lease of
/// protocol major 1 did; [`Pane::release`] gives control up.
pub struct Pane {
    link: ServerLink,
    id: Channel,
    /// Taken only when the Pane closes, releases or drops.
    channel: Option<ChannelHandle>,
    lease: Lease,
    events: Arc<Events>,
}

impl Drop for Pane {
    fn drop(&mut self) {
        if let Some(channel) = self.channel.take() {
            channel.detach();
        }
    }
}

impl Pane {
    pub fn lease(&self) -> Lease {
        self.lease
    }

    pub fn link(&self) -> &ServerLink {
        &self.link
    }

    fn handle(&self) -> Result<&ChannelHandle> {
        self.channel.as_ref().ok_or(ServerError::StaleControl)
    }

    pub fn channel(&self) -> Channel {
        self.id
    }

    /// Fails with the first input discard the server reported that no caller waited for, so a
    /// refused input fails the next operation, as its refused exchange did in protocol major 1.
    fn reported(&self) -> Result<()> {
        let mut received = self.events.lock();
        if let Some(Ending::Link(error)) = received.ending {
            return Err(error.into());
        }
        let discarded = received
            .outcomes
            .iter()
            .find_map(|(serial, outcome)| match outcome {
                InputOutcome::Discarded(refusal) => Some((*serial, *refusal)),
                InputOutcome::Paste(_) => None,
            });
        match discarded {
            Some((serial, refusal)) => {
                received.outcomes.remove(&serial);
                Err(refusal.into())
            }
            None => Ok(()),
        }
    }

    /// Sends one Input message without waiting and returns its serial. The server reports only a
    /// discard; see [`Pane::discarded`].
    pub fn input(&self, generation: u64, events: Vec<InputEvent>) -> Result<u32> {
        self.reported()?;
        Ok(self.handle()?.input(generation, events)?)
    }

    pub fn focus(&self, generation: u64, target: FocusTarget) -> Result<u32> {
        self.input(generation, vec![InputEvent::Focus(target)])
    }

    pub fn key(&self, generation: u64, key: Key) -> Result<u32> {
        self.input(generation, vec![InputEvent::Key(key)])
    }

    /// Resizing is never fenced by focus, so it names no focus generation.
    pub fn resize(&self, size: PaneSize) -> Result<u32> {
        self.input(0, vec![InputEvent::Resize(size)])
    }

    /// Focuses the terminal under focus generation 1, then types `text` and Return.
    pub fn command(&self, text: &str) -> Result<()> {
        self.input(
            1,
            vec![
                InputEvent::Focus(FocusTarget::Terminal),
                InputEvent::Key(text_key(text)),
                InputEvent::Key(enter_key()),
            ],
        )
        .map(drop)
    }

    /// Waits until the server discards the Input message `serial` and returns why. Accepted input
    /// has no report, so it fails after the wait.
    pub fn discarded(&self, serial: u32) -> Result<Refusal> {
        match self.outcome(serial)? {
            InputOutcome::Discarded(refusal) => Ok(refusal),
            InputOutcome::Paste(_) => Err(ServerError::Protocol),
        }
    }

    /// Waits for the outcome the server reports for the paste or paste resolution `serial`.
    fn settled(&self, serial: u32) -> Result<PasteOutcome> {
        match self.outcome(serial)? {
            InputOutcome::Paste(outcome) => Ok(outcome),
            InputOutcome::Discarded(refusal) => Err(refusal.into()),
        }
    }

    pub fn paste(&self, generation: u64, text: &str) -> Result<PasteOutcome> {
        self.reported()?;
        let serial = self.handle()?.paste(generation, text)?;
        self.settled(serial)
    }

    pub fn resolve_paste(&self, generation: u64, id: u64, confirm: bool) -> Result<PasteOutcome> {
        let serial = self.input(generation, vec![InputEvent::ResolvePaste { id, confirm }])?;
        self.settled(serial)
    }

    /// Completes a native effect request. The server reports no outcome for it.
    pub fn effect_done(
        &self,
        generation: u64,
        effect: u64,
        completion: effects::Completion,
    ) -> Result<()> {
        self.reported()?;
        Ok(self.handle()?.effect_done(generation, effect, completion)?)
    }

    /// The text of every retained Scrollback row of `history`, read through `ScrollbackPage` as a
    /// client caches it, oldest first.
    pub fn scrollback(&self, history: History) -> Result<Vec<String>> {
        let mut rows = Vec::new();
        let mut start = history.oldest;
        while start < history.total {
            let ReplyBody::Page(page) = self.request(Operation::ScrollbackPage {
                channel: self.channel(),
                epoch: history.epoch,
                start,
                rows: MAX_PAGE_ROWS,
            })?
            else {
                return Err(ServerError::Protocol);
            };
            if page.epoch != history.epoch || page.start < start {
                return Err(ServerError::Protocol);
            }
            if page.rows.is_empty() {
                break;
            }
            start = page.start + page.rows.len() as u64;
            rows.extend(
                page.rows
                    .iter()
                    .map(|row| row.runs.iter().map(|run| run.text.as_str()).collect()),
            );
        }
        Ok(rows)
    }

    pub fn request(&self, operation: Operation) -> Result<ReplyBody> {
        self.reported()?;
        request(&self.link, operation)
    }

    pub fn interim(&self, operation: Interim) -> Result<InterimReply> {
        self.reported()?;
        interim(&self.link, operation)
    }

    /// Applies the frames the server pushed, acknowledging each, until `found` returns a value
    /// or `wait` elapses. `None` means the wait elapsed. The end of the connection fails the wait
    /// unless `found` accepts it.
    fn until<T>(
        &self,
        wait: Duration,
        mut found: impl FnMut(&mut Received) -> Option<T>,
    ) -> Result<Option<T>> {
        let handle = self.handle()?;
        let deadline = Instant::now() + wait;
        let mut received = self.events.lock();
        loop {
            while let Some(frame) = received.frames.pop_front() {
                received.screen.apply(&frame)?;
                match handle.acknowledge(frame.frame) {
                    // An ended channel takes no acknowledgement and needs none.
                    Ok(()) | Err(LinkError::Unclaimed) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            if let Some(value) = found(&mut received) {
                return Ok(Some(value));
            }
            if let Some(Ending::Link(error)) = received.ending {
                return Err(error.into());
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            received = self
                .events
                .ready
                .wait_timeout(received, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Waits until the server reports on the Input message `serial`.
    fn outcome(&self, serial: u32) -> Result<InputOutcome> {
        self.until(WAIT, |received| received.outcomes.remove(&serial))?
            .ok_or(ServerError::Protocol)
    }

    /// Waits until the pushed screen satisfies `matches`, for at most `wait`, and returns it.
    /// `None` means it did not.
    pub fn await_screen(
        &self,
        wait: Duration,
        matches: impl Fn(&Screen) -> bool,
    ) -> Result<Option<Screen>> {
        self.until(wait, |received| {
            matches(&received.screen).then(|| received.screen.clone())
        })
    }

    /// Waits until a row reads exactly `marker` and returns the screen's text.
    pub fn await_output(&self, marker: &str) -> std::result::Result<String, Box<dyn Error>> {
        let screen = self
            .await_screen(WAIT, |screen| has_row(screen, marker))?
            .ok_or("terminal output did not arrive")?;
        Ok(screen.rows.join("\n"))
    }

    /// Takes the oldest native effect request the server pushed, waiting at most `wait` for one.
    /// Complete it with [`Pane::effect_done`].
    pub fn await_effect(&self, wait: Duration) -> Result<Option<effects::Request>> {
        self.until(wait, |received| received.effects.pop_front())
    }

    /// Waits until the channel ends and returns how it ended.
    pub fn ending(&self) -> Result<Ending> {
        self.until(WAIT, |received| received.ending.clone())?
            .ok_or(ServerError::Protocol)
    }

    /// How many frames the channel has received.
    pub fn frames(&self) -> u64 {
        self.events.lock().received
    }

    /// Shows or hides the channel's presentation. A hidden channel receives no frames, and
    /// showing it pushes the current screen.
    pub fn view(&self, visible: bool) -> Result<()> {
        self.reported()?;
        Ok(self.handle()?.view(visible, 60)?)
    }

    /// Closes the Pane and ends its Terminal Session.
    pub fn close(mut self) -> Result<()> {
        done(
            &self.link,
            Operation::Close {
                channel: self.channel(),
            },
        )?;
        if let Some(channel) = self.channel.take() {
            channel.detach();
        }
        Ok(())
    }

    /// Gives up control of the Pane. The release travels behind every earlier request and input.
    pub fn release(mut self) {
        drop(self.channel.take());
    }
}
