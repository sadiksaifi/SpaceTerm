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
    #[cfg(feature = "client")]
    storage: Option<(crate::storage::Directory, std::fs::File, Id, Id)>,
}
/// Clones share one attachment writer and invalidate old subscriptions on detach.
#[derive(Clone)]
pub struct Attachments(Arc<Mutex<State>>);
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
            #[cfg(feature = "client")]
            storage: None,
        })))
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
            storage: Some((directory, lock, server, client)),
        }))))
    }
    /// Preserve the existing Development adoption once. Later discoveries remain detached.
    pub fn reconcile(&self, inventory: &Inventory) -> Result<(), ServerError> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.choices.clone();
        if !next.initialized {
            for workspace in &inventory.workspaces {
                next.attach(workspace.id)?;
            }
            next.initialized = true;
        } else {
            next.attached
                .retain(|a| inventory.workspaces.iter().any(|w| w.id == a.workspace));
        }
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
    pub fn detach(&self, workspace: Id) -> Result<(), ServerError> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.choices.clone();
        if let Some(index) = next.attached.iter().position(|a| a.workspace == workspace) {
            let attachment = next.attached.remove(index);
            // A later release subsumes earlier generations of the same Workspace, since server
            // claims are fenced by the highest released generation for this exact client.
            next.pending.retain(|a| a.workspace != workspace);
            next.pending.push(attachment);
        }
        state.save(next)
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
        connection: &mut crate::pane::PaneConnection,
    ) -> Result<(), ServerError> {
        for attachment in self.pending() {
            match connection.exchange(&crate::pane::Request::ReleaseAttachment { attachment })? {
                crate::pane::Reply::Unchanged => self.released(attachment)?,
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
    #[cfg(feature = "client")]
    pub fn exchange(
        &self,
        connection: &mut crate::pane::PaneConnection,
        request: &crate::pane::Request,
    ) -> Result<crate::pane::Reply, ServerError> {
        use crate::pane::{AttachmentClaim, Request};
        if !self.active() {
            return Err(ServerError::SessionUnavailable);
        }
        let claim = match request {
            Request::RepairDirectory {
                target,
                replacement,
                size,
                style,
            } => Some((
                AttachmentClaim::Repair {
                    target: target.clone(),
                    replacement: replacement.clone(),
                },
                *size,
                style.clone(),
            )),
            Request::SelectPane {
                explicit,
                size,
                style,
                ..
            } => Some((
                AttachmentClaim::Select {
                    explicit: *explicit,
                },
                *size,
                style.clone(),
            )),
            Request::StartRestored {
                server,
                size,
                style,
                ..
            } => Some((
                AttachmentClaim::Start { server: *server },
                *size,
                style.clone(),
            )),
            Request::TakeOver {
                target,
                size,
                style,
            } => Some((
                AttachmentClaim::TakeOver { target: *target },
                *size,
                style.clone(),
            )),
            _ => None,
        };
        if let Some((claim, size, style)) = claim {
            self.attachments.release_pending(connection)?;
            // The saved generation precedes every claim, including one whose reply is lost.
            connection.exchange(&Request::ClaimPane {
                attachment: self.attachment,
                pane: self.pane,
                claim,
                size,
                style,
            })
        } else {
            connection.exchange(request)
        }
    }
}
impl Choices {
    fn attach(&mut self, workspace: Id) -> Result<(), ServerError> {
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
#[cfg(feature = "client")]
impl Drop for State {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        if let Some((_, lock, _, _)) = &self.storage {
            // SAFETY: this guard owns the live descriptor.
            unsafe {
                libc::flock(lock.as_raw_fd(), libc::LOCK_UN);
            }
        }
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
                    worktree_membership_fixed: false,
                    worktree_repository: None,
                    id: [2; 32],
                    root_pane: [3; 32],
                    panes: vec![Pane {
                        id: [3; 32],
                        source: None,
                        axis: Axis::Right,
                        starting_directory: "/tmp".into(),
                        confirmed_directory: None,
                        creator: None,
                    }],
                }],
            }],
        }
    }
    #[test]
    fn offline_detach_invalidates_old_presentations_and_retains_exact_release_after_reattach() {
        let choices = Attachments::memory();
        let inventory = inventory();
        choices.reconcile(&inventory).unwrap();
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
