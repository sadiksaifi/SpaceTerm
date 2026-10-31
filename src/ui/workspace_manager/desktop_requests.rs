//! Answers `spaceterm open` and `spaceterm show` from this account's Terminal Sessions.
//!
//! A request names exact server, Workspace, Tab, Pane and Terminal Session IDs. They select only
//! what this window presents and grant nothing. Only a Local Pane's own filesystem authority
//! opens a file, so a path from a Remote Pane never reaches the local filesystem.
//!
//! The main thread never touches the file. A worker thread validates it, because a network mount
//! can block that work without bound, and the main thread checks again that the Session is still
//! presented before it hands the file to the host. Nothing acts after the request's deadline.
use super::{ServerKey, ServerOrigin, WorkspaceManager};
use crate::desktop_channel::{Refusal, Reply, Request, SessionContext};
use crate::domain::WorkspaceId;
use crate::platform::local_filesystem::LocalFilesystemAuthority;
use crate::terminal::native_services::{FilePreviewTarget, PreparedFilePreview};
use crate::terminal::{HyperlinkTarget, TerminalLocalFileCapabilities};
use gpui::{App, Context, Task, Window};
use spaceterm_server::hierarchy::Id;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// How many file validations may run at once. Each one blocked on an unresponsive mount keeps
/// its worker thread, so this bounds the threads such a mount can hold.
const MAX_VALIDATIONS: usize = 4;
/// How often the window looks for a worker's answer.
const VALIDATION_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// The server ID behind one presented server, or `None` while this desktop cannot tell it.
pub(crate) type PresentedServer = Rc<dyn Fn(&ServerOrigin, &App) -> Option<Id>>;

/// What answering desktop requests needs beyond the window.
#[derive(Clone)]
pub(crate) struct DesktopRequests {
    server_of: PresentedServer,
    /// File validations running now.
    validations: Arc<AtomicUsize>,
}

impl DesktopRequests {
    pub(crate) fn new(server_of: PresentedServer) -> Self {
        Self {
            server_of,
            validations: Arc::default(),
        }
    }

    /// A validation slot, held until the worker finishes, or `None` when every slot is held.
    fn validation(&self) -> Option<ValidationSlot> {
        self.validations
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |running| {
                (running < MAX_VALIDATIONS).then_some(running + 1)
            })
            .ok()?;
        Some(ValidationSlot(Arc::clone(&self.validations)))
    }
}

struct ValidationSlot(Arc<AtomicUsize>);

impl Drop for ValidationSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A file the worker validated, ready for its host handoff.
enum Validated {
    Open(String),
    Preview(PreparedFilePreview),
}

impl WorkspaceManager {
    /// Answers `request` unless `act_by` passes first. `None` answers nothing, because the
    /// client stopped waiting.
    pub(crate) fn answer_desktop_request(
        &mut self,
        request: Request,
        act_by: Instant,
        context: &DesktopRequests,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<Reply>> {
        if Instant::now() >= act_by {
            return Task::ready(None);
        }
        // The person asked this desktop to show work, as their own input in the window would.
        self.person_input();
        match request {
            Request::Show {
                server,
                workspace,
                tab,
                pane,
            } => Task::ready(Some(reply(
                self.presented_workspace(server, workspace, context, cx)
                    .ok_or(Refusal::NotShown)
                    .and_then(|workspace_id| self.show_shared(workspace_id, tab, pane, window, cx)),
            ))),
            Request::Open {
                session,
                path,
                preview,
            } => self.open_local_file(session, path, preview, act_by, context, window, cx),
        }
    }

    /// The local Workspace presenting the server Workspace `workspace` of `server`.
    fn presented_workspace(
        &self,
        server: Id,
        workspace: Id,
        context: &DesktopRequests,
        cx: &App,
    ) -> Option<WorkspaceId> {
        self.shared.iter().find_map(|(key, shared)| {
            let workspace_id = shared.local(workspace)?;
            self.presents_server(key, server, context, cx)
                .then_some(workspace_id)
        })
    }

    fn presents_server(
        &self,
        key: ServerKey,
        server: Id,
        context: &DesktopRequests,
        cx: &App,
    ) -> bool {
        self.shared
            .origin(key)
            .and_then(|origin| (context.server_of)(origin, cx))
            == Some(server)
    }

    /// The Workspace presenting the Session's live Terminal Session, and the local filesystem
    /// authority of its Pane. It reads only this window's state.
    fn local_session(
        &self,
        session: SessionContext,
        context: &DesktopRequests,
        cx: &App,
    ) -> Result<(WorkspaceId, LocalFilesystemAuthority), Refusal> {
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
        if !self.presents_server(key, session.server, context, cx) {
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
        Ok((workspace_id, filesystem))
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

    #[allow(
        clippy::too_many_arguments,
        reason = "one request's facts, deadline and window"
    )]
    fn open_local_file(
        &mut self,
        session: SessionContext,
        path: PathBuf,
        preview: bool,
        act_by: Instant,
        context: &DesktopRequests,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<Reply>> {
        let filesystem = match self.local_session(session, context, cx) {
            Ok((_, filesystem)) => filesystem,
            Err(refusal) => return Task::ready(Some(Reply::Refused(refusal))),
        };
        let Some(slot) = context.validation() else {
            return Task::ready(Some(Reply::Refused(Refusal::Busy)));
        };
        let (validated, received) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("desktop-file".into())
            .spawn(move || {
                let _slot = slot;
                let (release, returned) = std::sync::mpsc::channel();
                let _ = validated.send(validate(&filesystem, &path, preview, release));
                // A prepared preview sends its file back however it ends, presented, refused or
                // expired, so its handle closes here and never on the window's thread. The slot
                // stays held until then.
                while let Ok(target) = returned.recv() {
                    drop(target);
                }
            });
        if worker.is_err() {
            return Task::ready(Some(Reply::Refused(Refusal::Busy)));
        }
        let context = context.clone();
        // The window polls for the worker's answer, so the worker never touches the GPUI
        // executor. A worker blocked past the deadline is left to finish on its own.
        cx.spawn_in(window, async move |manager, cx| {
            let validated = loop {
                match received.try_recv() {
                    Ok(validated) => break validated,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return None,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }
                if Instant::now() >= act_by {
                    return None;
                }
                cx.background_executor().timer(VALIDATION_POLL).await;
            };
            manager
                .update_in(cx, |manager, window, cx| {
                    manager.hand_off(session, validated, act_by, &context, window, cx)
                })
                .ok()?
        })
    }

    /// Hands a validated file to the host, once the Session is still presented.
    fn hand_off(
        &mut self,
        session: SessionContext,
        validated: Result<Validated, Refusal>,
        act_by: Instant,
        context: &DesktopRequests,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Reply> {
        if Instant::now() >= act_by {
            return None;
        }
        let workspace_id = match self.local_session(session, context, cx) {
            Ok((workspace_id, _)) => workspace_id,
            Err(refusal) => return Some(Reply::Refused(refusal)),
        };
        let answer = match validated {
            Err(refusal) => Err(refusal),
            Ok(Validated::Open(url)) => {
                cx.open_url(&url);
                Ok(())
            }
            Ok(Validated::Preview(prepared)) => {
                // File Preview presents for its Pane, so the person sees which Pane asked.
                if let Err(refusal) =
                    self.show_shared(workspace_id, None, Some(session.pane), window, cx)
                {
                    return Some(Reply::Refused(refusal));
                }
                let manager = self
                    .workspaces
                    .workspace(workspace_id)
                    .map(|entry| entry.payload().clone())?;
                if Instant::now() >= act_by {
                    return None;
                }
                // The worker prepared the target, so presenting it touches no file here.
                manager
                    .update(cx, |manager, cx| {
                        manager.preview_shared_local_file(session.pane, prepared, window, cx)
                    })
                    .then_some(())
                    .ok_or(Refusal::PreviewUnavailable)
            }
        };
        Some(reply(answer))
    }
}

fn reply(answer: Result<(), Refusal>) -> Reply {
    match answer {
        Ok(()) => Reply::Done,
        Err(refusal) => Reply::Refused(refusal),
    }
}

/// Validates `path` with the Pane's authority, on a worker thread. Opening the file for its
/// identity proves this account can read it.
fn validate(
    filesystem: &LocalFilesystemAuthority,
    path: &std::path::Path,
    preview: bool,
    release: std::sync::mpsc::Sender<FilePreviewTarget>,
) -> Result<Validated, Refusal> {
    let text = path.to_str().ok_or(Refusal::FileUnavailable)?;
    if !filesystem.path_semantics().is_absolute(path) {
        return Err(Refusal::FileUnavailable);
    }
    let target = filesystem
        .local_file(text, path)
        .and_then(HyperlinkTarget::from_local_file)
        .ok_or(Refusal::FileUnavailable)?;
    if preview {
        return FilePreviewTarget::prepare(
            &target,
            TerminalLocalFileCapabilities::Enabled,
            release,
        )
        .map(Validated::Preview)
        .ok_or(Refusal::FileUnavailable);
    }
    target
        .activation_url(TerminalLocalFileCapabilities::Enabled)
        .map(Validated::Open)
        .ok_or(Refusal::FileUnavailable)
}

/// Answers each request the endpoint accepts on the GPUI main thread, in the window presenting
/// this desktop's shared Workspaces. The endpoint's thread serves one connection at a time, and
/// every connection ends by its own deadlines.
pub(crate) fn serve(
    endpoint: Arc<dyn crate::desktop_channel::DesktopChannelEndpoint>,
    identity: &'static str,
    cx: &mut App,
) {
    struct Exchange {
        request: Request,
        act_by: Instant,
        reply: std::sync::mpsc::SyncSender<Reply>,
    }
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
                let request_by = Instant::now() + crate::desktop_channel::TRANSFER_WINDOW;
                let _ = crate::desktop_channel::serve(
                    stream,
                    identity,
                    request_by,
                    |request, act_by| {
                        let (reply, answer) = std::sync::mpsc::sync_channel(1);
                        requests
                            .send_blocking(Exchange {
                                request,
                                act_by,
                                reply,
                            })
                            .ok()?;
                        answer
                            .recv_timeout(act_by.saturating_duration_since(Instant::now()))
                            .ok()
                    },
                );
            }
        });
    if spawned.is_err() {
        return;
    }
    let context = DesktopRequests::new(Rc::new(presented_server));
    cx.spawn(async move |cx| {
        while let Ok(exchange) = received.recv().await {
            let answer = cx.update(|cx| {
                let Some(window) = crate::ui::shared_workspaces::presenting_window(cx) else {
                    return Task::ready(Some(Reply::Refused(Refusal::NotShown)));
                };
                window
                    .update(cx, |manager, window, cx| {
                        manager.answer_desktop_request(
                            exchange.request,
                            exchange.act_by,
                            &context,
                            window,
                            cx,
                        )
                    })
                    .unwrap_or_else(|_| Task::ready(Some(Reply::Refused(Refusal::NotShown))))
            });
            let reply = exchange.reply;
            // Each answer settles on its own, so a file still being checked holds no other
            // request back.
            cx.spawn(async move |_| {
                if let Some(answer) = answer.await {
                    let _ = reply.send(answer);
                }
            })
            .detach();
        }
    })
    .detach();
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
