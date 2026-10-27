//! The native checks' SpaceTerm Protocol client: requests on one admitted connection, and claimed
//! Panes with their input, reported input outcomes and polled screens.
//!
//! The spaceterm crate's native tests also include this file, so it names physical keys by their
//! protocol codes instead of through the spaceterm crate.
use spaceterm_server::{
    ServerConnection, ServerError,
    attachments::Attachment,
    connection::link::{ChannelEvent, ChannelHandle, LinkError, LinkEvent, ServerLink},
    hierarchy::{Id, Inventory, Mutation, RetainedOutcome},
    pane::{AttachmentClaim, FocusTarget, Key, Lease, PaneSize, PasteOutcome, Refusal, effects},
    protocol::{
        Channel, ClaimReply, InputEvent, InputOutcome, Operation, PaneEvent, ReplyBody, Restored,
        Takeover,
        interim::{Interim, InterimReply, ScreenFrame},
    },
};
use std::{
    collections::BTreeMap,
    error::Error,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

pub type Result<T> = std::result::Result<T, ServerError>;

/// How long a helper waits for the server to report on input or to show terminal output.
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

/// The text of each row of a polled keyframe. Rows the keyframe omits are blank, and a wide
/// character's spacer adds nothing.
pub fn rows(screen: &ScreenFrame) -> Vec<String> {
    let count = screen
        .update
        .geometry
        .map_or(0, |geometry| usize::from(geometry.rows));
    let mut rows = vec![String::new(); count];
    for row in &screen.update.rows {
        if let Some(text) = rows.get_mut(usize::from(row.index)) {
            *text = row.runs.iter().map(|run| run.text.as_str()).collect();
        }
    }
    rows
}

/// Whether a row of the screen reads exactly `marker`, ignoring trailing blanks.
pub fn has_row(screen: &ScreenFrame, marker: &str) -> bool {
    rows(screen).iter().any(|row| row.trim_end() == marker)
}

/// What the server reported on a channel: the outcome of each Input message it discarded or each
/// paste it settled, by serial, and the end of the connection.
#[derive(Default)]
struct Events {
    state: Mutex<Received>,
    ready: Condvar,
}

#[derive(Default)]
struct Received {
    outcomes: BTreeMap<u32, InputOutcome>,
    closed: Option<LinkError>,
}

impl Events {
    fn lock(&self) -> std::sync::MutexGuard<'_, Received> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs on the connection's I/O thread, so it only records the event.
    fn deliver(&self, event: ChannelEvent) {
        let mut received = self.lock();
        match event {
            ChannelEvent::Pane(PaneEvent::Input { serial, outcome }) => {
                received.outcomes.insert(serial, outcome);
            }
            ChannelEvent::Closed(error) => {
                received.closed.get_or_insert(error);
            }
            // These checks poll screens, effects and accessibility until push replaces polling.
            ChannelEvent::Frame(_) | ChannelEvent::Pane(_) => return,
        }
        drop(received);
        self.ready.notify_all();
    }

    /// Waits until the server reports on the Input message `serial`.
    fn outcome(&self, serial: u32) -> Result<InputOutcome> {
        let deadline = Instant::now() + WAIT;
        let mut received = self.lock();
        loop {
            if let Some(outcome) = received.outcomes.remove(&serial) {
                return Ok(outcome);
            }
            if let Some(error) = received.closed {
                return Err(error.into());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(ServerError::Protocol);
            }
            received = self
                .ready
                .wait_timeout(received, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
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
        if let Some(error) = received.closed {
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
        match self.events.outcome(serial)? {
            InputOutcome::Discarded(refusal) => Ok(refusal),
            InputOutcome::Paste(_) => Err(ServerError::Protocol),
        }
    }

    /// Waits for the outcome the server reports for the paste or paste resolution `serial`.
    fn settled(&self, serial: u32) -> Result<PasteOutcome> {
        match self.events.outcome(serial)? {
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

    pub fn request(&self, operation: Operation) -> Result<ReplyBody> {
        self.reported()?;
        request(&self.link, operation)
    }

    pub fn interim(&self, operation: Interim) -> Result<InterimReply> {
        self.reported()?;
        interim(&self.link, operation)
    }

    /// Polls the screen once. A screen arrives as a keyframe.
    pub fn poll(&self) -> Result<InterimReply> {
        self.interim(Interim::Poll {
            channel: self.channel(),
        })
    }

    /// The screen, when it changed since the last poll.
    pub fn screen(&self) -> Result<Option<Box<ScreenFrame>>> {
        match self.poll()? {
            InterimReply::Screen(screen) => Ok(Some(screen)),
            _ => Ok(None),
        }
    }

    /// Polls until a screen satisfies `matches`, for at most `wait`. `None` means none did.
    pub fn await_screen(
        &self,
        wait: Duration,
        matches: impl Fn(&ScreenFrame) -> bool,
    ) -> Result<Option<Box<ScreenFrame>>> {
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            if let Some(screen) = self.screen()?
                && matches(&screen)
            {
                return Ok(Some(screen));
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        Ok(None)
    }

    /// Waits until a row reads exactly `marker` and returns the screen's text.
    pub fn await_output(&self, marker: &str) -> std::result::Result<String, Box<dyn Error>> {
        let screen = self
            .await_screen(WAIT, |screen| has_row(screen, marker))?
            .ok_or("terminal output did not arrive")?;
        Ok(rows(&screen).join("\n"))
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
