//! Desktop presentation and fenced input for an authenticated server Pane.
mod accessibility;
mod claim;
mod control;
mod effects;
mod inbox;
#[cfg(test)]
pub(crate) mod push_tests;
use super::core_graphics as graphics;
mod link;
mod prediction;
mod reading;
mod recovery;
pub(crate) mod restart;
mod screen;
mod screen_decoder;
#[cfg(test)]
mod screen_decoder_tests;
mod scrollback;
#[cfg(test)]
mod semantic_equality_tests;
#[cfg(test)]
mod tests;
pub(crate) mod unsent;
use super::pointer_input::{PointerInput, WheelInput};
use super::session::clipboard::ClipboardAuthority;
use super::session::*;
use super::{emulator::*, geometry::TerminalGeometry, key::*};
use crate::machine_connection::{CoreError, MachineConnection};
pub(crate) use control::{ControlRoute, PaneControlRegistry, PaneControls};
use effects::ClientEffects;
#[cfg(test)]
use link::encode_key;
use link::{encode_modifiers, expect_unchanged, submit_input};
use screen::{ScreenReceiver, clear_presentation, encode_style};
use spaceterm_core::direct::DirectError;
use spaceterm_core::pane::{self, Lease, PaneSize, Reply, Request};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::{
    sync::{atomic::AtomicU64, mpsc},
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CorePaneStatus {
    Connecting,
    Restored,
    Connected,
    Takeover,
    TakeoverReconnecting,
    TakeoverFailed(CoreError),
    TakingOver,
    Reconnecting,
    SessionEnded(pane::SessionEnd),
    Ended,
    Failed(CoreError),
    /// The client is not connected to the Pane's server, so it presents the Pane from what it
    /// retained and reaches no Terminal Session.
    Offline,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Admission {
    Pending,
    Waiting,
    Granted,
}
/// How long a shell started in the background keeps its place in the window's start queue
/// without showing it is ready, so a shell that never reports a prompt or title still lets the
/// queue go on.
pub(crate) const BACKGROUND_READY_LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

/// One Pane's place in the window's background start queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reservation {
    Idle,
    /// The worker waits for a turn.
    Wanted,
    /// The window gave the Pane a turn its worker has not used yet.
    Granted,
    /// The worker used its turn. `session` is the Session SpaceTerm Core started, once it
    /// answered, and `until` when the place lapses without readiness.
    Starting {
        session: Option<[u8; 32]>,
        until: Option<std::time::Instant>,
    },
}

pub(crate) struct CorePaneState {
    status: Mutex<CorePaneStatus>,
    /// What the person typed that the Core refused or never confirmed, since the Pane last
    /// attached. Never sent again by itself; the person can copy it.
    unsent: Arc<Mutex<unsent::Unsent>>,
    /// A Terminal Session that survived the idle restart grant that refused its input, until
    /// the Pane's status says so.
    unsent_notice: Mutex<unsent::UnsentChange>,
    retaining_input: Arc<AtomicBool>,
    /// Admission settles before a hidden Pane receives its input-enabling screen.
    admission: Mutex<Admission>,
    /// Who controlled the Pane when the server last offered Take Over.
    controller: Mutex<Option<pane::control::Controller>>,
    /// Why the Pane's Session ended, as the server last said, while nobody started a new one.
    session_end: Mutex<Option<pane::SessionEnd>>,
    retry: AtomicBool,
    selected: Arc<AtomicBool>,
    /// The Pane is the Focused Pane of the Tab and Workspace the person has open.
    foreground: AtomicBool,
    /// The Core boot whose background start this Pane last asked for, so it asks once per boot.
    prepared: Mutex<Option<[u8; 32]>>,
    /// The Core run in which the window's start queue last asked this Pane's stopped worker to
    /// connect again.
    rearmed: Mutex<Option<[u8; 32]>>,
    /// This Pane's worker is starting its shell, or presents one that has not attached yet.
    shell_starting: AtomicBool,
    /// The shell this client started is attached and has not reached its first prompt, named
    /// itself or taken input, so its surface stays quiet.
    startup_quiet: AtomicBool,
    /// This Pane's one place in the window's background start queue, from the worker's wait for
    /// a turn until the shell it started shows it is ready, fails or is cancelled.
    background: Mutex<Reservation>,
    created_start: Arc<AtomicBool>,
    take_over: AtomicBool,
    start_restored: Arc<AtomicBool>,
    connection_recovery: Arc<Mutex<recovery::Recovery>>,
    /// Where this presentation leaves its view for the next one when its window closes.
    retained_view: Arc<Mutex<Option<reading::Reading>>>,
    recovery: Mutex<Option<pane::directories::Target>>,
    repair: Mutex<Option<(pane::directories::Target, std::path::PathBuf)>>,
    directory_core: Mutex<Option<MachineConnection>>,
    directory_busy: AtomicBool,
    directory_note: Mutex<Option<&'static str>>,
    epoch: AtomicU64,
    connected_epoch: AtomicU64,
    paste_unconfirmed: AtomicBool,
    interaction: Mutex<Interaction>,
    effect_authority: Arc<ClipboardAuthority>,
    hidden_input: AtomicBool,
    /// The Line Mode SpaceTerm Core last pushed, with the connection epoch it arrived on. It
    /// describes only that epoch's Terminal Session.
    line: Mutex<Option<(u64, pane::LineMode)>>,
    /// Whether the input the worker sends now was typed at a hidden prompt. Shared with the
    /// link, which records it per Input serial.
    sending_hidden: Arc<AtomicBool>,
    reading: Mutex<reading::Reading>,
    accessibility: Mutex<accessibility::RetainedSemantics>,
    accessibility_demand: AtomicBool,
    client_effects: Mutex<std::collections::VecDeque<ClientEffectRequest>>,
    presentable: AtomicBool,
    preparation: AtomicBool,
    wake: inbox::Wake,
    /// Hints the views that read what this Pane presents.
    changes: crate::terminal::changes::Changes,
}
impl CorePaneState {
    fn new() -> Self {
        Self {
            status: Mutex::new(CorePaneStatus::Connecting),
            unsent: Arc::default(),
            unsent_notice: Mutex::default(),
            retaining_input: Arc::default(),
            admission: Mutex::new(Admission::Pending),
            controller: Mutex::default(),
            session_end: Mutex::default(),
            retry: AtomicBool::new(false),
            selected: Arc::default(),
            foreground: AtomicBool::new(false),
            prepared: Mutex::default(),
            rearmed: Mutex::default(),
            shell_starting: AtomicBool::new(false),
            startup_quiet: AtomicBool::new(false),
            background: Mutex::new(Reservation::Idle),
            created_start: Arc::default(),
            take_over: AtomicBool::new(false),
            start_restored: Arc::default(),
            connection_recovery: Arc::default(),
            retained_view: Arc::default(),
            recovery: Mutex::default(),
            repair: Mutex::default(),
            directory_core: Mutex::default(),
            directory_busy: AtomicBool::new(false),
            directory_note: Mutex::default(),
            epoch: AtomicU64::new(0),
            connected_epoch: AtomicU64::new(0),
            paste_unconfirmed: AtomicBool::new(false),
            interaction: Mutex::new(Interaction::default()),
            hidden_input: AtomicBool::new(false),
            line: Mutex::default(),
            sending_hidden: Arc::default(),
            reading: Mutex::default(),
            accessibility: Mutex::default(),
            accessibility_demand: AtomicBool::new(false),
            client_effects: Mutex::new(std::collections::VecDeque::new()),
            presentable: AtomicBool::new(true),
            preparation: AtomicBool::new(false),
            wake: inbox::Wake::default(),
            changes: crate::terminal::changes::Changes::default(),
            effect_authority: Arc::new(ClipboardAuthority::default()),
        }
    }

    pub(crate) fn status(&self) -> CorePaneStatus {
        *self.status.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Leaves this client's view of the Pane, its Scrollback position and Selection, for the
    /// next window that presents it. This presentation keeps an empty view.
    pub(crate) fn retain_view(&self) {
        let view = self
            .reading
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain();
        *self.retained_view.lock().unwrap_or_else(|e| e.into_inner()) = Some(view);
    }
    /// Sends `hint` a data-free signal whenever the status, connection generation, directory
    /// progress, controller, dormancy or start confirmation may have changed, so a view reads
    /// them on change instead of on a timer.
    pub(crate) fn observe(&self, hint: async_channel::Sender<()>) {
        self.changes.observe(hint);
    }
    fn changed(&self) {
        self.changes.publish();
    }
    pub(crate) fn startup_pending(&self) -> bool {
        *self.admission.lock().unwrap_or_else(|e| e.into_inner()) == Admission::Pending
            || self.start_unconfirmed()
    }
    fn admission(&self, admission: Admission) {
        *self.admission.lock().unwrap_or_else(|e| e.into_inner()) = admission;
        self.changed();
    }
    /// Who controlled the Pane when the server last offered Take Over, if it ever did.
    pub(crate) fn controller(&self) -> Option<pane::control::Controller> {
        self.controller
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub(crate) fn session_end(&self) -> Option<pane::SessionEnd> {
        *self.session_end.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn set_session_end(&self, end: Option<pane::SessionEnd>) {
        *self.session_end.lock().unwrap_or_else(|e| e.into_inner()) = end;
    }
    /// Whether the current connection takes interactive terminal input yet.
    ///
    /// Only a Connected status accepts input: attachment marks the connection interactive before
    /// the status becomes Connected. The status lock is held while the flag is read, because an
    /// invalidation leaves Connected and clears the flag under that lock, so an interrupted
    /// connection reads as Pending.
    pub(crate) fn interactive_input(&self) -> super::InteractiveInput {
        let status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        let interactive = *status == CorePaneStatus::Connected
            && self
                .interaction
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .interactive;
        drop(status);
        if interactive {
            super::InteractiveInput::Accepted
        } else {
            super::InteractiveInput::Pending
        }
    }
    pub(crate) fn connection_generation(&self) -> u64 {
        self.connected_epoch.load(Ordering::Acquire)
    }
    /// A bound creation requests one start under the creator's existing control grant.
    pub(crate) fn start_created(&self) {
        if !self.start_unconfirmed() {
            self.created_start.store(true, Ordering::Release);
            self.wake.notify();
            self.changed();
        }
    }

    /// Selecting a Pane starts or acquires it, except a Pane whose Session ended in this server
    /// run: that one waits on Session Ended for Start Session, whoever holds its grant.
    /// Records whether the person has this Pane open as the Focused Pane of their open Tab and
    /// Workspace. Only a Core restart reads it: the Pane the person was using starts its fresh
    /// shell at once.
    pub(crate) fn set_foreground(&self, foreground: bool) {
        self.foreground.store(foreground, Ordering::Release);
    }

    pub(crate) fn foreground(&self) -> bool {
        self.foreground.load(Ordering::Acquire)
    }

    /// Whether this Pane's worker is starting its shell: one that has not attached yet, or one
    /// this client started that has not reached its first prompt.
    pub(crate) fn shell_starting(&self) -> bool {
        self.shell_starting.load(Ordering::Acquire) || self.startup_quiet()
    }

    /// Whether the shell this client started keeps its surface quiet until its first prompt.
    pub(crate) fn startup_quiet(&self) -> bool {
        self.startup_quiet.load(Ordering::Acquire)
    }

    /// Starts or ends the quiet of the shell this client started.
    pub(crate) fn set_startup_quiet(&self, quiet: bool) {
        if self.startup_quiet.swap(quiet, Ordering::AcqRel) != quiet {
            self.changed();
        }
    }

    /// Records that this Pane's worker starts its shell now.
    pub(crate) fn starting_shell(&self) {
        self.shell_starting.store(true, Ordering::Release);
    }

    /// Whether a background start of this Pane is on its way and unanswered.
    pub(crate) fn preparing(&self) -> bool {
        matches!(
            *self.reservation(),
            Reservation::Starting { session: None, .. }
        )
    }

    fn reservation(&self) -> std::sync::MutexGuard<'_, Reservation> {
        self.background.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether this Pane may still ask Core `boot` for its background start.
    pub(crate) fn will_prepare(&self, boot: [u8; 32]) -> bool {
        *self.prepared.lock().unwrap_or_else(|e| e.into_inner()) != Some(boot)
    }

    /// Whether this Pane may still ask Core `boot` for its background start, and from now on it
    /// may not.
    pub(crate) fn take_prepare(&self, boot: [u8; 32]) -> bool {
        let mut prepared = self.prepared.lock().unwrap_or_else(|e| e.into_inner());
        prepared.replace(boot) != Some(boot)
    }

    /// SpaceTerm Core refused this Pane's background start before admitting anything, so a later
    /// turn may ask Core `boot` again.
    pub(crate) fn release_prepare(&self, boot: [u8; 32]) {
        let mut prepared = self.prepared.lock().unwrap_or_else(|e| e.into_inner());
        if *prepared == Some(boot) {
            *prepared = None;
        }
    }

    /// The worker waits for this Pane's turn to start its shell in the background. A place whose
    /// time ran out waits for a new turn.
    pub(crate) fn want_turn(&self) {
        let mut reservation = self.reservation();
        let lapsed = matches!(
            *reservation,
            Reservation::Starting { until: Some(until), .. } if std::time::Instant::now() >= until
        );
        if *reservation == Reservation::Idle || lapsed {
            *reservation = Reservation::Wanted;
            drop(reservation);
            self.changed();
        }
    }

    /// Whether the worker waits for a turn the window's start queue has not given yet.
    pub(crate) fn awaiting_turn(&self) -> bool {
        *self.reservation() == Reservation::Wanted
    }

    /// Whether this Pane holds a turn its worker has not used yet.
    pub(crate) fn turn_granted(&self) -> bool {
        *self.reservation() == Reservation::Granted
    }

    /// Whether this Pane holds a place in the background start queue: a turn its worker has not
    /// used yet, or a start whose shell has not shown it is ready.
    pub(crate) fn starting_in_background(&self) -> bool {
        match *self.reservation() {
            Reservation::Granted => true,
            // A place whose time ran out lets the queue go on, even before its worker reads it.
            Reservation::Starting { until, .. } => {
                until.is_none_or(|until| std::time::Instant::now() < until)
            }
            Reservation::Idle | Reservation::Wanted => false,
        }
    }

    /// Gives this Pane its turn to start its shell in the background, and wakes its worker.
    pub(crate) fn grant_turn(&self) {
        let mut reservation = self.reservation();
        if *reservation == Reservation::Wanted {
            *reservation = Reservation::Granted;
            drop(reservation);
            self.wake.notify();
        }
    }

    /// Uses the turn this Pane holds, if any. The place stays held, with no moment between the
    /// turn and the start, until [`Self::prepared`] and readiness or [`Self::end_background`].
    pub(crate) fn take_turn(&self) -> bool {
        let mut reservation = self.reservation();
        if *reservation == Reservation::Granted {
            *reservation = Reservation::Starting {
                session: None,
                until: None,
            };
            true
        } else {
            false
        }
    }

    /// SpaceTerm Core started Session `session` for this Pane's background start. Its place stays
    /// held until the shell shows it is ready, or [`BACKGROUND_READY_LIMIT`] after `now`.
    pub(crate) fn prepared(&self, session: [u8; 32], now: std::time::Instant) {
        let mut reservation = self.reservation();
        if matches!(*reservation, Reservation::Starting { .. }) {
            *reservation = Reservation::Starting {
                session: Some(session),
                until: Some(now + BACKGROUND_READY_LIMIT),
            };
        }
    }

    /// The reply to this Pane's background start was lost after the start may have reached
    /// SpaceTerm Core, which may be starting its shell now. Its place stays held until what Core
    /// shows settles the start, or until `until`.
    pub(crate) fn unconfirmed_background(&self, until: std::time::Instant) {
        let mut reservation = self.reservation();
        if matches!(*reservation, Reservation::Starting { session: None, .. }) {
            *reservation = Reservation::Starting {
                session: None,
                until: Some(until),
            };
        }
    }

    /// A background start whose reply was lost settled on Session `session` of this Pane, which
    /// it or another start began. Its place lasts until that shell is ready or its time runs out.
    pub(crate) fn settled_background(&self, session: [u8; 32]) {
        let mut reservation = self.reservation();
        if let Reservation::Starting {
            session: None,
            until: Some(until),
        } = *reservation
        {
            *reservation = Reservation::Starting {
                session: Some(session),
                until: Some(until),
            };
        }
    }

    /// Gives up this Pane's place in the background start queue: its start failed, admitted
    /// nothing, or was cancelled.
    pub(crate) fn end_background(&self) {
        let mut reservation = self.reservation();
        if *reservation != Reservation::Idle {
            *reservation = Reservation::Idle;
            drop(reservation);
            self.changed();
        }
    }

    /// When the started shell's place lapses without readiness, if one is held.
    pub(crate) fn background_deadline(&self) -> Option<std::time::Instant> {
        match *self.reservation() {
            Reservation::Starting { until, .. } => until,
            _ => None,
        }
    }

    /// Ends the place of the shell this Pane started in the background once Pane `pane`'s
    /// published Terminal Metadata shows that Session at its prompt or naming itself, or its
    /// time ran out at `now`. Returns whether the place ended.
    pub(crate) fn settle_background(&self, pane: [u8; 32], now: std::time::Instant) -> bool {
        let Reservation::Starting {
            session: Some(session),
            until: Some(until),
        } = *self.reservation()
        else {
            return false;
        };
        if now < until && !self.session_ready(pane, session) {
            return false;
        }
        self.end_background();
        true
    }

    /// Whether SpaceTerm Core's latest inventory shows Session `session` of Pane `pane` started:
    /// its shell reached a prompt, finished a command or named itself with a terminal title. A
    /// title derived from a directory the shell reported is no name: zsh's `chpwd` reports one
    /// from its startup files, before the first prompt.
    pub(crate) fn session_ready(&self, pane: [u8; 32], session: [u8; 32]) -> bool {
        use spaceterm_core::pane::metadata::{PromptZone, TitleProvenance};
        let Some((_, revision)) = self
            .directory_core()
            .and_then(|core| core.live_link())
            .and_then(|link| link.inventory())
        else {
            return false;
        };
        let inventory = &revision.inventory;
        // The live Session facts name the Session the metadata belongs to, when published.
        let current = inventory.close.as_ref().is_none_or(|close| {
            close
                .sessions
                .iter()
                .any(|confirmed| confirmed.id == pane && confirmed.session == Some(session))
        });
        current
            && inventory
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.tabs)
                .flat_map(|tab| &tab.panes)
                .find(|candidate| candidate.id == pane)
                .and_then(|candidate| candidate.presentation.as_deref())
                .is_some_and(|metadata| {
                    metadata.prompt_zone != PromptZone::Unknown
                        || metadata.finished_commands > 0
                        || metadata.title_provenance == TitleProvenance::TerminalControl
                })
    }

    /// Whether this Pane's Core starts a retained shell in the background without granting
    /// control: one whose live work connection selected work 1.4 or later.
    pub(crate) fn prepares(&self) -> bool {
        self.directory_core().is_some_and(|core| {
            core.capabilities()
                .work()
                .is_ok_and(|work| work.minor >= spaceterm_core::pane::MINOR_PREPARE)
        })
    }

    /// Whether this Pane's Core offered work 1.4 at its latest admission, so it starts a
    /// retained shell in the background once its live work connection selects that version, or
    /// whether no admission of the route said what it offers yet, as while a replaced Core
    /// returns.
    pub(crate) fn offers_prepare(&self) -> bool {
        self.directory_core().is_some_and(|core| {
            core.core_versions().is_none_or(|offered| {
                offered.work.major == spaceterm_core::protocol::version::WORK.major
                    && offered.work.minor >= spaceterm_core::pane::MINOR_PREPARE
            })
        })
    }

    /// The run of the Core whose inventory the route's live link holds, when it holds one.
    fn live_boot(&self) -> Option<[u8; 32]> {
        let (_, revision) = self.directory_core()?.live_link()?.inventory()?;
        revision.inventory.close.as_ref().map(|close| close.boot)
    }

    /// Whether the route's live inventory lists Pane `pane`.
    pub(crate) fn listed(&self, pane: [u8; 32]) -> bool {
        self.directory_core()
            .and_then(|core| core.live_link())
            .and_then(|link| link.inventory())
            .is_some_and(|(_, revision)| {
                revision
                    .inventory
                    .workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.tabs)
                    .flat_map(|tab| &tab.panes)
                    .any(|candidate| candidate.id == pane)
            })
    }

    /// Asks this Pane's worker, which stopped reconnecting after a failure that needs the
    /// person, to connect once more, at most once per Core run, while the route reaches that
    /// run. A retained Pane whose worker gave up while its Core was replaced then still waits
    /// for its background turn instead of staying unstarted. Returns whether it asked.
    pub(crate) fn rearm_background(&self) -> bool {
        let parked = matches!(
            self.status(),
            CorePaneStatus::Failed(error) if error != CoreError::DirectoryUnavailable
        );
        if !parked {
            return false;
        }
        let Some(boot) = self.live_boot() else {
            return false;
        };
        let mut rearmed = self.rearmed.lock().unwrap_or_else(|e| e.into_inner());
        if *rearmed == Some(boot) {
            return false;
        }
        *rearmed = Some(boot);
        drop(rearmed);
        self.retry();
        true
    }

    pub(crate) fn select(&self) {
        let status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if !self.start_unconfirmed()
            && self.session_end().is_none()
            && (*status != CorePaneStatus::Connecting
                || *self.admission.lock().unwrap_or_else(|e| e.into_inner()) != Admission::Granted)
            && matches!(
                *status,
                CorePaneStatus::Connecting | CorePaneStatus::Restored | CorePaneStatus::Takeover
            )
        {
            self.selected.store(true, Ordering::Release);
            self.retry.store(true, Ordering::Release);
            self.wake.notify();
            self.changed();
        }
    }
    pub(crate) fn retry(&self) {
        self.connection_recovery
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retry();
        self.retry.store(true, Ordering::Release);
        self.wake.notify();
        self.changed();
    }
    /// The original shell start may still be in flight. Retry observes it without starting again.
    pub(crate) fn start_unconfirmed(&self) -> bool {
        self.connection_recovery
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .start_unconfirmed()
    }
    pub(crate) fn start_waiting(&self) -> bool {
        self.start_restored.load(Ordering::Acquire) || self.created_start.load(Ordering::Acquire)
    }
    /// What the person typed that the Core refused or never confirmed, since the Pane last
    /// attached to a Terminal Session.
    pub(crate) fn unsent_input(&self) -> Option<unsent::UnsentInput> {
        self.unsent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .input()
    }
    /// Keeps the text of `events`, which the Core refused or never confirmed. `hidden` says
    /// they were typed at a hidden prompt, so only the loss is kept.
    fn keep_input<'a>(
        &self,
        delivery: unsent::Delivery,
        events: impl IntoIterator<Item = &'a spaceterm_core::protocol::InputEvent>,
        hidden: bool,
    ) {
        self.unsent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keep(delivery, events, hidden);
    }
    /// The Pane attached to a Terminal Session. One that survived what interrupted it says
    /// once in the Pane's status what it kept. A notice the Pane has not shown yet is merged
    /// with the new one, never replaced.
    fn attached_session(&self) {
        let Some(notice) = self
            .unsent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .attached()
        else {
            return;
        };
        let mut pending = self.unsent_notice.lock().unwrap_or_else(|e| e.into_inner());
        pending.notice = Some(match pending.notice.take() {
            Some(earlier) => earlier.then(notice),
            None => notice,
        });
    }
    /// What the Pane's status says once about a Terminal Session that survived the idle restart
    /// grant that refused its input.
    fn take_unsent_notice(&self) -> unsent::UnsentChange {
        std::mem::take(&mut *self.unsent_notice.lock().unwrap_or_else(|e| e.into_inner()))
    }
    /// The Terminal Session the kept input was for ended or belongs to another client.
    fn left_session(&self) {
        let unshown = {
            let mut pending = self.unsent_notice.lock().unwrap_or_else(|e| e.into_inner());
            pending.session_left = true;
            pending.notice.take()
        };
        self.unsent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .left_session(unshown);
    }
    pub(crate) fn start_restored(&self) {
        let status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if !self.start_unconfirmed()
            && matches!(
                *status,
                CorePaneStatus::Restored
                    | CorePaneStatus::SessionEnded(_)
                    | CorePaneStatus::Failed(CoreError::DirectoryUnavailable)
                    | CorePaneStatus::Reconnecting
                    | CorePaneStatus::TakeoverReconnecting
            )
            && !self.directory_busy.swap(true, Ordering::AcqRel)
        {
            *self
                .directory_note
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            self.start_restored.store(true, Ordering::Release);
            self.wake.notify();
            self.changed();
        }
    }
    pub(crate) fn directory_progress(&self) -> (bool, Option<&'static str>) {
        (
            self.directory_busy.load(Ordering::Acquire),
            *self
                .directory_note
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        )
    }
    pub(crate) fn recovery_target(&self) -> Option<pane::directories::Target> {
        self.recovery
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub(crate) fn directory_core(&self) -> Option<MachineConnection> {
        self.directory_core
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub(crate) fn repair_directory(
        &self,
        target: pane::directories::Target,
        replacement: std::path::PathBuf,
    ) {
        if self.status() == CorePaneStatus::Failed(CoreError::DirectoryUnavailable)
            && !self.start_unconfirmed()
            && self.recovery_target().as_ref() == Some(&target)
        {
            self.directory_busy.store(true, Ordering::Release);
            *self
                .directory_note
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            *self.repair.lock().unwrap_or_else(|e| e.into_inner()) = Some((target, replacement));
            self.wake.notify();
            self.changed();
        }
    }
    pub(crate) fn take_over(&self) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if *status == CorePaneStatus::Takeover && !self.start_unconfirmed() {
            // This action supersedes earlier selection; a later eligible selection stays queued.
            self.selected.store(false, Ordering::Release);
            *status = CorePaneStatus::TakingOver;
            self.take_over.store(true, Ordering::Release);
            self.retry.store(true, Ordering::Release);
            self.wake.notify();
            self.changed();
        }
    }
    /// The server offered Take Over and named who controls the Pane. Every offer replaces the
    /// last one, so the Takeover Screen follows control as it moves. A Take Over in progress
    /// stays in progress until the server answers it.
    fn offered_take_over(
        &self,
        controller: pane::control::Controller,
        session_ended: Option<pane::SessionEnd>,
    ) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        self.set_session_end(session_ended);
        *self.controller.lock().unwrap_or_else(|e| e.into_inner()) = Some(controller);
        if *status != CorePaneStatus::TakingOver {
            *status = CorePaneStatus::Takeover;
        }
        drop(status);
        self.changed();
    }
    fn invalidate(&self) -> u64 {
        self.reading
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .disconnect();
        // A new connection epoch resolves Selection only against semantics it received itself.
        *self.accessibility.lock().unwrap_or_else(|e| e.into_inner()) =
            accessibility::RetainedSemantics::default();
        self.effect_authority.client_focus(None);
        self.client_effects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.hidden_input.store(false, Ordering::Release);
        *self.line.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if *status == CorePaneStatus::Connected {
            *status = CorePaneStatus::Reconnecting;
        }
        self.paste_unconfirmed.store(false, Ordering::Release);
        self.interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .disconnect();
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.connected_epoch.store(epoch, Ordering::Release);
        // A worker waiting for pushed events learns of the new epoch at once.
        self.wake.notify();
        drop(status);
        self.changed();
        epoch
    }
    /// Starts a connection epoch whose focus continues above every generation this client
    /// sent for the Pane, including through presentations that preceded this one.
    /// An exhausted sequence fails closed rather than reusing a generation.
    fn attach(&self, presenter: &control::Presenter) -> Result<u64, CoreError> {
        presenter
            .with_floor(|floor| {
                let next = floor.checked_add(1).ok_or(CoreError::Unsupported)?;
                let epoch = self.invalidate();
                let mut interaction = self.interaction.lock().unwrap_or_else(|e| e.into_inner());
                interaction.focus_generation = interaction.focus_generation.max(next);
                Ok(epoch)
            })
            .unwrap_or(Err(CoreError::NotRunning))
    }
    fn restart_announced(&self) -> bool {
        self.directory_core()
            .and_then(|core| core.update_facts())
            .is_some_and(|facts| restart::CoreRestart::of(&facts).is_some())
    }
    fn retain_start_after(&self, error: CoreError) -> bool {
        matches!(
            error,
            CoreError::ServiceBusy | CoreError::ActivationUnresolved
        ) || (matches!(
            error,
            CoreError::NotRunning | CoreError::Protocol | CoreError::Direct(DirectError::Transport)
        ) && self.restart_announced())
    }
    fn begin_restart_wait(&self) {
        if self.restart_announced() {
            self.connection_recovery
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .restart_wait(std::time::Instant::now());
        }
    }
    fn restart_expired(&self) -> bool {
        self.connection_recovery
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .restart_expired(std::time::Instant::now())
    }
    fn connection_failed(&self, error: CoreError, has_takeover: bool) -> bool {
        self.retaining_input.store(true, Ordering::Release);
        // An admitted server update refuses connections only until it resolves, so the Pane
        // keeps reconnecting on its backoff in every client, whoever started the update.
        let mut retryable = matches!(
            error,
            CoreError::DirectoryUnavailable
                | CoreError::Protocol
                | CoreError::NotRunning
                | CoreError::StaleControl
                | CoreError::Controlled
                | CoreError::ActivationUnresolved
                | CoreError::ServiceBusy
                | CoreError::Direct(DirectError::Transport)
        );
        let restarting = retryable
            && (matches!(
                error,
                CoreError::ServiceBusy | CoreError::ActivationUnresolved
            ) || self
                .directory_core()
                .and_then(|core| core.update_facts())
                .is_some_and(|facts| restart::CoreRestart::of(&facts).is_some()));
        let error = if retryable
            && (error != CoreError::DirectoryUnavailable || self.start_unconfirmed())
        {
            let (cause, allowed) = self
                .connection_recovery
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .failed(error, std::time::Instant::now());
            retryable = allowed;
            cause
        } else {
            error
        };
        if restarting {
            retryable = self
                .connection_recovery
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .restart_wait(std::time::Instant::now());
        }
        let error = if self.restart_expired() {
            retryable = false;
            CoreError::Protocol
        } else {
            error
        };
        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            // Take Over is scoped to the old control target. Unsent starts keep their intent.
            self.take_over.store(false, Ordering::Release);
            if !retryable {
                self.start_restored.store(false, Ordering::Release);
                self.created_start.store(false, Ordering::Release);
            }
            self.directory_busy.store(false, Ordering::Release);
            self.repair.lock().unwrap_or_else(|e| e.into_inner()).take();

            if !retryable {
                self.selected.store(false, Ordering::Release);
            }
            *status = if error == CoreError::DirectoryUnavailable {
                CorePaneStatus::Failed(error)
            } else if has_takeover && retryable {
                CorePaneStatus::TakeoverReconnecting
            } else if has_takeover {
                CorePaneStatus::TakeoverFailed(error)
            } else if retryable {
                CorePaneStatus::Reconnecting
            } else {
                CorePaneStatus::Failed(error)
            };
        }
        if !retryable
            || !matches!(
                error,
                CoreError::Protocol
                    | CoreError::NotRunning
                    | CoreError::ActivationUnresolved
                    | CoreError::ServiceBusy
            )
        {
            self.admission(Admission::Waiting);
        }
        self.changed();
        retryable
    }

    /// A confirmed inventory proves the shared route is reachable again. Renew only a direct
    /// transport outage; an identity refusal or exhausted protocol recovery still needs action.
    fn core_reachable(&self) {
        let status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(
            *status,
            CorePaneStatus::Reconnecting
                | CorePaneStatus::TakeoverReconnecting
                | CorePaneStatus::Failed(CoreError::Direct(DirectError::Transport))
                | CorePaneStatus::TakeoverFailed(CoreError::Direct(DirectError::Transport))
        ) && self
            .connection_recovery
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .direct_reachable()
        {
            self.retry.store(true, Ordering::Release);
            self.wake.notify();
        }
    }
    /// Sets the worker's status, as a test of the window's start queue needs.
    #[cfg(test)]
    pub(crate) fn set_status_for_test(&self, status: CorePaneStatus) {
        self.set(status);
    }
    fn set(&self, status: CorePaneStatus) {
        let mut current = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if status != CorePaneStatus::Connecting {
            self.shell_starting.store(false, Ordering::Release);
        }
        // Only a restored Pane waits for a background start, or is about to use its turn. A
        // started shell keeps its place until it shows it is ready, unless it ended or failed.
        let mut reservation = self.reservation();
        match (*reservation, &status) {
            (_, CorePaneStatus::Restored | CorePaneStatus::Connecting) => {}
            (
                Reservation::Starting {
                    session: Some(_), ..
                },
                CorePaneStatus::Takeover | CorePaneStatus::TakingOver,
            ) => {}
            _ => *reservation = Reservation::Idle,
        }
        drop(reservation);
        if status != CorePaneStatus::Connected {
            self.reading
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .disconnect();
            self.effect_authority.client_focus(None);
            self.hidden_input.store(false, Ordering::Release);
            *self.line.lock().unwrap_or_else(|e| e.into_inner()) = None;
            self.paste_unconfirmed.store(false, Ordering::Release);
        }
        *current = status;
        drop(current);
        self.changed();
    }
    fn connected(
        &self,
        epoch: u64,
        screen: ScreenMapping,
        paste_unconfirmed: bool,
    ) -> Result<(), CoreError> {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if self.epoch.load(Ordering::Acquire) != epoch {
            return Err(CoreError::Protocol);
        }
        {
            let mut interaction = self.interaction.lock().unwrap_or_else(|e| e.into_inner());
            let mapping = Some(screen).filter(|mapping| {
                interaction
                    .requested_size
                    .is_none_or(|size| size == mapping.size)
            });
            let same_geometry = interaction
                .screen
                .zip(mapping)
                .is_some_and(|(old, new)| old.epoch == new.epoch && old.size == new.size);
            if !same_geometry {
                interaction.pointer_geometry_start = mapping.map(|mapping| mapping.local);
            }
            interaction.screen = mapping;
        }
        let previous = self.connected_epoch.swap(epoch, Ordering::AcqRel);
        if paste_unconfirmed {
            self.paste_unconfirmed.store(true, Ordering::Release);
        }
        let changed = previous != epoch || *status != CorePaneStatus::Connected;
        if *status != CorePaneStatus::Connected {
            let interaction = self.interaction.lock().unwrap_or_else(|e| e.into_inner());
            self.effect_authority
                .client_focus(Some(interaction.target == pane::FocusTarget::Terminal));
        }
        *status = CorePaneStatus::Connected;
        self.shell_starting.store(false, Ordering::Release);
        // This client presents the Pane's shell now, so it no longer waits in the queue.
        *self.reservation() = Reservation::Idle;
        drop(status);
        // Each frame reaches here. Only a new status or generation concerns a view.
        if changed {
            self.changed();
        }
        Ok(())
    }
}
#[derive(Clone)]
pub(crate) struct CoreSessionFactory {
    core: MachineConnection,
    local_files: Option<crate::platform::local_filesystem::LocalFilesystemAuthority>,
    remote_metadata: Option<super::metadata::RemoteTerminalMetadataContext>,
    pane_id: spaceterm_core::hierarchy::Id,
    attachment: Option<spaceterm_core::attachments::AttachmentPermit>,
    control: Arc<control::PaneControl>,
    state: Arc<CorePaneState>,
    /// The desktop's counters, which this Pane's screen decoding and semantics record into.
    counters: Arc<spaceterm_core::diagnostics::TerminalCounters>,
}
impl CoreSessionFactory {
    /// Presents one server Pane. Construction connects to nothing.
    pub(crate) fn for_pane(
        core: MachineConnection,
        controls: &PaneControls,
        local_files: crate::platform::local_filesystem::LocalFilesystemAuthority,
        pane_id: spaceterm_core::hierarchy::Id,
        counters: Arc<spaceterm_core::diagnostics::TerminalCounters>,
    ) -> (Self, Arc<CorePaneState>) {
        let local_files = core.is_account_local().then_some(local_files);
        Self::for_origin(core, controls, local_files, None, pane_id, counters)
    }

    pub(crate) fn for_remote_pane(
        core: MachineConnection,
        controls: &PaneControls,
        machine_display: String,
        pane_id: spaceterm_core::hierarchy::Id,
        counters: Arc<spaceterm_core::diagnostics::TerminalCounters>,
    ) -> (Self, Arc<CorePaneState>) {
        let metadata = super::metadata::RemoteTerminalMetadataContext::new(
            machine_display,
            crate::domain::RemoteDirectory::home(),
        );
        Self::for_origin(core, controls, None, Some(metadata), pane_id, counters)
    }

    fn for_origin(
        core: MachineConnection,
        controls: &PaneControls,
        local_files: Option<crate::platform::local_filesystem::LocalFilesystemAuthority>,
        remote_metadata: Option<super::metadata::RemoteTerminalMetadataContext>,
        pane_id: spaceterm_core::hierarchy::Id,
        counters: Arc<spaceterm_core::diagnostics::TerminalCounters>,
    ) -> (Self, Arc<CorePaneState>) {
        let control = controls.pane(pane_id);
        // A view a closed window left of this Pane returns with the next window.
        let retained = control
            .view
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let state = Arc::new(CorePaneState {
            connection_recovery: Arc::clone(&control.recovery),
            start_restored: Arc::clone(&control.queued_start),
            created_start: Arc::clone(&control.queued_creation),
            selected: Arc::clone(&control.queued_selection),
            retained_view: Arc::clone(&control.view),
            reading: Mutex::new(retained.unwrap_or_default()),
            ..CorePaneState::new()
        });
        *state
            .directory_core
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(core.clone());
        (
            Self {
                core,
                local_files,
                remote_metadata,
                pane_id,
                attachment: None,
                control,
                state: Arc::clone(&state),
                counters,
            },
            state,
        )
    }
    pub(crate) fn with_attachment(
        mut self,
        permit: spaceterm_core::attachments::AttachmentPermit,
    ) -> Self {
        self.attachment = Some(permit);
        self
    }
}

enum Input {
    ClearScreenAndScrollback,
    Key(KeyInput),
    Commit(String, async_channel::Sender<InputMethodOutcome>),
    CopyOrForward(InputModifiers),
    Pointer(u64, pane::Pointer),
    Wheel(u64, pane::Wheel),
    Paste(
        String,
        async_channel::Sender<Result<super::PasteRequestOutcome, String>>,
    ),
    ResolvePaste(
        PendingPaste,
        super::PasteDecision,
        async_channel::Sender<Result<super::PasteResolution, String>>,
    ),
    Resize(PaneSize),
    Style(pane::PresentationStyle),
}
impl Input {
    fn eligible(&self, target: pane::FocusTarget) -> bool {
        match self {
            Self::ClearScreenAndScrollback => true,
            Self::Commit(..) => matches!(
                target,
                pane::FocusTarget::Terminal | pane::FocusTarget::FocusedPane
            ),
            Self::Key(key) if key.is_input_method_commit() => matches!(
                target,
                pane::FocusTarget::Terminal | pane::FocusTarget::FocusedPane
            ),
            Self::Key(_) | Self::CopyOrForward(_) | Self::Pointer(..) | Self::Wheel(..) => {
                target == pane::FocusTarget::Terminal
            }
            Self::Paste(_, reply) => target == pane::FocusTarget::Terminal && !reply.is_closed(),
            Self::ResolvePaste(pending, _, _) => {
                target == pane::FocusTarget::Terminal
                    || target
                        == (pane::FocusTarget::PasteConfirmation {
                            id: pending.wire_id,
                        })
            }
            Self::Resize(_) | Self::Style(_) => true,
        }
    }
}
struct SubmittedInput {
    epoch: u64,
    focus_generation: u64,
    command: Input,
    /// The program was reading hidden input, such as a password, when the person typed this.
    hidden: bool,
}
#[derive(Clone, Copy)]
struct PendingPaste {
    id: super::paste::PasteConfirmationId,
    wire_id: u64,
    epoch: u64,
    focus_generation: u64,
    deadline: std::time::Instant,
}
struct ResolvingPaste {
    pending: PendingPaste,
    restored_generation: Option<u64>,
}
#[derive(Clone, Copy)]
struct ScreenMapping {
    epoch: u64,
    local: PresentationGeneration,
    wire: u64,
    size: PaneSize,
    mouse_tracking: bool,
    alternate: bool,
    alternate_scroll: bool,
}
struct Interaction {
    interactive: bool,
    reading: bool,
    focus_generation: u64,
    target: pane::FocusTarget,
    screen: Option<ScreenMapping>,
    pointer_geometry_start: Option<PresentationGeneration>,
    requested_size: Option<PaneSize>,
    application_pointer_active: bool,
    pending: Option<PendingPaste>,
    resolving: Option<ResolvingPaste>,
    paste_request: Option<(u64, u64)>,
    next_paste: u64,
}
impl Default for Interaction {
    fn default() -> Self {
        Self {
            interactive: false,
            reading: false,
            focus_generation: 1,
            target: pane::FocusTarget::Unfocused,
            screen: None,
            pointer_geometry_start: None,
            requested_size: None,
            application_pointer_active: false,
            pending: None,
            resolving: None,
            paste_request: None,
            next_paste: 0,
        }
    }
}
impl Interaction {
    fn disconnect(&mut self) {
        self.application_pointer_active = false;
        self.interactive = false;
        self.reading = false;
        self.screen = None;
        self.pointer_geometry_start = None;
        self.pending = None;
        self.resolving = None;
        self.paste_request = None;
        if matches!(self.target, pane::FocusTarget::PasteConfirmation { .. }) {
            self.target = pane::FocusTarget::Unfocused;
        }
        self.focus_generation = self.focus_generation.saturating_add(1);
    }
    fn focus(&mut self, target: pane::FocusTarget) {
        if self.target != target {
            let restore = target == pane::FocusTarget::Terminal
                && matches!(self.target, pane::FocusTarget::PasteConfirmation { .. })
                && self.resolving.is_some();
            self.focus_generation = self.focus_generation.saturating_add(1);
            self.target = target;
            self.pending = None;
            if restore {
                if let Some(resolving) = &mut self.resolving {
                    resolving.restored_generation = Some(self.focus_generation);
                }
            } else {
                self.resolving = None;
            }
        }
    }
    fn wire_focus(&self) -> (u64, pane::FocusTarget) {
        // Modal dismissal restores local focus immediately. Preserve the wire
        // authority until its already-approved paste has been resolved in order.
        if let Some(resolving) = &self.resolving
            && resolving.restored_generation == Some(self.focus_generation)
        {
            return (
                resolving.pending.focus_generation,
                pane::FocusTarget::PasteConfirmation {
                    id: resolving.pending.wire_id,
                },
            );
        }
        (self.focus_generation, self.target)
    }
    fn resolution_is_current(&self, pending: PendingPaste) -> bool {
        self.resolving.as_ref().is_some_and(|resolving| {
            resolving.pending.id == pending.id
                && resolving.pending.epoch == pending.epoch
                && (self.focus_generation == pending.focus_generation
                    || resolving.restored_generation == Some(self.focus_generation))
        })
    }
}
struct PresentationHandle {
    input: mpsc::SyncSender<SubmittedInput>,
    state: Arc<CorePaneState>,
    alive: Arc<AtomicBool>,
    size: Arc<Mutex<PaneSize>>,
    appearance: Arc<Mutex<TerminalAppearanceUpdate>>,
    geometry: Mutex<TerminalGeometry>,
}
impl Drop for PresentationHandle {
    fn drop(&mut self) {
        self.state.effect_authority.client_focus(None);
        self.alive.store(false, Ordering::Release);
        // Wakes the worker, which retires this presentation at once.
        self.state.wake.notify();
    }
}
impl PresentationHandle {
    fn pointer_position(
        &self,
        generation: PresentationGeneration,
        position: super::pointer_input::SurfacePosition,
        continuation: bool,
    ) -> Option<(ScreenMapping, f32, f32)> {
        let interaction = self
            .state
            .interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mapping = interaction.screen?;
        let generation_matches = if continuation {
            interaction
                .pointer_geometry_start
                .is_some_and(|start| start <= generation && generation <= mapping.local)
        } else {
            generation == mapping.local
        };
        if !generation_matches || !position.x.is_finite() || !position.y.is_finite() {
            return None;
        }
        drop(interaction);
        let geometry = *self.geometry.lock().unwrap_or_else(|e| e.into_inner());
        let cell = geometry.logical_cell_size();
        let backing_cell = geometry.to_backing_position(super::geometry::LogicalPosition::new(
            cell.width,
            cell.height,
        ));
        let x = position.x * f32::from(mapping.size.cell_width) / backing_cell.x;
        let y = position.y * f32::from(mapping.size.cell_height) / backing_cell.y;
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        Some((mapping, x, y))
    }
    fn send(&self, input: Input) {
        submit_input(&self.state, &self.input, input);
    }
}

fn pane_size(geometry: TerminalGeometry) -> PaneSize {
    let grid = geometry.grid();
    let cell = geometry.backing_cell_size();
    PaneSize {
        columns: grid.cols.max(2),
        rows: grid.rows.max(2),
        cell_width: cell.width.clamp(1, 256) as u16,
        cell_height: cell.height.clamp(1, 256) as u16,
    }
}
impl TerminalSessionHandle for PresentationHandle {
    fn set_preparation_demand(&self, prepare: bool) {
        if self.state.preparation.swap(prepare, Ordering::AcqRel) != prepare {
            self.state.wake.notify();
        }
    }
    /// Assistive technology selects through this client's own Selection, the one the pointer
    /// sets and Copy reads, so the Pane never holds a second one.
    fn accessibility_selection_sender(&self) -> Option<super::AccessibilitySelectionSender> {
        let state = Arc::clone(&self.state);
        Some(super::AccessibilitySelectionSender::core(Arc::new(
            move |request| {
                let Some(selection) = state
                    .accessibility
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .resolve_selection(&request)
                else {
                    return;
                };
                let changed = {
                    let mut reading = state.reading.lock().unwrap_or_else(|e| e.into_inner());
                    reading.select(selection)
                };
                if changed {
                    state.wake.notify();
                }
            },
        )))
    }
    fn accessibility_demand_sender(&self) -> Option<super::AccessibilityDemandSender> {
        let state = Arc::clone(&self.state);
        Some(super::AccessibilityDemandSender::core(Arc::new(
            move || {
                state.accessibility_demand.store(true, Ordering::Release);
                state.wake.notify();
            },
        )))
    }
    fn set_presentable(&self, presentable: bool) {
        // A hidden Pane keeps its control, its Session, its semantic demand and its document;
        // only its visual demand ends.
        if self.state.presentable.swap(presentable, Ordering::AcqRel) != presentable {
            if !presentable {
                // A hidden view continues no drag; the server cancels its own on hidden demand.
                self.cancel_gesture();
            }
            self.state.wake.notify();
        }
    }
    fn reading_available(&self) -> bool {
        self.state.status() == CorePaneStatus::Connected
            && self
                .state
                .interaction
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .reading
    }
    fn copy_or_forward(
        &self,
        modifiers: InputModifiers,
    ) -> Result<Option<super::SelectionCopy>, super::SelectionCopyError> {
        if self.selection_present() {
            return Ok(self.copy());
        }
        self.send(Input::CopyOrForward(modifiers));
        Ok(None)
    }
    fn selection_copy_ready(&self) -> bool {
        self.copy_ready()
    }
    fn key(&self, k: KeyInput) {
        if k.validate().is_err()
            || k.text
                .as_ref()
                .is_some_and(|s| s.len() > pane::MAX_KEY_TEXT_BYTES)
            || k.logical_key.len() > 128
        {
            return;
        }
        if !k.is_modifier_key() && matches!(k.action, KeyAction::Press | KeyAction::Repeat) {
            self.typed();
        }
        self.send(Input::Key(k));
    }
    fn commit_input_method(
        &self,
        text: &str,
    ) -> Result<async_channel::Receiver<InputMethodOutcome>, InputMethodRefusal> {
        if text.len() > MAX_INPUT_METHOD_BYTES {
            return Err(InputMethodRefusal::TooLarge);
        }
        let (reply, receiver) = async_channel::bounded(1);
        self.typed();
        if submit_input(
            &self.state,
            &self.input,
            Input::Commit(text.to_owned(), reply),
        ) {
            Ok(receiver)
        } else {
            Err(InputMethodRefusal::Unavailable)
        }
    }
    fn resize(&self, geometry: TerminalGeometry) {
        let size = pane_size(geometry);
        *self.geometry.lock().unwrap_or_else(|e| e.into_inner()) = geometry;
        {
            let mut interaction = self
                .state
                .interaction
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            interaction.requested_size = Some(size);
            interaction.screen = interaction.screen.filter(|mapping| mapping.size == size);
            if interaction.screen.is_none() {
                interaction.pointer_geometry_start = None;
            }
        }
        *self.size.lock().unwrap_or_else(|e| e.into_inner()) = size;
        self.send(Input::Resize(size));
    }
    fn take_client_effects(&self) -> Vec<ClientEffectRequest> {
        self.state
            .client_effects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }
    fn hidden_input(&self) -> Option<bool> {
        Some(self.state.hidden_input.load(Ordering::Acquire))
    }
    fn foreground_job(&self) -> Option<bool> {
        let line = *self.state.line.lock().unwrap_or_else(|e| e.into_inner());
        let (epoch, mode) = line?;
        // A Line Mode from an earlier connection describes another Terminal Session, and an
        // unreadable PTY says nothing about what runs.
        (epoch == self.state.epoch.load(Ordering::Acquire) && mode != pane::LineMode::UNKNOWN)
            .then_some(!mode.shell_foreground)
    }
    fn focus(&self, focused: bool) {
        self.interactive_focus(focused, focused);
    }
    fn interactive_focus(&self, terminal_input: bool, input_method: bool) {
        let target = if terminal_input {
            pane::FocusTarget::Terminal
        } else if input_method {
            pane::FocusTarget::FocusedPane
        } else {
            pane::FocusTarget::Unfocused
        };
        let mut interaction = self
            .state
            .interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if interaction.target != target {
            self.state
                .effect_authority
                .client_focus(if interaction.interactive {
                    Some(terminal_input)
                } else {
                    None
                });
        }
        interaction.focus(target);
        drop(interaction);
        self.state.wake.notify();
    }
    fn interactive_input(&self) -> super::InteractiveInput {
        self.state.interactive_input()
    }
    fn input_generation(&self) -> u64 {
        self.state.epoch.load(Ordering::Acquire)
    }
    fn take_paste_unconfirmed(&self) -> bool {
        self.state.paste_unconfirmed.swap(false, Ordering::AcqRel)
    }
    fn take_unsent_input(&self) -> unsent::UnsentChange {
        self.state.take_unsent_notice()
    }
    fn focus_paste_confirmation(&self, id: super::paste::PasteConfirmationId) {
        let mut interaction = self
            .state
            .interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.state.effect_authority.client_focus(Some(false));
        if let Some(pending) = interaction.pending.filter(|pending| {
            pending.id == id
                && pending.epoch == self.state.epoch.load(Ordering::Acquire)
                && pending.focus_generation == interaction.focus_generation
                && std::time::Instant::now() < pending.deadline
        }) {
            interaction.target = pane::FocusTarget::PasteConfirmation {
                id: pending.wire_id,
            };
        } else {
            self.state.effect_authority.client_focus(Some(false));
            interaction.focus(pane::FocusTarget::Unfocused);
        }
        drop(interaction);
        self.state.wake.notify();
    }
    fn clear_screen_and_scrollback(&self) {
        self.send(Input::ClearScreenAndScrollback);
    }
    fn pointer(&self, input: PointerInput) {
        self.route_pointer(input);
    }
    fn pointer_and_copy_selection(
        &self,
        input: PointerInput,
    ) -> Result<Option<super::selection::SelectionCopy>, SelectionCopyError> {
        Ok(self.route_pointer(input))
    }
    fn wheel(&self, input: WheelInput) {
        self.route_wheel(input);
    }
    fn scroll_to(&self, offset: u64, _generation: PresentationGeneration) {
        self.read(|scrollback| scrollback.scroll_to(offset));
    }
    fn scroll_scrollback(&self, movement: ScrollbackMovement) {
        self.read(|scrollback| scrollback.scroll(movement));
    }
    fn set_find_query(&self, generation: super::FindQueryGeneration, text: String) {
        if text.len() <= super::find::MAX_QUERY_BYTES {
            // The query outlives a connection, so a Pane that opens with Find open finds once
            // its screen arrives.
            self.local(|reading| ((), reading.scrollback.set_find_query(generation, text)));
        } else {
            // Retire the previous search while the field presents this generation's limit.
            self.end_find(generation);
        }
    }
    fn navigate_find(
        &self,
        generation: super::FindQueryGeneration,
        direction: super::FindDirection,
    ) {
        self.read(|scrollback| scrollback.navigate_find(generation, direction));
    }
    fn end_find(&self, generation: super::FindQueryGeneration) {
        self.local(|reading| ((), reading.scrollback.end_find(generation)));
    }
    fn request_paste(
        &self,
        payload: super::PastePayload,
    ) -> async_channel::Receiver<Result<super::PasteRequestOutcome, String>> {
        let (tx, rx) = async_channel::bounded(1);
        if payload.text().len() > super::paste::MAX_PASTE_BYTES {
            let _ = tx.try_send(Ok(super::PasteRequestOutcome::Rejected(
                super::paste::PasteRejection::TooLarge {
                    limit: super::paste::MAX_PASTE_BYTES,
                },
            )));
            return rx;
        }
        self.typed();
        self.send(Input::Paste(payload.text().to_owned(), tx));
        rx
    }
    fn resolve_paste(
        &self,
        id: super::paste::PasteConfirmationId,
        decision: super::PasteDecision,
    ) -> async_channel::Receiver<Result<super::PasteResolution, String>> {
        let (tx, rx) = async_channel::bounded(1);
        let pending = {
            let mut interaction = self
                .state
                .interaction
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if interaction.pending.is_some_and(|pending| pending.id == id) {
                let pending = interaction.pending.take();
                if let Some(pending) = pending {
                    interaction.resolving = Some(ResolvingPaste {
                        pending,
                        restored_generation: None,
                    });
                }
                pending
            } else {
                None
            }
        };
        if let Some(pending) = pending {
            self.send(Input::ResolvePaste(pending, decision, tx));
        } else {
            let _ = tx.try_send(Ok(super::PasteResolution::Stale));
        }
        rx
    }
    fn copy_selection(&self) -> Result<Option<super::SelectionCopy>, SelectionCopyError> {
        Ok(self.copy())
    }
    fn take_prepared_pointer_copy(&self) -> Option<super::SelectionCopy> {
        self.take_deferred_copy()
    }
    /// The Selection is this client's own and keeps its rows while output arrives, so the copy
    /// of an earlier presentation is the current Selection's.
    fn copy_selection_at(
        &self,
        _generation: PresentationGeneration,
    ) -> Result<Option<super::SelectionCopy>, SelectionCopyError> {
        Ok(self.copy())
    }
    fn update_appearance(&self, update: TerminalAppearanceUpdate) {
        let style = encode_style(&update);
        *self.appearance.lock().unwrap_or_else(|e| e.into_inner()) = update;
        self.send(Input::Style(style));
    }
}
