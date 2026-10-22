//! Removal eligibility is resolved on the owning filesystem, including unattached Workspaces.
use super::*;
use spaceterm_server::hierarchy::Inventory;

#[derive(Default)]
pub(super) struct PinMembership {
    pins: BTreeSet<PathBuf>,
    roots: BTreeSet<PathBuf>,
    next_check: Option<Instant>,
}

impl PinMembership {
    pub(super) fn update(&mut self, inventory: &Inventory, snapshot: &mut Snapshot) {
        let pins = inventory
            .workspaces
            .iter()
            .filter_map(|workspace| workspace.pinned_directory.clone())
            .collect();
        let roots = snapshot
            .directories
            .iter()
            .filter_map(|facts| facts.worktrees.as_ref())
            .flat_map(|list| &list.worktrees)
            .map(|record| record.root.clone())
            .collect::<BTreeSet<_>>();
        let now = Instant::now();
        if self.pins == pins
            && self.roots == roots
            && self.next_check.is_some_and(|next| now < next)
        {
            return;
        }
        // Pin edits and new Git listings take effect immediately. Periodic revalidation also
        // notices a symbolic link retargeted outside SpaceTerm without scanning on every poll.
        self.pins = pins;
        self.roots = roots;
        self.next_check = Some(now + REFRESH);
        let pinned = self
            .roots
            .iter()
            .filter(|root| super::super::shared::pinned_in_worktree(inventory, root) == Ok(true))
            .cloned()
            .collect::<Vec<_>>();
        if pinned != snapshot.pinned_worktrees {
            let previous = std::mem::replace(&mut snapshot.pinned_worktrees, pinned);
            if serde_json::to_vec(snapshot).is_ok_and(|bytes| bytes.len() <= MAX_SNAPSHOT_BYTES) {
                snapshot.revision = snapshot.revision.saturating_add(1);
            } else {
                snapshot.pinned_worktrees = previous;
            }
        }
    }
}
