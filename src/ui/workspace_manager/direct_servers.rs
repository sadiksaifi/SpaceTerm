//! Presents each connected direct network server's Workspaces beside the others.
//!
//! The desktop's direct registry decides which routes stay presented: a connected route and one
//! reconnecting after an interruption keep their binding, so their Panes stay frozen in place, and
//! a disconnected, refused or unpaired one is withdrawn. Following the registry performs no
//! network work and asks for no credential.

use gpui::{Context, SharedString, Window};

use super::{ServerOrigin, WorkspaceManager};
use crate::direct_connections::DirectRouteId;
use crate::ui::shared_workspaces::{DirectServer, SharedPaneSessions};

impl WorkspaceManager {
    /// Follows the desktop's direct registry for this window's life. A build without one follows
    /// nothing.
    pub(super) fn follow_direct_servers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connections) = crate::ui::direct_servers::installed(cx) else {
            return;
        };
        let changes = connections.subscribe();
        self._direct_servers = Some(cx.spawn_in(window, async move |manager, cx| {
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
        }));
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
        let presented: Vec<(DirectServer, spaceterm_server::LocalServer)> = connections
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
            if !self.direct_route_presented(&direct.route) {
                self.present_direct_server(direct, server, window, cx);
            }
        }
        // Machine headings show each route's state, which changes without a new binding.
        cx.notify();
    }

    /// Presents one direct network server's Workspaces. Its Panes take no local file authority,
    /// and its client views are this client's own for that server.
    fn present_direct_server(
        &mut self,
        direct: DirectServer,
        server: spaceterm_server::LocalServer,
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
        let Some(sessions) = SharedPaneSessions::direct(
            server.clone(),
            client.controls.clone(),
            direct.clone(),
            client_home,
            &self.pane_construction,
        ) else {
            return;
        };
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

    /// The direct route `route`'s presented binding, if any.
    pub(super) fn direct_route_presented(&self, route: &DirectRouteId) -> bool {
        self.shared.keys().into_iter().any(|key| {
            matches!(
                self.shared.origin(key),
                Some(ServerOrigin::Direct(bound)) if bound.route == *route
            )
        })
    }
}
