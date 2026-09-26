#[cfg(not(test))]
use objc2::MainThreadMarker;
#[cfg(not(test))]
use objc2_app_kit::NSApplication;

#[cfg(not(test))]
pub(crate) fn is_active() -> bool {
    MainThreadMarker::new().is_some_and(|mtm| NSApplication::sharedApplication(mtm).isActive())
}

#[cfg(test)]
pub(crate) const fn is_active() -> bool {
    true
}

pub(crate) struct MacosApplicationActivity;

impl super::application_activity::ApplicationActivity for MacosApplicationActivity {
    fn is_active(&self, _: &gpui::App) -> bool {
        is_active()
    }
}
