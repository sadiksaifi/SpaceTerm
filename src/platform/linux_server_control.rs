//! The control-only desktop mode the Linux status item opens: one window for one server operation.
//!
//! It composes only appearance, fonts, and window chrome. It never opens a Workspace Window,
//! claims the application's single-instance name, checks for updates, or launches the server.
use crate::application_identity::ApplicationIdentity;
use crate::ui::server_control_window::{self, Operation};
use spaceterm_server::service::{Service, control::Controller};
use std::{rc::Rc, sync::Arc};

pub(crate) const ARGUMENT: &str = "--server-control";

/// Runs `--server-control start|stop|restart` and returns the process exit status.
pub(crate) fn main(argument: &std::ffi::OsStr) -> i32 {
    let Some(operation) = argument.to_str().and_then(Operation::parse) else {
        eprintln!("Usage: spaceterm {ARGUMENT} start|stop|restart");
        return 2;
    };
    let identity = ApplicationIdentity::current();
    let controller = match Service::for_identity(identity.directory_name()) {
        Ok(service) => Controller::new(service),
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    // The service's current state decides what the window asks, before anything is drawn.
    let Some(step) = server_control_window::first_step(&controller, operation) else {
        return 0;
    };
    let Ok(directories) =
        super::app_directories::AppDirectories::resolve(identity.directory_name())
    else {
        eprintln!("SpaceTerm could not find its settings.");
        return 1;
    };
    let Ok(paths) = super::app_paths::AppPaths::from_directories(
        directories,
        super::unix_local_socket::LOCAL_IPC_PATH_MAXIMUM,
        Arc::new(super::unix_secure_filesystem::UnixSecureFilesystem),
    ) else {
        eprintln!("SpaceTerm could not find its settings.");
        return 1;
    };
    let storage: Arc<dyn crate::settings::storage::SettingsStorage> = Arc::new(
        crate::settings::storage::ConfigSettingsStorage::new(Arc::new(paths)),
    );
    let bus = super::linux_session_bus::SessionBus::connect().ok();
    let appearance: Rc<dyn crate::platform::appearance::AppearancePlatform> = Rc::new(
        super::linux_appearance::LinuxAppearancePlatform::new(bus.clone()),
    );
    let controls = super::linux_window_style::capture(
        bus.as_ref(),
        &super::app_directories::DesktopResourceDirectories::capture(),
    );
    let window_frame =
        super::window_frame::WindowFrameGeometry::new(Some(controls.style.corner_radius()))
            .with_outer_edge_width(1.0);
    let window_chrome = super::window_chrome::WindowChrome::client().with_controls(controls);
    let window_movement: Rc<dyn super::window_movement::WindowMovementFactory> =
        Rc::new(super::linux_window_drag::LinuxWindowMovementFactory);
    let failed = Rc::new(std::cell::Cell::new(false));
    let reported = Rc::clone(&failed);
    gpui_platform::application()
        .with_assets(spaceterm_ui::EmbeddedAssets)
        .run(move |cx| {
            cx.set_app_identity(identity.application_id(), identity.display_name());
            cx.set_global(window_frame);
            cx.set_global(window_chrome.clone());
            window_chrome.install_controls(cx);
            let settings = crate::settings::Settings::load(storage);
            let composed = crate::ui::appearance_runtime::register_fonts(cx)
                .map_err(drop)
                .and_then(|()| {
                    crate::ui::appearance_runtime::install(settings, appearance, cx).map_err(drop)
                })
                .and_then(|()| crate::ui::initialize_controls(cx).map_err(drop))
                .and_then(|()| {
                    server_control_window::open(controller, operation, step, window_movement, cx)
                        .map_err(drop)
                });
            if composed.is_err() {
                reported.set(true);
                cx.quit();
                return;
            }
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
        });
    if failed.get() {
        eprintln!("SpaceTerm could not open the server control window.");
        return 1;
    }
    0
}
