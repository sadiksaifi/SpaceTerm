//! Account-wide server discovery is independent of launch environments and installation paths.
use std::path::{Path, PathBuf};

use crate::DirectoryError;

#[derive(Clone)]
pub struct ServerDirectories {
    pub data: PathBuf,
    pub runtime: PathBuf,
}

impl ServerDirectories {
    /// Use account-home XDG defaults for durable server state. Environment overrides remain
    /// client-specific: a service and desktop must resolve the same identity and endpoint.
    pub fn for_account(home: &Path, uid: u32, identity: &str) -> Result<Self, DirectoryError> {
        if !matches!(
            identity,
            "spaceterm" | "spaceterm-preflight" | "spaceterm-development"
        ) || !home.is_absolute()
        {
            return Err(DirectoryError::InvalidAppName);
        }
        #[cfg(target_os = "macos")]
        let temporary = Path::new("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let temporary = Path::new("/tmp");
        Ok(Self {
            data: home.join(".local/share").join(identity).join("server"),
            runtime: temporary.join(format!("{identity}-server-{uid}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn isolates_accounts_and_build_identities_without_launch_environment() {
        let development =
            ServerDirectories::for_account(Path::new("/home/owner"), 501, "spaceterm-development")
                .unwrap();
        let release =
            ServerDirectories::for_account(Path::new("/home/owner"), 501, "spaceterm").unwrap();
        let other =
            ServerDirectories::for_account(Path::new("/home/owner"), 502, "spaceterm-development")
                .unwrap();
        assert_ne!(development.data, release.data);
        assert_ne!(development.runtime, release.runtime);
        assert_ne!(development.runtime, other.runtime);
    }
}
