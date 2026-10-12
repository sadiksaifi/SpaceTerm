//! One reusable credential-free spawner per server start, owned by native-handoff B3.

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod boundary {
    use std::{convert::Infallible, os::fd::OwnedFd};

    use crate::{native_deployment::contracts::NativeFailure, shell_launch::PreparedShellLaunch};

    /// Ordinary account tool arguments, environment, directory authority, and intended streams.
    /// The implementation must bound the payload to 256 KiB and exclude custody handles.
    #[expect(dead_code, reason = "native-handoff B3 replaces the unavailable seed")]
    pub(crate) struct BoundedToolSpawn {
        _unavailable: Infallible,
    }

    /// Child ownership and bounded status/signal/reaping through the reusable spawner.
    #[expect(dead_code, reason = "native-handoff B3 replaces the unavailable seed")]
    pub(crate) struct OwnedToolProcess {
        _unavailable: Infallible,
    }

    /// The only process-creation route available to a secret-bearing server.
    #[expect(dead_code, reason = "native-handoff B3 replaces the unavailable seed")]
    pub(crate) trait CredentialFreeSpawn: Send + Sync {
        fn spawn_pty(
            &self,
            launch: PreparedShellLaunch,
            slave: OwnedFd,
        ) -> Result<Box<dyn portable_pty::Child + Send + Sync>, NativeFailure>;

        fn spawn_tool(&self, request: BoundedToolSpawn) -> Result<OwnedToolProcess, NativeFailure>;
    }

    /// Fail-closed route until B3 supplies the anonymous channel and native child ownership.
    #[expect(dead_code, reason = "native-handoff B3 replaces the unavailable seed")]
    pub(crate) struct UnavailableCredentialFreeSpawn;

    impl CredentialFreeSpawn for UnavailableCredentialFreeSpawn {
        fn spawn_pty(
            &self,
            _launch: PreparedShellLaunch,
            _slave: OwnedFd,
        ) -> Result<Box<dyn portable_pty::Child + Send + Sync>, NativeFailure> {
            Err(NativeFailure::NativeUnavailable)
        }

        fn spawn_tool(
            &self,
            _request: BoundedToolSpawn,
        ) -> Result<OwnedToolProcess, NativeFailure> {
            Err(NativeFailure::NativeUnavailable)
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[expect(
    unused_imports,
    reason = "native-handoff B3 consumes these private cross-track ports"
)]
pub(crate) use boundary::{BoundedToolSpawn, CredentialFreeSpawn, OwnedToolProcess};
