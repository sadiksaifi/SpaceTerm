//! Shared server Workspaces inside the main Workspace Window.
//!
//! A SpaceTerm Server owns these Workspaces, their Tabs, their Panes, and the Terminal Sessions
//! the Panes present. The desktop keeps its own Workspaces exactly as they are: adopting shared
//! Workspaces adds rows to the existing sidebar, Tabs to the existing Tab strip, and Panes to the
//! existing split geometry, and it never changes the Active Workspace or ends desktop-owned work.
//!
//! Every shared change goes to the server as one [`Mutation`]. Nothing here creates, renames, or
//! pins a Workspace locally and tells the server afterwards, so two clients cannot disagree about
//! membership. The server's answer is authoritative, a refused edit keeps the draft a person
//! typed, and an offline client offers no shared edit at all rather than queueing one.
//!
//! Closing a shared Pane or Tab, and Kill Workspace, end Terminal Sessions for every client. Each
//! one asks the server to end exactly the Panes and Sessions a person reviewed, so a confirmation
//! that another client's change made stale ends nothing.
//!
//! Operations the server path does not support yet stay unavailable here rather than falling back
//! to the desktop: repairing a Pane's directory, Repository Status and Worktrees, reading shell
//! content with assistive technology, paste, attaching and detaching, and retaining a client's
//! own arrangement.

use crate::application_identity::ApplicationIdentity;

/// Whether this build can present shared server Workspaces. Only SpaceTerm Development with its
/// developer tools can, because the Workspaces belong to the local Development server.
pub(crate) fn offered() -> bool {
    cfg!(feature = "developer-tools") && ApplicationIdentity::current().is_development()
}

/// Asks the front Workspace Window to present the server's shared Workspaces beside its own.
#[cfg(not(feature = "developer-tools"))]
pub(crate) fn adopt(_: &mut gpui::App) {}

/// Whether a Workspace Window already presents this device's shared Workspaces.
#[cfg(not(feature = "developer-tools"))]
pub(crate) fn adopted(_: &gpui::App) -> bool {
    false
}

#[cfg(all(test, feature = "developer-tools"))]
pub(crate) use development::testing;
#[cfg(feature = "developer-tools")]
pub(crate) use development::{
    POLL_INTERVAL, SharedAcceptance, SharedCloseApproval, SharedCloseReview, SharedCloseState,
    SharedCloseUnavailable, SharedEditSource, SharedPaneSessions, SharedPanes, SharedRefusal,
    SharedRefusalReason, SharedRemoval, SharedSubmission, SharedTabs, SharedWorkspaceConflict,
    SharedWorkspaceFacts, SharedWorkspaceHost, SharedWorkspacePlan, SharedWorkspaces, adopt,
    adopted, shared_close_changed_title,
};

#[cfg(all(test, feature = "developer-tools"))]
#[path = "shared_workspaces/tests.rs"]
mod tests;

#[cfg(feature = "developer-tools")]
mod development {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::Arc;

    use gpui::{App, SharedString};
    use spaceterm_server::hierarchy::{
        Axis, CloseConfirmation as ServerCloseConfirmation, CloseTarget as ServerCloseTarget, Id,
        Inventory, Mutation, Outcome, RetainedOutcome, Workspace as ServerWorkspace,
    };

    use crate::close_confirmation::CloseTarget;
    use crate::domain::{PaneId, TabId, ValidatedLocalDirectory, WorkspaceId};
    use crate::local_server::LocalServer;
    use crate::terminal::server_session::DevelopmentServerSessionFactory;
    use crate::terminal::shared_workspace::{SharedWorkspaceClient, SharedWorkspaceSnapshot};
    use crate::terminal::{PreparedWorkspaceTerminalLaunch, WorkspaceTerminalSessionFactory};
    use crate::ui::pane_lifecycle::PaneConstruction;
    use crate::ui::server_pane::{
        ServerPaneConnection, ServerPaneView, UnavailableTerminalAccessibility,
    };

    /// How often the Workspace Window reads the server's inventory and its own edit state.
    /// Reading takes one uncontended lock and clones the inventory.
    pub(crate) const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

    /// The one Workspace Window presenting this device's shared Workspaces.
    ///
    /// One device holds one durable edit journal and one connection per Pane, so a second window
    /// cannot present the same server: it would stay offline behind the journal lock and fence
    /// every Pane the first window holds. A repeated request activates this window instead.
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

    /// Whether a Workspace Window already presents this device's shared Workspaces.
    pub(crate) fn adopted(cx: &App) -> bool {
        host(cx).is_some()
    }

    /// Presents the server's shared Workspaces in a Workspace Window, or activates the window
    /// already presenting them.
    pub(crate) fn adopt(cx: &mut App) {
        if !super::offered() {
            return;
        }
        if let Some(existing) = host(cx) {
            cx.defer(move |cx| {
                let _ = existing.update(cx, |_, window, _| window.activate_window());
            });
            cx.activate(true);
            return;
        }
        let Some(handle) = crate::ui::updates::front_workspace_window(cx) else {
            eprintln!("shared Workspaces need an open Workspace Window");
            return;
        };
        let _ = handle.update(cx, |manager, window, cx| {
            manager.adopt_shared_workspaces(window, cx);
        });
        cx.activate(true);
    }

    /// The server-side edits one desktop client submits, and the last answer it received.
    ///
    /// The implementation owns durable operation identity, reconciliation after a lost reply, and
    /// bounded acknowledgement. This interface carries none of that: it offers one edit at a time
    /// and reports what the server decided.
    pub(crate) trait SharedWorkspaceEdits {
        fn snapshot(&self) -> SharedWorkspaceSnapshot;

        /// Whether this client is connected, and whether an edit already waits for an answer.
        /// Rendering reads this for every control, so it copies no hierarchy.
        fn availability(&self) -> (bool, bool);

        /// Submits one edit, or reports that nothing was submitted because this client is offline
        /// or is already waiting for an answer. A refusal is never queued.
        fn submit(&self, mutation: Mutation) -> bool;
    }

    impl SharedWorkspaceEdits for SharedWorkspaceClient {
        fn snapshot(&self) -> SharedWorkspaceSnapshot {
            Self::snapshot(self)
        }

        fn availability(&self) -> (bool, bool) {
            Self::availability(self)
        }

        fn submit(&self, mutation: Mutation) -> bool {
            Self::submit(self, mutation)
        }
    }

    /// One desktop client's shared edit channel: the server connection, and the one edit this
    /// process has not consumed yet.
    ///
    /// The channel is shared by every surface that submits, so the Workspace Window can tell an
    /// answer to its own request from an answer recovered after a relaunch. A recovered answer
    /// still reports a conflict, because the draft it rejected is a person's work; it never moves
    /// the Active Workspace, Tab, or Pane, because nobody asked for that in this session.
    #[derive(Clone)]
    pub(crate) struct SharedEditSource {
        edits: Rc<dyn SharedWorkspaceEdits>,
        submitted: Rc<std::cell::RefCell<Option<Mutation>>>,
        awaiting_poll: Rc<std::cell::Cell<bool>>,
    }

    impl SharedEditSource {
        pub(crate) fn new(edits: Rc<dyn SharedWorkspaceEdits>) -> Self {
            let initial = edits.snapshot();
            let awaiting_poll = !initial.connected || initial.pending || initial.outcome.is_some();
            Self {
                edits,
                submitted: Rc::new(std::cell::RefCell::new(None)),
                awaiting_poll: Rc::new(std::cell::Cell::new(awaiting_poll)),
            }
        }

        fn editability(&self) -> SharedEditability {
            match self.edits.availability() {
                (false, _) => SharedEditability::Offline,
                (true, pending) if pending || self.awaiting_poll.get() => {
                    SharedEditability::Pending
                }
                (true, _) => SharedEditability::Available,
            }
        }

        /// Submits one edit and records that this process asked for it.
        fn submit(&self, mutation: Mutation) -> SharedSubmission {
            let editability = self.editability();
            if !editability.available() {
                return SharedSubmission::Refused(editability.into());
            }
            if !self.edits.submit(mutation.clone()) {
                // The worker refuses an edit it cannot submit, including when the rendered enabled
                // state is one reading stale. Nothing is retained for later.
                return SharedSubmission::Refused(self.editability().into());
            }
            self.submitted.replace(Some(mutation));
            self.awaiting_poll.set(true);
            SharedSubmission::Submitted
        }

        /// Whether this process asked for the edit an answer settles, which it reports once.
        fn claims(&self, mutation: &Mutation) -> bool {
            let mut submitted = self.submitted.borrow_mut();
            if submitted.as_ref() == Some(mutation) {
                *submitted = None;
                return true;
            }
            false
        }

        fn snapshot(&self) -> SharedWorkspaceSnapshot {
            let snapshot = self.edits.snapshot();
            // Every submitting surface shares this gate. A completed reply must reach the
            // foreground before another request can replace the worker's retained answer.
            if snapshot.connected && !snapshot.pending {
                self.awaiting_poll.set(false);
            }
            snapshot
        }
    }

    /// Whether a shared edit can be asked for now.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum SharedEditability {
        Available,
        /// The client has no connection to the server, so shared edits are unavailable rather
        /// than queued.
        Offline,
        /// One edit is already waiting for the server's answer.
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
                Self::Offline => {
                    "Shared Workspace changes need the server. SpaceTerm is not connected, and \
                     nothing is saved for later."
                }
                Self::Pending => "SpaceTerm is waiting for the server to answer the last change.",
            }
        }

        /// The short status a sidebar row shows.
        pub(crate) const fn status(self) -> Option<&'static str> {
            match self {
                Self::Available => None,
                Self::Offline => Some("Not connected"),
                Self::Pending => Some("Saving…"),
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
                Self::Value => "The value is longer than the server accepts, so nothing was sent.",
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
    }

    impl SharedRefusal {
        pub(crate) const fn title(self) -> &'static str {
            match self {
                Self::Missing => "No Longer on the Server",
                Self::Invalid => "Change Not Accepted",
                Self::Limit => "Server Is Full",
            }
        }

        pub(crate) const fn detail(self) -> &'static str {
            match self {
                Self::Missing => {
                    "Another client removed what this change was for, so the server did not \
                     apply it."
                }
                Self::Invalid => "The server did not accept this value, so nothing changed.",
                Self::Limit => {
                    "The server already holds as many Workspaces, Tabs, or Panes as it allows, \
                     so nothing was created."
                }
            }
        }
    }

    /// What one shared Workspace's row and chrome present. Every value here is server data: it
    /// names no directory this client may read, so it is displayed and never acted on.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct SharedWorkspaceFacts {
        pub(crate) server: Id,
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
                .map_or_else(PathBuf::new, |pane| {
                    pane.confirmed_directory
                        .clone()
                        .unwrap_or_else(|| pane.starting_directory.clone())
                });
            Self {
                server: workspace.id,
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

    /// One Pane's passive subscription to the server: how it presents the Pane, its first launch
    /// reservation, and the connection whose status the Pane reads.
    pub(crate) type SharedPaneSubscription = (
        WorkspaceTerminalSessionFactory,
        PreparedWorkspaceTerminalLaunch,
        Arc<dyn ServerPaneConnection>,
    );

    /// Subscribes one Pane to the Terminal Session a SpaceTerm Server owns.
    ///
    /// Opening a subscription presents a Pane; it starts no shell and acquires no unclaimed grant,
    /// because only an explicit selection of that Pane asks the server for one.
    pub(crate) trait SharedPaneOpener {
        fn open(&self, pane: Id) -> Option<SharedPaneSubscription>;
    }

    /// The local server's own Panes.
    struct LocalServerPanes {
        server: LocalServer,
        /// Completes the launch plan the server factory ignores. It is client launch scaffolding
        /// and never a directory the server's Terminal Session starts in.
        client_home: ValidatedLocalDirectory,
    }

    impl SharedPaneOpener for LocalServerPanes {
        fn open(&self, pane: Id) -> Option<SharedPaneSubscription> {
            let (factory, state) =
                DevelopmentServerSessionFactory::for_pane(self.server.clone(), pane);
            let session_factory = WorkspaceTerminalSessionFactory::new_server(
                Rc::new(factory),
                self.client_home.clone(),
            );
            let prepared = session_factory
                .prepare_child_launch()
                .inspect_err(|_| eprintln!("a server-backed launch reserves no channel"))
                .ok()?;
            Some((session_factory, prepared, state))
        }
    }

    /// Opens passive subscriptions to the server's Panes and supplies their Pane construction.
    ///
    /// A shared Pane's Terminal Session supports screen presentation, keys, and resize only, so
    /// its Pane publishes no accessible shell text and offers no local file, clipboard, or paste
    /// operation.
    #[derive(Clone)]
    pub(crate) struct SharedPaneSessions {
        opener: Rc<dyn SharedPaneOpener>,
        panes: PaneConstruction,
    }

    impl SharedPaneSessions {
        pub(crate) fn new(
            server: LocalServer,
            client_home: ValidatedLocalDirectory,
            panes: &PaneConstruction,
        ) -> Self {
            Self::with_opener(
                Rc::new(LocalServerPanes {
                    server,
                    client_home,
                }),
                panes,
            )
        }

        pub(crate) fn with_opener(
            opener: Rc<dyn SharedPaneOpener>,
            panes: &PaneConstruction,
        ) -> Self {
            Self {
                opener,
                // Reading shell content through a server-owned Terminal Session is not available
                // yet, so a shared Pane publishes its controls and status without its text.
                panes: panes.with_accessibility(Rc::new(UnavailableTerminalAccessibility)),
            }
        }

        pub(crate) const fn panes(&self) -> &PaneConstruction {
            &self.panes
        }

        /// Subscribes to one server Pane.
        pub(crate) fn open(&self, pane: Id) -> Option<SharedPaneSubscription> {
            self.opener.open(pane)
        }
    }

    /// One shared Tab's Panes: their server identities, each one's view of its connection, and
    /// the focus the screen replacing a Pane rests on.
    pub(crate) struct SharedPanes {
        edits: SharedEditSource,
        sessions: SharedPaneSessions,
        workspace: Id,
        tab: Id,
        panes: BTreeMap<PaneId, Id>,
        views: BTreeMap<PaneId, ServerPaneView>,
        screen_focus: BTreeMap<PaneId, gpui::FocusHandle>,
    }

    impl SharedPanes {
        pub(crate) fn new(
            edits: SharedEditSource,
            sessions: SharedPaneSessions,
            workspace: Id,
            tab: Id,
        ) -> Self {
            Self {
                edits,
                sessions,
                workspace,
                tab,
                panes: BTreeMap::new(),
                views: BTreeMap::new(),
                screen_focus: BTreeMap::new(),
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
            self.panes.insert(pane_id, server);
            self.views.insert(pane_id, ServerPaneView::new(state));
            self.screen_focus.insert(pane_id, screen_focus);
        }

        pub(crate) fn forget(&mut self, pane_id: PaneId) {
            self.panes.remove(&pane_id);
            self.views.remove(&pane_id);
            self.screen_focus.remove(&pane_id);
        }

        pub(crate) fn server_pane(&self, pane_id: PaneId) -> Option<Id> {
            self.panes.get(&pane_id).copied()
        }

        pub(crate) fn local_pane(&self, server: Id) -> Option<PaneId> {
            self.panes
                .iter()
                .find_map(|(&pane_id, &id)| (id == server).then_some(pane_id))
        }

        /// Where the keyboard rests while a screen replaces the Pane.
        pub(crate) fn screen_focus(&self, pane_id: PaneId) -> Option<&gpui::FocusHandle> {
            self.screen_focus.get(&pane_id)
        }

        pub(crate) fn holds(&self, server: Id) -> bool {
            self.panes.values().any(|&pane| pane == server)
        }

        pub(crate) fn view(&self, pane_id: PaneId) -> Option<&ServerPaneView> {
            self.views.get(&pane_id)
        }

        pub(crate) fn view_mut(&mut self, pane_id: PaneId) -> Option<&mut ServerPaneView> {
            self.views.get_mut(&pane_id)
        }

        pub(crate) fn pane_ids(&self) -> Vec<PaneId> {
            self.views.keys().copied().collect()
        }

        pub(crate) fn editability(&self) -> SharedEditability {
            self.edits.editability()
        }

        /// Whether the server is reachable, which an explicit Pane selection needs: offline
        /// navigation must never ask the server to start a retained shell.
        pub(crate) fn reachable(&self) -> bool {
            self.edits.editability() != SharedEditability::Offline
        }

        /// Asks the server to split one of this Tab's Panes. The new Pane appears here only when
        /// the server publishes it, so two clients never disagree about the Pane Layout's
        /// membership.
        pub(crate) fn split(&self, source: PaneId, axis: Axis) -> SharedSubmission {
            let Some(source) = self.server_pane(source) else {
                return SharedSubmission::Refused(SharedRefusalReason::Value);
            };
            self.edits.submit(Mutation::SplitPane {
                workspace: self.workspace,
                tab: self.tab,
                source,
                axis,
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
                edits,
                sessions,
                workspace,
                tabs: BTreeMap::new(),
            }
        }

        pub(crate) const fn workspace(&self) -> Id {
            self.workspace
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
            self.tabs.get(&tab_id).copied()
        }

        pub(crate) fn local_tab(&self, server: Id) -> Option<TabId> {
            self.tabs
                .iter()
                .find_map(|(&tab_id, &id)| (id == server).then_some(tab_id))
        }

        pub(crate) fn editability(&self) -> SharedEditability {
            self.edits.editability()
        }

        /// Asks the server for another Tab in this Workspace.
        pub(crate) fn create_tab(&self) -> SharedSubmission {
            self.edits.submit(Mutation::CreateTab {
                workspace: self.workspace,
            })
        }
    }

    /// The Workspace Window's shared state: the server's inventory as this client last read it,
    /// the edit it is waiting for, and the answer it must still present.
    pub(crate) struct SharedWorkspaces {
        edits: SharedEditSource,
        sessions: SharedPaneSessions,
        /// Each server Workspace and the local Workspace presenting it.
        workspaces: BTreeMap<WorkspaceId, SharedWorkspaceFacts>,
        /// The inventory revision this client has applied.
        revision: Option<u64>,
        connected: bool,
        pending: bool,
        /// The sequence of the last outcome this client presented, so one answer is read once.
        consumed: Option<u64>,
        /// A refused same-field edit waiting for the local Workspace that presents its target.
        conflict: Option<PendingConflict>,
        refusal: Option<SharedRefusal>,
        /// The inventory the server published last, including the live Session facts a close
        /// confirmation reads. Those facts can change without a new membership revision.
        latest: Option<Inventory>,
        /// The close confirmation a person is reviewing now.
        review: Option<SharedCloseReview>,
        /// The close this process submitted, and the local target a fresh review would reopen.
        submitted_close: Option<(ServerCloseTarget, CloseTarget)>,
        /// A close the server refused because what it would end changed after the confirmation.
        stale_close: Option<ServerCloseTarget>,
    }

    /// A refused same-field edit whose draft is kept until this client can name the Workspace it
    /// belongs to. An answer recovered after a relaunch arrives before any local Workspace exists,
    /// so the draft waits for the projection rather than being dropped.
    struct PendingConflict {
        workspace: Id,
        field: SharedWorkspaceField,
        reviewed_version: u64,
        draft: SharedWorkspaceDraft,
        /// Captured from the snapshot that carried the refusal, so it never names a value a later
        /// accepted edit replaced.
        authoritative: SharedString,
    }

    impl SharedWorkspaces {
        pub(crate) fn new(edits: SharedEditSource, sessions: SharedPaneSessions) -> Self {
            Self {
                edits,
                sessions,
                workspaces: BTreeMap::new(),
                revision: None,
                connected: false,
                pending: false,
                consumed: None,
                conflict: None,
                refusal: None,
                latest: None,
                review: None,
                submitted_close: None,
                stale_close: None,
            }
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
                ServerCloseTarget::Pane { .. } => SharedCloseScope::Pane,
                ServerCloseTarget::Tab { .. } => SharedCloseScope::Tab,
                ServerCloseTarget::Workspace { .. } => SharedCloseScope::Workspace,
            };
            Ok(SharedCloseApproval {
                target,
                scope,
                workspace: workspace.name.clone(),
                confirmation,
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

        /// A close the server refused as stale, once, with the local target this process asked
        /// to close when it is the one that asked. A recovered answer names no local target.
        pub(crate) fn take_stale_close(
            &mut self,
        ) -> Option<(ServerCloseTarget, Option<CloseTarget>)> {
            let target = self.stale_close.take()?;
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
            self.workspaces
                .iter()
                .find_map(|(&workspace, facts)| (facts.server == server).then_some(workspace))
        }

        pub(crate) fn register(&mut self, workspace: WorkspaceId, facts: SharedWorkspaceFacts) {
            self.workspaces.insert(workspace, facts);
        }

        pub(crate) fn forget(&mut self, workspace: WorkspaceId) {
            self.workspaces.remove(&workspace);
        }

        /// The refused same-field edit to present, once this client can name the Workspace it
        /// belongs to. Until then the draft waits here; it is never dropped.
        pub(crate) fn take_conflict(&mut self) -> Option<SharedWorkspaceConflict> {
            let workspace = self.local(self.conflict.as_ref()?.workspace)?;
            let pending = self.conflict.take()?;
            Some(SharedWorkspaceConflict {
                workspace,
                field: pending.field,
                reviewed_version: pending.reviewed_version,
                draft: pending.draft,
                authoritative: pending.authoritative,
            })
        }

        pub(crate) fn take_refusal(&mut self) -> Option<SharedRefusal> {
            self.refusal.take()
        }

        /// Reads the server's answer and its inventory together, and reports both.
        ///
        /// One answer is read once. An answer recovered after a relaunch still reports the draft
        /// it rejected, because that draft is a person's work; only an answer this process asked
        /// for reports an acceptance the window may act on, so a publication from another client
        /// never moves the Active Workspace, Tab, or Pane.
        pub(crate) fn poll(&mut self) -> SharedPoll {
            let snapshot = self.edits.snapshot();
            let mut poll = SharedPoll::default();
            if self.connected != snapshot.connected || self.pending != snapshot.pending {
                self.connected = snapshot.connected;
                self.pending = snapshot.pending;
                poll.changed = true;
            }
            if let Some(outcome) = snapshot
                .outcome
                .as_ref()
                .filter(|outcome| self.consumed != Some(outcome.sequence))
            {
                self.consumed = Some(outcome.sequence);
                // The answer and the authoritative values come from one snapshot, so a conflict
                // never names a value an accepted edit has already replaced.
                let own = self.edits.claims(&outcome.mutation);
                self.settle(outcome, snapshot.inventory.as_ref(), own, &mut poll);
                poll.changed = true;
            }
            // Live Session facts change without a membership revision, so the latest reading is
            // kept whether or not membership changed.
            if snapshot.inventory.is_some() && self.latest != snapshot.inventory {
                self.latest.clone_from(&snapshot.inventory);
            }
            if let Some(inventory) = snapshot
                .inventory
                .filter(|inventory| self.revision != Some(inventory.revision))
            {
                // A waiting draft whose Workspace the server no longer has can never be asked
                // again, so it reports the removal instead of waiting for a mapping.
                if let Some(pending) = self.conflict.as_ref()
                    && !inventory
                        .workspaces
                        .iter()
                        .any(|workspace| workspace.id == pending.workspace)
                {
                    self.conflict = None;
                    self.refusal = Some(SharedRefusal::Missing);
                }
                self.revision = Some(inventory.revision);
                poll.changed = true;
                poll.inventory = Some(inventory);
            }
            poll
        }

        /// Presents one answer the server gave, against the inventory of the same snapshot.
        fn settle(
            &mut self,
            outcome: &RetainedOutcome,
            inventory: Option<&Inventory>,
            own: bool,
            poll: &mut SharedPoll,
        ) {
            match outcome.outcome {
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
                    self.conflict = inventory
                        .and_then(|inventory| Self::conflicted(&outcome.mutation, inventory));
                }
                Outcome::Missing => self.refusal = Some(SharedRefusal::Missing),
                Outcome::Invalid => self.refusal = Some(SharedRefusal::Invalid),
                Outcome::Limit => self.refusal = Some(SharedRefusal::Limit),
                // What the close would end changed after a person approved it. Ending the new
                // Sessions needs a fresh review, so nothing asks again on its own.
                Outcome::StaleConfirmation => {
                    if let Mutation::Close { target, .. } = &outcome.mutation {
                        self.stale_close = Some(*target);
                    }
                }
            }
        }

        /// The draft one refused same-field edit kept, beside the value the server holds in the
        /// snapshot that carried the refusal.
        fn conflicted(mutation: &Mutation, inventory: &Inventory) -> Option<PendingConflict> {
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
                Mutation::Close { .. } => None,
            }
        }

        /// Applies one server inventory to this client's own map, reporting what changed. The
        /// caller owns the local collections, so it performs the creations this plan names.
        pub(crate) fn plan(&self, inventory: &Inventory) -> Vec<SharedWorkspacePlan> {
            let mut plan = Vec::new();
            for workspace in &inventory.workspaces {
                let facts = SharedWorkspaceFacts::from_server(workspace);
                match self.local(workspace.id) {
                    Some(local) => plan.push(SharedWorkspacePlan::Update { local, facts }),
                    None => plan.push(SharedWorkspacePlan::Create { facts }),
                }
            }
            for (&local, facts) in &self.workspaces {
                if !inventory
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.id == facts.server)
                {
                    plan.push(SharedWorkspacePlan::Removed { local });
                }
            }
            plan
        }

        /// Asks the server for another shared Workspace.
        pub(crate) fn create_workspace(&mut self, name: String) -> SharedSubmission {
            self.ask(Mutation::CreateWorkspace { name })
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
                self.conflict = None;
                self.refusal = None;
                self.stale_close = None;
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
            | ServerCloseTarget::Workspace { workspace } => workspace,
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
        scope: SharedCloseScope,
        /// The Workspace's name as the server published it.
        workspace: String,
    }

    /// Which close a person asked for.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum SharedCloseScope {
        Pane,
        Tab,
        Workspace,
    }

    impl SharedCloseApproval {
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
                SharedCloseScope::Workspace => {
                    format!("Kill Workspace \u{201C}{}\u{201D}?", self.workspace)
                }
            }
        }

        pub(crate) const fn confirm_label(&self) -> &'static str {
            match self.scope {
                SharedCloseScope::Pane => "Close Pane",
                SharedCloseScope::Tab => "Close Tab",
                SharedCloseScope::Workspace => "Kill Workspace",
            }
        }

        /// What closes, what ends, and for whom. Every client loses these Panes and Sessions,
        /// including a client that controls one of the Panes.
        pub(crate) fn message(&self) -> String {
            let panes = self.confirmation.panes.len();
            let removal = match self.scope {
                SharedCloseScope::Workspace => {
                    "Its Tabs and Panes are permanently removed for every client.".to_owned()
                }
                _ if self.confirmation.removes_workspace => format!(
                    "This is the only {} in \u{201C}{}\u{201D}, so the Workspace is permanently \
                     removed for every client.",
                    if self.scope == SharedCloseScope::Tab {
                        "Tab"
                    } else {
                        "Pane"
                    },
                    self.workspace
                ),
                SharedCloseScope::Pane if self.confirmation.removes_tab => {
                    "This is the only Pane in its Tab, so the Tab closes for every client."
                        .to_owned()
                }
                SharedCloseScope::Pane => "The Pane closes for every client.".to_owned(),
                SharedCloseScope::Tab => match panes {
                    1 => "The Tab and its Pane close for every client.".to_owned(),
                    count => format!("The Tab and its {count} Panes close for every client."),
                },
            };
            let sessions = match self.sessions() {
                0 => "No shell is running there, so no Terminal Session ends.".to_owned(),
                1 => "This ends 1 Terminal Session for every client, including on a client that \
                      controls its Pane, and stops its running commands."
                    .to_owned(),
                count => format!(
                    "This ends {count} Terminal Sessions for every client, including in Panes \
                     another client controls, and stops their running commands."
                ),
            };
            format!("{removal} {sessions}")
        }

        /// What a removed Workspace leaves behind, when the close removes one.
        pub(crate) fn detail(&self) -> Option<&'static str> {
            self.confirmation.removes_workspace.then_some(
                "Other Workspaces keep running. Repository files and Git Worktree directories \
                 stay on disk. Neither a server restart nor another client can bring this \
                 Workspace back.",
            )
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

    /// What one inventory reading changed.
    #[derive(Default)]
    pub(crate) struct SharedPoll {
        /// The presentation changed, so the window repaints.
        pub(crate) changed: bool,
        /// The server published membership this client has not applied yet.
        pub(crate) inventory: Option<Inventory>,
        /// The server accepted an edit this process asked for, so the window opens what it
        /// created. An accepted edit from another client is absent here.
        pub(crate) accepted: Option<SharedAcceptance>,
    }

    /// One accepted edit this process asked for, and the server identity it created or changed.
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

        use spaceterm_server::hierarchy::{Inventory, Mutation, Outcome, RetainedOutcome};

        use super::SharedWorkspaceEdits;
        use crate::terminal::shared_workspace::SharedWorkspaceSnapshot;

        /// A scripted Pane whose status a test sets and whose explicit requests it counts.
        pub(crate) struct ScriptedPane {
            status: Mutex<crate::terminal::server_session::DevelopmentPaneStatus>,
            generation: std::sync::atomic::AtomicU64,
            selections: std::sync::atomic::AtomicUsize,
            take_overs: std::sync::atomic::AtomicUsize,
            restored_starts: std::sync::atomic::AtomicUsize,
            retries: std::sync::atomic::AtomicUsize,
        }

        impl ScriptedPane {
            pub(crate) fn new() -> std::sync::Arc<Self> {
                std::sync::Arc::new(Self {
                    status: Mutex::new(
                        crate::terminal::server_session::DevelopmentPaneStatus::Connecting,
                    ),
                    generation: std::sync::atomic::AtomicU64::new(0),
                    selections: std::sync::atomic::AtomicUsize::new(0),
                    take_overs: std::sync::atomic::AtomicUsize::new(0),
                    restored_starts: std::sync::atomic::AtomicUsize::new(0),
                    retries: std::sync::atomic::AtomicUsize::new(0),
                })
            }

            pub(crate) fn set(
                &self,
                status: crate::terminal::server_session::DevelopmentPaneStatus,
            ) {
                *self.status.lock().expect("status") = status;
            }

            /// A new connection reaches the Terminal Session and supplies its screen.
            pub(crate) fn reconnect(&self) {
                self.generation
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.set(crate::terminal::server_session::DevelopmentPaneStatus::Connected);
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

            fn connection_generation(&self) -> u64 {
                self.generation.load(std::sync::atomic::Ordering::SeqCst)
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
            }

            fn start_restored(&self) {
                self.restored_starts
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }

        /// Scripted Pane subscriptions over one local Terminal Session factory, so a test drives
        /// the real binding and reconciliation without a server.
        pub(crate) struct ScriptedPanes {
            session_factory: crate::terminal::WorkspaceTerminalSessionFactory,
            panes: Mutex<std::collections::BTreeMap<super::Id, std::sync::Arc<ScriptedPane>>>,
        }

        impl ScriptedPanes {
            pub(crate) fn new(
                session_factory: crate::terminal::WorkspaceTerminalSessionFactory,
            ) -> std::rc::Rc<Self> {
                std::rc::Rc::new(Self {
                    session_factory,
                    panes: Mutex::new(std::collections::BTreeMap::new()),
                })
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
            fn open(&self, pane: super::Id) -> Option<super::SharedPaneSubscription> {
                let connection = ScriptedPane::new();
                self.panes
                    .lock()
                    .expect("panes")
                    .insert(pane, std::sync::Arc::clone(&connection));
                let prepared = self.session_factory.prepare_child_launch().ok()?;
                Some((self.session_factory.clone(), prepared, connection))
            }
        }

        /// A scripted server whose inventory, connection, and answers the test sets.
        pub(crate) struct ScriptedEdits {
            snapshot: Mutex<SharedWorkspaceSnapshot>,
            submitted: Mutex<Vec<Mutation>>,
            /// The next sequence a settled answer carries.
            sequence: Mutex<u64>,
        }

        impl ScriptedEdits {
            pub(crate) fn new(inventory: Option<Inventory>) -> Self {
                Self {
                    snapshot: Mutex::new(SharedWorkspaceSnapshot {
                        inventory,
                        connected: true,
                        pending: false,
                        outcome: None,
                        error: None,
                    }),
                    submitted: Mutex::new(Vec::new()),
                    sequence: Mutex::new(1),
                }
            }

            pub(crate) fn set_inventory(&self, inventory: Inventory) {
                self.snapshot.lock().expect("snapshot").inventory = Some(inventory);
            }

            pub(crate) fn set_connected(&self, connected: bool) {
                self.snapshot.lock().expect("snapshot").connected = connected;
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
                let mut snapshot = self.snapshot.lock().expect("snapshot");
                snapshot.pending = false;
                snapshot.outcome = Some(RetainedOutcome {
                    sequence: *sequence,
                    mutation,
                    outcome,
                });
                *sequence += 1;
            }

            pub(crate) fn submitted(&self) -> Vec<Mutation> {
                self.submitted.lock().expect("submitted").clone()
            }

            /// Records a durable operation an earlier run submitted, which recovery answers after
            /// a relaunch without this process ever asking for it.
            pub(crate) fn submit_for_recovery(&self, mutation: Mutation) {
                self.submitted.lock().expect("submitted").push(mutation);
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
                snapshot.outcome = None;
                self.submitted.lock().expect("submitted").push(mutation);
                true
            }
        }
    }
}
