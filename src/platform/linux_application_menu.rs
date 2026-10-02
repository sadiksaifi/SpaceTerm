//! Linux desktops present no global application menu; menu-only commands are reached elsewhere.
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

    fn uses_command_palette(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn linux_menu_exposes_help_through_the_portable_adapter(cx: &mut gpui::TestAppContext) {
        let (_, events) = LinuxDesktopEvents::new();
        let adapter = LinuxApplicationMenuAdapter::new(ApplicationIdentity::current(), events);
        cx.update(|cx| {
            adapter.perform(
                super::super::application_menu::ApplicationMenuCommand::OpenHelp,
                cx,
            )
        })
        .unwrap();
        assert_eq!(
            cx.opened_url().as_deref(),
            Some(crate::platform::application_menu_model::HELP_URL)
        );
    }
}
