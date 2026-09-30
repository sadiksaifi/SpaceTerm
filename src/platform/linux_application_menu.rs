//! Linux desktops present no global application menu; menu-only commands are reached elsewhere.
use gpui::App;

use super::application_menu::{ApplicationMenuAdapter, ApplicationMenuCommand, ApplicationMenuError};
use crate::application_identity::ApplicationIdentity;

pub(super) struct LinuxApplicationMenuAdapter {
    identity: ApplicationIdentity,
}

impl LinuxApplicationMenuAdapter {
    pub(super) const fn new(identity: ApplicationIdentity) -> Self {
        Self { identity }
    }
}

impl ApplicationMenuAdapter for LinuxApplicationMenuAdapter {
    /// Names the application to the desktop shell, which titles its notifications with it.
    fn install(&self, cx: &mut App) -> Result<(), ApplicationMenuError> {
        cx.set_app_identity(self.identity.application_id(), self.identity.display_name());
        Ok(())
    }

    fn perform(&self, _: ApplicationMenuCommand) -> Result<(), ApplicationMenuError> {
        Err(ApplicationMenuError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_menu_commands_report_unavailable() {
        for command in [
            ApplicationMenuCommand::ShowAbout,
            ApplicationMenuCommand::ZoomActiveWindow,
            ApplicationMenuCommand::BringAllWindowsToFront,
            ApplicationMenuCommand::OpenHelp,
        ] {
            assert_eq!(
                LinuxApplicationMenuAdapter::new(ApplicationIdentity::current()).perform(command),
                Err(ApplicationMenuError::Unavailable)
            );
        }
    }
}
