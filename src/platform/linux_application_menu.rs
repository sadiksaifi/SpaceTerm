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
    use std::rc::Rc;

    use super::*;
    use crate::platform::window_movement::{
        OperatingSystemWindowDragPlatform, RecordingOperatingSystemWindowDragPlatform,
        WindowMovementFactory,
    };
    use crate::ui::about_window::AboutWindow;

    struct RecordingMovement;

    impl WindowMovementFactory for RecordingMovement {
        fn create(&self) -> Rc<dyn OperatingSystemWindowDragPlatform> {
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default())
        }
    }

    #[gpui::test]
    fn about_opens_spaceterms_own_window_and_help_stays_unavailable(cx: &mut gpui::TestAppContext) {
        let settings = crate::settings::Settings::load(
            crate::settings::storage::testing::MemoryStorage::with_document(
                &crate::settings::SettingsDocument::default(),
            ),
        );
        cx.update(|cx| {
            crate::ui::appearance_runtime::install(
                settings,
                Rc::new(crate::platform::appearance::testing::RecordingAppearancePlatform::default()),
                cx,
            )
            .unwrap();
            crate::ui::init(cx).unwrap();
            crate::ui::about_window::configure_window_chrome(Rc::new(RecordingMovement), cx);
            crate::app::init(
                cx,
                Rc::new(LinuxApplicationMenuAdapter),
                Rc::new(
                    crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
                ),
            )
            .unwrap();
        });
        let about_windows = |cx: &mut gpui::TestAppContext| {
            cx.windows()
                .into_iter()
                .filter(|window| window.downcast::<AboutWindow>().is_some())
                .count()
        };

        // Linux has no native About panel, so About opens SpaceTerm's own, and only once.
        for _ in 0..2 {
            cx.update(|cx| cx.dispatch_action(&crate::app::ShowAboutApplication));
            cx.run_until_parked();
            assert_eq!(about_windows(cx), 1);
        }

        cx.update(|cx| cx.dispatch_action(&crate::app::OpenApplicationHelp));
        cx.run_until_parked();
        assert_eq!(cx.opened_url(), None);
    }
}
