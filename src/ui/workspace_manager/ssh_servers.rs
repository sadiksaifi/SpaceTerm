//! Presents each connected SSH server's Workspaces beside the account-local server's.
//!
//! The desktop's SSH server registry decides which servers stay presented: a connected server
//! and one reconnecting after an interruption keep their binding, so their Panes stay frozen in
//! place, and a disconnected one is withdrawn. Following the registry performs no remote work.

use gpui::{Context, Window};

use super::{ServerOrigin, WorkspaceManager};
use crate::domain::SshDestination;
use crate::ui::shared_workspaces::SharedPaneSessions;

impl WorkspaceManager {
    /// Follows the desktop's SSH server registry for this window's life. A build without one
    /// follows nothing.
    pub(super) fn follow_ssh_servers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connections) = crate::ui::ssh_servers::installed(cx) else {
            return;
        };
        let changes = connections.subscribe();
        self._ssh_servers = Some(cx.spawn_in(window, async move |manager, cx| {
            loop {
                if manager
                    .update_in(cx, |manager, window, cx| {
                        manager.sync_ssh_servers(window, cx)
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

    /// Binds each newly presented SSH server and withdraws each one the registry no longer
    /// presents.
    fn sync_ssh_servers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connections) = crate::ui::ssh_servers::installed(cx) else {
            return;
        };
        // One device presents its servers in one Workspace Window.
        if let Some(existing) = crate::ui::shared_workspaces::presenting_window(cx)
            && window.window_handle().window_id() != existing.window_id()
        {
            return;
        }
        let presented: Vec<(SshDestination, spaceterm_server::LocalServer)> = connections
            .connections()
            .into_iter()
            .filter_map(|(destination, _)| {
                let server = connections.server(&destination)?;
                Some((destination, server))
            })
            .collect();
        for key in self.shared.keys() {
            let Some(ServerOrigin::Ssh(destination)) = self.shared.origin(key) else {
                continue;
            };
            if !presented
                .iter()
                .any(|(presented, _)| presented == destination)
            {
                let destination = destination.clone();
                self.withdraw_server_workspaces(key, window, cx);
                crate::ui::server_worktrees::Client::forget_ssh(&destination, cx);
            }
        }
        for (destination, server) in presented {
            let origin = ServerOrigin::Ssh(destination.clone());
            if self.shared.key_of(&origin).is_none() {
                self.present_ssh_server(destination, server, window, cx);
            }
        }
    }

    /// Presents one SSH server's Workspaces. Its Panes take no local file authority, and its
    /// client views are this client's own for that server.
    fn present_ssh_server(
        &mut self,
        destination: SshDestination,
        server: spaceterm_server::LocalServer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The client home only completes the launch plan a server-backed Terminal Session
        // ignores. It is never a directory on the other computer.
        let Ok(client_home) = self.local_home_directory() else {
            return;
        };
        let client = crate::ui::server_worktrees::Client::for_ssh(&destination, server.clone(), cx);
        let edits = client.source.clone();
        let sessions = SharedPaneSessions::remote(
            server.clone(),
            client.controls.clone(),
            destination.clone(),
            client_home,
            &self.pane_construction,
        );
        let key = self.present_server_workspaces(
            ServerOrigin::Ssh(destination),
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
}
