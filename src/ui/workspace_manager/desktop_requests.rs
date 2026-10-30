//! Answers `spaceterm open` and `spaceterm show` from this account's Terminal Sessions.
//!
//! A request names exact server, Workspace, Tab, Pane and Terminal Session IDs. They select only
//! what this window presents and grant nothing. Only a Local Pane's own filesystem authority
//! opens a file, so a path from a Remote Pane never reaches the local filesystem.
use super::{ServerKey, ServerOrigin, WorkspaceManager};
use crate::desktop_channel::{Refusal, Reply, Request, SessionContext};
use crate::domain::WorkspaceId;
use crate::terminal::{HyperlinkTarget, TerminalLocalFileCapabilities};
use gpui::{App, Context, Window};
use spaceterm_server::hierarchy::Id;
use std::path::PathBuf;

/// The server ID behind one presented server, or `None` while this desktop cannot tell it.
pub(crate) type PresentedServer<'a> = &'a dyn Fn(&ServerOrigin, &App) -> Option<Id>;

impl WorkspaceManager {
    pub(crate) fn answer_desktop_request(
        &mut self,
        request: Request,
        server_of: PresentedServer<'_>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Reply {
        let answer = match request {
            Request::Show {
                server,
                workspace,
                tab,
                pane,
            } => self
                .presented_workspace(server, workspace, server_of, cx)
                .ok_or(Refusal::NotShown)
                .and_then(|workspace_id| self.show_shared(workspace_id, tab, pane, window, cx)),
            Request::Open {
                session,
                path,
                preview,
            } => self.open_local_file(session, path, preview, server_of, window, cx),
        };
        match answer {
            Ok(()) => Reply::Done,
            Err(refusal) => Reply::Refused(refusal),
        }
    }

    /// The local Workspace presenting the server Workspace `workspace` of `server`.
    fn presented_workspace(
        &self,
        server: Id,
        workspace: Id,
        server_of: PresentedServer<'_>,
        cx: &App,
    ) -> Option<WorkspaceId> {
        self.shared.iter().find_map(|(key, shared)| {
            let workspace_id = shared.local(workspace)?;
            self.presents_server(key, server, server_of, cx)
                .then_some(workspace_id)
        })
    }

    fn presents_server(
        &self,
        key: ServerKey,
        server: Id,
        server_of: PresentedServer<'_>,
        cx: &App,
    ) -> bool {
        self.shared
            .origin(key)
            .and_then(|origin| server_of(origin, cx))
            == Some(server)
    }

    /// Brings this window's own view to a presented Workspace, and to its Tab and Pane when
    /// given, then brings the window forward. Nothing moves unless every level is presented.
    fn show_shared(
        &mut self,
        workspace_id: WorkspaceId,
        tab: Option<Id>,
        pane: Option<Id>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), Refusal> {
        let manager = self
            .workspaces
            .workspace(workspace_id)
            .map(|entry| entry.payload().clone())
            .ok_or(Refusal::NotShown)?;
        if !manager.read(cx).presents_shared_target(tab, pane, cx) {
            return Err(Refusal::NotShown);
        }
        if self.workspaces.active_workspace_id() != Some(workspace_id)
            && !self.activate_workspace_focusing(workspace_id, true, window, cx)
        {
            return Err(Refusal::NotShown);
        }
        self.focus(window, cx);
        self.scroll_active_workspace_into_view(cx);
        if let Some(target) = pane.or(tab) {
            manager.update(cx, |manager, cx| {
                manager.open_accepted_shared(target, window, cx);
            });
        }
        window.activate_window();
        cx.activate(true);
        Ok(())
    }

    fn open_local_file(
        &mut self,
        session: SessionContext,
        path: PathBuf,
        preview: bool,
        server_of: PresentedServer<'_>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), Refusal> {
        let (key, workspace_id) = self
            .workspaces
            .iter()
            .find_map(|entry| {
                let key = self.shared.owner(entry.id())?;
                entry
                    .payload()
                    .read(cx)
                    .presents_shared_pane(session.pane, cx)
                    .then_some((key, entry.id()))
            })
            .ok_or(Refusal::NotShown)?;
        if !self.presents_server(key, session.server, server_of, cx) {
            return Err(Refusal::NotShown);
        }
        let shared = self.shared.get(key).ok_or(Refusal::NotShown)?;
        // Only the account-local server's Panes carry this machine's filesystem authority.
        let filesystem = shared
            .sessions()
            .local_filesystem()
            .ok_or(Refusal::RemoteSession)?
            .clone();
        let live = shared
            .latest_inventory()
            .and_then(|inventory| inventory.close.as_ref())
            .and_then(|close| close.sessions.iter().find(|pane| pane.id == session.pane))
            .and_then(|pane| pane.session);
        if live != Some(session.session) {
            return Err(Refusal::NotShown);
        }
        let text = path.to_str().ok_or(Refusal::FileUnavailable)?;
        if !filesystem.path_semantics().is_absolute(&path) {
            return Err(Refusal::FileUnavailable);
        }
        // Opening the file for its identity proves this account can read it.
        let target = filesystem
            .local_file(text, &path)
            .and_then(HyperlinkTarget::from_local_file)
            .ok_or(Refusal::FileUnavailable)?;
        if !preview {
            let url = target
                .activation_url(TerminalLocalFileCapabilities::Enabled)
                .ok_or(Refusal::FileUnavailable)?;
            cx.open_url(&url);
            return Ok(());
        }
        // File Preview presents for its Pane, so the person sees which Pane asked.
        self.show_shared(workspace_id, None, Some(session.pane), window, cx)?;
        let manager = self
            .workspaces
            .workspace(workspace_id)
            .map(|entry| entry.payload().clone())
            .ok_or(Refusal::NotShown)?;
        if manager.update(cx, |manager, cx| {
            manager.preview_shared_local_file(session.pane, &target, window, cx)
        }) {
            Ok(())
        } else {
            Err(Refusal::PreviewUnavailable)
        }
    }
}

/// Answers each request the endpoint accepts on the GPUI main thread, in the window presenting
/// this desktop's shared Workspaces. The endpoint's thread waits for each answer.
pub(crate) fn serve(
    endpoint: std::sync::Arc<dyn crate::desktop_channel::DesktopChannelEndpoint>,
    identity: &'static str,
    cx: &mut App,
) {
    type Exchange = (Request, std::sync::mpsc::SyncSender<Reply>);
    let (requests, received) = async_channel::bounded::<Exchange>(1);
    let spawned = std::thread::Builder::new()
        .name("desktop-channel".into())
        .spawn(move || {
            loop {
                let stream = match endpoint.accept() {
                    Ok(Some(stream)) => stream,
                    Ok(None) => continue,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::Interrupted | std::io::ErrorKind::ConnectionAborted
                        ) =>
                    {
                        continue;
                    }
                    Err(_) => return,
                };
                let _ = crate::desktop_channel::serve(stream, identity, |request| {
                    let (reply, answer) = std::sync::mpsc::sync_channel(1);
                    if requests.send_blocking((request, reply)).is_err() {
                        return Reply::Refused(Refusal::NotShown);
                    }
                    answer.recv().unwrap_or(Reply::Refused(Refusal::NotShown))
                });
            }
        });
    if spawned.is_err() {
        return;
    }
    cx.spawn(async move |cx| {
        while let Ok((request, reply)) = received.recv().await {
            let answer = cx.update(|cx| answer(request, cx));
            let _ = reply.send(answer);
        }
    })
    .detach();
}

fn answer(request: Request, cx: &mut App) -> Reply {
    let Some(window) = crate::ui::shared_workspaces::presenting_window(cx) else {
        return Reply::Refused(Refusal::NotShown);
    };
    window
        .update(cx, |manager, window, cx| {
            manager.answer_desktop_request(request, &presented_server, window, cx)
        })
        .unwrap_or(Reply::Refused(Refusal::NotShown))
}

/// The local server's admitted ID, or the server ID a paired route pins.
fn presented_server(origin: &ServerOrigin, cx: &App) -> Option<Id> {
    match origin {
        ServerOrigin::Local => cx
            .try_global::<crate::ui::server_worktrees::Client>()?
            .server
            .admitted_server_id()
            .ok(),
        ServerOrigin::Direct(direct) => crate::ui::direct_servers::installed(cx)?
            .route(&direct.route)
            .map(|route| route.paired.pin.server),
    }
}
