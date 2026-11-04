//! The SpaceTerm Servers whose Workspaces one Workspace Window presents.
//!
//! The account-local server and each saved paired server publish their own inventory,
//! answers, and attachments. Each one keeps its own binding, so a Workspace's edits, closes, and
//! Pane connections always reach the server that owns it, and losing one server leaves the
//! others' Workspaces untouched.

use gpui::Task;

use crate::domain::WorkspaceId;
use crate::ui::workspaces::Workspaces;

/// Names one presented server for the window's lifetime. Deferred answers, such as an alert's
/// choice or a Workspace Switcher item, carry it to reach the server they were shown for.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct MachineKey(u32);

impl MachineKey {
    /// The account-local server, which every desktop presents first.
    pub(crate) const LOCAL: Self = Self(0);
}

/// Where a presented server runs, and the route this client reaches it by.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MachineOrigin {
    Local,
    /// A server reached over a paired route, with the name it gave.
    Direct(crate::ui::workspaces::DirectMachine),
}

impl MachineOrigin {
    /// The name another computer's server reads by, or `None` for this computer's.
    pub(crate) fn remote_name(&self) -> Option<String> {
        match self {
            Self::Local => None,
            Self::Direct(direct) => Some(direct.name.to_string()),
        }
    }
}

struct Binding {
    key: MachineKey,
    origin: MachineOrigin,
    workspaces: Workspaces,
    _poll: Option<Task<()>>,
}

/// Every server binding, the local one first.
#[derive(Default)]
pub(crate) struct CoreBindings {
    bindings: Vec<Binding>,
    next: u32,
}

impl CoreBindings {
    pub(crate) fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// The binding presenting `origin`, if one does.
    pub(crate) fn key_of(&self, origin: &MachineOrigin) -> Option<MachineKey> {
        self.bindings
            .iter()
            .find(|binding| &binding.origin == origin)
            .map(|binding| binding.key)
    }

    /// Adds a server's binding, or returns the existing binding's key unchanged.
    pub(crate) fn insert(
        &mut self,
        origin: MachineOrigin,
        workspaces: Workspaces,
    ) -> (MachineKey, bool) {
        if let Some(key) = self.key_of(&origin) {
            return (key, false);
        }
        let key = if origin == MachineOrigin::Local {
            MachineKey::LOCAL
        } else {
            self.next = self.next.saturating_add(1);
            MachineKey(self.next)
        };
        let binding = Binding {
            key,
            origin,
            workspaces,
            _poll: None,
        };
        if key == MachineKey::LOCAL {
            self.bindings.insert(0, binding);
        } else {
            self.bindings.push(binding);
        }
        (key, true)
    }

    /// Keeps the tasks that read one binding's server for as long as the binding lives.
    pub(crate) fn follow(&mut self, key: MachineKey, poll: Task<()>) {
        if let Some(binding) = self.binding_mut(key) {
            binding._poll = Some(poll);
        }
    }

    /// Removes a binding, which stops reading its server.
    pub(crate) fn remove(&mut self, key: MachineKey) -> Option<Workspaces> {
        let index = self
            .bindings
            .iter()
            .position(|binding| binding.key == key)?;
        Some(self.bindings.remove(index).workspaces)
    }

    fn binding_mut(&mut self, key: MachineKey) -> Option<&mut Binding> {
        self.bindings.iter_mut().find(|binding| binding.key == key)
    }

    pub(crate) fn get(&self, key: MachineKey) -> Option<&Workspaces> {
        self.bindings
            .iter()
            .find(|binding| binding.key == key)
            .map(|binding| &binding.workspaces)
    }

    pub(crate) fn get_mut(&mut self, key: MachineKey) -> Option<&mut Workspaces> {
        self.binding_mut(key).map(|binding| &mut binding.workspaces)
    }

    pub(crate) fn origin(&self, key: MachineKey) -> Option<&MachineOrigin> {
        self.bindings
            .iter()
            .find(|binding| binding.key == key)
            .map(|binding| &binding.origin)
    }

    /// The account-local server's binding.
    pub(crate) fn local(&self) -> Option<&Workspaces> {
        self.get(MachineKey::LOCAL)
    }

    pub(crate) fn local_mut(&mut self) -> Option<&mut Workspaces> {
        self.get_mut(MachineKey::LOCAL)
    }

    /// The server presenting one local Workspace.
    pub(crate) fn owner(&self, workspace_id: WorkspaceId) -> Option<MachineKey> {
        self.bindings
            .iter()
            .find(|binding| binding.workspaces.contains_workspace(workspace_id))
            .map(|binding| binding.key)
    }

    pub(crate) fn owning(&self, workspace_id: WorkspaceId) -> Option<&Workspaces> {
        self.bindings
            .iter()
            .map(|binding| &binding.workspaces)
            .find(|workspaces| workspaces.contains_workspace(workspace_id))
    }

    pub(crate) fn owning_mut(&mut self, workspace_id: WorkspaceId) -> Option<&mut Workspaces> {
        self.bindings
            .iter_mut()
            .map(|binding| &mut binding.workspaces)
            .find(|workspaces| workspaces.contains_workspace(workspace_id))
    }

    pub(crate) fn keys(&self) -> Vec<MachineKey> {
        self.bindings.iter().map(|binding| binding.key).collect()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (MachineKey, &Workspaces)> {
        self.bindings
            .iter()
            .map(|binding| (binding.key, &binding.workspaces))
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut Workspaces> {
        self.bindings
            .iter_mut()
            .map(|binding| &mut binding.workspaces)
    }
}
