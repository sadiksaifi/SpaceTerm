//! Linux desktops present no global application menu.
use gpui::App;

use super::application_menu::{ApplicationMenuAdapter, ApplicationMenuError};
pub(super) struct LinuxApplicationMenuAdapter;

impl ApplicationMenuAdapter for LinuxApplicationMenuAdapter {
    fn install(&self, _: &mut App) -> Result<(), ApplicationMenuError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn linux_menu_has_no_about_or_help_surface(cx: &mut gpui::TestAppContext) {
        use super::super::application_menu::ApplicationMenuCommand;
        let adapter = LinuxApplicationMenuAdapter;
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
