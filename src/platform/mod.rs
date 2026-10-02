pub(crate) mod app_directories;
pub(crate) mod app_paths;
pub(crate) mod appearance;
pub(crate) mod application_activity;
pub(crate) mod application_menu;
pub(crate) mod application_quit;
pub(crate) mod computer_use_access;
pub(crate) mod control_socket;
pub(crate) mod https_transport;
pub(crate) mod keyboard_layout;
#[cfg(target_os = "macos")]
mod macos_appearance;
#[cfg(target_os = "macos")]
mod macos_keyboard_layout;
#[cfg(target_os = "macos")]
mod macos_quick_look_window;
#[cfg(target_os = "macos")]
mod macos_selected_file;
#[cfg(target_os = "macos")]
pub(crate) mod macos_updates;
pub(crate) mod microphone_access;
pub(crate) mod secure_filesystem;
pub(crate) mod selected_file;
pub(crate) mod settings_file;
pub(crate) mod setup_guide_host;
pub(crate) mod window_frame;
pub(crate) mod window_visibility;

pub(crate) mod local_filesystem;
#[cfg(target_os = "macos")]
mod macos_local_identity;
pub(crate) mod native_pty;
pub(crate) mod terminal_accessibility;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_ssh_process;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_ssh_process;

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
mod macos_computer_use_access;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_computer_use_access;
#[cfg(target_os = "macos")]
mod macos_computer_use_probe;
#[cfg(target_os = "macos")]
mod macos_setup_guide_host;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_control_socket;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_control_socket;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_host_config_filesystem;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_host_config_filesystem;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_secure_filesystem;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_secure_filesystem;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_accessibility;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_accessibility;

pub(crate) mod ssh_askpass;

#[cfg(all(target_os = "macos", not(test)))]
mod macos_askpass_transport;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_askpass_transport;

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
mod macos_pty;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_pty;

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

#[cfg(all(target_os = "macos", not(test)))]
mod macos_window_backdrop;
#[cfg(all(target_os = "macos", test))]
pub(crate) mod macos_window_backdrop;

#[cfg(target_os = "macos")]
mod macos_window_frame;

pub(crate) mod shell_integration;
pub(crate) mod shell_launch;

#[cfg(target_os = "macos")]
pub(crate) mod launch_host;

#[cfg(not(target_os = "macos"))]
compile_error!("SpaceTerm currently supports macOS only");

pub(crate) mod askpass;
#[cfg(target_os = "macos")]
mod macos_composition;
pub(crate) mod permission_recovery;
pub(crate) mod services_registration;
pub(crate) mod window_movement;
#[cfg(target_os = "macos")]
pub(crate) use macos_composition::main;

/// Runs a helper role this process was started for, returning its exit code.
#[cfg(target_os = "macos")]
pub(crate) fn dispatch_helper_from_environment() -> Option<i32> {
    macos_askpass_transport::dispatch_helper_from_environment()
        .or_else(macos_computer_use_probe::dispatch_probe_from_environment)
}

pub(crate) mod locale;

#[cfg(all(test, target_os = "macos", feature = "macos-native-tests"))]
#[path = "macos_adapter_tests/mod.rs"]
pub(crate) mod macos_adapter_tests;

#[cfg(test)]
pub(crate) mod testing;

#[cfg(all(test, target_os = "macos", feature = "macos-native-tests"))]
#[allow(dead_code)]
pub(crate) mod native_main_thread_tests;

#[cfg(target_os = "macos")]
mod macos_reserved_shortcuts;
#[cfg(target_os = "macos")]
mod macos_shortcut_glyphs;
