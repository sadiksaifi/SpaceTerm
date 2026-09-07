use gpui::App;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApplicationMenuCommand {
    ShowAbout,
    ZoomActiveWindow,
    BringAllWindowsToFront,
    OpenHelp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[cfg_attr(
    test,
    expect(
        dead_code,
        reason = "ordinary tests replace native AppKit effects with the recording adapter"
    )
)]
pub(crate) enum ApplicationMenuError {
    #[error("the application menu command must run on the main thread")]
    OffMainThread,
    #[error("the application menu command is unavailable")]
    Unavailable,
    #[error("the application menu command requires an active Operating-System Window")]
    MissingActiveWindow,
    #[error("the application menu command was rejected")]
    Rejected,
}

/// Host-specific menu structure and native application effects.
pub(crate) trait ApplicationMenuAdapter {
    fn install(&self, cx: &mut App) -> Result<(), ApplicationMenuError>;

    fn perform(&self, command: ApplicationMenuCommand) -> Result<(), ApplicationMenuError>;
}

#[cfg(test)]
pub(crate) mod testing {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default)]
    pub(crate) struct RecordingApplicationMenuAdapter {
        commands: RefCell<Vec<ApplicationMenuCommand>>,
    }

    impl RecordingApplicationMenuAdapter {
        pub(crate) fn commands(&self) -> Vec<ApplicationMenuCommand> {
            self.commands.borrow().clone()
        }
    }

    impl ApplicationMenuAdapter for RecordingApplicationMenuAdapter {
        fn install(&self, cx: &mut App) -> Result<(), ApplicationMenuError> {
            cx.set_menus(Vec::new());
            Ok(())
        }

        fn perform(&self, command: ApplicationMenuCommand) -> Result<(), ApplicationMenuError> {
            self.commands.borrow_mut().push(command);
            Ok(())
        }
    }
}
