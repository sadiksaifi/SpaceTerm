//! Presents each connected paired server's Workspaces beside the others.
//!
//! The desktop's direct registry decides which routes stay presented: a connected route and one
//! reconnecting after an interruption keep their binding, so their Panes stay frozen in place, and
//! a disconnected, refused or unpaired one is withdrawn. Following the registry performs no
//! network work and asks for no credential.

use std::cmp::Ordering;

use gpui::{App, Context, SharedString, Window};

use super::{ServerKey, ServerOrigin, WorkspaceManager};
use crate::direct_connections::DirectRouteId;
use crate::ui::ServerTarget;
use crate::ui::shared_workspaces::{DirectServer, ServerIdentity, SharedPaneSessions};

impl WorkspaceManager {
    /// Follows the desktop's direct registry for this window's life. A build without one follows
    /// nothing.
    pub(super) fn follow_direct_servers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connections) = crate::ui::direct_servers::installed(cx) else {
            return;
        };
        self._direct_servers = Some(crate::ui::direct_servers::DirectChanges::follow(
            &connections,
            |changes| {
                cx.spawn_in(window, async move |manager, cx| {
                    loop {
                        if manager
                            .update_in(cx, |manager, window, cx| {
                                manager.sync_direct_servers(window, cx)
                            })
                            .is_err()
                        {
                            break;
                        }
                        if changes.recv().await.is_err() {
                            break;
                        }
                    }
                })
            },
        ));
    }

    /// Binds each newly presented direct route and withdraws each one the registry no longer
    /// presents.
    fn sync_direct_servers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connections) = crate::ui::direct_servers::installed(cx) else {
            return;
        };
        // One device presents its servers in one Workspace Window.
        if let Some(existing) = crate::ui::shared_workspaces::presenting_window(cx)
            && window.window_handle().window_id() != existing.window_id()
        {
            return;
        }
        let presented: Vec<(DirectServer, spaceterm_server::ServerConnection)> = connections
            .routes()
            .into_iter()
            .filter_map(|(route, _)| {
                let server = connections.server(&route)?;
                let name = connections.route(&route)?.label;
                Some((
                    DirectServer {
                        route,
                        name: SharedString::from(name),
                    },
                    server,
                ))
            })
            .collect();
        for key in self.shared.keys() {
            let Some(ServerOrigin::Direct(direct)) = self.shared.origin(key) else {
                continue;
            };
            if !presented
                .iter()
                .any(|(presented, _)| presented.route == direct.route)
            {
                let route = direct.route;
                self.withdraw_server_workspaces(key, window, cx);
                crate::ui::server_worktrees::Client::forget_direct(&route, cx);
            }
        }
        for (direct, server) in presented {
            // Another route already presents this server's Workspaces.
            let duplicate = server
                .server()
                .is_some_and(|server| self.shared.presenting(server).is_some());
            if !duplicate && !self.direct_route_presented(&direct.route) {
                self.present_direct_server(direct, server, window, cx);
            }
        }
        // The creation menus show each saved route, which changes without a new binding.
        cx.notify();
    }

    /// Presents one paired server's Workspaces. Its Panes take no local file authority,
    /// and its client views are this client's own for that server.
    fn present_direct_server(
        &mut self,
        direct: DirectServer,
        server: spaceterm_server::ServerConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The client home only completes the launch plan a server-backed Terminal Session
        // ignores. It is never a directory on the other computer.
        let Ok(client_home) = self.local_home_directory() else {
            return;
        };
        let client =
            crate::ui::server_worktrees::Client::for_direct(direct.route, server.clone(), cx);
        let edits = client.source.clone();
        let sessions = SharedPaneSessions::direct(
            server.clone(),
            client.controls.clone(),
            direct.clone(),
            client_home,
            &self.pane_construction,
        );
        let key = self.present_server_workspaces(
            ServerOrigin::Direct(direct),
            edits,
            sessions,
            window,
            cx,
        );
        let Some(shared) = self.shared.get_mut(key) else {
            return;
        };
        shared.identify(std::rc::Rc::new(server.clone()));
        shared.retain_client_views(server);
        let read = shared.client_views_read();
        cx.spawn_in(window, async move |manager, cx| {
            read.await;
            let _ = manager.update_in(cx, |manager, window, cx| {
                manager.sync_shared_workspaces(key, window, cx);
            });
        })
        .detach();
    }

    /// Whether an earlier binding presents the server the binding at `key` reaches, as when two
    /// saved routes reach one server. Only the earlier one presents that server's Workspaces, so
    /// each appears once. The saved routes stay as they are.
    pub(super) fn duplicates_presented_server(&self, key: ServerKey) -> bool {
        self.shared
            .get(key)
            .and_then(|shared| shared.server_identity())
            .and_then(|server| self.shared.presenting(server))
            .is_some_and(|first| first != key)
    }

    /// The direct route `route`'s presented binding, if any.
    pub(super) fn direct_route_presented(&self, route: &DirectRouteId) -> bool {
        self.shared.keys().into_iter().any(|key| {
            matches!(
                self.shared.origin(key),
                Some(ServerOrigin::Direct(bound)) if bound.route == *route
            )
        })
    }

    /// Asks the server `target` names for a new Workspace named `name` unless blank, which it
    /// presents to every connected client and this window activates once the server publishes
    /// it.
    pub(super) fn create_workspace_on(
        &mut self,
        target: &ServerTarget,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::shared_workspaces::{SharedRefusalReason, SharedSubmission};

        let Some(shared) = self
            .server_key(target)
            .and_then(|key| self.shared.get_mut(key))
        else {
            WorkspaceManager::show_shared_refusal(
                "create a Workspace",
                SharedRefusalReason::Offline,
                window,
                cx,
            );
            return;
        };
        if let SharedSubmission::Refused(reason) = shared.create_workspace(name, None) {
            WorkspaceManager::show_shared_refusal("create a Workspace", reason, window, cx);
        }
        cx.notify();
    }

    /// Every saved paired server the creation menus offer, by name. Only one this window
    /// presents can take a new Workspace now.
    pub(super) fn remote_creation_servers(
        &self,
        cx: &App,
    ) -> Vec<crate::ui::workspace_creation::RemoteCreationServer> {
        use crate::ui::workspace_creation::{RemoteCreationServer, endpoint_address};

        let Some(direct) = crate::ui::direct_servers::installed(cx) else {
            return Vec::new();
        };
        let saved: Vec<_> = direct
            .routes()
            .into_iter()
            .filter_map(|(route, _)| direct.route(&route))
            .collect();
        // Two saved servers may give one name, such as an earlier pairing and a newer one, so a
        // shared name carries the address each route reaches.
        let shares_name = |name: &str| {
            saved
                .iter()
                .filter(|other| other.label.to_lowercase() == name.to_lowercase())
                .count()
                > 1
        };
        let mut servers: Vec<_> = saved
            .iter()
            .map(|route| RemoteCreationServer {
                route: route.id,
                name: route.label.clone().into(),
                address: shares_name(&route.label)
                    .then(|| endpoint_address(&route.endpoint).to_owned().into()),
                available: self.direct_route_presented(&route.id),
            })
            .collect();
        servers.sort_by(|a, b| {
            by_name(&a.name, &b.name)
                .then_with(|| a.address.cmp(&b.address))
                .then_with(|| a.route.cmp(&b.route))
        });
        servers
    }

    /// The binding presenting `target`'s server, if this window presents it.
    pub(super) fn server_key(&self, target: &ServerTarget) -> Option<ServerKey> {
        self.shared
            .keys()
            .into_iter()
            .find(|key| match (self.shared.origin(*key), target) {
                (Some(ServerOrigin::Local), ServerTarget::Local) => true,
                (Some(ServerOrigin::Direct(bound)), ServerTarget::Direct(route)) => {
                    bound.route == *route
                }
                _ => false,
            })
    }
}

/// Orders names ignoring case, so spellings that differ only in case stay distinct but adjacent.
fn by_name(a: &str, b: &str) -> Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}
