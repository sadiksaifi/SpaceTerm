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
    DirectServer, POLL_INTERVAL, SharedAcceptance, SharedAttachmentFailure, SharedCloseApproval,
    SharedCloseIntent, SharedCloseReview, SharedCloseState, SharedCloseUnavailable,
    SharedEditSource, SharedPaneSessions, SharedPanes, SharedRefusal, SharedRefusalReason,
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

mod development {
    use std::collections::{BTreeMap, VecDeque};
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::Arc;

    use gpui::{App, SharedString};
    use spaceterm_server::hierarchy::{
        Axis, CloseConfirmation as ServerCloseConfirmation, CloseTarget as ServerCloseTarget,
        ConfirmedControl, Id, Inventory, Mutation, Outcome, RetainedOutcome,
        Workspace as ServerWorkspace,
    };

    use spaceterm_server::workspace_order::OwnerId;

    use crate::close_confirmation::CloseTarget;
    use crate::domain::{PaneId, TabId, ValidatedLocalDirectory, WorkspaceId};
    use crate::server_connection::ServerConnection;
    use crate::terminal::WorkspaceTerminalSessionFactory;
    use crate::terminal::server_session::{DevelopmentServerSessionFactory, PaneControls};
    use crate::terminal::shared_workspace::{SharedWorkspaceClient, SharedWorkspaceSnapshot};
    use crate::ui::pane_lifecycle::PaneConstruction;
    use crate::ui::server_pane::{ServerPaneConnection, ServerPaneView};

    /// How often the Workspace Window reads the server's inventory and its own edit state.
    /// Reading takes one uncontended lock and clones the inventory.
    pub(crate) const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

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

        /// A data-free hint after each change the snapshot publishes, or `None` when only
        /// periodic reading observes changes. The snapshot stays the authority.
        fn subscribe(&self) -> Option<async_channel::Receiver<()>> {
            None
        }

        /// Whether the server accepts Worktree writes, without copying the hierarchy.
        fn worktree_writes(&self) -> bool {
            self.snapshot().worktree_writes
        }
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

        fn subscribe(&self) -> Option<async_channel::Receiver<()>> {
            Some(Self::subscribe(self))
        }
    }

    /// Connected interaction intent is bounded and ordered here. The transport journals only
    /// the operation currently dispatched; a disconnected client retains no undispatched work.
    #[derive(Clone)]
    pub(crate) struct SharedEditSource {
        edits: Rc<dyn SharedWorkspaceEdits>,
        pending: Rc<std::cell::RefCell<super::pending::PendingEdits>>,
        subscribers: Rc<std::cell::RefCell<Vec<async_channel::Sender<()>>>>,
    }

    pub(crate) struct SharedEditNotifications {
        local: async_channel::Receiver<()>,
        remote: Option<async_channel::Receiver<()>>,
    }

    impl SharedEditNotifications {
        pub(crate) async fn recv(&self) -> Result<(), async_channel::RecvError> {
            match &self.remote {
                Some(remote) => futures_lite::future::race(self.local.recv(), remote.recv()).await,
                None => self.local.recv().await,
            }
        }
    }

    impl SharedEditSource {
        pub(crate) fn new(edits: Rc<dyn SharedWorkspaceEdits>) -> Self {
            Self {
                edits,
                pending: Rc::new(std::cell::RefCell::new(
                    super::pending::PendingEdits::default(),
                )),
                subscribers: Rc::default(),
            }
        }

        fn editability(&self) -> SharedEditability {
            if !self.edits.availability().0 {
                SharedEditability::Offline
            } else if self.pending.borrow().full() {
                SharedEditability::Pending
            } else {
                SharedEditability::Available
            }
        }

        pub(crate) fn submit(&self, mutation: Mutation) -> SharedSubmission {
            match self.enqueue(mutation, None) {
                Ok(()) => SharedSubmission::Submitted,
                Err(reason) => SharedSubmission::Refused(reason),
            }
        }

        fn enqueue(
            &self,
            mutation: Mutation,
            reply: Option<async_channel::Sender<Result<RetainedOutcome, SharedRefusalReason>>>,
        ) -> Result<(), SharedRefusalReason> {
            self.snapshot();
            let editability = self.editability();
            if !editability.available() {
                return Err(editability.into());
            }
            if !mutation.is_bounded() {
                return Err(SharedRefusalReason::Value);
            }
            self.pending.borrow_mut().push(mutation, reply)?;
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
            self.enqueue(mutation, Some(reply))?;
            Ok(receiver)
        }

        fn dispatch(&self) {
            let (connected, busy) = self.edits.availability();
            if connected && !busy {
                let mut pending = self.pending.borrow_mut();
                if let Some(mutation) = pending.next_dispatch() {
                    if self.edits.submit(mutation.clone()) {
                        pending.dispatched(mutation);
                    } else if !self.edits.availability().0 {
                        pending.disconnect();
                    }
                }
            }
        }

        pub(crate) fn snapshot(&self) -> SharedWorkspaceSnapshot {
            let snapshot = self.edits.snapshot();
            self.pending.borrow_mut().observe(&snapshot);
            self.dispatch();
            snapshot
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
            cx.spawn(async move |cx| {
                loop {
                    let snapshot = source.snapshot();
                    if !snapshot.pending && !source.has_pending() {
                        break;
                    }
                    cx.background_executor().timer(POLL_INTERVAL).await;
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
                Self::Read | Self::UnsafeStorage => "Couldn't Restore Workspaces",
                Self::Write => "Couldn't Confirm Saved Changes",
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
                    "SpaceTerm couldn't read your saved Workspace arrangement. Quit and reopen SpaceTerm to try again."
                }
                Self::UnsafeStorage => {
                    "SpaceTerm couldn't open your saved Workspace arrangement safely. Your saved arrangement is unchanged."
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

        /// Panes of another computer's server reached over a paired route. They take no local
        /// file authority.
        pub(crate) fn direct(
            server: ServerConnection,
            controls: Arc<PaneControls>,
            direct: DirectServer,
            client_home: ValidatedLocalDirectory,
            panes: &PaneConstruction,
        ) -> Self {
            Self::with_opener(
                Rc::new(RemoteServerPanes {
                    server,
                    controls,
                    direct,
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
                panes: panes.clone(),
                edits: None,
            }
        }

        pub(crate) fn local_filesystem(
            &self,
        ) -> Option<&crate::platform::local_filesystem::LocalFilesystemAuthority> {
            self.opener.local_filesystem()
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
        views: BTreeMap<PaneId, ServerPaneView>,
        screen_focus: BTreeMap<PaneId, gpui::FocusHandle>,
        pending_starts: std::cell::RefCell<std::collections::BTreeSet<PaneId>>,
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
                views: BTreeMap::new(),
                screen_focus: BTreeMap::new(),
                pending_starts: Default::default(),
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
            self.views.insert(pane_id, ServerPaneView::new(state));
            self.screen_focus.insert(pane_id, screen_focus);
        }

        pub(crate) fn forget(&mut self, pane_id: PaneId) {
            if let Some(previous) = self.panes.remove(&pane_id) {
                self.edits.retire_alias(previous);
            }
            self.pending_starts.borrow_mut().remove(&pane_id);
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

        pub(crate) fn needs_binding(&self, pane_id: PaneId, server: Id) -> bool {
            self.panes.get(&pane_id).is_some_and(|id| *id != server)
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
                .edits
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
                        })
                    }
                    None => Mutation::CreateTab {
                        workspace: self.workspace(),
                        source: Some(source),
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
        pending: bool,
        pending_operation: Option<Mutation>,
        /// The sequence of the last outcome this client presented, so one answer is read once.
        consumed: Option<u64>,
        /// Unread refused edits, each retaining its own draft and authoritative snapshot.
        conflicts: VecDeque<PendingConflict>,
        refusal: Option<SharedRefusal>,
        worktree_failure: Option<(
            spaceterm_server::worktree::Operation,
            spaceterm_server::worktree::Error,
        )>,
        /// The inventory the server published last, including the live Session facts a close
        /// confirmation reads. Those facts can change without a new membership revision.
        latest: Option<Inventory>,
        /// The Git facts the server read with its latest inventory.
        repositories: super::SharedRepositories,
        /// The close confirmation a person is reviewing now.
        review: Option<SharedCloseReview>,
        /// The close this process submitted, and the local target a fresh review would reopen.
        submitted_close: Option<(ServerCloseTarget, CloseTarget)>,
        /// A close the server refused because what it would end changed after the confirmation.
        stale_close: Option<ServerCloseTarget>,
        attachments: spaceterm_server::attachments::Attachments,
        retained_views: Option<crate::ui::client_view_state::RetainedClientViews>,
        saved_views: Option<spaceterm_server::client_view::ClientView>,
        last_saved_view: Option<spaceterm_server::client_view::ClientView>,
        view_error_reported: bool,
        attachment_error: Option<spaceterm_server::ServerError>,
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

    impl Drop for SharedWorkspaces {
        fn drop(&mut self) {
            self.edits
                .pending
                .borrow_mut()
                .release_conflicts(self.conflicts.len());
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
                pending: false,
                pending_operation: None,
                consumed: None,
                conflicts: VecDeque::new(),
                refusal: None,
                worktree_failure: None,
                latest: None,
                repositories: super::SharedRepositories::default(),
                review: None,
                submitted_close: None,
                stale_close: None,
                attachments: spaceterm_server::attachments::Attachments::memory(),
                retained_views: None,
                saved_views: None,
                last_saved_view: None,
                view_error_reported: false,
                attachment_error: None,
            }
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
            let transport = self.edits.edits.snapshot();
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
                SharedEditability::Offline if self.connection_error.is_some() => {
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
            if !self.edits.edits.availability().0 {
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

        /// A close the server refused as stale, once, with the local target this process asked
        /// to close when it is the one that asked. A recovered answer names no local target.
        pub(crate) fn take_worktree_failure(
            &mut self,
        ) -> Option<(
            spaceterm_server::worktree::Operation,
            spaceterm_server::worktree::Error,
        )> {
            self.worktree_failure.take()
        }
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

        /// Leaves ownership and admission capacity here until the notice is admitted.
        pub(crate) fn next_conflict(&self) -> Option<SharedWorkspaceConflict> {
            let pending = &self.conflicts[self.presentable_conflict_index()?];
            Some(SharedWorkspaceConflict {
                workspace: self.local(pending.workspace)?,
                field: pending.field,
                reviewed_version: pending.reviewed_version,
                draft: pending.draft.clone(),
                authoritative: pending.authoritative.clone(),
            })
        }

        pub(crate) fn take_conflict(&mut self) -> Option<SharedWorkspaceConflict> {
            let index = self.presentable_conflict_index()?;
            let pending = self.conflicts.remove(index)?;
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

        pub(crate) fn take_refusal(&mut self) -> Option<SharedRefusal> {
            self.refusal.take()
        }

        pub(crate) const fn connection_error(&self) -> Option<spaceterm_server::ServerError> {
            self.connection_error
        }

        /// Wakes for each change the server connection publishes, when it can say so.
        pub(crate) fn subscribe(&self) -> Option<SharedEditNotifications> {
            let (sender, local) = async_channel::bounded(1);
            self.edits.subscribers.borrow_mut().push(sender);
            Some(SharedEditNotifications {
                local,
                remote: self.edits.edits.subscribe(),
            })
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
            if let Some(inventory) = snapshot
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
                self.consumed = Some(completion.outcome.sequence);
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
                self.refusal = Some(SharedRefusal::Disconnected);
                poll.changed = true;
            }
            self.pending_operation
                .clone_from(&snapshot.pending_operation);
            if self.connection_error != snapshot.error {
                self.connection_error = snapshot.error;
                poll.changed = true;
            }
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
                self.settle(outcome, snapshot.inventory.as_ref(), false, &mut poll);
                poll.changed = true;
            }
            // Live Session facts and Pane directories change without a membership revision, so
            // the latest reading is kept whether or not membership changed, and repaints the rows
            // that read it.
            if snapshot.inventory.is_some() && self.latest != snapshot.inventory {
                self.latest.clone_from(&snapshot.inventory);
                poll.changed = true;
            }
            if let Some(inventory) = &snapshot.inventory {
                // A batch can contain an older conflict for a Workspace removed by its latest
                // publication. Keep every other draft and release only the missing targets.
                let before = self.conflicts.len();
                self.conflicts.retain(|pending| {
                    inventory
                        .workspaces
                        .iter()
                        .any(|w| w.id == pending.workspace)
                });
                let removed = before - self.conflicts.len();
                if removed != 0 {
                    self.edits.pending.borrow_mut().release_conflicts(removed);
                    self.refusal = Some(SharedRefusal::Missing);
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

        /// Presents one answer the server gave, against the inventory of the same snapshot.
        fn settle(
            &mut self,
            outcome: &RetainedOutcome,
            inventory: Option<&Inventory>,
            own: bool,
            poll: &mut SharedPoll,
        ) {
            match outcome.outcome {
                Outcome::Pending => {}
                Outcome::WorktreeFailed(error) => {
                    if let Mutation::Worktree(operation) = &outcome.mutation {
                        self.worktree_failure = Some((operation.clone(), error));
                    }
                }
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
                    if let Some(conflict) = inventory
                        .and_then(|inventory| Self::conflicted(&outcome.mutation, inventory))
                    {
                        // Moving a completion into review keeps its place in the shared bound.
                        self.edits.pending.borrow_mut().retain_conflict();
                        self.conflicts.push_back(conflict);
                    }
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
            self.ask(Mutation::CreateWorkspace { name, directory })
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
                    format!("Kill Workspace \"{}\"?", self.workspace)
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
            let sessions = match self.sessions() {
                0 => String::new(),
                1 => "This ends 1 Terminal Session for every client.".to_owned(),
                count => format!("This ends {count} Terminal Sessions for every client."),
            };
            if self.scope == SharedCloseScope::Workspace {
                if sessions.is_empty() {
                    "This can't be undone.".to_owned()
                } else {
                    format!("{sessions} This can't be undone.")
                }
            } else {
                sessions
            }
        }

        /// What a removed Workspace leaves behind, when the close removes one.
        pub(crate) fn detail(&self) -> Option<&'static str> {
            if self.scope == SharedCloseScope::Worktree {
                return Some("The Worktree, its files, and its branch are kept.");
            }
            self.confirmation.removes_workspace.then_some(
                "Repository files and Worktree directories stay on disk. This Workspace cannot be restored.",
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
                SharedCloseScope::Worktree => "Tabs Already Closed",
                SharedCloseScope::Workspace => "Workspace Already Removed",
            }
        }
    }

    /// The title of the notice that says a close ended nothing because what it would end changed
    /// after the confirmation opened.
    pub(crate) const fn shared_close_changed_title(target: ServerCloseTarget) -> &'static str {
        match target {
            ServerCloseTarget::Pane { .. } => "Pane Changed",
            ServerCloseTarget::Tab { .. } => "Tab Changed",
            ServerCloseTarget::Worktree { .. } => "Tabs Changed",
            ServerCloseTarget::Workspace { .. } => "Workspace Changed",
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
            session_dormant: std::sync::atomic::AtomicBool,
            generation: std::sync::atomic::AtomicU64,
            selections: std::sync::atomic::AtomicUsize,
            created_starts: std::sync::atomic::AtomicUsize,
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
                    startup_pending: std::sync::atomic::AtomicBool::new(true),
                    controller: Mutex::default(),
                    session_dormant: std::sync::atomic::AtomicBool::new(false),
                    generation: std::sync::atomic::AtomicU64::new(0),
                    selections: std::sync::atomic::AtomicUsize::new(0),
                    created_starts: std::sync::atomic::AtomicUsize::new(0),
                    take_overs: std::sync::atomic::AtomicUsize::new(0),
                    restored_starts: std::sync::atomic::AtomicUsize::new(0),
                    retries: std::sync::atomic::AtomicUsize::new(0),
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
            }

            /// An authenticated admission can precede every screen of a hidden Pane.
            pub(crate) fn admitted(&self) {
                self.startup_pending
                    .store(false, std::sync::atomic::Ordering::SeqCst);
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

            /// The server says whether End All Sessions left the Pane without a shell.
            pub(crate) fn set_session_dormant(&self, dormant: bool) {
                self.session_dormant
                    .store(dormant, std::sync::atomic::Ordering::SeqCst);
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

            fn session_dormant(&self) -> bool {
                self.session_dormant
                    .load(std::sync::atomic::Ordering::SeqCst)
            }

            fn connection_generation(&self) -> u64 {
                self.generation.load(std::sync::atomic::Ordering::SeqCst)
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
                        error: None,
                    }),
                    submitted: Mutex::new(Vec::new()),
                    sequence: Mutex::new(1),
                }
            }

            pub(crate) fn set_inventory(&self, inventory: Inventory) {
                self.snapshot.lock().expect("snapshot").inventory = Some(inventory);
            }

            /// Whether the scripted server negotiated End All Sessions.
            pub(crate) fn set_end_all_sessions(&self, offered: bool) {
                self.snapshot.lock().expect("snapshot").end_all_sessions = offered;
            }

            pub(crate) fn set_connected(&self, connected: bool) {
                self.snapshot.lock().expect("snapshot").connected = connected;
            }

            pub(crate) fn set_worktree_writes(&self, worktree_writes: bool) {
                self.snapshot.lock().expect("snapshot").worktree_writes = worktree_writes;
            }

            pub(crate) fn set_error(&self, error: spaceterm_server::ServerError) {
                self.snapshot.lock().expect("snapshot").error = Some(error);
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
                snapshot.pending_operation = None;
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
                let mut snapshot = self.snapshot.lock().expect("snapshot");
                snapshot.pending = true;
                snapshot.pending_operation = Some(mutation.clone());
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
                snapshot.pending_operation = Some(mutation.clone());
                snapshot.outcome = None;
                self.submitted.lock().expect("submitted").push(mutation);
                true
            }
        }
    }
}
