pub(crate) mod askpass;
pub(crate) mod cancellation;
pub(crate) mod command;
pub(crate) mod control_connection;
pub(crate) mod destination;
pub(crate) mod host_config;
pub(crate) mod live_connection;
#[cfg(feature = "desktop")]
pub(crate) mod managed_hosts;
pub(crate) mod process;
pub(crate) mod startup_environment;

#[cfg(feature = "desktop")]
pub(crate) mod server_connection;
#[cfg(feature = "desktop")]
pub(crate) mod server_intents;
#[cfg(feature = "desktop")]
pub(crate) mod server_maintenance;
#[cfg(feature = "desktop")]
pub(crate) mod server_operation_records;
#[cfg(feature = "desktop")]
pub(crate) mod server_operations;
#[cfg(feature = "desktop")]
pub(crate) mod server_process;

#[cfg(feature = "desktop")]
mod quiet_continuation;
