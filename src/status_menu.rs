//! The client's native status item: this build's server state and controls in the menu bar or
//! status area, owned by the same process as the client's windows. The independently supervised
//! server keeps running whether the client is windowed, menu-only, exited, or crashed.
pub(crate) mod model;
pub(crate) mod service_changes;
pub(crate) mod worker;

use gpui::{App, Global};
use spaceterm_server::service::ServiceStatus;
use std::rc::Rc;

/// Work deferred until no admitted server operation is running.
type AfterOperations = Box<dyn FnOnce(&mut App)>;

/// What the client's window lifecycle needs from its status item.
#[derive(Default)]
pub(crate) struct StatusArea {
    /// Whether the session shows the item. Without it, a client with no windows would be
    /// unreachable, so closing every window exits the client instead.
    available: bool,
    /// Server operations admitted from the status item and not yet finished.
    operations: usize,
    /// Work waiting for every admitted operation to finish, such as exiting the client.
    after_operations: Vec<AfterOperations>,
    /// Shows or removes the client's Dock presence, where the platform has one.
    dock: Option<Rc<dyn Fn(bool)>>,
}

impl Global for StatusArea {}

fn update(cx: &mut App, change: impl FnOnce(&mut StatusArea)) {
    change(cx.default_global::<StatusArea>());
}

/// Whether the session currently shows the client's status item.
pub(crate) fn available(cx: &App) -> bool {
    cx.try_global::<StatusArea>()
        .is_some_and(|status| status.available)
}

/// Records whether the session shows the item. Losing it while no window is open leaves nothing
/// that can reach the client, so the client exits through its quit policy, which waits for any
/// admitted operation.
pub(crate) fn set_available(cx: &mut App, available: bool) {
    let lost = {
        let status = cx.default_global::<StatusArea>();
        let lost = status.available && !available;
        status.available = available;
        lost
    };
    if lost && cx.windows().is_empty() {
        cx.dispatch_action(&crate::app::QuitApplication);
    }
}

/// Installs the platform's Dock presence switch. Only a platform with a Dock has one.
#[cfg_attr(
    not(any(target_os = "macos", test)),
    allow(dead_code, reason = "only macOS has a Dock presence to switch")
)]
pub(crate) fn set_dock(cx: &mut App, dock: Rc<dyn Fn(bool)>) {
    update(cx, |status| status.dock = Some(dock));
}

/// Shows or removes the client's Dock presence. The client and its status item keep running.
pub(crate) fn show_in_dock(cx: &mut App, visible: bool) {
    if let Some(dock) = cx
        .try_global::<StatusArea>()
        .and_then(|status| status.dock.clone())
    {
        dock(visible);
    }
}

/// Records a server operation the person admitted. Exiting waits until it finishes.
pub(crate) fn begin_operation(cx: &mut App) {
    update(cx, |status| status.operations += 1);
}

pub(crate) fn end_operation(cx: &mut App) {
    let waiting = {
        let status = cx.default_global::<StatusArea>();
        status.operations = status.operations.saturating_sub(1);
        if status.operations > 0 {
            return;
        }
        std::mem::take(&mut status.after_operations)
    };
    for work in waiting {
        work(cx);
    }
}

/// Whether a server operation the person admitted is still running.
pub(crate) fn operations_running(cx: &App) -> bool {
    cx.try_global::<StatusArea>()
        .is_some_and(|status| status.operations > 0)
}

/// Runs `work` once no admitted server operation is running, so exiting never abandons one.
pub(crate) fn after_operations(cx: &mut App, work: impl FnOnce(&mut App) + 'static) {
    if operations_running(cx) {
        update(cx, |status| status.after_operations.push(Box::new(work)));
    } else {
        work(cx);
    }
}

/// Subscribes `changes` to this client's link to its local server, now or once the client
/// creates that link's handle. The link reports a server this client used ending or admitting it
/// again. Without a link, the worker's [`service_changes`] watch reports the same changes.
pub(crate) fn follow_local_server(
    changes: std::sync::Weak<dyn spaceterm_server::connection::link::LinkSink>,
    cx: &mut App,
) {
    use crate::ui::server_worktrees::Client;
    if let Some(client) = cx.try_global::<Client>() {
        client.server.subscribe(changes);
        return;
    }
    let mut pending = Some(changes);
    cx.observe_global::<Client>(move |cx| {
        if let Some(changes) = pending.take() {
            cx.global::<Client>().server.subscribe(changes);
        }
    })
    .detach();
}

/// A status-surface operation succeeded. A server it left running reaches every Workspace Window
/// still waiting on it, as a Start from Settings does. Nothing here opens a window.
pub(crate) fn operation_succeeded(cx: &mut App, status: ServiceStatus) {
    crate::ui::settings_window::local_operations::LocalServerOperations::changed_elsewhere(
        cx, status,
    );
}
