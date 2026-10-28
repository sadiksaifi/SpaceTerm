//! Presents each saved paired route's Workspaces beside the others.
//!
//! A route's Workspaces stay presented from launch until the person removes its Saved
//! Connection. While the route has no connection, before its first one or after Disconnect, it
//! presents the Workspaces and Tabs it last confirmed, marked unavailable: navigation and personal
//! rearrangement work, shared edits don't, and its Panes reach no Terminal Session. A connection
//! reconciles them with the server's inventory in place. Following the registry performs no
//! network work and asks for no credential.

use std::cmp::Ordering;
use std::rc::Rc;

use gpui::{App, Context, SharedString, Window};

use super::{ServerKey, ServerOrigin, WorkspaceManager};
use crate::direct_connections::{
    DirectConnections, DirectRouteId, DirectRouteStatus, RefusalReason,
};
use crate::ui::ServerTarget;
use crate::ui::shared_workspaces::{
    DirectRoutePanes, DirectServer, SharedEditSource, SharedPaneOpener, SharedPaneSessions,
    UnavailableEdits,
};

/// One presented paired route: its binding, its Pane opener and the connection both follow.
pub(super) struct DirectRoute {
    key: ServerKey,
    panes: Rc<DirectRoutePanes>,
    server: Option<spaceterm_server::ServerConnection>,
}

/// The paired routes one Workspace Window presents, by route.
#[derive(Default)]
pub(super) struct DirectRoutes(std::collections::BTreeMap<DirectRouteId, DirectRoute>);

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

    /// Presents each saved route the registry lists and withdraws each one it no longer does.
    /// A route without a connection presents the Workspaces it retained.
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
        let presented = presented_routes(&connections, |route| self.direct_route_presented(route));
        for key in self.shared.keys() {
            let Some(ServerOrigin::Direct(direct)) = self.shared.origin(key) else {
                continue;
            };
            let route = direct.route;
            if presented.iter().any(|(kept, _)| kept.route == route) {
                continue;
            }
            // A removed or unpaired route never presents its Workspaces again.
            let removed = connections.retained_inventory_loaded()
                && match connections.status(&route) {
                    None => true,
                    Some(status) => status == DirectRouteStatus::Refused(RefusalReason::Revoked),
                };
            if removed && let Some(shared) = self.shared.get_mut(key) {
                shared.discard_retained_copy();
            }
            self.withdraw_server_workspaces(key, window, cx);
            self.direct_routes.0.remove(&route);
            crate::ui::server_worktrees::Client::forget_direct(&route, cx);
        }
        for (direct, server) in presented {
            if self.direct_route_presented(&direct.route) {
                self.follow_direct_connection(&direct.route, server, cx);
            } else if let Some(storage) = connections.storage(&direct.route) {
                self.present_direct_server(direct, storage, server, window, cx);
            }
        }
        // The creation menus show each saved route, which changes without a new binding.
        cx.notify();
    }

    /// Presents one paired route's Workspaces: the server's own through `server`, or those this
    /// client retained until it connects. Its client views, read through `storage`, are this
    /// client's own for that route.
    fn present_direct_server(
        &mut self,
        direct: DirectServer,
        storage: spaceterm_server::ServerConnection,
        server: Option<spaceterm_server::ServerConnection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let route = direct.route;
        let Some(key) = self.present_direct_route(direct, window, cx) else {
            return;
        };
        let Some(shared) = self.shared.get_mut(key) else {
            return;
        };
        shared.identify(Rc::new(storage.clone()));
        shared.retain_inventory_copies();
        shared.retain_client_views(storage);
        let read = shared.client_views_read();
        self.follow_direct_connection(&route, server, cx);
        cx.spawn_in(window, async move |manager, cx| {
            read.await;
            let _ = manager.update_in(cx, |manager, window, cx| {
                manager.present_retained_copy(&route);
                manager.sync_shared_workspaces(key, window, cx);
            });
        })
        .detach();
    }

    /// Binds one paired route with no connection yet. Its Panes take no local file authority.
    pub(super) fn present_direct_route(
        &mut self,
        direct: DirectServer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<ServerKey> {
        // The client home only completes the launch plan a server-backed Terminal Session
        // ignores. It is never a directory on the other computer.
        let client_home = self.local_home_directory().ok()?;
        let route = direct.route;
        let panes = Rc::new(DirectRoutePanes::new(direct.clone(), client_home));
        let sessions = SharedPaneSessions::with_opener(
            Rc::clone(&panes) as Rc<dyn SharedPaneOpener>,
            &self.pane_construction,
        );
        let edits = SharedEditSource::new(Rc::new(UnavailableEdits::default()));
        let key = self.present_server_workspaces(
            ServerOrigin::Direct(direct),
            edits,
            sessions,
            window,
            cx,
        );
        self.direct_routes.0.insert(
            route,
            DirectRoute {
                key,
                panes,
                server: None,
            },
        );
        Some(key)
    }

    /// Presents what `route` retained until its server publishes an inventory.
    pub(super) fn present_retained_copy(&mut self, route: &DirectRouteId) {
        let Some(followed) = self.direct_routes.0.get(route) else {
            return;
        };
        if let Some(shared) = self.shared.get_mut(followed.key) {
            shared.present_retained_copy();
            followed.panes.retain_titles(shared.retained_copy());
        }
    }

    /// Points one presented route at `server`, its connection now, or at what it retained when
    /// it has none.
    fn follow_direct_connection(
        &mut self,
        route: &DirectRouteId,
        server: Option<spaceterm_server::ServerConnection>,
        cx: &mut Context<Self>,
    ) {
        let client_home = self.local_home_directory();
        let Some(followed) = self.direct_routes.0.get_mut(route) else {
            return;
        };
        let unchanged = match (&followed.server, &server) {
            (None, None) => true,
            (Some(current), Some(next)) => current.same_handle(next),
            _ => false,
        };
        if unchanged {
            return;
        }
        followed.server.clone_from(&server);
        let direct = followed.panes.direct_route().cloned();
        let connection = match (server, direct, client_home) {
            (Some(server), Some(direct), Ok(client_home)) => {
                let client =
                    crate::ui::server_worktrees::Client::for_direct(*route, server.clone(), cx);
                let opener = SharedPaneSessions::remote_opener(
                    server,
                    client.controls.clone(),
                    direct,
                    client_home,
                );
                Some((client.transport(), opener))
            }
            _ => {
                crate::ui::server_worktrees::Client::forget_direct(route, cx);
                None
            }
        };
        let connected = connection.is_some();
        self.follow_direct_route(route, connection);
        if connected
            && let Some(key) = self.direct_routes.0.get(route).map(|route| route.key)
            && let Some(source) = self.shared.get(key).map(|shared| shared.edits())
        {
            crate::ui::server_worktrees::Client::adopt_source(route, source, cx);
        }
    }

    /// Points one presented route at a connection's transport and Pane opener, or at what it
    /// retained when `connection` is `None`. Its Workspaces, Tabs and this client's views of
    /// them stay in place; each Pane opens again through the connection, or as unavailable.
    pub(super) fn follow_direct_route(
        &mut self,
        route: &DirectRouteId,
        connection: Option<(
            Rc<dyn crate::ui::shared_workspaces::SharedWorkspaceEdits>,
            Rc<dyn SharedPaneOpener>,
        )>,
    ) {
        let Some(followed) = self.direct_routes.0.get(route) else {
            return;
        };
        let Some(shared) = self.shared.get_mut(followed.key) else {
            return;
        };
        match connection {
            Some((edits, opener)) => {
                followed.panes.follow(Some(opener));
                shared.replace_transport(edits);
            }
            None => {
                followed.panes.retain_titles(shared.retained_copy());
                followed.panes.follow(None);
                shared.replace_transport(Rc::new(UnavailableEdits::default()));
            }
        }
    }

    /// Whether this window's presentation of `route` is connected, so it can take shared edits.
    pub(super) fn direct_route_connected(&self, route: &DirectRouteId) -> bool {
        self.server_key(&ServerTarget::Direct(*route))
            .and_then(|key| self.shared.get(key))
            .is_some_and(crate::ui::shared_workspaces::SharedWorkspaces::connected)
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
    /// presents connected can take a new Workspace now.
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
                available: self.direct_route_connected(&route.id),
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

/// The routes a window presents, each with its connection if it has one.
///
/// Every connected route presents its server's Workspaces. Two saved routes may reach one
/// server, such as one over the local network and one over Tailscale, and each connected one
/// presents that server through its own pairing. A route without a connection presents what it
/// retained only while no route to its server is connected, and only one such route per server
/// does: the one already presented, then one the person keeps connecting, then the first.
fn presented_routes(
    connections: &DirectConnections,
    presented: impl Fn(&DirectRouteId) -> bool,
) -> Vec<(DirectServer, Option<spaceterm_server::ServerConnection>)> {
    let saved: Vec<_> = connections
        .routes()
        .into_iter()
        .filter_map(|(id, status)| {
            let route = connections.route(&id)?;
            Some((route, status, connections.server(&id)))
        })
        .collect();
    let mut chosen: Vec<(DirectServer, Option<spaceterm_server::ServerConnection>)> = Vec::new();
    let mut retained_servers = Vec::new();
    for (route, _, server) in &saved {
        if let Some(server) = server {
            chosen.push((direct_server(route), Some(server.clone())));
        }
    }
    let connected = |pin: &[u8; 32]| {
        saved
            .iter()
            .any(|(route, _, server)| server.is_some() && route.paired.pin.server == *pin)
    };
    let mut offline: Vec<_> = saved
        .iter()
        .filter(|(route, status, server)| {
            server.is_none()
                && *status != DirectRouteStatus::Refused(RefusalReason::Revoked)
                && !connected(&route.paired.pin.server)
        })
        .collect();
    offline.sort_by_key(|(route, status, _)| {
        (
            !presented(&route.id),
            *status == DirectRouteStatus::Disconnected,
            route.id,
        )
    });
    for (route, _, _) in offline {
        if !retained_servers.contains(&route.paired.pin.server) {
            retained_servers.push(route.paired.pin.server);
            chosen.push((direct_server(route), None));
        }
    }
    chosen
}

fn direct_server(route: &crate::direct_connections::DirectRoute) -> DirectServer {
    DirectServer {
        route: route.id,
        name: SharedString::from(route.label.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::presented_routes;
    use crate::direct_connections::tests::connected_routes_to_one_server;

    /// Two saved routes reach one server, as one over the local network and one over Tailscale
    /// do. Retained copies never present that server twice where its connected routes don't.
    #[test]
    fn a_server_reached_by_two_routes_should_present_at_most_one_retained_copy() {
        let (direct, routes) = connected_routes_to_one_server(&["lan", "tailscale"], [0x5E; 32]);
        let presented = |presented: &[usize]| {
            let kept: Vec<_> = presented.iter().map(|index| routes[*index]).collect();
            presented_routes(&direct, |route| kept.contains(route))
                .into_iter()
                .map(|(server, connection)| {
                    (
                        routes.iter().position(|route| *route == server.route),
                        connection.is_some(),
                    )
                })
                .collect::<Vec<_>>()
        };

        let both = presented(&[0, 1]);
        direct.disconnect(&routes[1]);
        let one_connected = presented(&[0, 1]);
        direct.disconnect(&routes[0]);
        let none_connected = presented(&[0, 1]);
        let tailscale_kept = presented(&[1]);
        let nothing_presented = presented(&[]);

        assert_eq!(both, [(Some(0), true), (Some(1), true)]);
        assert_eq!(
            one_connected,
            [(Some(0), true)],
            "a connected route's server needs no retained copy"
        );
        assert_eq!(none_connected, [(Some(0), false)]);
        assert_eq!(
            tailscale_kept,
            [(Some(1), false)],
            "the copy already presented stays"
        );
        assert_eq!(nothing_presented, [(Some(0), false)]);
    }
}
