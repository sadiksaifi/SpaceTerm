//! Adapts the existing window-owned, zeroized AskPass broker to native SSH tunnels.

use crate::{
    platform::app_paths::AppPaths,
    ssh::askpass::{AskPassAttemptFactory, AskPassBrokerLease, AskPassWindowFactory},
};
use spaceterm_host::{
    ssh_tunnel::{AskPassHandoff, TunnelAskPass},
    transport::TransportError,
};
use std::sync::{Arc, Mutex};

pub(crate) struct DesktopTunnelAskPass {
    paths: Arc<AppPaths>,
    windows: Arc<dyn AskPassWindowFactory>,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    attempts: Option<Arc<dyn AskPassAttemptFactory>>,
    broker: Option<AskPassBrokerLease>,
}
impl DesktopTunnelAskPass {
    pub(crate) fn new(paths: Arc<AppPaths>, windows: Arc<dyn AskPassWindowFactory>) -> Arc<Self> {
        Arc::new(Self {
            paths,
            windows,
            state: Mutex::new(State::default()),
        })
    }
    pub(crate) fn bind_window(&self, window: &gpui::Window, cx: &mut gpui::App) {
        if let Ok(mut state) = self.state.lock() {
            state.broker = None;
            state.attempts = self.windows.create(window, cx).ok();
        }
    }
}
impl TunnelAskPass for DesktopTunnelAskPass {
    fn handoff(&self) -> Result<AskPassHandoff, TransportError> {
        let mut state = self.state.lock().map_err(|_| TransportError::Unavailable)?;
        if state.broker.is_none() {
            let factory = state.attempts.as_ref().ok_or(TransportError::Unavailable)?;
            let attempt = factory
                .start_attempt(&self.paths)
                .map_err(|_| TransportError::Unavailable)?;
            state.broker = Some(attempt.lease);
        }
        let broker = state.broker.as_ref().ok_or(TransportError::Unavailable)?;
        let mut helper = None;
        let mut endpoint = None;
        for (name, value) in broker.ordinary_spawn_entries() {
            match name {
                "SSH_ASKPASS" => helper = Some(std::path::PathBuf::from(value)),
                "SPACETERM_SSH_ASKPASS_SOCKET" => endpoint = Some(value.to_owned()),
                _ => {}
            }
        }
        let capability = broker.capability_copy_for_spawn();
        AskPassHandoff::new(
            helper.ok_or(TransportError::Unavailable)?,
            endpoint.ok_or(TransportError::Unavailable)?,
            zeroize::Zeroizing::new(capability.as_bytes().to_vec()),
        )
    }
}
