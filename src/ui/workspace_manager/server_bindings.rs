//! The SpaceTerm Servers whose Workspaces one Workspace Window presents.
//!
//! The account-local server and each connected paired server publish their own inventory,
//! answers, and attachments. Each one keeps its own binding, so a shared Workspace's edits, closes, and
//! Pane connections always reach the server that owns it, and losing one server leaves the
//! others' Workspaces untouched.

use gpui::Task;

use crate::domain::WorkspaceId;
use crate::ui::shared_workspaces::SharedWorkspaces;

/// Names one presented server for the window's lifetime. Deferred answers, such as an alert's
/// choice or a Workspace Switcher item, carry it to reach the server they were shown for.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ServerKey(u32);

impl ServerKey {
    /// The account-local server, which every desktop presents first.
    pub(crate) const LOCAL: Self = Self(0);
}

/// Where a presented server runs, and the route this client reaches it by.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ServerOrigin {
    Local,
    /// A server reached over a paired route, with the name it gave.
    Direct(crate::ui::shared_workspaces::DirectServer),
}

impl ServerOrigin {
    /// The name another computer's server reads by, or `None` for this computer's.
    pub(crate) fn remote_name(&self) -> Option<String> {
        match self {
            Self::Local => None,
            Self::Direct(direct) => Some(direct.name.to_string()),
        }
    }
}

struct Binding {
    key: ServerKey,
    origin: ServerOrigin,
    workspaces: SharedWorkspaces,
    _poll: Option<Task<()>>,
    _wake: Option<Task<()>>,
}

/// Every server binding, the local one first.
#[derive(Default)]
pub(crate) struct ServerBindings {
    bindings: Vec<Binding>,
    next: u32,
}

impl ServerBindings {
    pub(crate) fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// The binding presenting `origin`, if one does.
    pub(crate) fn key_of(&self, origin: &ServerOrigin) -> Option<ServerKey> {
        self.bindings
            .iter()
            .find(|binding| &binding.origin == origin)
            .map(|binding| binding.key)
    }

    /// Adds a server's binding, or returns the existing binding's key unchanged.
    pub(crate) fn insert(
        &mut self,
        origin: ServerOrigin,
        workspaces: SharedWorkspaces,
    ) -> (ServerKey, bool) {
        if let Some(key) = self.key_of(&origin) {
            return (key, false);
        }
        let key = if origin == ServerOrigin::Local {
            ServerKey::LOCAL
        } else {
            self.next = self.next.saturating_add(1);
            ServerKey(self.next)
        };
        let binding = Binding {
            key,
            origin,
            workspaces,
            _poll: None,
            _wake: None,
        };
        if key == ServerKey::LOCAL {
            self.bindings.insert(0, binding);
        } else {
            self.bindings.push(binding);
        }
        (key, true)
    }

    /// Keeps the tasks that read one binding's server for as long as the binding lives.
    pub(crate) fn follow(&mut self, key: ServerKey, poll: Task<()>, wake: Option<Task<()>>) {
        if let Some(binding) = self.binding_mut(key) {
            binding._poll = Some(poll);
            binding._wake = wake;
        }
    }

    /// Removes a binding, which stops reading its server.
    pub(crate) fn remove(&mut self, key: ServerKey) -> Option<SharedWorkspaces> {
        let index = self
            .bindings
            .iter()
            .position(|binding| binding.key == key)?;
        Some(self.bindings.remove(index).workspaces)
    }

    fn binding_mut(&mut self, key: ServerKey) -> Option<&mut Binding> {
        self.bindings.iter_mut().find(|binding| binding.key == key)
    }

    pub(crate) fn get(&self, key: ServerKey) -> Option<&SharedWorkspaces> {
        self.bindings
            .iter()
            .find(|binding| binding.key == key)
            .map(|binding| &binding.workspaces)
    }

    pub(crate) fn get_mut(&mut self, key: ServerKey) -> Option<&mut SharedWorkspaces> {
        self.binding_mut(key).map(|binding| &mut binding.workspaces)
    }

    pub(crate) fn origin(&self, key: ServerKey) -> Option<&ServerOrigin> {
        self.bindings
            .iter()
            .find(|binding| binding.key == key)
            .map(|binding| &binding.origin)
    }

    /// The account-local server's binding.
    pub(crate) fn local(&self) -> Option<&SharedWorkspaces> {
        self.get(ServerKey::LOCAL)
    }

    pub(crate) fn local_mut(&mut self) -> Option<&mut SharedWorkspaces> {
        self.get_mut(ServerKey::LOCAL)
    }

    /// The server presenting one local Workspace.
    pub(crate) fn owner(&self, workspace_id: WorkspaceId) -> Option<ServerKey> {
        self.bindings
            .iter()
            .find(|binding| binding.workspaces.is_shared(workspace_id))
            .map(|binding| binding.key)
    }

    pub(crate) fn owning(&self, workspace_id: WorkspaceId) -> Option<&SharedWorkspaces> {
        self.bindings
            .iter()
            .map(|binding| &binding.workspaces)
            .find(|workspaces| workspaces.is_shared(workspace_id))
    }

    pub(crate) fn owning_mut(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> Option<&mut SharedWorkspaces> {
        self.bindings
            .iter_mut()
            .map(|binding| &mut binding.workspaces)
            .find(|workspaces| workspaces.is_shared(workspace_id))
    }

    pub(crate) fn keys(&self) -> Vec<ServerKey> {
        self.bindings.iter().map(|binding| binding.key).collect()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (ServerKey, &SharedWorkspaces)> {
        self.bindings
            .iter()
            .map(|binding| (binding.key, &binding.workspaces))
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut SharedWorkspaces> {
        self.bindings
            .iter_mut()
            .map(|binding| &mut binding.workspaces)
    }
}
