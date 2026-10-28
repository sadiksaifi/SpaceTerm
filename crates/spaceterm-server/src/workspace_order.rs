//! This desktop client's continuous Workspace order, separate from per-server arrangements.
use crate::ServerError;
use crate::hierarchy::Id;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

// Bound client state independently of any one server's Workspace limit.
const MAX_ENTRIES: usize = 16_384;

/// A nonsecret fingerprint of one authenticated client presentation scope.
/// Possession grants no connection or filesystem authority.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OwnerId(Id);

impl OwnerId {
    /// Restores a presentation fingerprint, without granting its owner's authority.
    pub const fn from_bytes(bytes: Id) -> Self {
        Self(bytes)
    }
}

/// Stable presentation identity. These values grant no connection or filesystem authority.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceReference {
    pub owner: OwnerId,
    pub workspace: Id,
}

/// Retains slots for detached Workspaces and servers that have not reconnected yet.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceOrder {
    workspaces: Vec<WorkspaceReference>,
}

impl WorkspaceOrder {
    /// Restores the visible subset in retained order, appending newly encountered Workspaces.
    /// Apply the result to the presentation before recording its next order.
    pub fn restore(&self, presented: &[WorkspaceReference]) -> Vec<WorkspaceReference> {
        let mut remaining: BTreeSet<_> = presented.iter().copied().collect();
        let mut ordered: Vec<_> = self
            .workspaces
            .iter()
            .copied()
            .filter(|workspace| remaining.remove(workspace))
            .collect();
        ordered.extend(
            presented
                .iter()
                .copied()
                .filter(|workspace| remaining.remove(workspace)),
        );
        ordered
    }

    /// Records authoritative presentation order while leaving absent Workspaces in their slots.
    /// Detach and partial inventories must not erase another Workspace's retained position.
    pub fn record(&mut self, presented: &[WorkspaceReference]) -> Result<(), ServerError> {
        self.validate()?;
        validate(presented)?;
        let visible: BTreeSet<_> = presented.iter().copied().collect();
        let previous: BTreeSet<_> = self.workspaces.iter().copied().collect();
        let additional = visible.difference(&previous).count();
        if self.workspaces.len() + additional > MAX_ENTRIES {
            return Err(ServerError::Capacity);
        }
        let mut next = presented.iter().copied();
        for workspace in &mut self.workspaces {
            if visible.contains(workspace)
                && let Some(replacement) = next.next()
            {
                *workspace = replacement;
            }
        }
        self.workspaces.extend(next);
        Ok(())
    }

    /// Forgets deleted Workspaces only after a complete inventory for this presentation owner.
    /// Detached Workspaces and other owners' presentations retain their positions.
    pub fn reconcile_owner(&mut self, owner: OwnerId, inventory: &[Id]) {
        let surviving: BTreeSet<_> = inventory.iter().copied().collect();
        self.workspaces.retain(|workspace| {
            workspace.owner != owner || surviving.contains(&workspace.workspace)
        });
    }

    fn validate(&self) -> Result<(), ServerError> {
        validate(&self.workspaces)
    }
}

fn validate(workspaces: &[WorkspaceReference]) -> Result<(), ServerError> {
    if workspaces.len() > MAX_ENTRIES {
        return Err(ServerError::Capacity);
    }
    let mut unique = BTreeSet::new();
    for workspace in workspaces {
        if workspace.owner.0 == [0; 32]
            || workspace.workspace == [0; 32]
            || !unique.insert(*workspace)
        {
            return Err(ServerError::UnsafeStorage);
        }
    }
    Ok(())
}

#[cfg(feature = "client")]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    client: Id,
    order: WorkspaceOrder,
}

/// One local desktop writer, independent of per-server connections and ordinary Settings.
#[cfg(feature = "client")]
pub struct WorkspaceOrderStore {
    directory: crate::storage::Directory,
    _lock: crate::storage::Lock,
    record: Record,
    blocked: bool,
}

#[cfg(feature = "client")]
impl WorkspaceOrderStore {
    pub(crate) fn open(
        directory: crate::storage::Directory,
        client: Id,
    ) -> Result<Self, ServerError> {
        let lock = directory.lock("workspace-order.lock", true)?;
        let record = directory
            .read_json::<Record>("workspace-order.json")?
            .unwrap_or(Record {
                version: 1,
                client,
                order: WorkspaceOrder::default(),
            });
        if record.version != 1 || record.client != client {
            return Err(ServerError::AuthenticationRefused);
        }
        record.order.validate()?;
        Ok(Self {
            directory,
            _lock: lock,
            record,
            blocked: false,
        })
    }

    pub fn order(&self) -> &WorkspaceOrder {
        &self.record.order
    }

    pub fn save(&mut self, order: WorkspaceOrder) -> Result<(), ServerError> {
        if self.blocked {
            return Err(ServerError::StorageUnavailable);
        }
        order.validate()?;
        if self.record.order == order {
            return Ok(());
        }
        self.record.order = order;
        if let Err(error) = self
            .directory
            .write_json(c"workspace-order.json", &self.record)
        {
            self.blocked = true;
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(owner: u8, workspace: u8) -> WorkspaceReference {
        WorkspaceReference {
            owner: OwnerId::from_bytes([owner; 32]),
            workspace: [workspace; 32],
        }
    }

    #[test]
    fn partial_reconnects_preserve_interleaved_order_regardless_of_server_arrival() {
        let local = [workspace(1, 1), workspace(1, 2)];
        let remote = [workspace(2, 1), workspace(2, 2)];
        let expected = [remote[1], local[0], remote[0], local[1]];
        for initial in [local, remote] {
            let mut order = WorkspaceOrder::default();
            order.record(&expected).unwrap();
            let partial = order.restore(&initial);
            order.record(&partial).unwrap();
            assert_eq!(
                order.restore(&[local[0], local[1], remote[0], remote[1]]),
                expected
            );
        }
    }

    #[test]
    fn detach_and_reorder_preserve_absent_slots_and_append_new_workspaces() {
        let a = workspace(1, 1);
        let b = workspace(2, 1);
        let c = workspace(1, 2);
        let d = workspace(2, 2);
        let mut order = WorkspaceOrder::default();
        order.record(&[a, b, c]).unwrap();
        order.record(&[c, a, d]).unwrap();
        assert_eq!(order.restore(&[a, b, c, d]), [c, b, a, d]);
    }

    #[test]
    fn complete_inventory_prunes_only_its_owners_deleted_workspaces() {
        let a = workspace(1, 1);
        let b = workspace(2, 1);
        let c = workspace(1, 2);
        let mut order = WorkspaceOrder::default();
        order.record(&[a, b, c]).unwrap();
        order.reconcile_owner(a.owner, &[c.workspace]);
        assert_eq!(order.restore(&[a, b, c]), [b, c, a]);
    }

    #[test]
    fn owners_of_the_same_workspace_keep_independent_positions_across_roundtrip() {
        let a = workspace(1, 1);
        let b = workspace(2, 1);
        let c = workspace(3, 1);
        let mut order = WorkspaceOrder::default();
        order.record(&[b, c, a]).unwrap();
        order.record(&[c, a]).unwrap();
        let mut restored: WorkspaceOrder =
            serde_json::from_slice(&serde_json::to_vec(&order).unwrap()).unwrap();
        assert_eq!(restored.restore(&[a, b, c]), [b, c, a]);
        restored.reconcile_owner(c.owner, &[]);
        assert_eq!(restored.restore(&[a, b, c]), [b, a, c]);
    }

    #[test]
    fn invalid_recording_does_not_replace_retained_order() {
        let a = workspace(1, 1);
        let b = workspace(2, 1);
        let mut order = WorkspaceOrder::default();
        order.record(&[b, a]).unwrap();
        for invalid in [vec![a, a], vec![workspace(0, 1)], vec![workspace(1, 0)]] {
            assert_eq!(order.record(&invalid), Err(ServerError::UnsafeStorage));
            assert_eq!(order.restore(&[a, b]), [b, a]);
        }
    }

    #[cfg(feature = "client")]
    #[test]
    fn interleaved_order_survives_relaunch_and_refuses_another_writer_or_client() {
        use crate::storage::Directory;
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "st-workspace-order-{:x}",
            u128::from_be_bytes(crate::protocol::random().unwrap()[..16].try_into().unwrap())
        ));
        let open =
            |client| WorkspaceOrderStore::open(Directory::open(&root, true).unwrap(), client);
        let a = workspace(1, 1);
        let b = workspace(2, 1);
        let c = workspace(1, 2);
        let mut order = WorkspaceOrder::default();
        order.record(&[b, a, c]).unwrap();
        {
            let mut store = open([9; 32]).unwrap();
            store.save(order).unwrap();
            assert!(open([9; 32]).is_err());
        }
        assert_eq!(
            open([8; 32]).err(),
            Some(ServerError::AuthenticationRefused)
        );
        {
            let mut store = open([9; 32]).unwrap();
            let mut retained = store.order().clone();
            retained.record(&retained.restore(&[a, c])).unwrap();
            store.save(retained).unwrap();
        }
        {
            let store = open([9; 32]).unwrap();
            assert_eq!(store.order().restore(&[a, c, b]), [b, a, c]);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(feature = "client")]
    #[test]
    fn malformed_order_is_preserved_instead_of_replaced() {
        use crate::storage::Directory;
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "st-bad-workspace-order-{:x}",
            u128::from_be_bytes(crate::protocol::random().unwrap()[..16].try_into().unwrap())
        ));
        let mut order = WorkspaceOrder::default();
        order.record(&[workspace(1, 1)]).unwrap();
        WorkspaceOrderStore::open(Directory::open(&root, true).unwrap(), [9; 32])
            .unwrap()
            .save(order)
            .unwrap();
        let directory = Directory::open(&root, false).unwrap();
        std::fs::write(root.join("workspace-order.json"), b"incomplete document").unwrap();
        assert_eq!(
            WorkspaceOrderStore::open(directory, [9; 32]).err(),
            Some(ServerError::UnsafeStorage)
        );
        assert_eq!(
            std::fs::read(root.join("workspace-order.json")).unwrap(),
            b"incomplete document"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
