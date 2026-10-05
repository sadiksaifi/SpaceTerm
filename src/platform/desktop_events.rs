use gpui::App;

/// Installs application identity and foreground delivery of native desktop callbacks.
pub(crate) trait DesktopEventAdapter {
    fn install(&self, cx: &mut App);
}
