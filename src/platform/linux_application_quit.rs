//! Linux quit requests run the portable quit policy before GPUI ends the application.
use std::cell::RefCell;

use gpui::App;

use super::application_quit::{
    ApplicationQuitAdapter, ApplicationQuitDecision, ApplicationQuitError, ApplicationQuitHandler,
};

pub(super) struct LinuxApplicationQuitAdapter {
    handler: RefCell<Option<ApplicationQuitHandler>>,
    quit: Box<dyn Fn(&mut App)>,
}

impl LinuxApplicationQuitAdapter {
    pub(super) fn new(quit: impl Fn(&mut App) + 'static) -> Self {
        Self {
            handler: RefCell::new(None),
            quit: Box::new(quit),
        }
    }
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
            (self.quit)(cx);
        }
    }

    fn confirm_quit(&self, cx: &mut App) {
        (self.quit)(cx);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    fn recording_adapter() -> (LinuxApplicationQuitAdapter, Rc<Cell<bool>>) {
        let quit = Rc::new(Cell::new(false));
        let recorded = Rc::clone(&quit);
        (
            LinuxApplicationQuitAdapter::new(move |_| recorded.set(true)),
            quit,
        )
    }

    #[gpui::test]
    fn linux_quit_cancel_keeps_the_application_running(cx: &mut gpui::TestAppContext) {
        let (adapter, quit) = recording_adapter();
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
        assert!(!quit.get());
    }

    #[gpui::test]
    fn linux_quit_proceed_quits_the_application(cx: &mut gpui::TestAppContext) {
        let (adapter, quit) = recording_adapter();
        let requests = Rc::new(Cell::new(0));
        cx.update(|cx| {
            let counted = Rc::clone(&requests);
            adapter
                .install(ApplicationQuitHandler::new(
                    cx,
                    Rc::new(move |_| {
                        counted.set(counted.get() + 1);
                        ApplicationQuitDecision::Proceed
                    }),
                ))
                .unwrap();
            adapter.request_quit(cx);
        });
        assert_eq!(requests.get(), 1);
        assert!(quit.get());
    }

    #[gpui::test]
    fn linux_quit_confirmation_quits_without_consulting_the_handler(cx: &mut gpui::TestAppContext) {
        let (adapter, quit) = recording_adapter();
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
            adapter.confirm_quit(cx);
        });
        assert_eq!(requests.get(), 0);
        assert!(quit.get());
    }

    #[gpui::test]
    fn linux_quit_without_a_handler_quits_the_application(cx: &mut gpui::TestAppContext) {
        let (adapter, quit) = recording_adapter();
        cx.update(|cx| adapter.request_quit(cx));
        assert!(quit.get());
    }
}
