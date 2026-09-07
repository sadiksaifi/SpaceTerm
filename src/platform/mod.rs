pub(crate) mod app_paths;
pub(crate) mod application_activity;
pub(crate) mod application_menu;
pub(crate) mod control_socket;
pub(crate) mod secure_filesystem;
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

pub(crate) mod locale;

#[cfg(all(test, target_os = "macos", feature = "macos-native-tests"))]
#[path = "macos_adapter_tests/mod.rs"]
pub(crate) mod macos_adapter_tests;

#[cfg(test)]
pub(crate) mod testing;
