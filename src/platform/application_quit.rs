use std::rc::Rc;

use gpui::{App, AsyncApp};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApplicationQuitDecision {
    Proceed,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ApplicationQuitError {
    #[error("the application quit handler is already installed")]
    AlreadyInstalled,
    #[error("the application quit handler must run on the main thread")]
    OffMainThread,
    #[error("the native application delegate is unavailable")]
    DelegateUnavailable,
    #[error("the native application delegate already owns termination policy")]
    DelegateConflict,
    #[error("the application quit handler could not be retained")]
    HandlerUnavailable,
}

#[derive(Clone)]
pub(crate) struct ApplicationQuitHandler {
    app: AsyncApp,
    request: Rc<dyn Fn(&mut App) -> ApplicationQuitDecision>,
}

impl ApplicationQuitHandler {
    pub(crate) fn new(cx: &App, request: Rc<dyn Fn(&mut App) -> ApplicationQuitDecision>) -> Self {
        Self {
            app: cx.to_async(),
            request,
        }
    }

    pub(crate) fn handle(&self, cx: &mut App) -> ApplicationQuitDecision {
        (self.request)(cx)
    }

    pub(crate) fn handle_native(&self) -> ApplicationQuitDecision {
        self.app
            .update(|cx| self.handle(cx))
            .unwrap_or(ApplicationQuitDecision::Cancel)
    }
}

/// Connects native application termination requests to portable quit policy.
pub(crate) trait ApplicationQuitAdapter {
    fn install(&self, handler: ApplicationQuitHandler) -> Result<(), ApplicationQuitError>;

    fn request_quit(&self, cx: &mut App);

    fn confirm_quit(&self, cx: &mut App);
}

#[cfg(test)]
pub(crate) mod testing {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[derive(Default)]
    pub(crate) struct RecordingApplicationQuitAdapter {
        handler: RefCell<Option<ApplicationQuitHandler>>,
        requests: Rc<Cell<usize>>,
        confirmations: Rc<Cell<usize>>,
    }

    impl RecordingApplicationQuitAdapter {
        pub(crate) fn requests(&self) -> usize {
            self.requests.get()
        }

        pub(crate) fn confirmations(&self) -> usize {
            self.confirmations.get()
        }

        pub(crate) fn simulate_native_request(&self) -> ApplicationQuitDecision {
            self.dispatch_request()
        }

        fn dispatch_request(&self) -> ApplicationQuitDecision {
            self.requests.set(self.requests.get() + 1);
            self.handler
                .borrow()
                .as_ref()
                .map_or(ApplicationQuitDecision::Proceed, |handler| {
                    handler.handle_native()
                })
        }
    }

    impl ApplicationQuitAdapter for RecordingApplicationQuitAdapter {
        fn install(&self, handler: ApplicationQuitHandler) -> Result<(), ApplicationQuitError> {
            let mut installed = self.handler.borrow_mut();
            if installed.is_some() {
                return Err(ApplicationQuitError::AlreadyInstalled);
            }
            *installed = Some(handler);
            Ok(())
        }

        fn request_quit(&self, cx: &mut App) {
            let requests = Rc::clone(&self.requests);
            let handler = self.handler.borrow().clone();
            cx.defer(move |cx| {
                requests.set(requests.get() + 1);
                if let Some(handler) = handler {
                    handler.handle(cx);
                }
            });
        }

        fn confirm_quit(&self, _: &mut App) {
            self.confirmations.set(self.confirmations.get() + 1);
        }
    }
}
