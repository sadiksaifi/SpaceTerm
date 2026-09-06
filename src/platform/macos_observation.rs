//! Acceptance capability discovery occurs before any startup environment is captured.
use crate::observation::{
    AcceptanceObservationError, AuthenticatedObservation, clean_acceptance_environment,
};
use std::{env, path::PathBuf, sync::Arc};
const SOCKET_ENV: &str = "SPACETERM_ACCEPTANCE_SOCKET";
pub(crate) fn discover() -> Result<Option<AuthenticatedObservation>, AcceptanceObservationError> {
    let Some(socket) = take_socket_environment()? else {
        return Ok(None);
    };
    sanitize_acceptance_process_environment()?;
    let package = super::macos_observation_package::capture()?;
    let mut transport = super::macos_observation_transport::connect(&socket)?;
    let authentication = package.authenticate(&mut transport)?;
    AuthenticatedObservation::configure(
        Box::new(transport),
        authentication,
        Box::new(package),
        Arc::new(super::macos_observation_clock::ContinuousClock),
    )
    .map(Some)
}
fn sanitize_acceptance_process_environment() -> Result<(), AcceptanceObservationError> {
    let current = env::vars_os().collect::<Vec<_>>();
    let clean = clean_acceptance_environment(
        &super::macos_observation_environment::PrivateDirectories,
        current.iter().cloned(),
    )?;
    // SAFETY: discovery runs in host composition, before GPUI or any
    // application worker thread exists. Replacing the environment here cannot race another thread.
    unsafe {
        for (key, _) in &current {
            env::remove_var(key);
        }
        for (key, value) in clean {
            env::set_var(key, value);
        }
    }
    Ok(())
}

fn take_socket_environment() -> Result<Option<PathBuf>, AcceptanceObservationError> {
    let value = env::var_os(SOCKET_ENV);
    // This runs at the beginning of main, before GPUI creates worker threads. The socket name is
    // never inherited by the Shell Process, and the connected descriptor is close-on-exec.
    unsafe {
        env::remove_var(SOCKET_ENV);
    }
    value
        .map(|value| {
            value
                .into_string()
                .map(PathBuf::from)
                .map_err(|_| AcceptanceObservationError::InvalidSocket)
        })
        .transpose()
}
