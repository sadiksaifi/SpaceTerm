//! Durable client attachment choices and exact deferred releases, separate from preferences.
use crate::ServerError;
use crate::hierarchy::{Id, Inventory, MAX_WORKSPACES};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// One attachment lifetime. Reattachment always receives a higher generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attachment {
    pub workspace: Id,
    pub generation: u64,
}
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Choices {
    initialized: bool,
    next_generation: u64,
    attached: Vec<Attachment>,
    pending: Vec<Attachment>,
}
#[cfg(feature = "client")]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    server: Id,
    client: Id,
    choices: Choices,
}
struct State {
    choices: Choices,
    catalog: BTreeMap<Id, Id>,
    blocked: bool,
    /// Called after a detach leaves a release to send.
    watchers: Vec<std::sync::Weak<dyn Fn() + Send + Sync>>,
    #[cfg(feature = "client")]
    storage: Option<(crate::storage::Directory, crate::storage::Lock, Id, Id)>,
}
/// Clones share one attachment writer and invalidate old subscriptions on detach.
#[derive(Clone)]
pub struct Attachments(Arc<Mutex<State>>);
/// An attachment writer that another handle to the same client directory may share while it
/// stays open.
#[cfg(feature = "client")]
pub(crate) struct WeakAttachments(std::sync::Weak<Mutex<State>>);
#[cfg(feature = "client")]
impl WeakAttachments {
    pub(crate) fn alive(&self) -> bool {
        self.0.strong_count() > 0
    }
    /// The open writer, when it writes for exactly this server and client.
    pub(crate) fn upgrade(&self, server: Id, client: Id) -> Option<Attachments> {
        let state = self.0.upgrade()?;
        let binds = state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .storage
            .as_ref()
            .is_some_and(|(_, _, s, c)| *s == server && *c == client);
        binds.then_some(Attachments(state))
    }
}
/// One presentation's authority to resume control. Reattachment issues a new authority.
#[derive(Clone)]
pub struct AttachmentPermit {
    attachments: Attachments,
    attachment: Attachment,
    #[cfg(feature = "client")]
    pane: Id,
}
impl Attachments {
    pub fn memory() -> Self {
        Self(Arc::new(Mutex::new(State {
            choices: Choices::default(),
            catalog: BTreeMap::new(),
            blocked: false,
            watchers: Vec::new(),
            #[cfg(feature = "client")]
            storage: None,
        })))
    }
    /// A writer as a relaunched client opens it: its saved choices attach `workspaces`, and it
    /// has reconciled no inventory yet, so it permits no Pane.
    #[cfg(feature = "test-support")]
    pub fn relaunched(workspaces: &[Id]) -> Result<Self, ServerError> {
        let attachments = Self::memory();
        {
            let mut state = attachments.0.lock().unwrap_or_else(|e| e.into_inner());
            for workspace in workspaces {
                state.choices.attach(*workspace)?;
            }
            state.choices.initialized = true;
        }
        Ok(attachments)
    }
    #[cfg(feature = "client")]
    pub(crate) fn downgrade(&self) -> WeakAttachments {
        WeakAttachments(Arc::downgrade(&self.0))
    }
    #[cfg(feature = "client")]
    pub(crate) fn open(
        directory: crate::storage::Directory,
        server: Id,
        client: Id,
    ) -> Result<Self, ServerError> {
        let lock = directory.lock("attachments.lock", true)?;
        let record = directory
            .read_json::<Record>("attachments.json")?
            .unwrap_or(Record {
                version: 1,
                server,
                client,
                choices: Choices::default(),
            });
        if record.version != 1 || record.server != server || record.client != client {
            return Err(ServerError::AuthenticationRefused);
        }
        let choices = record.choices;
        let unique: std::collections::BTreeSet<_> =
            choices.attached.iter().map(|a| a.workspace).collect();
        let generations: std::collections::BTreeSet<_> = choices
            .attached
            .iter()
            .chain(&choices.pending)
            .map(|a| a.generation)
            .collect();
        if unique.len() != choices.attached.len()
            || unique.len() > MAX_WORKSPACES
            || choices.pending.len() > MAX_WORKSPACES
            || (!choices.initialized && choices != Choices::default())
            || generations.len() != choices.attached.len() + choices.pending.len()
            || generations
                .iter()
                .any(|g| *g == 0 || *g > choices.next_generation)
        {
            return Err(ServerError::UnsafeStorage);
        }
        Ok(Self(Arc::new(Mutex::new(State {
            choices,
            catalog: BTreeMap::new(),
            blocked: false,
            watchers: Vec::new(),
            storage: Some((directory, lock, server, client)),
        }))))
    }
    /// Prepare a new client's exact initial attachment, or its deliberately empty view.
    /// Retained choices always win. Returns whether this call made the client's initial choice.
    pub fn initialize(
        &self,
        inventory: &Inventory,
        workspace: Option<Id>,
    ) -> Result<bool, ServerError> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.choices.initialized {
            drop(state);
            self.reconcile(inventory)?;
            return Ok(false);
        }
        let mut next = state.choices.clone();
        if let Some(workspace) = workspace {
            if !inventory.workspaces.iter().any(|w| w.id == workspace) {
                return Err(ServerError::SessionUnavailable);
            }
            next.attach(workspace)?;
        }
        next.initialized = true;
        state.save(next)?;
        drop(state);
        self.reconcile(inventory)?;
        Ok(true)
    }
    /// Forget attachments to Workspaces the server no longer has. Never attaches anything.
    pub fn reconcile(&self, inventory: &Inventory) -> Result<(), ServerError> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.choices.clone();
        next.attached
            .retain(|a| inventory.workspaces.iter().any(|w| w.id == a.workspace));
        state.save(next)?;
        state.catalog = inventory
            .workspaces
            .iter()
            .flat_map(|w| {
                w.tabs
                    .iter()
                    .flat_map(move |t| t.panes.iter().map(move |p| (p.id, w.id)))
            })
            .collect();
        Ok(())
    }
    pub fn changeable(&self) -> bool {
        !self.0.lock().unwrap_or_else(|e| e.into_inner()).blocked
    }
    pub fn is_attached(&self, workspace: Id) -> bool {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        !state.blocked
            && state
                .choices
                .attached
                .iter()
                .any(|a| a.workspace == workspace)
    }
    pub fn attach(&self, workspace: Id) -> Result<bool, ServerError> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !state.catalog.values().any(|id| *id == workspace) {
            return Ok(false);
        }
        let mut next = state.choices.clone();
        next.attach(workspace)?;
        state.save(next)?;
        Ok(true)
    }
    /// Publish removal and its exact deferred release together, without contacting the server.
    /// Watchers learn that a release waits.
    pub fn detach(&self, workspace: Id) -> Result<(), ServerError> {
        let watchers = {
            let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            let mut next = state.choices.clone();
            if let Some(index) = next.attached.iter().position(|a| a.workspace == workspace) {
                let attachment = next.attached.remove(index);
                // A later release subsumes earlier generations of the same Workspace, since server
                // claims are fenced by the highest released generation for this exact client.
                next.pending.retain(|a| a.workspace != workspace);
                next.pending.push(attachment);
            }
            state.save(next)?;
            state.watchers.retain(|watcher| watcher.strong_count() > 0);
            state.watchers.clone()
        };
        for watcher in watchers.iter().filter_map(std::sync::Weak::upgrade) {
            watcher();
        }
        Ok(())
    }
    /// Calls `watcher` after each detach while it lives, outside this writer's lock.
    pub fn watch(&self, watcher: std::sync::Weak<dyn Fn() + Send + Sync>) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .watchers
            .push(watcher);
    }
    pub fn permit(&self, pane: Id) -> Option<AttachmentPermit> {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let workspace = *state.catalog.get(&pane)?;
        let attachment = *state
            .choices
            .attached
            .iter()
            .find(|a| a.workspace == workspace)?;
        (!state.blocked).then(|| AttachmentPermit {
            attachments: self.clone(),
            attachment,
            #[cfg(feature = "client")]
            pane,
        })
    }
    pub fn pending(&self) -> Vec<Attachment> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .choices
            .pending
            .clone()
    }
    /// A lost reply retains the original release. A later attachment cannot be retired with it.
    pub fn released(&self, attachment: Attachment) -> Result<(), ServerError> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.choices.clone();
        next.pending.retain(|old| *old != attachment);
        state.save(next)
    }
    #[cfg(feature = "client")]
    pub fn release_pending(
        &self,
        link: &crate::connection::link::ServerLink,
    ) -> Result<(), ServerError> {
        for attachment in self.pending() {
            match link
                .request(crate::protocol::Operation::ReleaseAttachment { attachment })?
                .wait()?
            {
                crate::protocol::ReplyBody::Done => self.released(attachment)?,
                _ => return Err(ServerError::Protocol),
            }
        }
        Ok(())
    }
}
impl AttachmentPermit {
    pub fn active(&self) -> bool {
        let state = self.attachments.0.lock().unwrap_or_else(|e| e.into_inner());
        !state.blocked && state.choices.attached.contains(&self.attachment)
    }
    pub fn attachment(&self) -> Attachment {
        self.attachment
    }
    /// Calls `watcher` after each detach while it lives, so a holder learns when this permit
    /// may have stopped being active.
    pub fn watch(&self, watcher: std::sync::Weak<dyn Fn() + Send + Sync>) {
        self.attachments.watch(watcher);
    }
    /// The claim this permit makes for a Pane request that selects, starts, repairs or takes over
    /// its Pane. Releases of earlier attachments go first, and the saved generation precedes every
    /// claim, including one whose reply is lost.
    #[cfg(feature = "client")]
    pub fn claim(
        &self,
        link: &crate::connection::link::ServerLink,
        request: &crate::pane::Request,
    ) -> Result<Option<PaneClaim>, ServerError> {
        use crate::pane::{AttachmentClaim, Request};
        if !self.active() {
            return Err(ServerError::SessionUnavailable);
        }
        let (claim, size, style) = match request {
            Request::RepairDirectory {
                target,
                replacement,
                size,
                style,
            } => (
                AttachmentClaim::Repair {
                    target: target.clone(),
                    replacement: replacement.clone(),
                },
                *size,
                style.clone(),
            ),
            Request::SelectPane {
                explicit,
                size,
                style,
                ..
            } => (
                AttachmentClaim::Select {
                    explicit: *explicit,
                },
                *size,
                style.clone(),
            ),
            Request::StartRestored {
                server,
                size,
                style,
                ..
            } => (
                AttachmentClaim::Start { server: *server },
                *size,
                style.clone(),
            ),
            Request::TakeOver {
                target,
                size,
                style,
            } => (
                AttachmentClaim::TakeOver { target: *target },
                *size,
                style.clone(),
            ),
            _ => return Ok(None),
        };
        self.attachments.release_pending(link)?;
        Ok(Some(PaneClaim {
            attachment: self.attachment,
            pane: self.pane,
            claim,
            size,
            style,
        }))
    }
}
/// What [`crate::connection::link::ChannelHandle::claim`] sends for one Pane.
#[cfg(feature = "client")]
pub struct PaneClaim {
    pub attachment: Attachment,
    pub pane: Id,
    pub claim: crate::pane::AttachmentClaim,
    pub size: crate::pane::PaneSize,
    pub style: Option<crate::pane::PresentationStyle>,
}

impl Choices {
    /// An explicit attachment is itself the client's choice, so it completes initialization.
    fn attach(&mut self, workspace: Id) -> Result<(), ServerError> {
        self.initialized = true;
        if !self.attached.iter().any(|a| a.workspace == workspace) {
            self.next_generation = self
                .next_generation
                .checked_add(1)
                .ok_or(ServerError::Capacity)?;
            self.attached.push(Attachment {
                workspace,
                generation: self.next_generation,
            });
        }
        Ok(())
    }
}
impl State {
    fn save(&mut self, next: Choices) -> Result<(), ServerError> {
        if self.blocked {
            return Err(ServerError::StorageUnavailable);
        }
        if next.attached.len() > MAX_WORKSPACES || next.pending.len() > MAX_WORKSPACES {
            return Err(ServerError::Capacity);
        }
        #[cfg(feature = "client")]
        if let Some((directory, _, server, client)) = &self.storage
            && self.choices != next
            && let Err(error) = directory.write_json(
                c"attachments.json",
                &Record {
                    version: 1,
                    server: *server,
                    client: *client,
                    choices: next.clone(),
                },
            )
        {
            self.blocked = true;
            return Err(error);
        }
        self.choices = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hierarchy::{Axis, Pane, Tab, Workspace};
    pub(super) fn inventory() -> Inventory {
        Inventory {
            repositories: None,
            revision: 1,
            close: None,
            workspaces: vec![Workspace {
                id: [1; 32],
                name: "work".into(),
                name_version: 0,
                pin_version: 0,
                pinned_directory: None,
                root_tab: [2; 32],
                tabs: vec![Tab {
                    worktree: None,
                    worktree_membership: crate::hierarchy::WorktreeMembership::Following,
                    worktree_repository: None,
                    id: [2; 32],
                    root_pane: [3; 32],
                    panes: vec![Pane {
                        id: [3; 32],
                        source: None,
                        axis: Axis::Right,
                        starting_directory: "/tmp".into(),
                        current_directory: None,
                        repository_directory: None,
                        presentation: None,
                        confirmed_directory: None,
                        creator: None,
                    }],
                }],
            }],
        }
    }
    #[test]
    fn reconciliation_attaches_nothing_until_one_explicit_initial_choice() {
        let choices = Attachments::memory();
        let inventory = inventory();
        choices.reconcile(&inventory).unwrap();
        assert!(!choices.is_attached([1; 32]));
        assert!(choices.permit([3; 32]).is_none());
        assert!(choices.initialize(&inventory, Some([1; 32])).unwrap());
        assert!(choices.is_attached([1; 32]));
        choices.detach([1; 32]).unwrap();
        assert!(
            !choices.initialize(&inventory, Some([1; 32])).unwrap(),
            "a client chooses its initial view once"
        );
        assert!(!choices.is_attached([1; 32]));
    }

    #[test]
    fn initializing_an_empty_view_never_adopts_available_work_on_reconciliation() {
        let choices = Attachments::memory();
        let inventory = inventory();
        choices.initialize(&inventory, None).unwrap();
        choices.reconcile(&inventory).unwrap();
        assert!(!choices.is_attached([1; 32]));
        assert!(choices.permit([3; 32]).is_none());
        assert!(choices.attach([1; 32]).unwrap());
        assert!(choices.permit([3; 32]).is_some());
    }

    #[test]
    fn initialization_attaches_only_the_selected_available_workspace() {
        let choices = Attachments::memory();
        let mut inventory = inventory();
        let mut other = inventory.workspaces[0].clone();
        other.id = [4; 32];
        other.root_tab = [5; 32];
        other.tabs[0].id = [5; 32];
        other.tabs[0].root_pane = [6; 32];
        other.tabs[0].panes[0].id = [6; 32];
        inventory.workspaces.push(other);
        assert_eq!(
            choices.initialize(&inventory, Some([9; 32])),
            Err(ServerError::SessionUnavailable)
        );
        choices.initialize(&inventory, Some([4; 32])).unwrap();
        choices.reconcile(&inventory).unwrap();
        assert!(choices.is_attached([4; 32]));
        assert!(!choices.is_attached([1; 32]));
        assert!(choices.permit([3; 32]).is_none());
        assert!(choices.permit([6; 32]).is_some());
    }

    #[cfg(feature = "client")]
    #[test]
    fn empty_and_detached_initial_choices_survive_reopen_without_renewing_control() {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "st-initial-view-{}",
            u128::from_be_bytes(crate::protocol::random().unwrap()[..16].try_into().unwrap())
        ));
        let open = || {
            Attachments::open(
                crate::storage::Directory::open(&root, true).unwrap(),
                [8; 32],
                [9; 32],
            )
            .unwrap()
        };
        let inventory = inventory();
        {
            let choices = open();
            choices.initialize(&inventory, None).unwrap();
        }
        let original = {
            let choices = open();
            choices.initialize(&inventory, Some([1; 32])).unwrap();
            choices.reconcile(&inventory).unwrap();
            assert!(
                !choices.is_attached([1; 32]),
                "retained empty view wins over a startup default"
            );
            choices.attach([1; 32]).unwrap();
            let permit = choices.permit([3; 32]).unwrap();
            choices.detach([1; 32]).unwrap();
            assert!(!permit.active());
            permit.attachment()
        };
        {
            let choices = open();
            choices.initialize(&inventory, Some([1; 32])).unwrap();
            assert!(!choices.is_attached([1; 32]));
            assert!(choices.permit([3; 32]).is_none());
            assert_eq!(choices.pending(), vec![original]);
            choices.attach([1; 32]).unwrap();
            let later = choices.permit([3; 32]).unwrap().attachment();
            assert!(later.generation > original.generation);
            choices.released(original).unwrap();
            assert!(choices.is_attached([1; 32]));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn offline_detach_invalidates_old_presentations_and_retains_exact_release_after_reattach() {
        let choices = Attachments::memory();
        let inventory = inventory();
        choices.initialize(&inventory, Some([1; 32])).unwrap();
        let original = choices.permit([3; 32]).unwrap();
        choices.detach([1; 32]).unwrap();
        assert!(!original.active());
        choices.reconcile(&inventory).unwrap();
        assert!(
            !choices.is_attached([1; 32]),
            "publications cannot undo detach"
        );
        assert!(choices.attach([1; 32]).unwrap());
        let later = choices.permit([3; 32]).unwrap();
        assert!(later.attachment().generation > original.attachment().generation);
        assert!(!original.active());
        assert_eq!(choices.pending(), vec![original.attachment()]);
        choices.released(original.attachment()).unwrap();
        assert!(later.active());
        choices.detach([1; 32]).unwrap();
        choices
            .reconcile(&Inventory {
                workspaces: vec![],
                ..inventory
            })
            .unwrap();
        assert!(
            !choices.attach([1; 32]).unwrap(),
            "killed work is never recreated"
        );
    }
}
