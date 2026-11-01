#[cfg(feature = "desktop")]
mod about;
#[cfg(feature = "desktop")]
mod app;
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        unused_imports,
        reason = "desktop policy shares types with the headless runtime"
    )
)]
mod appearance;
#[cfg(feature = "desktop")]
mod application_identity;
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        unused_imports,
        reason = "desktop policy shares types with the headless runtime"
    )
)]
mod background_image;
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        unused_imports,
        reason = "desktop policy shares types with the headless runtime"
    )
)]
mod bundled_font;
mod cancellation;
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        unused_imports,
        reason = "desktop policy shares types with the headless runtime"
    )
)]
mod close_confirmation;
#[cfg(feature = "desktop")]
mod command_line_tool;
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        unused_imports,
        reason = "desktop policy shares types with the headless runtime"
    )
)]
mod domain;
#[cfg(feature = "desktop")]
mod host_fonts;
#[cfg(feature = "desktop")]
mod invitation_links;
#[cfg(feature = "desktop")]
mod keybindings;
mod platform;
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        unused_imports,
        reason = "shared Git policy is composed by the headless runtime"
    )
)]
mod repository_status;
#[cfg(feature = "desktop")]
mod server_context;
mod server_settings;
#[cfg(feature = "desktop")]
mod settings;
#[cfg(feature = "desktop")]
mod ssh;
#[cfg(feature = "desktop")]
mod status_menu;
mod terminal;
#[cfg(feature = "desktop")]
mod theme_registry;
#[cfg(feature = "desktop")]
mod ui;
#[cfg(feature = "desktop")]
mod updates;
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        unused_imports,
        reason = "shared Git policy is composed by the headless runtime"
    )
)]
mod worktrees;

#[cfg(feature = "desktop")]
mod desktop_profile;

/// The account-local channel from `spaceterm` to the desktop on this machine.
pub mod desktop_channel;

#[cfg(test)]
#[cfg(feature = "desktop")]
mod architecture_tests;

#[cfg(test)]
#[cfg(feature = "desktop")]
mod allocation_count;

#[cfg(feature = "desktop")]
use spaceterm_connections as direct_connections;
#[cfg(feature = "desktop")]
mod direct_connection_storage;
#[cfg(feature = "desktop")]
mod directory_selection;
mod local_path;
#[cfg(feature = "desktop")]
pub mod local_startup;

/// Terminal runtime contracts shared by graphical and headless hosts.
pub mod runtime;

#[cfg(feature = "desktop")]
pub(crate) use spaceterm_server as server_connection;
