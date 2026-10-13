//! Shortcut-free actions for server-backed Pane presentation.

use gpui::actions;

// Register a focused-Pane handler only while Take Over is offered and armed. Native menus use
// handler availability; the handler must still validate the current Pane token before acting.
actions!(terminal, [TakeOverPane]);

/// Commands delivered by a Takeover Screen's context menu, never by terminal input.
/// Both retry commands reconnect only, without Session startup, server startup, or bootstrap.
/// The Pane owner maps these to its presentation requests and validates current authority.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the desktop Pane owner installs the presentation menu"
    )
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PanePresentationMenuCommand {
    TakeOver,
    Retry,
    RetryConnection,
    ServerSettings,
}
