//! Application activity from GPUI's Operating-System Window focus facts.
use gpui::App;

/// Linux has no application-level activation state, so SpaceTerm is active while one of its
/// Operating-System Windows holds keyboard focus.
pub(super) struct LinuxApplicationActivity;

impl super::application_activity::ApplicationActivity for LinuxApplicationActivity {
    fn is_active(&self, cx: &App) -> bool {
        cx.active_window().is_some()
    }
}
