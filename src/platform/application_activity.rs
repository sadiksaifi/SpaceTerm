use gpui::App;

/// Application activity is independent of which product window owns the keyboard.
pub(crate) trait ApplicationActivity {
    fn is_active(&self, cx: &App) -> bool;
}

#[cfg(test)]
pub(crate) struct TestApplicationActivity;

#[cfg(test)]
impl ApplicationActivity for TestApplicationActivity {
    fn is_active(&self, cx: &App) -> bool {
        cx.active_window().is_some()
    }
}
