mod app;
mod domain;
mod platform;
mod ssh;
mod terminal;
mod theme;
mod ui;

use gpui::{App, Application};

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
    Application::new()
        .with_assets(spaceterm_ui::ControlAssets)
        .run(move |cx: &mut App| {
            ui::init(cx);
            app::init(cx);
            app::open(cx, startup);
        });
    if let Err(error) = platform::acceptance_observation::finish_runtime_observation() {
        eprintln!("acceptance runtime observation did not complete: {error}");
    }
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

    use super::dispatch_or_prepare_application;

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
