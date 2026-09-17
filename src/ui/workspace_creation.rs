//! Canonical descriptors for the Workspace creation rows.
//!
//! The sidebar footer menu and the Workspace switcher offer the same two rows
//! (Local Workspace / Remote Workspace). Labels, icons, and shortcuts shared
//! here cannot drift apart when one surface changes.

use gpui::SharedString;
use spaceterm_ui::CustomIconName;

/// Canonical label for the Local creation row on both surfaces.
pub(crate) const LOCAL_WORKSPACE_LABEL: &str = "Local Workspace";
/// Canonical label for the Remote creation row on both surfaces.
pub(crate) const REMOTE_WORKSPACE_LABEL: &str = "Remote Workspace";
/// Leading icon for the Local creation row on both surfaces.
pub(crate) const LOCAL_WORKSPACE_ICON: CustomIconName = CustomIconName::RectangleStackBadgePlus;
/// Leading icon for the Remote creation row on both surfaces.
pub(crate) const REMOTE_WORKSPACE_ICON: CustomIconName = CustomIconName::GlobePlus;

/// Trigger tooltip for the sidebar creation menu.
///
/// The menu trigger stays enabled while Remote creation is unavailable (Local
/// creation still works), so the tooltip keeps the trigger name and appends
/// the backend reason instead of replacing it.
pub(crate) fn new_workspace_trigger_tooltip(remote_unavailable: Option<&str>) -> SharedString {
    match remote_unavailable {
        Some(reason) => format!("New Workspace: {reason}").into(),
        None => "New Workspace".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_tooltip_should_surface_remote_unavailable_reason() {
        assert_eq!(
            new_workspace_trigger_tooltip(None).to_string(),
            "New Workspace"
        );
        let reason = "OpenSSH 8.2 or later is required";
        let tooltip = new_workspace_trigger_tooltip(Some(reason)).to_string();
        assert!(
            tooltip.contains("New Workspace"),
            "tooltip should keep trigger context, got {tooltip:?}"
        );
        assert!(
            tooltip.contains(reason),
            "tooltip should surface the reason, got {tooltip:?}"
        );
    }
}
