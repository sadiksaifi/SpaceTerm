mod app;
mod domain;
mod platform;
mod ssh;
mod terminal;
mod theme;
mod ui;

use std::{cell::RefCell, rc::Rc};

use gpui::{App, Application};

const MAX_INITIALIZATION_ERROR_CHARACTERS: usize = 512;

fn main() {
    let startup = match dispatch_or_prepare_application(
        platform::macos_askpass_transport::dispatch_helper_from_environment,
        app::StartupDependencies::capture,
    ) {
        Ok(Ok(startup)) => startup,
        Ok(Err(error)) => {
            eprintln!("failed to resolve SpaceTerm startup paths: {error}");
            std::process::exit(2);
        }
        Err(exit_code) => std::process::exit(exit_code),
    };
    if let Err(error) = platform::acceptance_observation::configure_from_environment() {
        eprintln!("failed to configure acceptance observation: {error}");
        std::process::exit(2);
    }
    let initialization_error = Rc::new(RefCell::new(None));
    let reported_initialization_error = Rc::clone(&initialization_error);
    Application::new().run(move |cx: &mut App| {
        if let Err(error) = initialize_application(cx, ui::init, |cx| {
            app::init(cx);
            app::open(cx, startup);
        }) {
            *reported_initialization_error.borrow_mut() =
                Some(bounded_initialization_error(&error));
            cx.quit();
        }
    });
    if let Some(error) = initialization_error.borrow_mut().take() {
        eprintln!("failed to initialize SpaceTerm UI: {error}");
        std::process::exit(2);
    }
    if let Err(error) = platform::acceptance_observation::finish_runtime_observation() {
        eprintln!("acceptance runtime observation did not complete: {error}");
    }
}

fn bounded_initialization_error(error: &impl std::fmt::Display) -> String {
    let message = error.to_string();
    if message.chars().count() <= MAX_INITIALIZATION_ERROR_CHARACTERS {
        return message;
    }

    message
        .chars()
        .take(MAX_INITIALIZATION_ERROR_CHARACTERS - 1)
        .chain(std::iter::once('…'))
        .collect()
}

fn initialize_application<T, E>(
    state: &mut T,
    initialize_ui: impl FnOnce(&mut T) -> Result<(), E>,
    open_application: impl FnOnce(&mut T),
) -> Result<(), E> {
    initialize_ui(state)?;
    open_application(state);
    Ok(())
}

fn dispatch_or_prepare_application<T>(
    dispatch_helper: impl FnOnce() -> Option<i32>,
    prepare_application: impl FnOnce() -> T,
) -> Result<T, i32> {
    match dispatch_helper() {
        Some(exit_code) => Err(exit_code),
        None => Ok(prepare_application()),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::{
        MAX_INITIALIZATION_ERROR_CHARACTERS, bounded_initialization_error,
        dispatch_or_prepare_application, initialize_application,
    };

    #[test]
    fn ui_initialization_failure_should_prevent_opening_an_operating_system_window() {
        let mut window_opened = false;

        let result = initialize_application(
            &mut window_opened,
            |_| Err::<(), _>("font registration failed"),
            |window_opened| *window_opened = true,
        );

        assert_eq!(result, Err("font registration failed"));
        assert!(!window_opened);
    }

    #[test]
    fn ui_initialization_error_report_should_be_bounded() {
        let message = bounded_initialization_error(&"x".repeat(1_024));

        assert_eq!(message.chars().count(), MAX_INITIALIZATION_ERROR_CHARACTERS);
        assert!(message.ends_with('…'));
    }

    #[test]
    fn askpass_helper_dispatch_should_precede_and_bypass_application_startup_capture() {
        let calls = RefCell::new(Vec::new());

        let outcome = dispatch_or_prepare_application(
            || {
                calls.borrow_mut().push("helper");
                Some(17)
            },
            || {
                calls.borrow_mut().push("capture");
                "application"
            },
        );

        assert_eq!(outcome, Err(17));
        assert_eq!(calls.into_inner(), ["helper"]);
    }

    #[test]
    fn application_startup_should_capture_once_after_helper_declines_dispatch() {
        let calls = RefCell::new(Vec::new());

        let outcome = dispatch_or_prepare_application(
            || {
                calls.borrow_mut().push("helper");
                None
            },
            || {
                calls.borrow_mut().push("capture");
                "application"
            },
        );

        assert_eq!(outcome, Ok("application"));
        assert_eq!(calls.into_inner(), ["helper", "capture"]);
    }
}
