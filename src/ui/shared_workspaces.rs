//! Shared server Workspaces inside the main Workspace Window.
//!
//! A SpaceTerm Server owns these Workspaces, their Tabs, their Panes, and the Terminal Sessions
//! the Panes present. Normal launch presents the account server's Workspaces this client has
//! attached beside any Remote Workspaces the window owns. Another client's Workspace joins as
//! membership and never changes the Active Workspace.
//!
//! Every shared change goes to the server as one [`Mutation`]. Connected interactions project
//! their presentation immediately over authoritative membership. Provisional identities open no
//! Terminal Session and grant no input authority. A refusal recomputes the affected presentation
//! without replacing later intent, and an offline client queues no new shared work.
//!
//! Closing a shared Pane or Tab, and Kill Workspace, end Terminal Sessions for every client. Each
//! one asks the server to end exactly the Panes and Sessions a person reviewed, so a confirmation
//! that another client's change made stale ends nothing.
//!
//! The server reads Repository Status and Worktree inventory for every directory its Workspaces
//! show, so inactive Workspaces stay current too. This client adds its own Git Settings and Pull
//! Requests from its own GitHub CLI, and never reads git for a shared Workspace itself.
//!
pub(crate) use development::ArrangementUnavailable;
pub(crate) use development::{
    DirectServer, DrawnAnswer, OutcomeNotice, OutcomeOwner, OutcomeTicket, SharedAcceptance,
    SharedAttachmentFailure, SharedCloseApproval, SharedCloseIntent, SharedCloseReview,
    SharedCloseState, SharedCloseUnavailable, SharedEditSource, SharedPaneOpener,
    SharedPaneSessions, SharedPaneSubscription, SharedPanes, SharedRefusal, SharedRefusalReason,
    SharedRemoval, SharedRowStatus, SharedSubmission, SharedTabs, SharedWorkspaceConflict,
    SharedWorkspaceEdits, SharedWorkspaceFacts, SharedWorkspaceHost, SharedWorkspacePlan,
    SharedWorkspaces, presenting_window, shared_close_changed_title,
};
#[cfg(test)]
pub(crate) use development::{WorkspaceOwner, testing};

#[path = "shared_workspaces/repositories.rs"]
mod repositories;
pub(crate) use repositories::{
    LocalPresentation, SharedRepositories, pane_directory, workspace_directory,
};

#[cfg(test)]
#[path = "shared_workspaces/tests.rs"]
mod tests;

#[path = "shared_workspaces/pending.rs"]
mod pending;

#[path = "shared_workspaces/direct_route.rs"]
mod direct_route;
pub(crate) use direct_route::{
    DirectRoutePanes, PresentationRecord, PresentationRetries, UnavailableEdits,
};

mod development {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::Arc;

    use gpui::{App, SharedString};
    use spaceterm_server::hierarchy::{
        Axis, CloseConfirmation as ServerCloseConfirmation, CloseTarget as ServerCloseTarget,
        ConfirmedControl, Id, Inventory, Mutation, Outcome, RetainedOutcome,
        Workspace as ServerWorkspace,
    };

    use spaceterm_server::client_view::RetainedInventory;
    use spaceterm_server::workspace_order::OwnerId;

    use crate::close_confirmation::CloseTarget;
    use crate::domain::{PaneId, TabId, ValidatedLocalDirectory, WorkspaceId};
    use crate::server_connection::ServerConnection;
    use crate::terminal::WorkspaceTerminalSessionFactory;
    use crate::terminal::server_session::{DevelopmentServerSessionFactory, PaneControls};
    use crate::terminal::shared_workspace::{SharedWorkspaceClient, SharedWorkspaceSnapshot};
    use crate::ui::pane_lifecycle::PaneConstruction;
    use crate::ui::server_pane::{ServerPaneConnection, ServerPaneView};

    /// The one Workspace Window presenting this device's shared Workspaces.
    ///
    /// One device holds one durable edit journal and one claim on each Pane, so a second window
    /// cannot present the same server: it would stay offline behind the journal lock and fence
    /// every Pane the first window holds. Another window activates this one instead.
    pub(crate) struct SharedWorkspaceHost(
        pub(crate) gpui::WindowHandle<crate::ui::WorkspaceManager>,
    );

    impl gpui::Global for SharedWorkspaceHost {}

    /// The live window presenting shared Workspaces. Membership decides this, not a view read:
    /// the root view is leased while an action dispatches inside the window.
    fn host(cx: &App) -> Option<gpui::WindowHandle<crate::ui::WorkspaceManager>> {
        let handle = cx.try_global::<SharedWorkspaceHost>()?.0;
        cx.windows()
            .iter()
            .any(|window| window.window_id() == handle.window_id())
            .then_some(handle)
    }

    /// The other live Workspace Window already presenting this device's shared Workspaces.
    pub(crate) fn presenting_window(
        cx: &App,
    ) -> Option<gpui::WindowHandle<crate::ui::WorkspaceManager>> {
        host(cx)
    }

    /// The server-side edits one desktop client submits, and the last answer it received.
    ///
    /// The implementation owns durable operation identity, reconciliation after a lost reply, and
    /// bounded acknowledgement. This transport accepts one operation at a time; SharedEditSource
    /// orders the bounded connected interactions waiting for it.
    pub(crate) trait SharedWorkspaceEdits {
        fn snapshot(&self) -> SharedWorkspaceSnapshot;

        /// Whether this client is connected, and whether an edit already waits for an answer.
        /// Rendering reads this for every control, so it copies no hierarchy.
        fn availability(&self) -> (bool, bool);

        /// Dispatches one Original Operation, or refuses while offline or resolving another one.
        /// SharedEditSource owns the connected interactions waiting to dispatch.
        fn submit(&self, mutation: Mutation) -> bool;

        /// A data-free hint now and after each change the snapshot publishes. The snapshot
        /// stays the authority.
        fn subscribe(&self) -> async_channel::Receiver<()>;

        /// Whether the server accepts Worktree writes, without copying the hierarchy.
        fn worktree_writes(&self) -> bool {
            self.snapshot().worktree_writes
        }

        /// Reconnects now, rather than after the reconnect delay, while the connection is down.
        fn retry(&self) {}

        /// Records that a surface presented the outcome of `sequence`. The snapshot no longer
        /// lists it as unseen, in this process or a later one.
        fn presented(&self, _sequence: u64) {}

        /// Records that a surface said the first `total` forgotten outcomes went unseen.
        fn forgotten_presented(&self, _total: u64) {}
    }

    impl SharedWorkspaceEdits for SharedWorkspaceClient {
        fn snapshot(&self) -> SharedWorkspaceSnapshot {
            Self::snapshot(self)
        }

        fn worktree_writes(&self) -> bool {
            Self::supports_worktree_writes(self)
        }

        fn availability(&self) -> (bool, bool) {
            Self::availability(self)
        }

        fn submit(&self, mutation: Mutation) -> bool {
            Self::submit(self, mutation)
        }

        fn subscribe(&self) -> async_channel::Receiver<()> {
            Self::subscribe(self)
        }

        fn retry(&self) {
            Self::retry(self);
        }

        fn presented(&self, sequence: u64) {
            Self::presented(self, sequence);
        }

        fn forgotten_presented(&self, total: u64) {
            Self::forgotten_presented(self, total);
        }
    }

    /// Stands for the notice that the bound forgot some unseen outcomes, where a held or handed
    /// sequence is expected. The server never assigns this sequence.
    const FORGOTTEN: u64 = u64::MAX;

    /// An alert's lifecycle observer. When the person answers the alert, with any of its actions
    /// or Escape, `answered` runs. When it closes any other way, as with its window, `unanswered`
    /// runs instead. When the alert reports neither, both drop.
    fn when_answered(
        answered: impl FnOnce() + 'static,
        unanswered: impl FnOnce() + 'static,
    ) -> impl Fn(&spaceterm_ui::ModalLifecycleEvent, &mut App) + 'static {
        type Outcomes = Option<(Box<dyn FnOnce()>, Box<dyn FnOnce()>)>;
        let pending: Rc<std::cell::RefCell<Outcomes>> = Rc::new(std::cell::RefCell::new(Some((
            Box::new(answered),
            Box::new(unanswered),
        ))));
        move |event, _| {
            let spaceterm_ui::ModalLifecycleEvent::Closed(_, reason) = event else {
                return;
            };
            let taken = pending.borrow_mut().take();
            if let Some((answered, unanswered)) = taken {
                match reason {
                    spaceterm_ui::ModalCloseReason::Action
                    | spaceterm_ui::ModalCloseReason::Cancelled => answered(),
                    _ => unanswered(),
                }
            }
        }
    }

    /// Connected interaction intent is bounded and ordered here. The transport journals only
    /// the operation currently dispatched; a disconnected client retains no undispatched work.
    ///
    /// A paired route's transport changes as its connections come and go. Every clone follows
    /// the replacement, and the interactions already queued keep their order.
    #[derive(Clone)]
    pub(crate) struct SharedEditSource {
        edits: Rc<std::cell::RefCell<Rc<dyn SharedWorkspaceEdits>>>,
        /// Counts transport replacements, so a subscription knows to follow the new one.
        generation: Rc<std::cell::Cell<u64>>,
        /// The inventory a paired route last confirmed, presented while its transport has none.
        stand_in: Rc<std::cell::RefCell<Option<Inventory>>>,
        /// Whether the last reading presented the stand-in. Shared edits wait for the server's
        /// own inventory, even on a connected transport.
        standing_in: Rc<std::cell::Cell<bool>>,
        pending: Rc<std::cell::RefCell<super::pending::PendingEdits>>,
        subscribers: Rc<std::cell::RefCell<Vec<async_channel::Sender<()>>>>,
        /// Unseen outcomes a surface holds to present, so no other surface presents them too.
        held: Rc<std::cell::RefCell<BTreeSet<u64>>>,
        /// The latest presented outcomes, newest last, and the forgotten total a surface said
        /// went unseen. A replacement transport records them too.
        acknowledged: Rc<std::cell::RefCell<VecDeque<u64>>>,
        forgotten_acknowledged: Rc<std::cell::Cell<u64>>,
    }

    /// A surface that presents the answer to its own edit, as the New Worktree form presents
    /// its Create's. Until it presents that answer or drops this, no other surface presents it;
    /// dropping it unpresented leaves the answer to the Workspace Window.
    pub(crate) struct OutcomeOwner {
        source: SharedEditSource,
        answer: Rc<std::cell::Cell<Option<u64>>>,
    }

    /// Where [`SharedEditSource::submit_owned`] tells an [`OutcomeOwner`] its answer.
    pub(crate) struct OutcomeTicket(std::rc::Weak<std::cell::Cell<Option<u64>>>);

    impl OutcomeOwner {
        pub(crate) fn ticket(&self) -> OutcomeTicket {
            OutcomeTicket(Rc::downgrade(&self.answer))
        }

        /// The sequence of the owned answer, once it has arrived.
        pub(crate) fn sequence(&self) -> Option<u64> {
            self.answer.get()
        }

        /// Records that the person answered the surface showing the answer, so nothing presents
        /// it again.
        pub(crate) fn presented(&self) {
            if let Some(sequence) = self.answer.take() {
                self.source.presented(sequence);
            }
        }

        /// The observer of the owner's own alert, which records the answer once the person
        /// answers the alert. An alert closed any other way, or never admitted, leaves the
        /// answer to the Workspace Window.
        pub(crate) fn lifecycle(
            self,
        ) -> impl Fn(&spaceterm_ui::ModalLifecycleEvent, &mut App) + 'static + use<> {
            when_answered(move || self.presented(), || {})
        }
    }

    /// Whether the person's response answers what a surface shows. It does only once a render
    /// drew that very answer, which [`Self::drawn`] records by the answer's key.
    #[derive(Default)]
    pub(crate) struct DrawnAnswer(Option<u64>);

    impl DrawnAnswer {
        /// A render drew the answer `key` names.
        pub(crate) fn drawn(&mut self, key: Option<u64>) {
            self.0 = key;
        }

        /// The person responded while the surface held the answer `key` names. Returns whether
        /// a render had drawn that answer, so the response answers it.
        pub(crate) fn answers(&mut self, key: Option<u64>) -> bool {
            let drawn = self.0.take();
            key.is_some() && key == drawn
        }

        /// The surface gave its answer up, or holds a new one no render has drawn yet.
        pub(crate) fn forget(&mut self) {
            self.0 = None;
        }
    }

    impl Drop for OutcomeOwner {
        fn drop(&mut self) {
            if let Some(sequence) = self.answer.take() {
                self.source.release(sequence);
            }
        }
    }

    /// The unseen outcomes one alert presents. The person's answer to the alert records them as
    /// seen; an alert closed any other way leaves them for the next presentation.
    #[derive(Clone)]
    pub(crate) struct OutcomeNotice {
        source: SharedEditSource,
        sequences: Rc<[u64]>,
        /// The forgotten total, for the notice that says how many outcomes went unseen.
        forgotten: Option<u64>,
        /// The reading's outcomes handed to alerts the person has not answered yet.
        handed: Rc<std::cell::RefCell<BTreeSet<u64>>>,
    }

    impl OutcomeNotice {
        /// The alert's lifecycle observer.
        pub(crate) fn lifecycle(
            &self,
        ) -> impl Fn(&spaceterm_ui::ModalLifecycleEvent, &mut App) + 'static + use<> {
            let answered = self.clone();
            let unanswered = self.clone();
            when_answered(move || answered.acknowledge(), move || unanswered.release())
        }

        /// Records the outcomes as seen. Call it only for the person's answer.
        pub(crate) fn acknowledge(&self) {
            for sequence in self.sequences.iter() {
                self.handed.borrow_mut().remove(sequence);
                self.source.presented(*sequence);
            }
            if let Some(total) = self.forgotten {
                self.handed.borrow_mut().remove(&FORGOTTEN);
                self.source.forgotten_presented(total);
            }
        }

        /// Leaves the outcomes unseen for the next reading.
        fn release(&self) {
            let forgotten = self.forgotten.map(|_| FORGOTTEN);
            for sequence in self.sequences.iter().copied().chain(forgotten) {
                if self.handed.borrow_mut().remove(&sequence) {
                    self.source.release(sequence);
                }
            }
        }
    }

    pub(crate) struct SharedEditNotifications {
        source: SharedEditSource,
        local: async_channel::Receiver<()>,
        remote: std::cell::RefCell<(u64, async_channel::Receiver<()>)>,
    }

    impl SharedEditNotifications {
        pub(crate) async fn recv(&self) -> Result<(), async_channel::RecvError> {
            loop {
                let (generation, remote) = {
                    let mut remote = self.remote.borrow_mut();
                    let current = self.source.generation.get();
                    if remote.0 != current {
                        *remote = (current, self.source.transport().subscribe());
                    }
                    (remote.0, remote.1.clone())
                };
                match futures_lite::future::race(self.local.recv(), remote.recv()).await {
                    // A replaced transport's hints end; the replacement's follow.
                    Err(_) if self.source.generation.get() != generation => {}
                    result => return result,
                }
            }
        }
    }

    impl SharedEditSource {
        pub(crate) fn new(edits: Rc<dyn SharedWorkspaceEdits>) -> Self {
            Self {
                edits: Rc::new(std::cell::RefCell::new(edits)),
                generation: Rc::default(),
                stand_in: Rc::default(),
                standing_in: Rc::default(),
                pending: Rc::new(std::cell::RefCell::new(
                    super::pending::PendingEdits::default(),
                )),
                subscribers: Rc::default(),
                held: Rc::default(),
                acknowledged: Rc::default(),
                forgotten_acknowledged: Rc::default(),
            }
        }

        /// Holds an unseen outcome for one surface to present. Returns false when another
        /// surface holds it already or presented it.
        fn hold(&self, sequence: u64) -> bool {
            (sequence == FORGOTTEN || !self.acknowledged.borrow().contains(&sequence))
                && self.held.borrow_mut().insert(sequence)
        }

        /// Records that a surface said the first `total` forgotten outcomes went unseen.
        fn forgotten_presented(&self, total: u64) {
            self.held.borrow_mut().remove(&FORGOTTEN);
            self.forgotten_acknowledged
                .set(self.forgotten_acknowledged.get().max(total));
            self.transport().forgotten_presented(total);
        }

        /// Gives up a held outcome unpresented, so a reader presents it.
        pub(crate) fn release(&self, sequence: u64) {
            if self.held.borrow_mut().remove(&sequence) {
                self.hint();
            }
        }

        /// Records that a surface presented the outcome of `sequence`.
        pub(crate) fn presented(&self, sequence: u64) {
            self.held.borrow_mut().remove(&sequence);
            {
                let mut acknowledged = self.acknowledged.borrow_mut();
                if !acknowledged.contains(&sequence) {
                    if acknowledged.len() >= spaceterm_server::hierarchy::UNSEEN_OUTCOMES {
                        acknowledged.pop_front();
                    }
                    acknowledged.push_back(sequence);
                }
            }
            self.transport().presented(sequence);
        }

        /// An owner for the answer to one edit, which [`Self::submit_owned`] asks for.
        pub(crate) fn owner(&self) -> OutcomeOwner {
            OutcomeOwner {
                source: self.clone(),
                answer: Rc::default(),
            }
        }

        /// Asks the transport to reconnect now.
        pub(crate) fn retry(&self) {
            self.transport().retry();
        }

        /// The transport this source submits to now.
        pub(crate) fn transport(&self) -> Rc<dyn SharedWorkspaceEdits> {
            Rc::clone(&self.edits.borrow())
        }

        /// Submits through `edits` from now on, as when a paired route connects or disconnects.
        /// An operation already dispatched resolves through the journal the new transport
        /// reads, and every follower hears of the change.
        pub(crate) fn replace_transport(&self, edits: Rc<dyn SharedWorkspaceEdits>) {
            self.generation.set(self.generation.get().wrapping_add(1));
            for sequence in self.acknowledged.borrow().iter() {
                edits.presented(*sequence);
            }
            if self.forgotten_acknowledged.get() != 0 {
                edits.forgotten_presented(self.forgotten_acknowledged.get());
            }
            *self.edits.borrow_mut() = edits;
            self.snapshot();
            self.hint();
        }

        /// Presents `inventory` whenever the transport has published none, as for a paired
        /// route that has not connected since launch or that the person disconnected.
        pub(crate) fn stand_in(&self, inventory: Option<Inventory>) {
            *self.stand_in.borrow_mut() = inventory;
            self.hint();
        }

        fn hint(&self) {
            self.subscribers.borrow_mut().retain(|sender| {
                !matches!(
                    sender.try_send(()),
                    Err(async_channel::TrySendError::Closed(()))
                )
            });
        }

        fn editability(&self) -> SharedEditability {
            if !self.transport().availability().0 || self.standing_in.get() {
                SharedEditability::Offline
            } else if self.pending.borrow().full() {
                SharedEditability::Pending
            } else {
                SharedEditability::Available
            }
        }

        pub(crate) fn submit(&self, mutation: Mutation) -> SharedSubmission {
            match self.enqueue(mutation, None, None) {
                Ok(()) => SharedSubmission::Submitted,
                Err(reason) => SharedSubmission::Refused(reason),
            }
        }

        fn enqueue(
            &self,
            mutation: Mutation,
            reply: Option<async_channel::Sender<Result<RetainedOutcome, SharedRefusalReason>>>,
            owner: Option<std::rc::Weak<std::cell::Cell<Option<u64>>>>,
        ) -> Result<(), SharedRefusalReason> {
            self.snapshot();
            let editability = self.editability();
            if !editability.available() {
                return Err(editability.into());
            }
            if !mutation.is_bounded() {
                return Err(SharedRefusalReason::Value);
            }
            self.pending.borrow_mut().push(mutation, reply, owner)?;
            self.dispatch();
            self.subscribers.borrow_mut().retain(|sender| {
                !matches!(
                    sender.try_send(()),
                    Err(async_channel::TrySendError::Closed(()))
                )
            });
            Ok(())
        }

        /// Each receiver belongs to one queue entry, including identical consecutive mutations.
        pub(crate) fn submit_waiting(
            &self,
            mutation: Mutation,
        ) -> Result<
            async_channel::Receiver<Result<RetainedOutcome, SharedRefusalReason>>,
            SharedRefusalReason,
        > {
            let (reply, receiver) = async_channel::bounded(1);
            self.enqueue(mutation, Some(reply), None)?;
            Ok(receiver)
        }

        /// Like [`Self::submit_waiting`], for a surface that presents the answer itself while
        /// `owner` lives.
        pub(crate) fn submit_owned(
            &self,
            mutation: Mutation,
            owner: OutcomeTicket,
        ) -> Result<
            async_channel::Receiver<Result<RetainedOutcome, SharedRefusalReason>>,
            SharedRefusalReason,
        > {
            let (reply, receiver) = async_channel::bounded(1);
            self.enqueue(mutation, Some(reply), Some(owner.0))?;
            Ok(receiver)
        }

        fn dispatch(&self) {
            let edits = self.transport();
            let (connected, busy) = edits.availability();
            if connected && !busy {
                let mut pending = self.pending.borrow_mut();
                if let Some(mutation) = pending.next_dispatch() {
                    if edits.submit(mutation.clone()) {
                        pending.dispatched(mutation);
                    } else if !edits.availability().0 {
                        pending.disconnect();
                    }
                }
            }
        }

        pub(crate) fn snapshot(&self) -> SharedWorkspaceSnapshot {
            self.read().0
        }

        /// The transport's snapshot, and whether its inventory is the stand-in because the
        /// transport has published none.
        fn read(&self) -> (SharedWorkspaceSnapshot, bool) {
            let mut snapshot = self.transport().snapshot();
            let standing_in = snapshot.inventory.is_none() && self.stand_in.borrow().is_some();
            if standing_in {
                snapshot.inventory.clone_from(&self.stand_in.borrow());
            }
            self.standing_in.set(standing_in);
            let owned = self.pending.borrow_mut().observe(&snapshot);
            for sequence in owned {
                self.hold(sequence);
            }
            self.dispatch();
            (snapshot, standing_in)
        }

        pub(crate) fn presentation_inventory(&self) -> Option<Inventory> {
            let snapshot = self.snapshot();
            snapshot
                .inventory
                .map(|inventory| self.pending.borrow_mut().project(inventory))
        }

        pub(crate) fn canonical(&self, id: Id) -> Id {
            self.pending.borrow().canonical(id)
        }
        pub(crate) fn provisional(&self, id: Id) -> bool {
            self.pending.borrow().provisional(id)
        }
        pub(crate) fn retire_alias(&self, id: Id) {
            self.pending.borrow_mut().forget_alias(id);
        }

        pub(crate) fn has_pending(&self) -> bool {
            self.pending.borrow().has_pending()
        }

        /// A final authoritative close dismisses its window before acknowledgement finishes.
        /// Keep the original transport alive independently until its durable operation retires.
        pub(crate) fn retain_until_settled(&self, cx: &mut App) {
            let source = self.clone();
            let changes = self.transport().subscribe();
            // Each answer the server publishes hints here, and reading the snapshot dispatches
            // the next queued edit.
            cx.spawn(async move |_| {
                while changes.recv().await.is_ok() {
                    let snapshot = source.snapshot();
                    if !snapshot.pending && !source.has_pending() {
                        break;
                    }
                }
            })
            .detach();
        }
        pub(crate) fn closing(&self, pane: Id) -> bool {
            self.pending.borrow().closing(pane)
        }
        pub(crate) fn take_presentation(&self) -> Option<SharedAcceptance> {
            self.pending.borrow_mut().take_presentation()
        }
    }

    /// Whether a shared edit can be asked for now.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum SharedEditability {
        Available,
        /// The client has no connection to the server, so shared edits are unavailable rather
        /// than queued.
        Offline,
        /// The bounded connected interaction queue is full.
        Pending,
    }

    impl SharedEditability {
        pub(crate) const fn available(self) -> bool {
            matches!(self, Self::Available)
        }

        /// What a disabled shared action says about itself.
        pub(crate) const fn explanation(self) -> &'static str {
            match self {
                Self::Available => "",
                Self::Offline => "Reconnect to make changes to this Workspace.",
                Self::Pending => "Wait for the current changes to finish, then try again.",
            }
        }
    }

    /// What one shared Workspace's sidebar row says in place of its directory.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) struct SharedRowStatus {
        pub(crate) text: &'static str,
        /// Whether the status reports a problem, rather than work in progress.
        pub(crate) warning: bool,
    }

    impl SharedRowStatus {
        const fn progress(text: &'static str) -> Self {
            Self {
                text,
                warning: false,
            }
        }
    }

    /// What asking for a shared edit did. A refusal submitted nothing and retains nothing.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum SharedSubmission {
        Submitted,
        Refused(SharedRefusalReason),
    }

    /// Why this client submitted nothing.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum SharedRefusalReason {
        Offline,
        Pending,
        /// The value itself is outside what the SpaceTerm Protocol carries, such as a name or a
        /// directory longer than the server accepts.
        Value,
    }

    impl SharedRefusalReason {
        pub(crate) const fn explanation(self) -> &'static str {
            match self {
                Self::Offline => SharedEditability::Offline.explanation(),
                Self::Pending => SharedEditability::Pending.explanation(),
                Self::Value => "Enter a shorter name or directory.",
            }
        }
    }

    impl From<SharedEditability> for SharedRefusalReason {
        fn from(editability: SharedEditability) -> Self {
            match editability {
                // A submission refused while every control reads available names the value.
                SharedEditability::Available => Self::Value,
                SharedEditability::Offline => Self::Offline,
                SharedEditability::Pending => Self::Pending,
            }
        }
    }

    /// A same-field edit the server refused because another client changed the field first. The
    /// rejected draft stays here, so a person reads it beside the server's value and decides
    /// whether to ask again. Nothing retries on its own.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct SharedWorkspaceConflict {
        pub(crate) workspace: WorkspaceId,
        pub(crate) field: SharedWorkspaceField,
        reviewed_version: u64,
        /// The exact value this client asked for, kept so asking again sends the same draft.
        pub(crate) draft: SharedWorkspaceDraft,
        /// The value the server holds now, which another client's accepted edit wrote.
        pub(crate) authoritative: SharedString,
    }

    /// A rejected shared edit's own value, kept exactly as a person supplied it.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) enum SharedWorkspaceDraft {
        Name(String),
        Pin(Option<PathBuf>),
    }

    impl SharedWorkspaceDraft {
        /// The draft as a person reads it.
        pub(crate) fn display(&self) -> SharedString {
            match self {
                Self::Name(name) => SharedString::from(name.clone()),
                Self::Pin(directory) => display_pin(directory.as_deref()),
            }
        }
    }

    fn display_pin(directory: Option<&Path>) -> SharedString {
        match directory {
            Some(directory) => SharedString::from(directory.display().to_string()),
            None => SharedString::new_static("No pinned directory"),
        }
    }

    /// Which Workspace field a shared edit changed.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum SharedWorkspaceField {
        Name,
        PinnedDirectory,
    }

    impl SharedWorkspaceField {
        pub(crate) const fn title(self) -> &'static str {
            match self {
                Self::Name => "Workspace Renamed Elsewhere",
                Self::PinnedDirectory => "Pinned Directory Changed Elsewhere",
            }
        }

        pub(crate) const fn retry_label(self) -> &'static str {
            match self {
                Self::Name => "Use My Name",
                Self::PinnedDirectory => "Use My Directory",
            }
        }

        pub(crate) const fn label(self) -> &'static str {
            match self {
                Self::Name => "name",
                Self::PinnedDirectory => "pinned directory",
            }
        }
    }

    /// A shared edit the server refused for a reason no retry repairs.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum SharedRefusal {
        /// The target no longer exists on the server.
        Missing,
        /// The server rejected the value.
        Invalid,
        /// The server is at its limit for Workspaces, Tabs, or Panes.
        Limit,
        Disconnected,
    }

    impl SharedRefusal {
        pub(crate) const fn title(self) -> &'static str {
            match self {
                Self::Missing => "Item Already Removed",
                Self::Invalid => "Change Not Accepted",
                Self::Limit => "Server Limit Reached",
                Self::Disconnected => "Changes Not Sent",
            }
        }

        pub(crate) const fn detail(self) -> &'static str {
            match self {
                Self::Missing => "Another client removed the item you were changing.",
                Self::Invalid => "Check the value and try again.",
                Self::Disconnected => {
                    "The connection ended before these changes were sent. Reconnect, then try them again."
                }
                Self::Limit => "Close unused Workspaces, Tabs, or Panes before creating more.",
            }
        }
    }

    /// Why this client cannot read or keep its own arrangement of shared Workspaces: their Tab
    /// display order and Pane Layouts. A failed read leaves the saved arrangement in place. A
    /// failed save may already have replaced it, so no case claims which arrangement is saved.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum ArrangementUnavailable {
        /// The saved arrangement could not be read, so shared Workspaces stay closed rather than
        /// opening in a default arrangement that a later save would write over it.
        Read,
        /// The saved arrangement or its storage could not be validated, so shared Workspaces stay
        /// closed and the saved data is kept.
        UnsafeStorage,
        /// SpaceTerm cannot confirm which arrangement was saved. The window keeps working, and
        /// SpaceTerm makes no more saves until it quits and reopens.
        Write,
    }

    impl ArrangementUnavailable {
        /// Why reading the saved arrangement failed, from the server's bounded classification.
        pub(crate) const fn reading(error: spaceterm_server::ServerError) -> Self {
            match error {
                spaceterm_server::ServerError::UnsafeStorage => Self::UnsafeStorage,
                _ => Self::Read,
            }
        }

        pub(crate) const fn title(self) -> &'static str {
            match self {
                Self::Read | Self::UnsafeStorage => "Couldn\u{2019}t Restore Workspaces",
                Self::Write => "Couldn\u{2019}t Confirm Saved Changes",
            }
        }

        const fn accessibility_label(self) -> &'static str {
            match self {
                Self::Read | Self::UnsafeStorage => "Workspaces unavailable",
                Self::Write => "Changes could not be saved",
            }
        }

        pub(crate) const fn detail(self) -> &'static str {
            match self {
                Self::Read => {
                    "SpaceTerm couldn\u{2019}t read your saved Workspace arrangement. Quit and reopen SpaceTerm to try again."
                }
                Self::UnsafeStorage => {
                    "SpaceTerm couldn\u{2019}t open your saved Workspace arrangement safely. Your saved arrangement is unchanged."
                }
                Self::Write => {
                    "Quit and reopen SpaceTerm before you rearrange Workspaces again. Your Workspaces keep running."
                }
            }
        }

        /// Presents the notice as a warning with one OK. The caller presents each failure once.
        pub(crate) fn present<T: 'static>(self, window: &gpui::Window, cx: &mut gpui::Context<T>) {
            let (id, ok) = match self {
                Self::Read | Self::UnsafeStorage => (
                    "shared-arrangement-unavailable",
                    "shared-arrangement-unavailable-ok",
                ),
                Self::Write => (
                    "shared-arrangement-not-saved",
                    "shared-arrangement-not-saved-ok",
                ),
            };
            let _ = spaceterm_ui::Alert::new(
                spaceterm_ui::ModalId::new(id),
                self.accessibility_label(),
                self.title(),
                self.detail().to_owned(),
                vec![spaceterm_ui::ModalAction::new(
                    (),
                    "OK",
                    spaceterm_ui::ModalActionRole::Cancel,
                    ok,
                )],
            )
            .intent(spaceterm_ui::AlertIntent::Warning)
            .present(window, cx, |_, _| {});
        }
    }

    /// One Workspace's presented server metadata and its local creation binding.
    /// Directory values grant no local filesystem authority.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct SharedWorkspaceFacts {
        pub(crate) server: Id,
        /// The server has published this identity. A refused provisional creation removes its
        /// row without dismissing the empty window that must explain the refusal.
        pub(crate) confirmed: bool,
        pub(crate) name: String,
        pub(crate) name_version: u64,
        pub(crate) pin_version: u64,
        pub(crate) pinned_directory: Option<PathBuf>,
        /// The directory the server reports for the Workspace's Root Pane.
        pub(crate) directory: PathBuf,
    }

    impl SharedWorkspaceFacts {
        pub(crate) fn from_server(workspace: &ServerWorkspace) -> Self {
            let directory = workspace
                .tabs
                .iter()
                .find(|tab| tab.id == workspace.root_tab)
                .and_then(|tab| tab.panes.iter().find(|pane| pane.id == tab.root_pane))
                .map_or_else(PathBuf::new, |pane| pane.presented_directory().to_owned());
            Self {
                server: workspace.id,
                confirmed: true,
                name: workspace.name.clone(),
                name_version: workspace.name_version,
                pin_version: workspace.pin_version,
                pinned_directory: workspace.pinned_directory.clone(),
                directory,
            }
        }

        /// The directory line the sidebar shows: the Pinned Directory when the Workspace has one,
        /// and the Root Pane's directory otherwise.
        pub(crate) fn displayed_directory(&self) -> &Path {
            self.pinned_directory.as_deref().unwrap_or(&self.directory)
        }

        pub(crate) fn pinned(&self) -> bool {
            self.pinned_directory.is_some()
        }
    }

    /// One Pane's passive subscription to the server: how it presents the Pane and the connection
    /// whose status the Pane reads.
    pub(crate) type SharedPaneSubscription = (
        WorkspaceTerminalSessionFactory,
        Arc<dyn ServerPaneConnection>,
    );

    /// Subscribes one Pane to the Terminal Session a SpaceTerm Server owns.
    ///
    /// Opening a subscription presents a Pane; it starts no shell and acquires no unclaimed grant,
    /// because only an explicit selection of that Pane asks the server for one.
    pub(crate) trait SharedPaneOpener {
        fn open(&self, pane: Id) -> Option<SharedPaneSubscription>;
        fn placeholder(&self) -> WorkspaceTerminalSessionFactory;

        /// Only an account-local route can interpret server directory text on this client.
        fn local_filesystem(
            &self,
        ) -> Option<&crate::platform::local_filesystem::LocalFilesystemAuthority> {
            None
        }

        /// The name another computer's server displays by, or `None` for this computer's.
        fn remote_display(&self) -> Option<String> {
            self.direct_route().map(|direct| direct.display())
        }

        /// The paired route reaching another computer's server, if that is how this client
        /// reaches it.
        fn direct_route(&self) -> Option<&DirectServer> {
            None
        }

        /// Changes whenever what [`Self::open`] returns for a Pane changes, as when a paired
        /// route connects or disconnects. Each Pane opened earlier opens again.
        fn generation(&self) -> u64 {
            0
        }
    }

    /// Another computer's server as a direct network route reaches it.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct DirectServer {
        pub(crate) route: crate::direct_connections::DirectRouteId,
        /// The server's name, which labels it and grants nothing.
        pub(crate) name: SharedString,
    }

    impl DirectServer {
        /// The label the Panes' terminal metadata displays for this server. It grants nothing,
        /// and a name that leaves nothing to show reads as a generic server.
        fn display(&self) -> String {
            let name = self.name.trim();
            if name.is_empty() {
                "server".to_owned()
            } else {
                name.to_owned()
            }
        }
    }

    /// The local server's own Panes.
    struct LocalServerPanes {
        server: ServerConnection,
        controls: Arc<PaneControls>,
        /// Completes the launch plan the server factory ignores. It is client launch scaffolding
        /// and never a directory the server's Terminal Session starts in.
        client_home: ValidatedLocalDirectory,
        local_files: crate::platform::local_filesystem::LocalFilesystemAuthority,
    }

    impl SharedPaneOpener for LocalServerPanes {
        fn placeholder(&self) -> WorkspaceTerminalSessionFactory {
            WorkspaceTerminalSessionFactory::pending(self.client_home.clone())
        }

        fn local_filesystem(
            &self,
        ) -> Option<&crate::platform::local_filesystem::LocalFilesystemAuthority> {
            Some(&self.local_files)
        }

        fn open(&self, pane: Id) -> Option<SharedPaneSubscription> {
            let (factory, state) = DevelopmentServerSessionFactory::for_pane(
                self.server.clone(),
                &self.controls,
                self.local_files.clone(),
                pane,
            );
            let permit = self.server.attachments().ok()?.permit(pane)?;
            let session_factory = WorkspaceTerminalSessionFactory::new_server(
                Rc::new(factory.with_attachment(permit)),
                self.client_home.clone(),
            );
            Some((session_factory, state))
        }
    }

    /// Another computer's server Panes, reached over a paired route. Nothing here grants local
    /// file authority: the server's paths and links name files on that computer.
    struct RemoteServerPanes {
        server: ServerConnection,
        controls: Arc<PaneControls>,
        direct: DirectServer,
        /// Completes the launch plan the server factory ignores, as for the local server.
        client_home: ValidatedLocalDirectory,
    }

    impl SharedPaneOpener for RemoteServerPanes {
        fn placeholder(&self) -> WorkspaceTerminalSessionFactory {
            WorkspaceTerminalSessionFactory::pending(self.client_home.clone())
        }

        fn direct_route(&self) -> Option<&DirectServer> {
            Some(&self.direct)
        }

        fn open(&self, pane: Id) -> Option<SharedPaneSubscription> {
            let (factory, state) = DevelopmentServerSessionFactory::for_remote_pane(
                self.server.clone(),
                &self.controls,
                self.direct.display(),
                pane,
            );
            let permit = self.server.attachments().ok()?.permit(pane)?;
            let session_factory = WorkspaceTerminalSessionFactory::new_server(
                Rc::new(factory.with_attachment(permit)),
                self.client_home.clone(),
            );
            Some((session_factory, state))
        }
    }

    /// Opens passive subscriptions to the server's Panes and supplies their Pane construction.
    ///
    /// Shared Panes reuse native client effects and obtain accessible shell text from the server.
    /// Same-machine file authority applies only to Panes on this machine's server.
    #[derive(Clone)]
    pub(crate) struct SharedPaneSessions {
        opener: Rc<dyn SharedPaneOpener>,
        panes: PaneConstruction,
        edits: Option<SharedEditSource>,
    }

    impl SharedPaneSessions {
        pub(crate) fn new(
            server: ServerConnection,
            controls: Arc<PaneControls>,
            client_home: ValidatedLocalDirectory,
            local_files: crate::platform::local_filesystem::LocalFilesystemAuthority,
            panes: &PaneConstruction,
        ) -> Self {
            Self::with_opener(
                Rc::new(LocalServerPanes {
                    server,
                    controls,
                    client_home,
                    local_files,
                }),
                panes,
            )
        }

        /// Opens Panes of another computer's server through one connection of a paired route.
        /// They take no local file authority.
        pub(crate) fn remote_opener(
            server: ServerConnection,
            controls: Arc<PaneControls>,
            direct: DirectServer,
            client_home: ValidatedLocalDirectory,
        ) -> Rc<dyn SharedPaneOpener> {
            Rc::new(RemoteServerPanes {
                server,
                controls,
                direct,
                client_home,
            })
        }

        pub(crate) fn with_opener(
            opener: Rc<dyn SharedPaneOpener>,
            panes: &PaneConstruction,
        ) -> Self {
            Self {
                opener,
                panes: panes.clone(),
                edits: None,
            }
        }

        pub(crate) fn local_filesystem(
            &self,
        ) -> Option<&crate::platform::local_filesystem::LocalFilesystemAuthority> {
            self.opener.local_filesystem()
        }

        /// Changes whenever each Pane must open again, as when a paired route connects or
        /// disconnects.
        pub(crate) fn generation(&self) -> u64 {
            self.opener.generation()
        }

        /// The paired route reaching another computer's server, if any.
        pub(crate) fn direct_route(&self) -> Option<&DirectServer> {
            self.opener.direct_route()
        }

        /// The name of another computer's server, or `None` for this computer's. Only this
        /// computer's server takes Start Server from this client.
        pub(crate) fn remote_name(&self) -> Option<SharedString> {
            self.opener.remote_display().map(SharedString::from)
        }

        pub(crate) const fn panes(&self) -> &PaneConstruction {
            &self.panes
        }

        /// Subscribes to one server Pane.
        pub(crate) fn open(&self, pane: Id) -> Option<SharedPaneSubscription> {
            if self
                .edits
                .as_ref()
                .is_some_and(|edits| edits.provisional(pane))
            {
                return Some((self.opener.placeholder(), Arc::new(PendingPane)));
            }
            self.opener.open(pane)
        }

        fn with_edits(mut self, edits: SharedEditSource) -> Self {
            self.edits = Some(edits);
            self
        }

        /// Reads the Terminal Metadata the server published for one of its Panes with this
        /// route's locality, or `None` if the publication is malformed. Only this computer's
        /// server names local directories; another computer's server is named by its display
        /// label.
        pub(crate) fn metadata(
            &self,
            published: &spaceterm_server::pane::metadata::Snapshot,
        ) -> Option<Arc<crate::terminal::metadata::TerminalMetadataSnapshot>> {
            let remote = self.opener.remote_display().map(|display| {
                crate::terminal::metadata::RemoteTerminalMetadataContext::new(
                    display,
                    crate::domain::RemoteDirectory::home(),
                )
            });
            crate::terminal::server_metadata::decode(
                published.clone(),
                self.local_filesystem(),
                remote.as_ref(),
            )
            .ok()
        }
    }

    struct PendingPane;
    impl ServerPaneConnection for PendingPane {
        fn status(&self) -> crate::terminal::server_session::DevelopmentPaneStatus {
            crate::terminal::server_session::DevelopmentPaneStatus::Connecting
        }
        fn controller(&self) -> Option<spaceterm_server::pane::control::Controller> {
            None
        }
        fn connection_generation(&self) -> u64 {
            0
        }
        // A placeholder never changes; the published Pane replaces it.
        fn observe(&self, hint: async_channel::Sender<()>) {
            let _ = hint.try_send(());
        }
        fn select(&self) {}
        fn retry(&self) {}
        fn take_over(&self) {}
        fn start_restored(&self) {}
    }

    /// One shared Tab's Panes: their server identities, each one's view of its connection, and
    /// the focus the screen replacing a Pane rests on.
    pub(crate) struct SharedPanes {
        edits: SharedEditSource,
        sessions: SharedPaneSessions,
        workspace: Id,
        tab: Id,
        panes: BTreeMap<PaneId, Id>,
        /// The opener generation each Pane's subscription came from.
        opened: BTreeMap<PaneId, u64>,
        views: BTreeMap<PaneId, ServerPaneView>,
        screen_focus: BTreeMap<PaneId, gpui::FocusHandle>,
        pending_starts: std::cell::RefCell<std::collections::BTreeSet<PaneId>>,
        /// One hint for a change to any registered Pane's connection.
        changes: (async_channel::Sender<()>, async_channel::Receiver<()>),
    }

    impl Drop for SharedPanes {
        fn drop(&mut self) {
            for id in self.panes.values() {
                self.edits.retire_alias(*id);
            }
        }
    }

    impl SharedPanes {
        pub(crate) fn new(
            edits: SharedEditSource,
            sessions: SharedPaneSessions,
            workspace: Id,
            tab: Id,
        ) -> Self {
            Self {
                sessions: sessions.with_edits(edits.clone()),
                edits,
                workspace,
                tab,
                panes: BTreeMap::new(),
                opened: BTreeMap::new(),
                views: BTreeMap::new(),
                screen_focus: BTreeMap::new(),
                pending_starts: Default::default(),
                changes: async_channel::bounded(1),
            }
        }

        pub(crate) const fn sessions(&self) -> &SharedPaneSessions {
            &self.sessions
        }

        pub(crate) fn register(
            &mut self,
            pane_id: PaneId,
            server: Id,
            state: Arc<dyn ServerPaneConnection>,
            screen_focus: gpui::FocusHandle,
        ) {
            if let Some(previous) = self.panes.insert(pane_id, server)
                && previous != server
            {
                self.edits.retire_alias(previous);
            }
            if self.edits.provisional(server) {
                self.pending_starts.borrow_mut().insert(pane_id);
            } else if self.pending_starts.borrow_mut().remove(&pane_id) {
                state.start_created();
            }
            state.observe(self.changes.0.clone());
            self.opened.insert(pane_id, self.sessions.generation());
            self.views.insert(pane_id, ServerPaneView::new(state));
            self.screen_focus.insert(pane_id, screen_focus);
        }

        /// Hints after any registered Pane's connection changes. Read every view after each.
        pub(crate) fn changes(&self) -> async_channel::Receiver<()> {
            self.changes.1.clone()
        }

        pub(crate) fn forget(&mut self, pane_id: PaneId) {
            if let Some(previous) = self.panes.remove(&pane_id) {
                self.edits.retire_alias(previous);
            }
            self.pending_starts.borrow_mut().remove(&pane_id);
            self.opened.remove(&pane_id);
            self.views.remove(&pane_id);
            self.screen_focus.remove(&pane_id);
        }

        /// A provisional creation already owns its startup intent. Only a confirmed Pane
        /// receives selection or control acquisition.
        pub(crate) fn select(&self, pane_id: PaneId) {
            if self
                .panes
                .get(&pane_id)
                .is_some_and(|id| self.edits.provisional(*id))
            {
                return;
            }
            if let Some(view) = self.views.get(&pane_id) {
                view.select();
            }
        }

        pub(crate) fn server_pane(&self, pane_id: PaneId) -> Option<Id> {
            self.panes.get(&pane_id).map(|id| self.edits.canonical(*id))
        }

        pub(crate) fn closing(&self, pane_id: PaneId) -> bool {
            self.server_pane(pane_id)
                .is_some_and(|id| self.edits.closing(id))
        }

        /// Whether the Pane's route connected or disconnected since the Pane opened.
        pub(crate) fn reopened(&self, pane_id: PaneId) -> bool {
            self.opened
                .get(&pane_id)
                .is_some_and(|generation| *generation != self.sessions.generation())
        }

        /// Whether the Pane must open again: the server confirmed its provisional identity, or
        /// its route connected or disconnected since it opened.
        pub(crate) fn needs_binding(&self, pane_id: PaneId, server: Id) -> bool {
            self.panes.get(&pane_id).is_some_and(|id| *id != server) || self.reopened(pane_id)
        }

        pub(crate) fn edits(&self) -> SharedEditSource {
            self.edits.clone()
        }

        pub(crate) fn canonicalize(&mut self) {
            self.workspace = self.edits.canonical(self.workspace);
            self.tab = self.edits.canonical(self.tab);
        }

        pub(crate) fn presentation_tab(&self) -> Option<spaceterm_server::hierarchy::Tab> {
            self.edits
                .presentation_inventory()?
                .workspaces
                .into_iter()
                .find(|w| w.id == self.edits.canonical(self.workspace))?
                .tabs
                .into_iter()
                .find(|t| t.id == self.edits.canonical(self.tab))
        }

        pub(crate) fn local_pane(&self, server: Id) -> Option<PaneId> {
            self.panes
                .iter()
                .find_map(|(&pane_id, &id)| (self.edits.canonical(id) == server).then_some(pane_id))
        }

        /// Where the keyboard rests while a screen replaces the Pane.
        pub(crate) fn screen_focus(&self, pane_id: PaneId) -> Option<&gpui::FocusHandle> {
            self.screen_focus.get(&pane_id)
        }

        pub(crate) fn holds(&self, server: Id) -> bool {
            self.panes
                .values()
                .any(|&pane| self.edits.canonical(pane) == server)
        }

        pub(crate) fn missing_worktree_for_directory(&self, directory: &Path) -> bool {
            let snapshot = self.edits.snapshot();
            let Some(inventory) = snapshot.inventory else {
                return false;
            };
            let Some(tab) = inventory
                .workspaces
                .iter()
                .find(|w| w.id == self.workspace)
                .and_then(|w| w.tabs.iter().find(|tab| tab.id == self.tab))
            else {
                return false;
            };
            let Some(root) = tab
                .worktree
                .as_ref()
                .filter(|root| directory.starts_with(root))
            else {
                return false;
            };
            inventory.repositories.as_ref().is_some_and(|snapshot| {
                snapshot
                    .directories
                    .iter()
                    .filter_map(|facts| facts.worktrees.as_ref())
                    .flat_map(|list| &list.worktrees)
                    .any(|record| record.root == *root && record.missing)
            })
        }

        pub(crate) fn view(&self, pane_id: PaneId) -> Option<&ServerPaneView> {
            self.views.get(&pane_id)
        }

        pub(crate) fn view_mut(&mut self, pane_id: PaneId) -> Option<&mut ServerPaneView> {
            self.views.get_mut(&pane_id)
        }

        pub(crate) fn pane_ids(&self) -> Vec<PaneId> {
            self.panes.keys().copied().collect()
        }

        pub(crate) fn editability(&self) -> SharedEditability {
            self.edits.editability()
        }

        /// Whether the server is reachable, which an explicit Pane selection needs: offline
        /// navigation must never ask the server to start a retained shell.
        pub(crate) fn reachable(&self) -> bool {
            self.edits.editability() != SharedEditability::Offline
        }

        /// Requests a shared split. Its provisional Pane is presentation only until the server
        /// supplies its authoritative identity and membership.
        pub(crate) fn split(&self, source: PaneId, axis: Axis) -> SharedSubmission {
            let Some(source) = self.server_pane(source) else {
                return SharedSubmission::Refused(SharedRefusalReason::Value);
            };
            self.edits.submit(Mutation::SplitPane {
                workspace: self.edits.canonical(self.workspace),
                tab: self.edits.canonical(self.tab),
                source,
                axis,
                control: spaceterm_server::hierarchy::CreateControl::Creator,
            })
        }
    }

    /// One shared Workspace's Tabs and the identities its Panes split within.
    pub(crate) struct SharedTabs {
        edits: SharedEditSource,
        sessions: SharedPaneSessions,
        workspace: Id,
        tabs: BTreeMap<TabId, Id>,
    }

    impl SharedTabs {
        pub(crate) fn new(
            edits: SharedEditSource,
            sessions: SharedPaneSessions,
            workspace: Id,
        ) -> Self {
            Self {
                sessions: sessions.with_edits(edits.clone()),
                edits,
                workspace,
                tabs: BTreeMap::new(),
            }
        }

        pub(crate) fn workspace(&self) -> Id {
            self.edits.canonical(self.workspace)
        }
        pub(crate) fn optimistically_closing(&self, tab: TabId) -> bool {
            let Some(server) = self.server_tab(tab) else {
                return false;
            };
            self.edits
                .transport()
                .snapshot()
                .inventory
                .as_ref()
                .and_then(|i| {
                    i.workspaces
                        .iter()
                        .flat_map(|w| &w.tabs)
                        .find(|t| t.id == server)
                })
                .is_some_and(|t| t.panes.iter().all(|p| self.edits.closing(p.id)))
        }

        pub(crate) fn presentation_workspace(&self) -> Option<ServerWorkspace> {
            self.edits
                .presentation_inventory()?
                .workspaces
                .into_iter()
                .find(|w| w.id == self.workspace())
        }
        pub(crate) fn aliases_to_apply(&self) -> Vec<Id> {
            std::iter::once(self.workspace)
                .chain(self.tabs.values().copied())
                .filter(|id| self.edits.canonical(*id) != *id)
                .collect()
        }

        pub(crate) fn canonicalize(&mut self) {
            self.workspace = self.edits.canonical(self.workspace);
            for id in self.tabs.values_mut() {
                *id = self.edits.canonical(*id);
            }
        }

        pub(crate) const fn sessions(&self) -> &SharedPaneSessions {
            &self.sessions
        }

        pub(crate) fn edits(&self) -> SharedEditSource {
            self.edits.clone()
        }

        pub(crate) fn register(&mut self, tab_id: TabId, server: Id) {
            self.tabs.insert(tab_id, server);
        }

        pub(crate) fn forget(&mut self, tab_id: TabId) {
            self.tabs.remove(&tab_id);
        }

        pub(crate) fn server_tab(&self, tab_id: TabId) -> Option<Id> {
            self.tabs.get(&tab_id).map(|id| self.edits.canonical(*id))
        }

        pub(crate) fn local_tab(&self, server: Id) -> Option<TabId> {
            self.tabs
                .iter()
                .find_map(|(&tab_id, &id)| (self.edits.canonical(id) == server).then_some(tab_id))
        }

        pub(crate) fn editability(&self) -> SharedEditability {
            self.edits.editability()
        }

        /// Asks the server for another Tab in this Workspace.
        pub(crate) fn create_tab(&self, source: Id) -> SharedSubmission {
            let inventory = self.edits.presentation_inventory();
            let tab = inventory
                .as_ref()
                .and_then(|i| i.workspaces.iter().find(|w| w.id == self.workspace()))
                .and_then(|w| {
                    w.tabs
                        .iter()
                        .find(|t| t.panes.iter().any(|p| p.id == source))
                });
            let mutation =
                match tab.and_then(|t| t.worktree_repository.as_ref().zip(t.worktree.as_ref())) {
                    Some((root, path)) => {
                        Mutation::Worktree(spaceterm_server::worktree::Operation::OpenTab {
                            workspace: self.workspace(),
                            root: root.clone(),
                            path: path.clone(),
                            source: Some(source),
                            control: spaceterm_server::hierarchy::CreateControl::Creator,
                        })
                    }
                    None => Mutation::CreateTab {
                        workspace: self.workspace(),
                        source: Some(source),
                        control: spaceterm_server::hierarchy::CreateControl::Creator,
                    },
                };
            self.edits.submit(mutation)
        }
    }

    /// One available server Workspace and its optional presentation in this client.
    #[derive(Clone, Debug)]
    pub(crate) struct SharedAvailableWorkspace {
        pub(crate) server: Id,
        pub(crate) name: String,
        pub(crate) local: Option<WorkspaceId>,
    }
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum SharedAttachmentFailure {
        Unchanged,
        Uncertain,
    }

    /// Names the authenticated client presentation one binding is, once its connection has been
    /// admitted. Two saved routes to one server are two owners.
    pub(crate) trait WorkspaceOwner {
        fn owner(&self) -> Option<OwnerId>;
    }

    impl WorkspaceOwner for ServerConnection {
        fn owner(&self) -> Option<OwnerId> {
            self.workspace_owner().ok()
        }
    }

    /// The Workspace Window's authoritative inventory, attachments and pending presentation.
    pub(crate) struct SharedWorkspaces {
        edits: SharedEditSource,
        /// The presentation this binding is, which names its Workspaces in this client's order.
        owner: Option<Rc<dyn WorkspaceOwner>>,
        sessions: SharedPaneSessions,
        /// Each server Workspace and the local Workspace presenting it.
        workspaces: BTreeMap<WorkspaceId, SharedWorkspaceFacts>,
        /// The inventory revision this client has applied.
        revision: Option<u64>,
        presentation: Option<Inventory>,
        connected: bool,
        connection_error: Option<spaceterm_server::ServerError>,
        failed_attempts: u64,
        pending: bool,
        pending_operation: Option<Mutation>,
        /// Unread refused edits, each retaining its own draft and authoritative snapshot.
        conflicts: VecDeque<PendingConflict>,
        /// Refusals still to present, each with the unseen outcomes it stands for. Outcomes
        /// refused for the same reason share one notice.
        refusals: VecDeque<(SharedRefusal, Vec<u64>)>,
        /// The forgotten total the notice that some outcomes went unseen would record.
        forgotten: Option<u64>,
        /// Failed Worktree operations no surface has presented yet.
        worktree_failures: VecDeque<WorktreeFailure>,
        /// Outcomes this reading handed to alerts the person has not answered yet.
        handed: Rc<std::cell::RefCell<BTreeSet<u64>>>,
        /// The inventory the server published last, including the live Session facts a close
        /// confirmation reads. Those facts can change without a new membership revision.
        latest: Option<Inventory>,
        /// The Git facts the server read with its latest inventory.
        repositories: super::SharedRepositories,
        /// The close confirmation a person is reviewing now.
        review: Option<SharedCloseReview>,
        /// The close this process submitted, and the local target a fresh review would reopen.
        submitted_close: Option<(ServerCloseTarget, CloseTarget)>,
        /// Closes the server refused because what they would end changed after the
        /// confirmation, each with its outcome's sequence.
        stale_closes: VecDeque<(ServerCloseTarget, u64)>,
        attachments: spaceterm_server::attachments::Attachments,
        retained_views: Option<crate::ui::client_view_state::RetainedClientViews>,
        saved_views: Option<spaceterm_server::client_view::ClientView>,
        last_saved_view: Option<spaceterm_server::client_view::ClientView>,
        view_error_reported: bool,
        attachment_error: Option<spaceterm_server::ServerError>,
        /// Whether this binding keeps a copy of the inventory it presents, for a paired route.
        retains_copies: bool,
        /// The Workspaces and Tabs the server last confirmed to this client.
        confirmed: Option<RetainedInventory>,
        /// The copy last written to this client's storage.
        saved: Option<RetainedInventory>,
        /// Whether the presented inventory is the retained copy rather than the server's.
        standing_in: bool,
    }

    /// A refused same-field edit whose draft is kept until this client can name the Workspace it
    /// belongs to. An answer recovered after a relaunch arrives before any local Workspace exists,
    /// so the draft waits for the projection rather than being dropped.
    struct PendingConflict {
        /// The sequence of the refusal, which draft review presents.
        sequence: u64,
        workspace: Id,
        field: SharedWorkspaceField,
        reviewed_version: u64,
        draft: SharedWorkspaceDraft,
        /// Captured from the snapshot that carried the refusal, so it never names a value a later
        /// accepted edit replaced.
        authoritative: SharedString,
    }

    /// A failed Worktree operation the Workspace Window presents, unless the surface that asked
    /// for it does.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) struct WorktreeFailure {
        pub(crate) sequence: u64,
        pub(crate) operation: spaceterm_server::worktree::Operation,
        pub(crate) error: spaceterm_server::worktree::Error,
    }

    impl Drop for SharedWorkspaces {
        fn drop(&mut self) {
            self.edits
                .pending
                .borrow_mut()
                .release_conflicts(self.conflicts.len());
            // What this window never presented stays unseen for the next window.
            let sequences: Vec<u64> = self
                .conflicts
                .iter()
                .map(|conflict| conflict.sequence)
                .chain(
                    self.refusals
                        .iter()
                        .flat_map(|(_, refused)| refused.iter().copied()),
                )
                .chain(self.forgotten.map(|_| FORGOTTEN))
                .chain(self.stale_closes.iter().map(|(_, sequence)| *sequence))
                .chain(
                    self.worktree_failures
                        .iter()
                        .map(|failure| failure.sequence),
                )
                .chain(std::mem::take(&mut *self.handed.borrow_mut()))
                .collect();
            for sequence in sequences {
                self.edits.release(sequence);
            }
        }
    }

    impl SharedWorkspaces {
        pub(crate) fn new(edits: SharedEditSource, sessions: SharedPaneSessions) -> Self {
            Self {
                sessions: sessions.with_edits(edits.clone()),
                edits,
                owner: None,
                workspaces: BTreeMap::new(),
                revision: None,
                presentation: None,
                connected: false,
                connection_error: None,
                failed_attempts: 0,
                pending: false,
                pending_operation: None,
                conflicts: VecDeque::new(),
                refusals: VecDeque::new(),
                forgotten: None,
                worktree_failures: VecDeque::new(),
                handed: Rc::default(),
                latest: None,
                repositories: super::SharedRepositories::default(),
                review: None,
                submitted_close: None,
                stale_closes: VecDeque::new(),
                attachments: spaceterm_server::attachments::Attachments::memory(),
                retained_views: None,
                saved_views: None,
                last_saved_view: None,
                view_error_reported: false,
                attachment_error: None,
                retains_copies: false,
                confirmed: None,
                saved: None,
                standing_in: false,
            }
        }

        /// Keeps a copy of each inventory the server confirms, so this client can present its
        /// Workspaces and Tabs while the paired route is not connected.
        pub(crate) fn retain_inventory_copies(&mut self) {
            self.retains_copies = true;
        }

        /// The Workspaces and Tabs the server last confirmed, as this client retains them.
        pub(crate) fn retained_copy(&self) -> Option<RetainedInventory> {
            self.confirmed.clone().or_else(|| {
                self.retained_views
                    .as_ref()?
                    .retained_inventory()
                    .filter(|_| self.retains_copies)
            })
        }

        /// Presents the retained copy until the server publishes an inventory. Call once the
        /// client views are read.
        pub(crate) fn present_retained_copy(&mut self) {
            if self.saved.is_none() {
                self.saved = self.retained_copy();
            }
            if self.confirmed.is_none() {
                self.confirmed.clone_from(&self.saved);
            }
            self.edits
                .stand_in(self.confirmed.as_ref().map(RetainedInventory::inventory));
        }

        /// Follows a paired route's new transport. The presented Workspaces, Tabs and this
        /// client's views of them stay; the next inventory reconciles them, and until the
        /// transport publishes one the retained copy stands in.
        pub(crate) fn replace_transport(&mut self, edits: Rc<dyn SharedWorkspaceEdits>) {
            self.edits
                .stand_in(self.confirmed.as_ref().map(RetainedInventory::inventory));
            // The next inventory is read afresh: it initializes attachments and rebinds Panes.
            self.revision = None;
            self.presentation = None;
            self.edits.replace_transport(edits);
        }

        /// The title each Pane presents while its route has no connection: the title a program
        /// set, while this process still holds the server's inventory, and otherwise the shell
        /// name this client retained. Program-set titles are never written to storage.
        pub(crate) fn unavailable_titles(&self) -> BTreeMap<Id, String> {
            use spaceterm_server::pane::metadata::{MAX_TITLE_CHARS, TitleProvenance};
            let mut titles: BTreeMap<Id, String> = self
                .confirmed
                .iter()
                .flat_map(|copy| &copy.workspaces)
                .flat_map(|workspace| &workspace.tabs)
                .flat_map(|tab| &tab.panes)
                .filter_map(|pane| Some((pane.id, pane.shell.clone()?)))
                .collect();
            let published = self.latest.iter().filter(|_| !self.standing_in);
            for pane in published
                .flat_map(|inventory| &inventory.workspaces)
                .flat_map(|workspace| &workspace.tabs)
                .flat_map(|tab| &tab.panes)
            {
                let Some(presentation) = pane.presentation.as_deref() else {
                    continue;
                };
                let title = match presentation.title_provenance {
                    TitleProvenance::TerminalControl => &presentation.title,
                    TitleProvenance::Fallback | TitleProvenance::WorkingDirectory => {
                        &presentation.shell_name
                    }
                };
                let title: String = title
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(MAX_TITLE_CHARS)
                    .collect();
                if !title.trim().is_empty() {
                    titles.insert(pane.id, title.trim().to_owned());
                }
            }
            titles
        }

        /// Removes the retained copy for good, as when the person removes the paired route.
        pub(crate) fn discard_retained_copy(&mut self) {
            self.retains_copies = false;
            self.confirmed = None;
            self.saved = None;
            self.edits.stand_in(None);
            if let Some(views) = &self.retained_views {
                views.discard_inventory();
            }
        }

        /// Retains `copy` and attaches its Workspaces, as a relaunched client reads them from
        /// its storage.
        #[cfg(test)]
        pub(crate) fn retain_copy_for_test(&mut self, copy: RetainedInventory) {
            let attached: Vec<_> = copy
                .workspaces
                .iter()
                .map(|workspace| workspace.id)
                .collect();
            if let Ok(attachments) =
                spaceterm_server::attachments::Attachments::relaunched(&attached)
            {
                self.attachments = attachments;
            }
            self.retains_copies = true;
            self.saved = Some(copy.clone());
            self.confirmed = Some(copy);
        }

        /// The copy last written to this client's storage.
        #[cfg(test)]
        pub(crate) const fn saved_copy(&self) -> Option<&RetainedInventory> {
            self.saved.as_ref()
        }

        /// Whether this client's attachments let it open the server Pane `pane`.
        #[cfg(test)]
        pub(crate) fn permits(&self, pane: Id) -> bool {
            self.attachments.permit(pane).is_some()
        }

        /// Whether this binding presents the retained copy rather than the server's inventory.
        #[cfg(test)]
        pub(crate) const fn presents_retained_copy(&self) -> bool {
            self.standing_in
        }

        pub(crate) fn available_workspaces(&self) -> Vec<SharedAvailableWorkspace> {
            self.latest
                .iter()
                .flat_map(|inventory| &inventory.workspaces)
                .filter_map(|workspace| {
                    let local = self.local(workspace.id);
                    (self.connected || local.is_some()).then(|| SharedAvailableWorkspace {
                        server: workspace.id,
                        name: workspace.name.clone(),
                        local,
                    })
                })
                .collect()
        }
        /// This client's exact approved close still owns an unresolved Original Operation.
        /// Published deletion can retire the view; the edit owner outlives its window.
        pub(crate) fn optimistically_closing(&self, local: WorkspaceId) -> bool {
            let Some(facts) = self.facts(local) else {
                return false;
            };
            let server = self.edits.canonical(facts.server);
            if self
                .edits
                .pending
                .borrow()
                .closing_workspace(server, self.latest.as_ref())
            {
                return true;
            }
            // A validated committed close can precede acknowledgement. Its transport stays
            // owned even when published deletion has retired the view or the route disconnects.
            let transport = self.edits.transport().snapshot();
            transport.pending
                && transport
                    .pending_operation
                    .as_ref()
                    .is_some_and(|mutation| super::pending::closes_workspace(mutation, server))
        }

        pub(crate) fn latest_inventory(&self) -> Option<&Inventory> {
            self.latest.as_ref()
        }

        /// Names the presentation this binding is.
        pub(crate) fn identify(&mut self, owner: Rc<dyn WorkspaceOwner>) {
            self.owner = Some(owner);
        }

        /// The presentation this binding is, once its connection names it.
        pub(crate) fn owner(&self) -> Option<OwnerId> {
            self.owner.as_ref()?.owner()
        }

        /// Whether `workspace` presents a creation the server hasn't confirmed yet.
        pub(crate) fn is_provisional(&self, workspace: WorkspaceId) -> bool {
            self.facts(workspace)
                .is_some_and(|facts| self.edits.provisional(facts.server))
        }

        /// Names `workspace` in this client's Workspace order, once the server has published it.
        /// A provisional Workspace has no reference yet.
        pub(crate) fn workspace_reference(
            &self,
            workspace: WorkspaceId,
        ) -> Option<spaceterm_server::workspace_order::WorkspaceReference> {
            let owner = self.owner()?;
            let id = self.edits.canonical(self.facts(workspace)?.server);
            self.latest
                .as_ref()?
                .workspaces
                .iter()
                .any(|published| published.id == id)
                .then_some(spaceterm_server::workspace_order::WorkspaceReference {
                    owner,
                    workspace: id,
                })
        }

        /// The Git facts the server read for the directories its Workspaces show.
        pub(crate) const fn repositories(&self) -> &super::SharedRepositories {
            &self.repositories
        }

        /// Whether this client is connected to the server now.
        pub(crate) const fn connected(&self) -> bool {
            self.connected
        }

        /// The server Workspace one local Workspace presents, as the server published it last.
        /// Live facts such as a Pane's confirmed directory can change without a membership
        /// revision, so this reads the latest inventory rather than the applied one.
        pub(crate) fn server_workspace(&self, local: WorkspaceId) -> Option<&ServerWorkspace> {
            let server = self.facts(local)?.server;
            self.latest
                .as_ref()?
                .workspaces
                .iter()
                .find(|workspace| workspace.id == server)
        }
        pub(crate) fn attachments_changeable(&self) -> bool {
            self.attachments.changeable()
                && (self.retained_views.is_none() || self.saved_views.is_some())
        }
        pub(crate) fn attach(&mut self, server: Id) -> Result<(), SharedAttachmentFailure> {
            if !self.connected || !self.attachments_changeable() {
                return Err(SharedAttachmentFailure::Unchanged);
            }
            match self.attachments.attach(server) {
                Ok(true) => Ok(()),
                Ok(false) => Err(SharedAttachmentFailure::Unchanged),
                Err(_) => Err(if self.attachments.changeable() {
                    SharedAttachmentFailure::Unchanged
                } else {
                    SharedAttachmentFailure::Uncertain
                }),
            }
        }
        pub(crate) fn detach(&mut self, local: WorkspaceId) -> Result<(), SharedAttachmentFailure> {
            if !self.attachments_changeable() {
                return Err(SharedAttachmentFailure::Unchanged);
            }
            let server = self
                .facts(local)
                .ok_or(SharedAttachmentFailure::Unchanged)?
                .server;
            self.attachments.detach(server).map_err(|_| {
                if self.attachments.changeable() {
                    SharedAttachmentFailure::Unchanged
                } else {
                    SharedAttachmentFailure::Uncertain
                }
            })
        }

        pub(crate) fn retain_client_views(&mut self, server: ServerConnection) {
            self.retained_views = Some(crate::ui::client_view_state::RetainedClientViews::new(
                server,
            ));
        }

        /// Completes once the saved arrangement the first inventory waits for has been read,
        /// or at once when this client retains none.
        pub(crate) fn client_views_read(&self) -> impl std::future::Future<Output = ()> + 'static {
            let read = self
                .retained_views
                .as_ref()
                .map(crate::ui::client_view_state::RetainedClientViews::read);
            async move {
                if let Some(read) = read {
                    read.await;
                }
            }
        }

        /// Wait for the saved arrangement before applying the first inventory.
        pub(crate) fn prepare_client_views(
            &mut self,
        ) -> Result<bool, crate::server_connection::ServerError> {
            if self.saved_views.is_some() || self.retained_views.is_none() {
                return Ok(true);
            }
            match self
                .retained_views
                .as_ref()
                .and_then(|views| views.loaded())
            {
                Some(Ok(saved)) => {
                    if let Some(attachments) = self
                        .retained_views
                        .as_ref()
                        .and_then(|views| views.attachments())
                    {
                        self.attachments = attachments?;
                    } else {
                        return Ok(false);
                    }
                    self.saved_views = Some(saved);
                    Ok(true)
                }
                Some(Err(error)) => Err(error),
                None => Ok(false),
            }
        }

        pub(crate) fn saved_arrangement(
            &self,
            workspace: Id,
        ) -> Option<&spaceterm_server::client_view::WorkspaceView> {
            self.saved_views
                .as_ref()?
                .workspaces
                .iter()
                .find(|view| view.workspace == workspace)
        }

        pub(crate) fn save_arrangements(
            &mut self,
            mut view: spaceterm_server::client_view::ClientView,
        ) {
            // Presentation identities and optimistic removals are never retained as membership.
            if self.edits.has_pending() {
                return;
            }
            // Detached Workspaces keep their arrangement until explicitly reattached.
            if let Some(previous) = self.last_saved_view.as_ref().or(self.saved_views.as_ref()) {
                for saved in &previous.workspaces {
                    if !view
                        .workspaces
                        .iter()
                        .any(|v| v.workspace == saved.workspace)
                        && self
                            .latest
                            .as_ref()
                            .is_some_and(|i| i.workspaces.iter().any(|w| w.id == saved.workspace))
                    {
                        view.workspaces.push(saved.clone());
                    }
                }
            }
            if self.revision.is_none()
                || (self.saved_views.is_none() && self.retained_views.is_some())
                || self.last_saved_view.as_ref() == Some(&view)
            {
                return;
            }
            if let Some(retained) = &self.retained_views {
                retained.save(view.clone());
            }
            self.saved_views = Some(view.clone());
            self.last_saved_view = Some(view);
        }

        pub(crate) fn take_view_error(&mut self) -> Option<ArrangementUnavailable> {
            if self.view_error_reported {
                return None;
            }
            let retained = self.retained_views.as_ref()?;
            let notice = if !self.attachments.changeable() {
                ArrangementUnavailable::Write
            } else if let Some(error) = self.attachment_error {
                ArrangementUnavailable::reading(error)
            } else if let Some(error) = retained.attachments().and_then(Result::err) {
                ArrangementUnavailable::reading(error)
            } else if let Some(error) = retained.read_error() {
                ArrangementUnavailable::reading(error)
            } else if retained.error().is_some() {
                ArrangementUnavailable::Write
            } else {
                return None;
            };
            self.view_error_reported = true;
            Some(notice)
        }

        pub(crate) fn flush_client_views(&mut self) {
            self.retained_views.take();
        }

        pub(crate) fn worktree_status(
            &self,
            workspace: Id,
            repository: Option<&Path>,
        ) -> Option<&'static str> {
            if !self.pending || !self.connected {
                return None;
            }
            match self.pending_operation.as_ref() {
                Some(Mutation::Worktree(spaceterm_server::worktree::Operation::Create {
                    workspace: target,
                    ..
                })) if *target == workspace => Some("Creating Worktree…"),
                Some(Mutation::Worktree(spaceterm_server::worktree::Operation::Remove {
                    root,
                    ..
                })) if repository == Some(root.as_path()) => Some("Removing Worktree…"),
                _ => None,
            }
        }
        /// What `workspace`'s sidebar row says in place of its directory, if anything.
        ///
        /// Ordinary name and pin edits project their values immediately. Worktree operations
        /// retain their progress, and "Not connected" waits for an established connection loss.
        pub(crate) fn row_status(
            &self,
            workspace: Id,
            repository: Option<&Path>,
        ) -> Option<SharedRowStatus> {
            if let Some(text) = self.worktree_status(workspace, repository) {
                return Some(SharedRowStatus::progress(text));
            }
            match self.edits.editability() {
                SharedEditability::Offline
                    if self.connection_error.is_some() || self.standing_in =>
                {
                    Some(SharedRowStatus {
                        text: "Not connected",
                        warning: true,
                    })
                }
                _ => None,
            }
        }
        pub(crate) fn removing_worktree(&self, path: &Path) -> bool {
            self.pending
                && matches!(self.pending_operation.as_ref(),Some(Mutation::Worktree(spaceterm_server::worktree::Operation::Remove {path:target,..})) if target==path)
        }
        pub(crate) const fn sessions(&self) -> &SharedPaneSessions {
            &self.sessions
        }

        pub(crate) fn edits(&self) -> SharedEditSource {
            self.edits.clone()
        }

        pub(crate) fn editability(&self) -> SharedEditability {
            self.edits.editability()
        }

        /// Whether a new edit can go to the server now: its connection is up, its first
        /// inventory is read and reconciled, and this client's retained views are applied, so an
        /// accepted creation is attached to a view that a later reading won't replace.
        pub(crate) fn accepts_edits(&self) -> bool {
            self.editability().available()
                && self.latest.is_some()
                && (self.saved_views.is_some() || self.retained_views.is_none())
        }

        /// Waits for exact creation identities before capturing any destructive approval.
        pub(crate) fn defer_close(
            &self,
            intent: SharedCloseIntent,
        ) -> Result<bool, SharedRefusalReason> {
            if !self.edits.transport().availability().0 {
                return Err(SharedRefusalReason::Offline);
            }
            self.edits.pending.borrow_mut().defer_close(intent)
        }

        pub(crate) fn take_close_intent(&self) -> Option<SharedCloseIntent> {
            self.edits.pending.borrow_mut().take_close_intent()
        }

        /// Captures what closing `target` ends, as the server reported it last. The capture is
        /// what a person reviews and what a confirmed close submits; nothing rebuilds it later.
        pub(crate) fn close_approval(
            &self,
            target: ServerCloseTarget,
        ) -> Result<SharedCloseApproval, SharedCloseUnavailable> {
            let editability = self.editability();
            if !editability.available() {
                return Err(SharedCloseUnavailable::Edits(editability.into()));
            }
            let inventory = self
                .latest
                .as_ref()
                .ok_or(SharedCloseUnavailable::Unreported)?;
            let workspace = target_workspace(inventory, target)
                .filter(|_| target_exists(inventory, target))
                .ok_or(SharedCloseUnavailable::Missing)?;
            // Membership exists, so a missing confirmation means the server has not reported the
            // live Sessions a person would be approving.
            let confirmation = inventory
                .close_confirmation(target)
                .ok_or(SharedCloseUnavailable::Unreported)?;
            let scope = match target {
                _ if confirmation.removes_workspace => SharedCloseScope::Workspace,
                ServerCloseTarget::Pane { .. } => SharedCloseScope::Pane,
                ServerCloseTarget::Tab { .. } => SharedCloseScope::Tab,
                ServerCloseTarget::Workspace { .. } => SharedCloseScope::Workspace,
                ServerCloseTarget::Worktree { .. } => SharedCloseScope::Worktree,
            };
            Ok(SharedCloseApproval {
                target,
                scope,
                workspace: workspace.name.clone(),
                tabs: workspace
                    .tabs
                    .iter()
                    .filter(|tab| {
                        tab.panes.iter().any(|pane| {
                            confirmation
                                .panes
                                .iter()
                                .any(|confirmed| confirmed.id == pane.id)
                        })
                    })
                    .count(),
                confirmation,
                controls: inventory.close.as_ref().map(|live| live.controls.clone()),
            })
        }

        /// Whether what an approval would end and remove is still exactly what the server
        /// reports now.
        pub(crate) fn close_state(&self, approval: &SharedCloseApproval) -> SharedCloseState {
            let Some(inventory) = self.latest.as_ref() else {
                return SharedCloseState::Changed;
            };
            if !target_exists(inventory, approval.target) {
                return SharedCloseState::Missing;
            }
            match inventory.close_confirmation(approval.target) {
                Some(current) if current == approval.confirmation => SharedCloseState::Current,
                _ => SharedCloseState::Changed,
            }
        }

        /// Asks the server to end exactly what a person approved. `local` is the target a fresh
        /// review reopens if the server finds the approval stale.
        pub(crate) fn close(
            &mut self,
            approval: SharedCloseApproval,
            local: CloseTarget,
        ) -> SharedSubmission {
            let target = approval.target;
            let submission = self.ask(Mutation::Close {
                target,
                confirmation: approval.confirmation,
            });
            if submission == SharedSubmission::Submitted {
                self.submitted_close = Some((target, local));
            }
            submission
        }

        /// The oldest failed Worktree operation still to present, and its alert's presentation.
        /// It stays until [`Self::take_worktree_failure`].
        pub(crate) fn next_worktree_failure(&self) -> Option<(WorktreeFailure, OutcomeNotice)> {
            let failure = self.worktree_failures.front()?.clone();
            let notice = self.notice([failure.sequence]);
            Some((failure, notice))
        }
        pub(crate) fn take_worktree_failure(&mut self) -> Option<WorktreeFailure> {
            let failure = self.worktree_failures.pop_front()?;
            self.hand(failure.sequence);
            Some(failure)
        }

        /// Records that an admitted alert presents `sequence` until the person answers it.
        fn hand(&self, sequence: u64) {
            if sequence == FORGOTTEN || !self.edits.acknowledged.borrow().contains(&sequence) {
                self.handed.borrow_mut().insert(sequence);
            }
        }

        /// The alert presentation of these unseen outcomes.
        fn notice(&self, sequences: impl Into<Rc<[u64]>>) -> OutcomeNotice {
            OutcomeNotice {
                source: self.edits.clone(),
                sequences: sequences.into(),
                forgotten: None,
                handed: Rc::clone(&self.handed),
            }
        }

        /// The oldest close the server refused as stale, with the local target this process
        /// asked to close when it is the one that asked, and its alert's presentation. A
        /// recovered answer names no local target. It stays until [`Self::take_stale_close`].
        pub(crate) fn next_stale_close(
            &self,
        ) -> Option<(ServerCloseTarget, Option<CloseTarget>, OutcomeNotice)> {
            let (target, sequence) = *self.stale_closes.front()?;
            let local = self
                .submitted_close
                .filter(|(submitted, _)| *submitted == target)
                .map(|(_, local)| local);
            Some((target, local, self.notice([sequence])))
        }
        pub(crate) fn take_stale_close(
            &mut self,
        ) -> Option<(ServerCloseTarget, Option<CloseTarget>)> {
            let (target, sequence) = self.stale_closes.pop_front()?;
            self.hand(sequence);
            let local = self
                .submitted_close
                .take()
                .filter(|(submitted, _)| *submitted == target)
                .map(|(_, local)| local);
            Some((target, local))
        }

        pub(crate) const fn review(&self) -> Option<&SharedCloseReview> {
            self.review.as_ref()
        }

        pub(crate) fn begin_review(&mut self, review: SharedCloseReview) {
            self.review = Some(review);
        }

        /// Ends the review with `generation`, returning it once.
        pub(crate) fn end_review(&mut self, generation: u64) -> Option<SharedCloseReview> {
            self.review
                .take_if(|review| review.generation == generation)
        }

        pub(crate) fn facts(&self, workspace: WorkspaceId) -> Option<&SharedWorkspaceFacts> {
            self.workspaces.get(&workspace)
        }

        pub(crate) fn is_shared(&self, workspace: WorkspaceId) -> bool {
            self.workspaces.contains_key(&workspace)
        }

        pub(crate) fn local(&self, server: Id) -> Option<WorkspaceId> {
            self.workspaces.iter().find_map(|(&workspace, facts)| {
                (self.edits.canonical(facts.server) == server).then_some(workspace)
            })
        }

        pub(crate) fn register(&mut self, workspace: WorkspaceId, facts: SharedWorkspaceFacts) {
            self.workspaces.insert(workspace, facts);
        }

        pub(crate) fn forget(&mut self, workspace: WorkspaceId) {
            if let Some(facts) = self.workspaces.remove(&workspace) {
                self.edits.retire_alias(facts.server);
            }
        }

        /// The refused same-field edit to present, once this client can name the Workspace it
        /// belongs to. Until then the draft waits here; it is never dropped.
        fn presentable_conflict_index(&self) -> Option<usize> {
            self.conflicts
                .iter()
                .position(|pending| self.local(pending.workspace).is_some())
        }

        /// Leaves ownership and admission capacity here until the notice is admitted. Draft
        /// review's presentation records it.
        pub(crate) fn next_conflict(&self) -> Option<(SharedWorkspaceConflict, OutcomeNotice)> {
            let pending = &self.conflicts[self.presentable_conflict_index()?];
            Some((
                SharedWorkspaceConflict {
                    workspace: self.local(pending.workspace)?,
                    field: pending.field,
                    reviewed_version: pending.reviewed_version,
                    draft: pending.draft.clone(),
                    authoritative: pending.authoritative.clone(),
                },
                self.notice([pending.sequence]),
            ))
        }

        pub(crate) fn take_conflict(&mut self) -> Option<SharedWorkspaceConflict> {
            let index = self.presentable_conflict_index()?;
            let pending = self.conflicts.remove(index)?;
            self.hand(pending.sequence);
            let workspace = self.local(pending.workspace)?;
            self.edits.pending.borrow_mut().release_conflicts(1);
            Some(SharedWorkspaceConflict {
                workspace,
                field: pending.field,
                reviewed_version: pending.reviewed_version,
                draft: pending.draft,
                authoritative: pending.authoritative,
            })
        }

        /// The oldest refusal to present, and its alert's presentation of exactly the outcomes
        /// it stands for. It stays until [`Self::take_refusal`].
        pub(crate) fn next_refusal(&self) -> Option<(SharedRefusal, OutcomeNotice)> {
            let (refusal, refused) = self.refusals.front()?;
            Some((*refusal, self.notice(refused.as_slice())))
        }
        pub(crate) fn take_refusal(&mut self) -> Option<SharedRefusal> {
            let (refusal, refused) = self.refusals.pop_front()?;
            for sequence in refused {
                self.hand(sequence);
            }
            Some(refusal)
        }

        /// The notice that the bound forgot some outcomes nobody saw. It stays until
        /// [`Self::take_forgotten`].
        pub(crate) fn next_forgotten(&self) -> Option<OutcomeNotice> {
            Some(OutcomeNotice {
                forgotten: Some(self.forgotten?),
                ..self.notice([])
            })
        }
        pub(crate) fn take_forgotten(&mut self) -> Option<u64> {
            let total = self.forgotten.take()?;
            self.hand(FORGOTTEN);
            Some(total)
        }

        pub(crate) const fn connection_error(&self) -> Option<spaceterm_server::ServerError> {
            self.connection_error
        }

        /// How many connection attempts have failed, as the connection last published.
        pub(crate) const fn failed_attempts(&self) -> u64 {
            self.failed_attempts
        }

        /// Asks the server connection to reconnect now instead of after its delay.
        pub(crate) fn retry_connection(&self) {
            self.edits.retry();
        }

        /// Wakes now, after each change the server connection publishes, and after each edit
        /// this window queues.
        pub(crate) fn subscribe(&self) -> SharedEditNotifications {
            let (sender, local) = async_channel::bounded(1);
            self.edits.subscribers.borrow_mut().push(sender);
            SharedEditNotifications {
                source: self.edits.clone(),
                local,
                remote: std::cell::RefCell::new((
                    self.edits.generation.get(),
                    self.edits.transport().subscribe(),
                )),
            }
        }

        /// Reads the server's answer and its inventory together, and reports both.
        ///
        /// Each unseen answer is taken by one surface and presented once, across windows and
        /// relaunches. An answer recovered after a relaunch still reports the draft
        /// it rejected, because that draft is a person's work; only an answer this process asked
        /// for reports an acceptance the window may act on, so a publication from another client
        /// never moves the Active Workspace, Tab, or Pane.
        pub(crate) fn poll(&mut self) -> SharedPoll {
            let (snapshot, standing_in) = self.edits.read();
            let mut poll = SharedPoll::default();
            if self.standing_in != standing_in {
                // The server's first inventory after the retained copy initializes attachments
                // and opens Panes even when its revision is the copy's.
                if !standing_in {
                    self.revision = None;
                }
                self.standing_in = standing_in;
                poll.changed = true;
            }
            if let Some(inventory) = snapshot
                .inventory
                .as_ref()
                .filter(|inventory| self.revision != Some(inventory.revision))
                .filter(|_| standing_in)
            {
                // The retained copy names only what the server last confirmed, so it never
                // initializes or reconciles attachments.
                self.revision = Some(inventory.revision);
                poll.changed = true;
            } else if let Some(inventory) = snapshot
                .inventory
                .as_ref()
                .filter(|inventory| self.revision != Some(inventory.revision))
            {
                // A new client's first view presents the server's first Workspace. Retained
                // choices, including a deliberately empty view, always win.
                let initial = inventory.workspaces.first().map(|workspace| workspace.id);
                match self.attachments.initialize(inventory, initial) {
                    Ok(true) => poll.initial = initial,
                    Ok(false) => {}
                    Err(error) => {
                        self.attachment_error = Some(error);
                        return poll;
                    }
                }
                self.revision = Some(inventory.revision);
                poll.changed = true;
            }
            let completed = self.edits.pending.borrow_mut().take_completions();
            for completion in completed {
                // An answer some surface already holds is presented there.
                if completion.outcome.outcome.needs_presentation()
                    && !self.edits.hold(completion.outcome.sequence)
                {
                    continue;
                }
                // Attachment authority records only server identities, never provisional ones.
                if let RetainedOutcome {
                    mutation: Mutation::CreateWorkspace { .. },
                    outcome: Outcome::Accepted { target },
                    ..
                } = &completion.outcome
                    && let Err(error) = self.attachments.attach(*target)
                {
                    self.attachment_error = Some(error);
                }
                self.settle(
                    &completion.outcome,
                    completion.inventory.as_ref(),
                    !completion.optimistic,
                    &mut poll,
                );
                poll.changed = true;
            }
            if std::mem::take(&mut self.edits.pending.borrow_mut().disconnected) {
                if !self
                    .refusals
                    .iter()
                    .any(|(refusal, _)| *refusal == SharedRefusal::Disconnected)
                {
                    self.refusals
                        .push_back((SharedRefusal::Disconnected, Vec::new()));
                }
                poll.changed = true;
            }
            self.pending_operation
                .clone_from(&snapshot.pending_operation);
            if self.connection_error != snapshot.error
                || self.failed_attempts != snapshot.failed_attempts
            {
                self.connection_error = snapshot.error;
                self.failed_attempts = snapshot.failed_attempts;
                poll.changed = true;
            }
            if self.connected != snapshot.connected || self.pending != snapshot.pending {
                self.connected = snapshot.connected;
                self.pending = snapshot.pending;
                poll.changed = true;
            }
            // Each unseen answer is presented once, from this run or an earlier one, by the
            // surface that holds it. The server's own inventory names what a draft review shows.
            if !standing_in && snapshot.inventory.is_some() {
                // The bound forgot some outcomes nobody saw. One notice says so.
                if self.forgotten.is_none()
                    && snapshot.forgotten
                        > snapshot
                            .forgotten_presented
                            .max(self.edits.forgotten_acknowledged.get())
                    && self.edits.hold(FORGOTTEN)
                {
                    self.forgotten = Some(snapshot.forgotten);
                    poll.changed = true;
                }
                for outcome in &snapshot.unseen {
                    if !self.edits.hold(outcome.sequence) {
                        continue;
                    }
                    // The answer and the authoritative values come from one snapshot, so a
                    // conflict never names a value an accepted edit has already replaced.
                    self.settle(outcome, snapshot.inventory.as_ref(), false, &mut poll);
                    poll.changed = true;
                }
            }
            // Live Session facts and Pane directories change without a membership revision, so
            // the latest reading is kept whether or not membership changed, and repaints the rows
            // that read it.
            if snapshot.inventory.is_some() && self.latest != snapshot.inventory {
                self.latest.clone_from(&snapshot.inventory);
                poll.changed = true;
            }
            if self.retains_copies
                && !standing_in
                && let Some(inventory) = &snapshot.inventory
            {
                let copy = RetainedInventory::retain(inventory);
                // Only a change of Workspaces, Tabs, Panes, Splits or names is written.
                if self
                    .saved
                    .as_ref()
                    .is_none_or(|saved| !saved.same_structure(&copy))
                {
                    if let Some(views) = &self.retained_views {
                        views.save_inventory(copy.clone());
                    }
                    self.saved = Some(copy.clone());
                }
                if self.confirmed.as_ref() != Some(&copy) {
                    self.confirmed = Some(copy);
                }
            }
            if let Some(inventory) = &snapshot.inventory {
                // A batch can contain an older conflict for a Workspace removed by its latest
                // publication. Keep every other draft and release only the missing targets.
                let mut removed = Vec::new();
                self.conflicts.retain(|pending| {
                    let kept = inventory
                        .workspaces
                        .iter()
                        .any(|w| w.id == pending.workspace);
                    if !kept {
                        removed.push(pending.sequence);
                    }
                    kept
                });
                if !removed.is_empty() {
                    self.edits
                        .pending
                        .borrow_mut()
                        .release_conflicts(removed.len());
                    // The Missing refusal presents what happened to each draft.
                    for sequence in removed {
                        self.refuse(SharedRefusal::Missing, sequence);
                    }
                    poll.changed = true;
                }
            }
            // Git facts carry their own revision, so a new reading reaches every view even when
            // membership is unchanged. Facts kept from before a disconnection stay presented as
            // last known.
            if let Some(inventory) = &snapshot.inventory
                && self
                    .repositories
                    .accept(inventory.repositories.as_ref(), std::time::Instant::now())
            {
                poll.changed = true;
            }
            if let Some(inventory) = snapshot.inventory {
                let projected = self.edits.pending.borrow_mut().project(inventory);
                if self.presentation.as_ref() != Some(&projected) {
                    self.presentation = Some(projected.clone());
                    poll.inventory = Some(projected);
                    poll.changed = true;
                }
            }
            // Creation takes its place when requested. The eventual acknowledgement substitutes
            // server identities and cannot move a newer Workspace, Tab, sidebar, Find or chooser.
            if let Some(presentation) = self.edits.pending.borrow_mut().take_presentation() {
                poll.accepted = Some(presentation);
            }
            poll
        }

        /// Presents one answer the server gave, against the inventory of the same snapshot. An
        /// answer that needs presenting is held for this window; one with nothing to present is
        /// recorded as presented.
        fn settle(
            &mut self,
            outcome: &RetainedOutcome,
            inventory: Option<&Inventory>,
            own: bool,
            poll: &mut SharedPoll,
        ) {
            let sequence = outcome.sequence;
            match outcome.outcome {
                Outcome::Pending => {}
                Outcome::WorktreeFailed(error) => match &outcome.mutation {
                    Mutation::Worktree(operation) => {
                        self.worktree_failures.push_back(WorktreeFailure {
                            sequence,
                            operation: operation.clone(),
                            error,
                        });
                    }
                    _ => self.edits.presented(sequence),
                },
                // An accepted change reaches every client through the inventory, which names the
                // server's own identities rather than a reply this client could have lost. The
                // client that asked for a creation also opens it, once.
                Outcome::Accepted { target } => {
                    if own {
                        poll.accepted = Some(SharedAcceptance {
                            mutation: outcome.mutation.clone(),
                            target,
                        });
                    }
                }
                Outcome::Conflict => {
                    if let Some(conflict) = inventory.and_then(|inventory| {
                        Self::conflicted(&outcome.mutation, inventory, sequence)
                    }) {
                        // Moving a completion into review keeps its place in the shared bound.
                        self.edits.pending.borrow_mut().retain_conflict();
                        self.conflicts.push_back(conflict);
                    } else {
                        // Another client removed the Workspace before this client could name
                        // it, as a relaunch can find. The draft has nowhere to go.
                        self.refuse(SharedRefusal::Missing, sequence);
                    }
                }
                Outcome::Missing => self.refuse(SharedRefusal::Missing, sequence),
                Outcome::Invalid => self.refuse(SharedRefusal::Invalid, sequence),
                Outcome::Limit => self.refuse(SharedRefusal::Limit, sequence),
                // What the close would end changed after a person approved it. Ending the new
                // Sessions needs a fresh review, so nothing asks again on its own.
                Outcome::StaleConfirmation => match &outcome.mutation {
                    Mutation::Close { target, .. } => {
                        self.stale_closes.push_back((*target, sequence));
                    }
                    _ => self.edits.presented(sequence),
                },
            }
        }

        /// Queues `refusal` for `sequence`, beside earlier outcomes refused for the same reason.
        fn refuse(&mut self, refusal: SharedRefusal, sequence: u64) {
            match self
                .refusals
                .iter_mut()
                .find(|(queued, _)| *queued == refusal)
            {
                Some((_, refused)) => refused.push(sequence),
                None => self.refusals.push_back((refusal, vec![sequence])),
            }
        }

        /// The draft one refused same-field edit kept, beside the value the server holds in the
        /// snapshot that carried the refusal.
        fn conflicted(
            mutation: &Mutation,
            inventory: &Inventory,
            sequence: u64,
        ) -> Option<PendingConflict> {
            let authority = |server: Id| {
                inventory
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == server)
            };
            match mutation {
                Mutation::RenameWorkspace {
                    workspace, name, ..
                } => Some(PendingConflict {
                    sequence,
                    workspace: *workspace,
                    field: SharedWorkspaceField::Name,
                    reviewed_version: authority(*workspace)?.name_version,
                    draft: SharedWorkspaceDraft::Name(name.clone()),
                    authoritative: SharedString::from(authority(*workspace)?.name.clone()),
                }),
                Mutation::PinWorkspace {
                    workspace,
                    directory,
                    ..
                } => Some(PendingConflict {
                    sequence,
                    workspace: *workspace,
                    field: SharedWorkspaceField::PinnedDirectory,
                    reviewed_version: authority(*workspace)?.pin_version,
                    draft: SharedWorkspaceDraft::Pin(directory.clone()),
                    authoritative: display_pin(authority(*workspace)?.pinned_directory.as_deref()),
                }),
                // Independent creations never conflict with each other, so the server answers
                // them with their own identities instead.
                Mutation::CreateWorkspace { .. }
                | Mutation::CreateTab { .. }
                | Mutation::SplitPane { .. } => None,
                // A stale close is answered with its own outcome rather than a field conflict.
                Mutation::Close { .. } | Mutation::Worktree(_) => None,
                // Ending Sessions edits no field, and its refusals reach Settings directly.
                Mutation::EndAllSessions { .. } => None,
            }
        }

        /// Applies one server inventory to this client's own map, reporting what changed. The
        /// caller owns the local collections, so it performs the creations this plan names.
        pub(crate) fn plan(&self, inventory: &Inventory) -> Vec<SharedWorkspacePlan> {
            let mut plan = Vec::new();
            for workspace in &inventory.workspaces {
                if !self.attachments.is_attached(workspace.id)
                    && !self.edits.provisional(workspace.id)
                {
                    continue;
                }
                let mut facts = SharedWorkspaceFacts::from_server(workspace);
                facts.confirmed = !self.edits.provisional(workspace.id);
                match self.local(workspace.id) {
                    Some(local) => plan.push(SharedWorkspacePlan::Update { local, facts }),
                    None => plan.push(SharedWorkspacePlan::Create { facts }),
                }
            }
            for (&local, facts) in &self.workspaces {
                if !inventory.workspaces.iter().any(|workspace| {
                    workspace.id == self.edits.canonical(facts.server)
                        && (self.attachments.is_attached(workspace.id)
                            || self.edits.provisional(workspace.id))
                }) {
                    plan.push(SharedWorkspacePlan::Removed { local });
                }
            }
            plan
        }

        /// Asks the server for a Workspace, pinned at `directory` with its first Pane there.
        pub(crate) fn create_workspace(
            &mut self,
            name: String,
            directory: Option<PathBuf>,
        ) -> SharedSubmission {
            self.ask(Mutation::CreateWorkspace {
                name,
                directory,
                control: spaceterm_server::hierarchy::CreateControl::Creator,
            })
        }

        /// Asks the server to rename a shared Workspace.
        ///
        /// `observed` is the name version the draft began at, which fences the edit to the value
        /// the person was editing: a publication that arrives while the editor is open then
        /// conflicts instead of being silently overwritten. `None` uses the version read now,
        /// which only a conflict a person reviewed asks for.
        pub(crate) fn rename(
            &mut self,
            workspace: WorkspaceId,
            name: String,
            observed: Option<u64>,
        ) -> SharedSubmission {
            let Some(facts) = self.facts(workspace) else {
                return SharedSubmission::Refused(SharedRefusalReason::Value);
            };
            let version = observed.unwrap_or(facts.name_version);
            self.ask(Mutation::RenameWorkspace {
                workspace: facts.server,
                version,
                name,
            })
        }

        /// Asks the server to pin or unpin a shared Workspace's directory.
        ///
        /// `observed` is the pin version the chooser opened at, for the same reason a rename keeps
        /// the version its editor began with.
        pub(crate) fn pin(
            &mut self,
            workspace: WorkspaceId,
            directory: Option<PathBuf>,
            observed: Option<u64>,
        ) -> SharedSubmission {
            let Some(facts) = self.facts(workspace) else {
                return SharedSubmission::Refused(SharedRefusalReason::Value);
            };
            let version = observed.unwrap_or(facts.pin_version);
            self.ask(Mutation::PinWorkspace {
                workspace: facts.server,
                version,
                directory,
            })
        }

        /// The pin version a shared Workspace shows now, which its chooser keeps while it is open.
        pub(crate) fn pin_version(&self, workspace: WorkspaceId) -> Option<u64> {
            Some(self.facts(workspace)?.pin_version)
        }

        /// Asks again with the preserved draft, fenced to the version the conflict displayed.
        /// Only a person's explicit request calls this.
        pub(crate) fn ask_again(&mut self, conflict: &SharedWorkspaceConflict) -> SharedSubmission {
            // The person read the server's value beside their draft, so asking again is fenced
            // to the version they reviewed rather than the one the draft began at.
            match conflict.draft.clone() {
                SharedWorkspaceDraft::Name(name) => {
                    self.rename(conflict.workspace, name, Some(conflict.reviewed_version))
                }
                SharedWorkspaceDraft::Pin(directory) => self.pin(
                    conflict.workspace,
                    directory,
                    Some(conflict.reviewed_version),
                ),
            }
        }

        fn ask(&mut self, mutation: Mutation) -> SharedSubmission {
            let submission = self.edits.submit(mutation);
            if submission == SharedSubmission::Submitted {
                self.pending = true;
                // A new answer replaces these notices; what they would present stays unseen.
                let sequences: Vec<u64> = self
                    .refusals
                    .drain(..)
                    .flat_map(|(_, refused)| refused)
                    .chain(self.stale_closes.drain(..).map(|(_, sequence)| sequence))
                    .collect();
                for sequence in sequences {
                    self.edits.release(sequence);
                }
                self.submitted_close = None;
            }
            submission
        }
    }

    fn target_workspace(
        inventory: &Inventory,
        target: ServerCloseTarget,
    ) -> Option<&ServerWorkspace> {
        let workspace = match target {
            ServerCloseTarget::Pane { workspace, .. }
            | ServerCloseTarget::Tab { workspace, .. }
            | ServerCloseTarget::Workspace { workspace }
            | ServerCloseTarget::Worktree { workspace, .. } => workspace,
        };
        inventory
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
    }

    /// Whether the server still has the Workspace, Tab, or Pane a close names.
    fn target_exists(inventory: &Inventory, target: ServerCloseTarget) -> bool {
        target_workspace(inventory, target).is_some_and(|workspace| {
            let tab = |id: Id| workspace.tabs.iter().find(|tab| tab.id == id);
            match target {
                ServerCloseTarget::Workspace { .. } => true,
                ServerCloseTarget::Tab { tab: id, .. } => tab(id).is_some(),
                ServerCloseTarget::Worktree { tab: id, .. } => tab(id).is_some_and(|anchor| {
                    anchor.worktree.is_some()
                        && workspace.tabs.iter().any(|tab| {
                            tab.worktree != anchor.worktree
                                || tab.worktree_repository != anchor.worktree_repository
                        })
                }),
                ServerCloseTarget::Pane { tab: id, pane, .. } => {
                    tab(id).is_some_and(|tab| tab.panes.iter().any(|p| p.id == pane))
                }
            }
        })
    }

    /// What closing one shared target ends and removes, captured from the server's report when
    /// the confirmation opened. The server ends these Panes and Sessions only if they and the
    /// Tab and Workspace the close removes are still exactly what it holds, whichever client
    /// controls them.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct SharedCloseApproval {
        target: ServerCloseTarget,
        confirmation: ServerCloseConfirmation,
        controls: Option<Vec<ConfirmedControl>>,
        scope: SharedCloseScope,
        /// The Tabs that hold the reviewed Panes.
        tabs: usize,
        /// The Workspace's name as the server published it.
        workspace: String,
    }

    /// Which close a person asked for.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum SharedCloseScope {
        Pane,
        Tab,
        Workspace,
        Worktree,
    }

    impl SharedCloseApproval {
        /// A direct close binds every reviewed Pane to its exact control grant. A Pane without a
        /// reported grant requires explicit confirmation.
        pub(crate) fn direct_close(
            &self,
            directly_closable: impl Fn(Id, bool) -> bool,
        ) -> Option<Self> {
            if self.scope == SharedCloseScope::Workspace {
                return None;
            }
            let controls = self.controls.as_ref()?;
            let own_controls = self
                .confirmation
                .panes
                .iter()
                .map(|pane| {
                    if !directly_closable(pane.id, pane.session.is_some()) {
                        return None;
                    }
                    controls
                        .iter()
                        .find(|control| control.pane == pane.id && control.session == pane.session)
                        .cloned()
                })
                .collect::<Option<Vec<_>>>()?;
            let mut approval = self.clone();
            approval.confirmation.own_controls = Some(own_controls);
            Some(approval)
        }

        /// The live Terminal Sessions the close ends. A retained Pane whose shell has not
        /// started yet has none.
        fn sessions(&self) -> usize {
            self.confirmation
                .panes
                .iter()
                .filter(|pane| pane.session.is_some())
                .count()
        }

        pub(crate) fn title(&self) -> String {
            match self.scope {
                SharedCloseScope::Pane => "Close Pane?".to_owned(),
                SharedCloseScope::Tab => "Close Tab?".to_owned(),
                SharedCloseScope::Worktree => "Close Tabs?".to_owned(),
                SharedCloseScope::Workspace => {
                    format!("Kill Workspace \u{201C}{}\u{201D}?", self.workspace)
                }
            }
        }

        pub(crate) const fn confirm_label(&self) -> &'static str {
            match self.scope {
                SharedCloseScope::Pane => "Close Pane",
                SharedCloseScope::Tab => "Close Tab",
                SharedCloseScope::Worktree => "Close Tabs",
                SharedCloseScope::Workspace => "Kill Workspace",
            }
        }

        /// What closes, what ends, and for whom. Every client loses these Panes and Sessions,
        /// including a client that controls one of the Panes.
        pub(crate) fn message(&self) -> String {
            let sessions = self.sessions();
            let ends = match sessions {
                0 => None,
                1 => Some("1 Terminal Session".to_owned()),
                count => Some(format!("{count} Terminal Sessions")),
            };
            let stops = if sessions == 1 { "its" } else { "their" };
            match self.target {
                ServerCloseTarget::Workspace { .. } => {
                    let tabs = match self.tabs {
                        1 => "1 Tab".to_owned(),
                        count => format!("{count} Tabs"),
                    };
                    match ends {
                        Some(ends) => format!(
                            "Its {tabs} and {ends} end for every client. This can\u{2019}t be undone."
                        ),
                        None => format!(
                            "Its {tabs} close for every client. This can\u{2019}t be undone."
                        ),
                    }
                }
                ServerCloseTarget::Pane { .. }
                | ServerCloseTarget::Tab { .. }
                | ServerCloseTarget::Worktree { .. }
                    if self.confirmation.removes_workspace =>
                {
                    let only = match self.target {
                        ServerCloseTarget::Pane { .. } => "This is the only Pane",
                        ServerCloseTarget::Worktree { .. } if self.tabs != 1 => {
                            "These are the only Tabs"
                        }
                        _ => "This is the only Tab",
                    };
                    let removal = format!(
                        "{only} in \u{201C}{}\u{201D}, so the Workspace is removed for every client.",
                        self.workspace
                    );
                    match ends {
                        Some(ends) => format!("{removal} This ends {ends}."),
                        None => removal,
                    }
                }
                ServerCloseTarget::Pane { .. } if self.confirmation.removes_tab => {
                    let removal =
                        "This is the only Pane in its Tab, so the Tab closes for every client.";
                    match ends {
                        Some(ends) => format!(
                            "{removal} This ends {ends} and stops {stops} running commands."
                        ),
                        None => format!("{removal} No Terminal Session is running in it."),
                    }
                }
                ServerCloseTarget::Pane { .. } => match ends {
                    Some(ends) => format!(
                        "This ends {ends} and stops {stops} running commands for every client."
                    ),
                    None => {
                        "The Pane closes for every client. No Terminal Session is running in it."
                            .to_owned()
                    }
                },
                ServerCloseTarget::Tab { .. } => {
                    let removal = match self.confirmation.panes.len() {
                        1 => "The Tab and its Pane close for every client.".to_owned(),
                        count => format!("The Tab and its {count} Panes close for every client."),
                    };
                    match ends {
                        Some(ends) => format!(
                            "{removal} This ends {ends} and stops {stops} running commands."
                        ),
                        None => format!("{removal} No Terminal Session is running in it."),
                    }
                }
                ServerCloseTarget::Worktree { .. } => ends
                    .map(|ends| format!("This ends {ends} for every client."))
                    .unwrap_or_default(),
            }
        }

        /// What the close keeps, and whether it can be undone.
        pub(crate) fn detail(&self) -> Option<&'static str> {
            match self.target {
                ServerCloseTarget::Worktree { .. } if !self.confirmation.removes_workspace => {
                    Some("The Worktree, its files, and its branch are kept.")
                }
                ServerCloseTarget::Workspace { .. } => {
                    Some("Repository files and Worktrees stay on disk.")
                }
                _ if self.confirmation.removes_workspace => Some(
                    "Repository files and Worktrees stay on disk. This can\u{2019}t be undone.",
                ),
                _ => None,
            }
        }

        /// The title of the notice that replaces a confirmation another client made stale.
        pub(crate) const fn changed_title(&self) -> &'static str {
            shared_close_changed_title(self.target)
        }

        /// The title of the notice that replaces a confirmation whose target another client
        /// already removed.
        pub(crate) const fn missing_title(&self) -> &'static str {
            match self.scope {
                SharedCloseScope::Pane => "Pane Already Closed",
                SharedCloseScope::Tab => "Tab Already Closed",
                SharedCloseScope::Worktree => "Tabs Already Closed",
                SharedCloseScope::Workspace => "Workspace Already Removed",
            }
        }
    }

    /// The title of the notice that says a close ended nothing because what it would end changed
    /// after the confirmation opened.
    pub(crate) const fn shared_close_changed_title(target: ServerCloseTarget) -> &'static str {
        match target {
            ServerCloseTarget::Pane { .. } => "Pane Changed on Another Client",
            ServerCloseTarget::Tab { .. } => "Tab Changed on Another Client",
            ServerCloseTarget::Worktree { .. } => "Tabs Changed on Another Client",
            ServerCloseTarget::Workspace { .. } => "Workspace Changed on Another Client",
        }
    }

    /// Why a shared close cannot be reviewed now. Nothing was sent.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum SharedCloseUnavailable {
        Edits(SharedRefusalReason),
        /// The server no longer has the target.
        Missing,
        /// The server has not reported the live Sessions the close would end, so a person cannot
        /// see what they would approve.
        Unreported,
    }

    /// Whether an approval still names exactly what the server holds.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum SharedCloseState {
        Current,
        /// A Pane joined or left, or a Session started or ended, after the confirmation opened.
        Changed,
        /// Another client already removed the target.
        Missing,
    }

    /// An unapproved close waiting for the exact provisional creation it names.
    pub(crate) struct SharedCloseIntent {
        pub(crate) server: ServerCloseTarget,
        pub(crate) local: CloseTarget,
        /// The initiating keyboard owner, before any confirmation or later navigation.
        pub(crate) keyboard: Option<gpui::FocusHandle>,
    }

    /// One shared close confirmation on screen: its Close Confirmation generation, the local
    /// target it closes, what it approves, and the presentation another client's change replaces.
    pub(crate) struct SharedCloseReview {
        pub(crate) generation: u64,
        pub(crate) target: CloseTarget,
        pub(crate) approval: SharedCloseApproval,
        pub(crate) presentation: Option<spaceterm_ui::ModalPresentationHandle>,
    }

    /// Why this window removes what the server no longer has. Only this client's own accepted
    /// close moves the keyboard to what remains, as a local close does; another client's change
    /// never does.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum SharedRemoval {
        Published,
        OwnClose,
    }

    /// What one authoritative reading and its pending presentation changed.
    #[derive(Default)]
    pub(crate) struct SharedPoll {
        /// The presentation changed, so the window repaints.
        pub(crate) changed: bool,
        /// The server published membership this client has not applied yet.
        pub(crate) inventory: Option<Inventory>,
        /// The initiating interaction presents a provisional creation, or a nonoptimistic edit
        /// completed. An older creation's acknowledgement never opens its target again.
        pub(crate) accepted: Option<SharedAcceptance>,
        /// The Workspace this client chose as its first-ever view just now. Opening it is the
        /// person's first use, so the window opens its Active Tab for them.
        pub(crate) initial: Option<Id>,
    }

    /// An initiating presentation target or a completed edit this process submitted.
    pub(crate) struct SharedAcceptance {
        pub(crate) mutation: Mutation,
        pub(crate) target: Id,
    }

    /// One step of applying a server inventory to this client's own Workspaces.
    pub(crate) enum SharedWorkspacePlan {
        Create {
            facts: SharedWorkspaceFacts,
        },
        Update {
            local: WorkspaceId,
            facts: SharedWorkspaceFacts,
        },
        /// The server no longer has the Workspace, so this client stops presenting it as shared.
        Removed {
            local: WorkspaceId,
        },
    }

    /// An edit source whose answers a test sets and whose submissions it reads.
    #[cfg(test)]
    pub(crate) mod testing {
        use std::sync::Mutex;

        /// A presentation whose owner the test names.
        pub(crate) struct FixedOwner(pub(crate) spaceterm_server::hierarchy::Id);

        impl super::WorkspaceOwner for FixedOwner {
            fn owner(&self) -> Option<spaceterm_server::workspace_order::OwnerId> {
                Some(spaceterm_server::workspace_order::OwnerId::from_bytes(
                    self.0,
                ))
            }
        }

        use spaceterm_server::hierarchy::{Inventory, Mutation, Outcome, RetainedOutcome};

        use super::SharedWorkspaceEdits;
        use crate::terminal::shared_workspace::SharedWorkspaceSnapshot;

        /// A scripted Pane whose status a test sets and whose explicit requests it counts.
        pub(crate) struct ScriptedPane {
            status: Mutex<crate::terminal::server_session::DevelopmentPaneStatus>,
            startup_pending: std::sync::atomic::AtomicBool,
            controller: Mutex<Option<spaceterm_server::pane::control::Controller>>,
            session_end: Mutex<Option<spaceterm_server::pane::SessionEnd>>,
            generation: std::sync::atomic::AtomicU64,
            selections: std::sync::atomic::AtomicUsize,
            created_starts: std::sync::atomic::AtomicUsize,
            take_overs: std::sync::atomic::AtomicUsize,
            restored_starts: std::sync::atomic::AtomicUsize,
            retries: std::sync::atomic::AtomicUsize,
            /// Hints the Tab after each change, as a real connection does.
            changes: crate::terminal::changes::Changes,
        }

        impl ScriptedPane {
            pub(crate) fn new() -> std::sync::Arc<Self> {
                std::sync::Arc::new(Self {
                    status: Mutex::new(
                        crate::terminal::server_session::DevelopmentPaneStatus::Connecting,
                    ),
                    startup_pending: std::sync::atomic::AtomicBool::new(true),
                    controller: Mutex::default(),
                    session_end: Mutex::default(),
                    generation: std::sync::atomic::AtomicU64::new(0),
                    selections: std::sync::atomic::AtomicUsize::new(0),
                    created_starts: std::sync::atomic::AtomicUsize::new(0),
                    take_overs: std::sync::atomic::AtomicUsize::new(0),
                    restored_starts: std::sync::atomic::AtomicUsize::new(0),
                    retries: std::sync::atomic::AtomicUsize::new(0),
                    changes: crate::terminal::changes::Changes::default(),
                })
            }

            pub(crate) fn set(
                &self,
                status: crate::terminal::server_session::DevelopmentPaneStatus,
            ) {
                self.startup_pending.store(
                    matches!(
                        status,
                        crate::terminal::server_session::DevelopmentPaneStatus::Connecting
                            | crate::terminal::server_session::DevelopmentPaneStatus::TakingOver
                            | crate::terminal::server_session::DevelopmentPaneStatus::Reconnecting
                    ),
                    std::sync::atomic::Ordering::SeqCst,
                );
                *self.status.lock().expect("status") = status;
                self.changes.publish();
            }

            /// An authenticated admission can precede every screen of a hidden Pane.
            pub(crate) fn admitted(&self) {
                self.startup_pending
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                self.changes.publish();
            }

            /// The server offers Take Over and says who controls the Pane.
            pub(crate) fn offer_take_over(
                &self,
                controller: spaceterm_server::pane::control::Controller,
            ) {
                *self.controller.lock().expect("controller") = Some(controller);
                self.set(crate::terminal::server_session::DevelopmentPaneStatus::Takeover);
            }

            /// A new connection reaches the Terminal Session and supplies its screen.
            pub(crate) fn reconnect(&self) {
                self.generation
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.set(crate::terminal::server_session::DevelopmentPaneStatus::Connected);
            }

            /// The server says why the Pane's Session ended, if it did.
            pub(crate) fn set_session_end(&self, end: Option<spaceterm_server::pane::SessionEnd>) {
                *self.session_end.lock().expect("session end") = end;
                self.changes.publish();
            }

            /// A new connection reaches the server, which answers with `status`.
            pub(crate) fn reconnect_to(
                &self,
                status: crate::terminal::server_session::DevelopmentPaneStatus,
            ) {
                self.generation
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.set(status);
            }

            /// How many bound creations requested startup independently of selection.
            pub(crate) fn created_starts(&self) -> usize {
                self.created_starts
                    .load(std::sync::atomic::Ordering::SeqCst)
            }

            /// How many times a person's explicit action asked the server for this Pane.
            pub(crate) fn selections(&self) -> usize {
                self.selections.load(std::sync::atomic::Ordering::SeqCst)
            }

            pub(crate) fn take_overs(&self) -> usize {
                self.take_overs.load(std::sync::atomic::Ordering::SeqCst)
            }

            pub(crate) fn restored_starts(&self) -> usize {
                self.restored_starts
                    .load(std::sync::atomic::Ordering::SeqCst)
            }

            pub(crate) fn retries(&self) -> usize {
                self.retries.load(std::sync::atomic::Ordering::SeqCst)
            }
        }

        impl crate::ui::server_pane::ServerPaneConnection for ScriptedPane {
            fn status(&self) -> crate::terminal::server_session::DevelopmentPaneStatus {
                *self.status.lock().expect("status")
            }

            fn startup_pending(&self) -> bool {
                self.startup_pending
                    .load(std::sync::atomic::Ordering::SeqCst)
            }

            fn controller(&self) -> Option<spaceterm_server::pane::control::Controller> {
                self.controller.lock().expect("controller").clone()
            }

            fn session_end(&self) -> Option<spaceterm_server::pane::SessionEnd> {
                *self.session_end.lock().expect("session end")
            }

            fn connection_generation(&self) -> u64 {
                self.generation.load(std::sync::atomic::Ordering::SeqCst)
            }

            fn observe(&self, hint: async_channel::Sender<()>) {
                self.changes.observe(hint);
            }

            fn start_created(&self) {
                self.created_starts
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }

            fn select(&self) {
                self.selections
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }

            fn retry(&self) {
                self.retries
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }

            fn take_over(&self) {
                self.take_overs
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut status = self.status.lock().expect("status");
                if *status == crate::terminal::server_session::DevelopmentPaneStatus::Takeover {
                    *status = crate::terminal::server_session::DevelopmentPaneStatus::TakingOver;
                }
                drop(status);
                self.changes.publish();
            }

            fn start_restored(&self) {
                self.restored_starts
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }

        /// Scripted Pane subscriptions over one local Terminal Session factory, so a test drives
        /// the real binding and reconciliation without a server.
        pub(crate) struct ScriptedPanes {
            local_files: crate::platform::local_filesystem::LocalFilesystemAuthority,
            session_factory: crate::terminal::WorkspaceTerminalSessionFactory,
            panes: Mutex<std::collections::BTreeMap<super::Id, std::sync::Arc<ScriptedPane>>>,
        }

        impl ScriptedPanes {
            pub(crate) fn new(
                session_factory: crate::terminal::WorkspaceTerminalSessionFactory,
            ) -> std::rc::Rc<Self> {
                std::rc::Rc::new(Self {
                    local_files: crate::runtime::local_filesystem(),
                    session_factory,
                    panes: Mutex::new(std::collections::BTreeMap::new()),
                })
            }

            /// The scripted connections behind every server Pane opened so far.
            pub(crate) fn opened(&self) -> Vec<std::sync::Arc<ScriptedPane>> {
                self.panes
                    .lock()
                    .expect("panes")
                    .values()
                    .cloned()
                    .collect()
            }

            /// The scripted connection behind one server Pane, once it has been opened.
            pub(crate) fn pane(&self, pane: super::Id) -> std::sync::Arc<ScriptedPane> {
                std::sync::Arc::clone(
                    self.panes
                        .lock()
                        .expect("panes")
                        .get(&pane)
                        .expect("the Pane must have been opened"),
                )
            }
        }

        impl super::SharedPaneOpener for ScriptedPanes {
            fn placeholder(&self) -> super::WorkspaceTerminalSessionFactory {
                super::WorkspaceTerminalSessionFactory::pending(
                    crate::terminal::testing::test_local_directory(std::path::PathBuf::from(
                        "/tmp/spaceterm-shared-test",
                    )),
                )
            }

            fn local_filesystem(
                &self,
            ) -> Option<&crate::platform::local_filesystem::LocalFilesystemAuthority> {
                Some(&self.local_files)
            }

            fn open(&self, pane: super::Id) -> Option<super::SharedPaneSubscription> {
                let connection = ScriptedPane::new();
                self.panes
                    .lock()
                    .expect("panes")
                    .insert(pane, std::sync::Arc::clone(&connection));
                Some((self.session_factory.clone(), connection))
            }
        }

        /// A scripted server whose inventory, connection, and answers the test sets.
        pub(crate) struct ScriptedEdits {
            snapshot: Mutex<SharedWorkspaceSnapshot>,
            submitted: Mutex<Vec<Mutation>>,
            /// The next sequence a settled answer carries.
            sequence: Mutex<u64>,
            /// How many times a person asked to reconnect now.
            retries: std::sync::atomic::AtomicUsize,
            /// Hints readers after each change, as the real client does.
            changes: crate::terminal::changes::Changes,
        }

        impl ScriptedEdits {
            pub(crate) fn new(inventory: Option<Inventory>) -> Self {
                Self {
                    snapshot: Mutex::new(SharedWorkspaceSnapshot {
                        end_all_sessions: true,
                        worktree_writes: false,
                        inventory,
                        connected: true,
                        pending: false,
                        pending_operation: None,
                        rejected: None,
                        outcome: None,
                        unseen: Vec::new(),
                        presenting: Vec::new(),
                        forgotten: 0,
                        forgotten_presented: 0,
                        error: None,
                        failed_attempts: 0,
                    }),
                    submitted: Mutex::new(Vec::new()),
                    sequence: Mutex::new(1),
                    retries: std::sync::atomic::AtomicUsize::new(0),
                    changes: crate::terminal::changes::Changes::default(),
                }
            }

            pub(crate) fn retries(&self) -> usize {
                self.retries.load(std::sync::atomic::Ordering::SeqCst)
            }

            /// One connection attempt failed with `error`, as the real client publishes it.
            pub(crate) fn fail_attempt(&self, error: spaceterm_server::ServerError) {
                self.publish(|snapshot| {
                    snapshot.connected = false;
                    snapshot.error = Some(error);
                    snapshot.failed_attempts += 1;
                });
            }

            /// Changes the snapshot, then hints its readers.
            fn publish(&self, update: impl FnOnce(&mut SharedWorkspaceSnapshot)) {
                update(&mut self.snapshot.lock().expect("snapshot"));
                self.changes.publish();
            }

            pub(crate) fn set_inventory(&self, inventory: Inventory) {
                self.publish(|snapshot| snapshot.inventory = Some(inventory));
            }

            /// Whether the scripted server negotiated End All Sessions.
            pub(crate) fn set_end_all_sessions(&self, offered: bool) {
                self.publish(|snapshot| snapshot.end_all_sessions = offered);
            }

            pub(crate) fn set_connected(&self, connected: bool) {
                self.publish(|snapshot| snapshot.connected = connected);
            }

            pub(crate) fn set_worktree_writes(&self, worktree_writes: bool) {
                self.publish(|snapshot| snapshot.worktree_writes = worktree_writes);
            }

            pub(crate) fn set_error(&self, error: spaceterm_server::ServerError) {
                self.publish(|snapshot| snapshot.error = Some(error));
            }

            /// Answers the last submitted edit with `outcome`.
            pub(crate) fn answer(&self, outcome: Outcome) {
                let mutation = self
                    .submitted
                    .lock()
                    .expect("submitted")
                    .last()
                    .cloned()
                    .expect("an answered edit must have been submitted");
                let mut sequence = self.sequence.lock().expect("sequence");
                let answer = RetainedOutcome {
                    sequence: *sequence,
                    mutation,
                    outcome,
                };
                self.publish(|snapshot| {
                    snapshot.pending = false;
                    snapshot.pending_operation = None;
                    if answer.outcome.needs_presentation() {
                        snapshot.unseen.push(answer.clone());
                    }
                    snapshot.outcome = Some(answer);
                });
                *sequence += 1;
            }

            /// The same server's writer in a relaunched process, which reads this one's journal:
            /// the last answer and every answer nobody presented.
            pub(crate) fn relaunched(&self) -> Self {
                let snapshot = self.snapshot.lock().expect("snapshot").clone();
                let relaunched = Self::new(snapshot.inventory.clone());
                relaunched.publish(|fresh| {
                    fresh.worktree_writes = snapshot.worktree_writes;
                    fresh.outcome = snapshot.outcome;
                    fresh.unseen = snapshot.unseen;
                    fresh.forgotten = snapshot.forgotten;
                    fresh.forgotten_presented = snapshot.forgotten_presented;
                });
                *relaunched.sequence.lock().expect("sequence") =
                    *self.sequence.lock().expect("sequence");
                relaunched
            }

            /// The answers no surface has presented yet.
            pub(crate) fn unseen(&self) -> Vec<RetainedOutcome> {
                self.snapshot.lock().expect("snapshot").unseen.clone()
            }

            /// The journal's bound forgets `count` more unseen answers.
            pub(crate) fn forget(&self, count: u64) {
                self.publish(|snapshot| snapshot.forgotten += count);
            }

            /// How many forgotten answers a surface has not said went unseen.
            pub(crate) fn forgotten_unsaid(&self) -> u64 {
                let snapshot = self.snapshot.lock().expect("snapshot");
                snapshot.forgotten - snapshot.forgotten_presented
            }

            pub(crate) fn submitted(&self) -> Vec<Mutation> {
                self.submitted.lock().expect("submitted").clone()
            }

            /// Records a durable operation an earlier run submitted, which recovery answers after
            /// a relaunch without this process ever asking for it.
            pub(crate) fn submit_for_recovery(&self, mutation: Mutation) {
                self.submitted
                    .lock()
                    .expect("submitted")
                    .push(mutation.clone());
                self.publish(|snapshot| {
                    snapshot.pending = true;
                    snapshot.pending_operation = Some(mutation);
                });
            }
        }

        impl SharedWorkspaceEdits for ScriptedEdits {
            fn snapshot(&self) -> SharedWorkspaceSnapshot {
                self.snapshot.lock().expect("snapshot").clone()
            }

            fn availability(&self) -> (bool, bool) {
                let snapshot = self.snapshot.lock().expect("snapshot");
                (snapshot.connected, snapshot.pending)
            }

            fn submit(&self, mutation: Mutation) -> bool {
                let mut snapshot = self.snapshot.lock().expect("snapshot");
                if !snapshot.connected || snapshot.pending {
                    return false;
                }
                snapshot.pending = true;
                snapshot.pending_operation = Some(mutation.clone());
                snapshot.outcome = None;
                drop(snapshot);
                self.submitted.lock().expect("submitted").push(mutation);
                self.changes.publish();
                true
            }

            fn subscribe(&self) -> async_channel::Receiver<()> {
                self.changes.subscribe()
            }

            fn retry(&self) {
                self.retries
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }

            fn presented(&self, sequence: u64) {
                self.publish(|snapshot| {
                    snapshot.unseen.retain(|unseen| unseen.sequence != sequence);
                });
            }

            fn forgotten_presented(&self, total: u64) {
                self.publish(|snapshot| {
                    snapshot.forgotten_presented = snapshot
                        .forgotten_presented
                        .max(total.min(snapshot.forgotten));
                });
            }
        }
    }
}
