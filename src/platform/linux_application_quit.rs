//! Linux quit requests run the portable quit policy before GPUI ends the application.
use std::cell::RefCell;

use gpui::App;

use super::application_quit::{
    ApplicationQuitAdapter, ApplicationQuitDecision, ApplicationQuitError, ApplicationQuitHandler,
};

#[derive(Default)]
pub(super) struct LinuxApplicationQuitAdapter {
    handler: RefCell<Option<ApplicationQuitHandler>>,
}

impl ApplicationQuitAdapter for LinuxApplicationQuitAdapter {
    fn last_window_policy(&self) -> super::application_quit::LastWindowPolicy {
        super::application_quit::LastWindowPolicy::Quit
    }

    fn install(&self, handler: ApplicationQuitHandler) -> Result<(), ApplicationQuitError> {
        let mut installed = self.handler.borrow_mut();
        if installed.is_some() {
            return Err(ApplicationQuitError::AlreadyInstalled);
        }
        *installed = Some(handler);
        Ok(())
    }

    fn request_quit(&self, cx: &mut App) {
        let handler = self.handler.borrow().clone();
        let decision = handler.map_or(ApplicationQuitDecision::Proceed, |handler| {
            handler.handle(cx)
        });
        if decision == ApplicationQuitDecision::Proceed {
            cx.quit();
        }
    }

    fn confirm_quit(&self, cx: &mut App) {
        cx.quit();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    #[gpui::test]
    fn linux_quit_consults_the_installed_handler_once(cx: &mut gpui::TestAppContext) {
        let adapter = LinuxApplicationQuitAdapter::default();
        let requests = Rc::new(Cell::new(0));
        cx.update(|cx| {
            let counted = Rc::clone(&requests);
            adapter
                .install(ApplicationQuitHandler::new(
                    cx,
                    Rc::new(move |_| {
                        counted.set(counted.get() + 1);
                        ApplicationQuitDecision::Cancel
                    }),
                ))
                .unwrap();
            adapter.request_quit(cx);
            assert_eq!(
                adapter.install(ApplicationQuitHandler::new(
                    cx,
                    Rc::new(|_| ApplicationQuitDecision::Proceed)
                )),
                Err(ApplicationQuitError::AlreadyInstalled)
            );
        });
        assert_eq!(requests.get(), 1);
    }
}
