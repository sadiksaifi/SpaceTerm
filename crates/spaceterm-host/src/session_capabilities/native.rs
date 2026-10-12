//! Read-only native session observation, composed with the account owner's provider.

use super::{CapabilityFailure, GuiSessionFacts, GuiSessionObserver};
use crate::native_deployment::{account::NativeAccountProvider, contracts::AccountBuildKey};

#[cfg(unix)]
mod endpoints;
#[cfg(unix)]
pub use endpoints::{NativeSessionLocations, SystemSessionConnector, SystemSessionLocations};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

/// The OS boundary returns one current graphical-session incarnation, not environment values.
pub trait NativeGuiSessionPeer: Send + Sync {
    /// An inactive, logged-out, or another account's session returns `None`.
    fn observe(&self, account_uid: u32) -> Result<Option<[u8; 16]>, CapabilityFailure>;
}

/// Native console/audit-session observation on macOS and logind observation on Linux.
#[derive(Default)]
pub struct SystemNativeGuiSessionPeer {
    #[cfg(target_os = "linux")]
    state: std::sync::Mutex<Option<linux::SessionReference>>,
}
impl NativeGuiSessionPeer for SystemNativeGuiSessionPeer {
    fn observe(&self, account_uid: u32) -> Result<Option<[u8; 16]>, CapabilityFailure> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if account_uid == 0
            || unsafe { libc::getuid() } != account_uid
            || unsafe { libc::geteuid() } != account_uid
        {
            return Ok(None);
        }
        #[cfg(target_os = "macos")]
        {
            macos::observe(account_uid)
        }
        #[cfg(target_os = "linux")]
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| CapabilityFailure::NativeUnavailable)?;
            linux::observe(account_uid, &mut state)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = account_uid;
            Err(CapabilityFailure::NativeUnavailable)
        }
    }
}

/// Binds a native session to the same independently observed account incarnation.
/// This provider observes facts only. It cannot enable deployment or credential authority.
pub struct SystemGuiSessionObserver<A, N = SystemNativeGuiSessionPeer> {
    account: A,
    key: AccountBuildKey,
    native: N,
}
impl<A: NativeAccountProvider> SystemGuiSessionObserver<A> {
    /// Production composition supplies the account owner's native incarnation provider.
    pub fn new(account: A, key: AccountBuildKey) -> Self {
        Self {
            account,
            key,
            native: SystemNativeGuiSessionPeer::default(),
        }
    }
}
impl<A: NativeAccountProvider, N: NativeGuiSessionPeer> SystemGuiSessionObserver<A, N> {
    /// Constructor injection at the native session boundary, never a runtime test switch.
    pub fn with_native_peer(account: A, key: AccountBuildKey, native: N) -> Self {
        Self {
            account,
            key,
            native,
        }
    }
}
impl<A: NativeAccountProvider, N: NativeGuiSessionPeer> GuiSessionObserver
    for SystemGuiSessionObserver<A, N>
{
    fn observe(&self) -> Result<Option<GuiSessionFacts>, CapabilityFailure> {
        let before = self
            .account
            .observe(self.key)
            .map_err(|_| CapabilityFailure::NativeUnavailable)?;
        if before.key() != self.key {
            return Err(CapabilityFailure::StaleAssessment);
        }
        let Some(incarnation) = self.native.observe(self.key.account_uid)? else {
            return Ok(None);
        };
        let after = self
            .account
            .observe(self.key)
            .map_err(|_| CapabilityFailure::NativeUnavailable)?;
        if before.compare(&after).is_err()
            || self.native.observe(self.key.account_uid)? != Some(incarnation)
        {
            return Err(CapabilityFailure::StaleAssessment);
        }
        GuiSessionFacts::new(before.sha256(), incarnation).map(Some)
    }
}
