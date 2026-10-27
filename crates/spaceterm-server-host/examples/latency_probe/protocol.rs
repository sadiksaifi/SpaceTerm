//! Every protocol call the latency probe makes. When the client protocol changes, port this
//! module and keep its public items: [`Owner`], [`Claim`], [`Route`] and [`EchoPane`].
use spaceterm::runtime::PhysicalKey;
use spaceterm_server::attachments::Attachment;
use spaceterm_server::connection::link::{ChannelEvent, ChannelHandle, LinkEvent, ServerLink};
use spaceterm_server::direct::{
    ClaimId, DeviceCredential, DirectConnection, DirectConnector, DirectError, Enrollment,
    Invitation, NetworkPolicy, PairedDevice,
};
use spaceterm_server::hierarchy::{CloseTarget, Id, Inventory, Mutation, Outcome};
use spaceterm_server::pane::{AttachmentClaim, FocusTarget, Key, PaneSize, Refusal};
use spaceterm_server::protocol::interim::{Interim, InterimReply};
use spaceterm_server::protocol::{
    ClaimReply, InputEvent, InputOutcome, Operation, PaneEvent, ReplyBody,
};
use spaceterm_server::{ServerConnection, ServerError};
use std::cell::RefCell;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A failed probe stage and the typed classification of its cause.
#[derive(Debug)]
pub struct Failure {
    pub stage: &'static str,
    pub cause: Option<String>,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.cause {
            Some(cause) => write!(f, "{}: {cause}", self.stage),
            None => f.write_str(self.stage),
        }
    }
}
impl std::error::Error for Failure {}

pub type Result<T> = std::result::Result<T, Failure>;

pub fn fail<T>(stage: &'static str) -> Result<T> {
    Err(Failure { stage, cause: None })
}

/// Server and direct errors are typed classifications without terminal contents or paths.
fn at<T, E: std::fmt::Debug>(stage: &'static str, result: std::result::Result<T, E>) -> Result<T> {
    result.map_err(|error| Failure {
        stage,
        cause: Some(format!("{error:?}")),
    })
}

/// Selects a separate account-local client identity for this process, so the probe never shares
/// the desktop's attachments or Pane control. Call it before any other thread starts.
pub fn name_client(name: &str) {
    // SAFETY: the probe calls this first in main, while it has a single thread.
    unsafe { std::env::set_var("SPACETERM_DEVELOPMENT_CLIENT", name) };
}

/// The owning machine's account-local authority over network exposure and pairing.
pub struct Owner {
    server: ServerConnection,
    /// Invitations this owner issued and has not seen consumed by an approved claim.
    issued: RefCell<Vec<ClaimId>>,
}

/// Network exposure to restore after a temporary change.
pub struct Exposure {
    /// The state before the change, or None when exposure already matched.
    previous: Option<(Option<NetworkPolicy>, bool)>,
}

impl Owner {
    pub fn open(identity: &str) -> Result<Self> {
        Ok(Self {
            server: at("owner", ServerConnection::for_identity(identity))?,
            issued: RefCell::default(),
        })
    }

    /// Enables exposure at `bind`. A policy change disconnects every paired client, so an
    /// exposure already enabled at `bind` stays untouched.
    pub fn expose(&self, bind: SocketAddr) -> Result<Exposure> {
        let status = at("exposure status", self.server.direct_status())?;
        if status.enabled && status.policy.as_ref().map(|policy| policy.bind) == Some(bind) {
            return Ok(Exposure { previous: None });
        }
        let policy = at("exposure address", NetworkPolicy::new(bind))?;
        at("expose", self.server.set_direct_policy(Some(policy), true))?;
        Ok(Exposure {
            previous: Some((status.policy, status.enabled)),
        })
    }

    /// Restores the previous address and enabled state. When no address was configured
    /// before, the server keeps the probe's address, disabled.
    pub fn restore(&self, exposure: Exposure) -> Result<()> {
        let Some((policy, enabled)) = exposure.previous else {
            return Ok(());
        };
        at(
            "restore exposure",
            self.server.set_direct_policy(policy, enabled),
        )
        .map(drop)
    }

    pub fn invite(&self) -> Result<String> {
        let invitation = at("invite", self.server.invite_device())?;
        self.issued.borrow_mut().push(invitation.id());
        Ok(invitation.display_text().as_str().to_owned())
    }

    /// Approves the pending claim from `device` on an invitation this owner issued. Returns
    /// false when no such claim exists.
    pub fn approve(&self, device: &str) -> Result<bool> {
        for claim in at("claims", self.server.pairing_claims())? {
            if claim.device_name.as_str() == device
                && self.issued.borrow().contains(&claim.invitation_id)
            {
                at("approve", self.server.approve_device(&claim))?;
                self.issued
                    .borrow_mut()
                    .retain(|issued| *issued != claim.invitation_id);
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Withdraws every invitation this owner issued that no approved claim used. The server
    /// also refuses any pending claim on those invitations.
    pub fn abandon(&self) -> Result<()> {
        let issued = std::mem::take(&mut *self.issued.borrow_mut());
        for id in issued {
            at("withdraw invitation", self.server.withdraw_invitation(id))?;
        }
        Ok(())
    }

    /// Revokes every pairing named `device` and returns how many it revoked.
    pub fn revoke(&self, device: &str) -> Result<usize> {
        let mut revoked = 0;
        for paired in at("devices", self.server.paired_devices())? {
            if paired.name.as_str() == device {
                let mut operation = [0; 32];
                // SAFETY: getentropy writes exactly 32 bytes to this live writable array.
                if unsafe { libc::getentropy(operation.as_mut_ptr().cast(), operation.len()) } != 0
                {
                    return fail("randomness");
                }
                at(
                    "revoke",
                    self.server.revoke_device(operation, paired.pairing),
                )?;
                revoked += 1;
            }
        }
        Ok(revoked)
    }
}

/// A submitted pairing claim that waits for the owner's approval.
pub struct Claim {
    endpoint: String,
    credential: DeviceCredential,
    enrollment: Enrollment,
}

impl Claim {
    pub fn submit(invitation: &str, device: &str) -> Result<Self> {
        let invitation = at("invitation", Invitation::parse(invitation))?;
        let credential = at("credential", DeviceCredential::generate())?;
        let enrollment = at(
            "claim",
            Enrollment::claim(&invitation, device.into(), &credential),
        )?;
        Ok(Self {
            endpoint: invitation.endpoint().to_owned(),
            credential,
            enrollment,
        })
    }

    pub fn finish(mut self) -> Result<Route> {
        let Some(paired) = at("enrollment", self.enrollment.finish())? else {
            return fail("enrollment not approved");
        };
        Ok(Route::Paired(Arc::new(Paired {
            endpoint: self.endpoint,
            paired,
            credential: self.credential,
        })))
    }
}

pub struct Paired {
    endpoint: String,
    paired: PairedDevice,
    credential: DeviceCredential,
}
impl DirectConnector for Paired {
    fn connect(&self) -> std::result::Result<DirectConnection, DirectError> {
        DirectConnection::connect(&self.endpoint, &self.paired, &self.credential)
    }
}

/// How the probe reaches the server: account-local IPC or a paired WSS connection.
pub enum Route {
    Local,
    Paired(Arc<Paired>),
}

/// A probe Workspace whose Pane runs `cat` on a raw terminal, so each typed key echoes once.
///
/// Screens are polled until the server pushes frames. The probe keeps only the generation of the
/// last screen it saw, which `PollChanges` names as its base, and the cursor that the echo moves.
pub struct EchoPane {
    server: ServerConnection,
    link: ServerLink,
    boot: Id,
    workspace: Id,
    channel: Option<ChannelHandle>,
    /// The first input the server discarded. The connection's I/O thread records it.
    discarded: Arc<Mutex<Option<Refusal>>>,
    generation: Option<u64>,
    cursor: Option<(u16, u16, u8)>,
}

const READY: &str = "PROBE-READY";

/// Every claim names attachment generation 1. The probe releases no attachment, so the server
/// never holds a newer generation for its Workspace.
const GENERATION: u64 = 1;

fn connect(server: &ServerConnection) -> std::result::Result<ServerLink, ServerError> {
    server.connect(Arc::new(|_: LinkEvent| {}))
}

fn request(link: &ServerLink, stage: &'static str, operation: Operation) -> Result<ReplyBody> {
    let pending = at(stage, link.request(operation))?;
    at(stage, pending.wait())
}

fn interim(link: &ServerLink, stage: &'static str, operation: Interim) -> Result<InterimReply> {
    match request(link, stage, Operation::Interim(operation))? {
        ReplyBody::Interim(reply) => Ok(reply),
        _ => fail(stage),
    }
}

/// The newest pushed inventory. The server pushes one when it admits the link.
fn inventory(link: &ServerLink, stage: &'static str) -> Result<(Inventory, u64)> {
    match link.next_inventory(0) {
        Ok((_, revision)) => Ok((revision.inventory.clone(), revision.next_sequence)),
        Err(_) => fail(stage),
    }
}

impl EchoPane {
    pub fn open(identity: &str, route: &Route) -> Result<Self> {
        let server = match route {
            Route::Local => at("connect", ServerConnection::for_identity(identity))?,
            Route::Paired(paired) => at(
                "connect",
                ServerConnection::direct(
                    identity,
                    paired.paired.client,
                    paired.paired.clone(),
                    paired.clone(),
                ),
            )?,
        };
        let link = at("connect", connect(&server))?;
        let (inventory, next_sequence) = inventory(&link, "inventory")?;
        let Some(boot) = inventory.close.as_ref().map(|close| close.boot) else {
            return fail("server boot");
        };
        let mutation = Mutation::CreateWorkspace {
            name: format!("Latency probe {}", std::process::id()),
            directory: None,
        };
        let outcome = mutate(&link, next_sequence, mutation)?;
        let Outcome::Accepted { target: workspace } = outcome else {
            return fail("Workspace creation refused");
        };
        let mut pane = Self {
            server,
            link,
            boot,
            workspace,
            channel: None,
            discarded: Arc::default(),
            generation: None,
            cursor: None,
        };
        // Close the Workspace if the Pane never becomes ready.
        match pane.start() {
            Ok(()) => Ok(pane),
            Err(error) => {
                let _ = pane.close();
                Err(error)
            }
        }
    }

    fn start(&mut self) -> Result<()> {
        let (inventory, _) = inventory(&self.link, "inventory")?;
        let workspace = self.workspace;
        let Some(pane) = inventory
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .and_then(|workspace| workspace.tabs.first())
            .map(|tab| tab.root_pane)
        else {
            return fail("probe Pane missing");
        };
        let size = PaneSize {
            columns: 100,
            rows: 30,
            cell_width: 8,
            cell_height: 18,
        };
        let discarded = Arc::clone(&self.discarded);
        let channel = at(
            "attach",
            self.link.open_channel(Arc::new(move |event: ChannelEvent| {
                if let ChannelEvent::Pane(PaneEvent::Input {
                    outcome: InputOutcome::Discarded(refusal),
                    ..
                }) = event
                {
                    discarded
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get_or_insert(refusal);
                }
            })),
        )?;
        let attachment = Attachment {
            workspace,
            generation: GENERATION,
        };
        let claim = AttachmentClaim::Select { explicit: true };
        let pending = at("attach", channel.claim(attachment, pane, claim, size, None))?;
        let ReplyBody::Claim(ClaimReply::Attached { .. }) = at("attach", pending.wait())? else {
            channel.detach();
            return fail("attach");
        };
        let channel = self.channel.insert(channel);
        at(
            "focus",
            channel.input(GENERATION, vec![InputEvent::Focus(FocusTarget::Terminal)]),
        )?;
        let demand = Interim::PresentationDemand {
            channel: channel.channel(),
            visible: true,
        };
        interim(&self.link, "visible", demand)?;
        // The printf output differs from the typed command, so only the program prints READY.
        std::thread::sleep(Duration::from_millis(1500));
        self.input(vec![
            InputEvent::Key(text_key(
                "stty raw -echo; printf 'PROBE%sREADY' -; exec cat",
            )),
            InputEvent::Key(enter_key()),
        ])?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let poll = Interim::Poll {
                channel: self.channel()?.channel(),
            };
            if let InterimReply::Screen(screen) = interim(&self.link, "ready", poll)?
                && screen.rows.iter().any(|row| {
                    row.iter()
                        .map(|cell| cell.text.as_str())
                        .collect::<String>()
                        .contains(READY)
                })
            {
                break;
            }
            if Instant::now() > deadline {
                return fail("the probe Pane did not start cat");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(300));
        self.generation = None;
        self.poll("screen")?;
        Ok(())
    }

    fn channel(&self) -> Result<&ChannelHandle> {
        self.channel
            .as_ref()
            .map_or_else(|| fail("probe Pane not attached"), Ok)
    }

    /// Fails when the server discarded earlier input.
    fn reported(&self) -> Result<()> {
        match *self.discarded.lock().unwrap_or_else(|e| e.into_inner()) {
            Some(refusal) => Err(Failure {
                stage: "input",
                cause: Some(format!("{refusal:?}")),
            }),
            None => Ok(()),
        }
    }

    /// Sends one Input message under focus generation 1 and returns its serial. Input has no
    /// reply; the server reports only a discard.
    fn input(&mut self, events: Vec<InputEvent>) -> Result<u32> {
        self.reported()?;
        at("input", self.channel()?.input(GENERATION, events))
    }

    /// Polls the screen's changes since the last screen the probe saw, and returns whether the
    /// cursor moved.
    fn poll(&mut self, stage: &'static str) -> Result<bool> {
        self.reported()?;
        let poll = Interim::PollChanges {
            channel: self.channel()?.channel(),
            base: self.generation,
        };
        let screen = match interim(&self.link, stage, poll)? {
            InterimReply::Screen(screen) => *screen,
            InterimReply::ScreenChanges(changes) => changes.screen,
            InterimReply::Unchanged | InterimReply::AccessibilityRevision { .. } => {
                return Ok(false);
            }
            _ => return fail(stage),
        };
        self.generation = Some(screen.generation);
        let moved = screen.cursor != self.cursor;
        self.cursor = screen.cursor;
        Ok(moved)
    }

    /// One request that changes nothing. It releases an attachment of a Workspace that never
    /// existed, which the server answers before it reads any attachment state.
    pub fn round_trip(&mut self) -> Result<()> {
        let nothing = Operation::ReleaseAttachment {
            attachment: Attachment {
                workspace: [0; 32],
                generation: 0,
            },
        };
        match request(&self.link, "round trip", nothing)? {
            ReplyBody::Done => Ok(()),
            _ => fail("round trip"),
        }
    }

    /// Types `character` and returns the serial of its Input message.
    pub fn send_key(&mut self, character: char) -> Result<u32> {
        self.input(vec![InputEvent::Key(text_key(&character.to_string()))])
    }

    /// Returns once the client has seen the cursor move.
    pub fn await_echo(&mut self, deadline: Instant) -> Result<()> {
        while Instant::now() < deadline {
            if self.poll("poll")? {
                return Ok(());
            }
        }
        fail("echo timed out")
    }

    /// Sends Return, which moves the raw cursor back to the first column of the same line.
    pub fn rewind(&mut self) -> Result<()> {
        self.input(vec![InputEvent::Key(enter_key())])?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.cursor.map(|cursor| cursor.0) != Some(0) {
            self.await_echo(deadline)?;
        }
        Ok(())
    }

    /// Closes the probe Workspace on a fresh connection, unless the server restarted.
    pub fn close(mut self) -> Result<()> {
        // Closing the Workspace ends the Pane, so its channel needs no release.
        if let Some(channel) = self.channel.take() {
            channel.detach();
        }
        let link = at("cleanup connect", connect(&self.server))?;
        let (inventory, next_sequence) = inventory(&link, "cleanup inventory")?;
        if inventory.close.as_ref().map(|close| close.boot) != Some(self.boot) {
            return fail("the server restarted; nothing to close");
        }
        let target = CloseTarget::Workspace {
            workspace: self.workspace,
        };
        let Some(confirmation) = inventory.close_confirmation(target) else {
            return Ok(());
        };
        let outcome = mutate(
            &link,
            next_sequence,
            Mutation::Close {
                target,
                confirmation,
            },
        )?;
        match outcome {
            Outcome::Accepted { .. } => Ok(()),
            _ => fail("probe Workspace close refused"),
        }
    }
}

fn mutate(link: &ServerLink, sequence: u64, mutation: Mutation) -> Result<Outcome> {
    let reply = request(link, "mutate", Operation::Mutate { sequence, mutation })?;
    let ReplyBody::Outcome(result) = reply else {
        return fail("mutation outcome");
    };
    Ok(result.outcome)
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
