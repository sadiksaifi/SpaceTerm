//! Linux desktops present no global application menu.
use gpui::App;

use super::application_menu::{ApplicationMenuAdapter, ApplicationMenuError};
use super::linux_desktop_events::LinuxDesktopEvents;
use crate::application_identity::ApplicationIdentity;

pub(super) struct LinuxApplicationMenuAdapter {
    identity: ApplicationIdentity,
    events: LinuxDesktopEvents,
}

impl LinuxApplicationMenuAdapter {
    pub(super) fn new(identity: ApplicationIdentity, events: LinuxDesktopEvents) -> Self {
        Self { identity, events }
    }
}

impl ApplicationMenuAdapter for LinuxApplicationMenuAdapter {
    /// Names the application to the desktop shell, which titles its notifications with it.
    fn install(&self, cx: &mut App) -> Result<(), ApplicationMenuError> {
        cx.set_app_identity(self.identity.application_id(), self.identity.display_name());
        self.events.install(cx);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn linux_menu_has_no_about_or_help_surface(cx: &mut gpui::TestAppContext) {
        use super::super::application_menu::ApplicationMenuCommand;
        let (_, events) = LinuxDesktopEvents::new();
        let adapter = LinuxApplicationMenuAdapter::new(ApplicationIdentity::current(), events);
        for command in [
            ApplicationMenuCommand::ShowAbout,
            ApplicationMenuCommand::OpenHelp,
        ] {
            assert_eq!(
                cx.update(|cx| adapter.perform(command, cx)),
                Err(ApplicationMenuError::Unavailable)
            );
        }
        assert_eq!(cx.opened_url(), None);
    }
}
