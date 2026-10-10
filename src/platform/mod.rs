pub(crate) mod app_directories;
pub(crate) mod app_paths;
pub(crate) mod appearance;
pub(crate) mod application_activity;
pub(crate) mod application_menu;
#[cfg(any(target_os = "macos", test))]
pub(crate) mod application_menu_model;
pub(crate) mod application_quit;
pub(crate) mod control_socket;
pub(crate) mod desktop_events;
pub(crate) mod https_transport;
pub(crate) mod keyboard_layout;
#[cfg(target_os = "macos")]
mod macos_appearance;
#[cfg(target_os = "macos")]
mod macos_keyboard_layout;
#[cfg(target_os = "macos")]
mod macos_quick_look_window;
#[cfg(target_os = "macos")]
pub(crate) mod macos_updates;
pub(crate) mod microphone_access;
pub(crate) mod permission_access;
pub(crate) mod repository_watch;
pub(crate) mod secure_filesystem;
pub(crate) mod selected_file;
pub(crate) mod settings_file;
pub(crate) mod setup_guide_host;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod unix_selected_file;
pub(crate) mod window_chrome;
pub(crate) mod window_frame;
pub(crate) mod window_visibility;

#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "selected by Linux composition and portable accessibility tests"
    )
)]
pub(crate) mod accesskit_terminal_accessibility;
pub(crate) mod local_filesystem;
pub(crate) mod native_pty;
pub(crate) mod terminal_accessibility;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_attention;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_attention;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_application;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_application;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_application_menu;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_application_menu;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_application_quit;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_application_quit;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_permission_access;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_permission_access;
#[cfg(target_os = "macos")]
mod macos_permission_probe;
#[cfg(target_os = "macos")]
mod macos_setup_guide_host;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_accessibility;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_accessibility;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_keyboard;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_keyboard;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_locale;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_locale;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_microphone_access;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_microphone_access;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_notification;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_notification;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_pasteboard;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_pasteboard;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_quick_look;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_quick_look;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_render_lifecycle;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_render_lifecycle;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_secure_input;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_secure_input;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_scroll;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_scroll;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_services;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_services;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_system_settings;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_system_settings;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_window_drag;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_window_drag;

#[cfg(target_os = "macos")]
mod macos_image_inspector;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_window_backdrop;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_window_backdrop;

#[cfg(target_os = "macos")]
mod macos_window_frame;

pub(crate) mod shell_integration;
pub(crate) mod shell_launch;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) mod launch_host;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("SpaceTerm supports macOS and Linux only");

#[cfg(target_os = "macos")]
mod macos_composition;
pub(crate) mod permission_recovery;
pub(crate) mod services_registration;
pub(crate) mod window_movement;
#[cfg(target_os = "macos")]
pub(crate) use macos_composition::main;
#[cfg(target_os = "linux")]
mod linux_composition;
#[cfg(all(target_os = "linux", not(test)))]
mod linux_fonts;
#[cfg(all(target_os = "linux", test))]
pub(crate) mod linux_fonts;
#[cfg(target_os = "linux")]
pub(crate) use linux_composition::main;
/// Runs a helper role this process was started for, returning its exit code.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn dispatch_helper_from_environment() -> Option<i32> {
    let result = unix_askpass_transport::dispatch_helper_from_environment();
    #[cfg(target_os = "macos")]
    let result = result.or_else(macos_permission_probe::dispatch_probe_from_environment);
    result
}

pub(crate) mod locale;

#[cfg(all(test, target_os = "macos", feature = "native-tests"))]
#[path = "macos_adapter_tests/mod.rs"]
pub(crate) mod macos_adapter_tests;

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux"),
    feature = "native-tests"
))]
#[path = "unix_adapter_tests/mod.rs"]
pub(crate) mod unix_adapter_tests;

#[cfg_attr(
    all(target_os = "macos", not(feature = "native-tests")),
    allow(dead_code, reason = "only macOS native tests start a peer process")
)]
#[cfg(all(any(target_os = "macos", target_os = "linux"), test))]
mod unix_peer_credentials_tests;

#[cfg(test)]
pub(crate) mod testing;

#[cfg(all(test, target_os = "macos", feature = "native-tests"))]
#[allow(dead_code)]
pub(crate) mod native_main_thread_tests;

#[cfg(target_os = "macos")]
mod macos_repository_tools;
#[cfg(target_os = "macos")]
mod macos_reserved_shortcuts;
#[cfg(target_os = "macos")]
mod macos_shortcut_glyphs;

// POSIX mechanics shared by macOS and Linux.
#[cfg(all(any(target_os = "macos", target_os = "linux"), not(test)))]
mod unix_askpass_transport;
#[cfg(all(any(target_os = "macos", target_os = "linux"), test))]
pub(crate) mod unix_askpass_transport;

#[cfg(all(any(target_os = "macos", target_os = "linux"), not(test)))]
mod unix_host_config_filesystem;
#[cfg(all(any(target_os = "macos", target_os = "linux"), test))]
pub(crate) mod unix_host_config_filesystem;

#[cfg(all(any(target_os = "macos", target_os = "linux"), not(test)))]
mod unix_local_identity;
#[cfg(all(any(target_os = "macos", target_os = "linux"), test))]
pub(crate) mod unix_local_identity;

#[cfg(all(any(target_os = "macos", target_os = "linux"), not(test)))]
mod unix_local_socket;
#[cfg(all(any(target_os = "macos", target_os = "linux"), test))]
pub(crate) mod unix_local_socket;

#[cfg(all(any(target_os = "macos", target_os = "linux"), not(test)))]
mod unix_pty;
#[cfg(all(any(target_os = "macos", target_os = "linux"), test))]
pub(crate) mod unix_pty;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) mod repository_marker_reader;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod repository_status_host;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) mod unix_repository_program;

#[cfg(all(any(target_os = "macos", target_os = "linux"), not(test)))]
mod unix_secure_filesystem;
#[cfg(all(any(target_os = "macos", target_os = "linux"), test))]
pub(crate) mod unix_secure_filesystem;

#[cfg(all(any(target_os = "macos", target_os = "linux"), not(test)))]
mod unix_ssh_process;
#[cfg(all(any(target_os = "macos", target_os = "linux"), test))]
pub(crate) mod unix_ssh_process;

// Irreducible macOS differences inside the shared POSIX mechanics.
#[cfg(target_os = "macos")]
mod macos_atomic_rename;

#[cfg(target_os = "macos")]
mod macos_peer_credentials;

#[cfg(target_os = "macos")]
mod macos_pty_host;

// Linux host adapters.
#[cfg(target_os = "linux")]
mod linux_appearance;

#[cfg(target_os = "linux")]
mod linux_application;

#[cfg(target_os = "linux")]
mod linux_application_menu;

#[cfg(target_os = "linux")]
mod linux_application_quit;

#[cfg(target_os = "linux")]
mod linux_atomic_rename;

#[cfg(target_os = "linux")]
mod linux_attention;

#[cfg(target_os = "linux")]
mod linux_clipboard;

#[cfg(target_os = "linux")]
mod linux_default_keymap;

#[cfg(target_os = "linux")]
mod linux_file_preview;

#[cfg(target_os = "linux")]
mod linux_keyboard;

#[cfg(target_os = "linux")]
mod linux_keycodes;

#[cfg(target_os = "linux")]
mod linux_keyboard_layout;

#[cfg(target_os = "linux")]
mod linux_locale;

#[cfg(target_os = "linux")]
mod linux_notification;

#[cfg(target_os = "linux")]
mod linux_peer_credentials;

#[cfg(target_os = "linux")]
mod linux_pty_host;

#[cfg(target_os = "linux")]
mod linux_repository_tools;

#[cfg(target_os = "linux")]
mod linux_reserved_shortcuts;

#[cfg(target_os = "linux")]
mod linux_scroll;

#[cfg(target_os = "linux")]
mod linux_shortcut_text;

#[cfg(target_os = "linux")]
mod linux_window_drag;

#[cfg(target_os = "linux")]
mod linux_window_style;
#[cfg(target_os = "linux")]
mod linux_window_visibility;

#[cfg(target_os = "linux")]
mod linux_application_instance;
#[cfg(target_os = "linux")]
mod linux_desktop_events;
#[cfg(target_os = "linux")]
mod linux_session_bus;

#[cfg(target_os = "linux")]
mod linux_ssh_executable;

#[cfg(target_os = "linux")]
mod linux_updates;
