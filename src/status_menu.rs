//! The client's native status item: the desktop's own update status and every Machine's
//! SpaceTerm Core status and actions, in the menu bar or status area, owned by the same process as
//! the client's windows. Each independently supervised Core keeps running whether the client is
//! windowed, menu-only, exited, or crashed.
mod controller;
pub(crate) mod model;
#[cfg(test)]
mod tests;

pub(crate) use controller::{Alert, StatusHost, StatusMenu};

use gpui::{App, Global};
use spaceterm_core::service::ServiceStatus;
use std::rc::Rc;

/// What the client's window lifecycle needs from its status item.
#[derive(Default)]
pub(crate) struct StatusArea {
    /// Whether the session shows the item. Without it, a client with no windows would be
    /// unreachable, so closing every window exits the client instead.
    available: bool,
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
/// that can reach the client, so the client exits through its quit policy.
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

/// A status-surface operation succeeded. A server it left running reaches every Workspace Window
/// still waiting on it, as a Start from Settings does. Nothing here opens a window.
pub(crate) fn operation_succeeded(cx: &mut App, status: ServiceStatus) {
    crate::ui::settings_window::local_operations::LocalCoreOperations::changed_elsewhere(
        cx, status,
    );
}
