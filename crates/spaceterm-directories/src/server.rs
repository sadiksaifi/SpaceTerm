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

/// The account-private directory holding the desktop's channel endpoint. Like the server's
/// runtime directory it ignores launch environments, so a shell the server starts finds the
/// desktop the person launched.
pub fn desktop_channel_directory(uid: u32, identity: &str) -> Result<PathBuf, DirectoryError> {
    let server = ServerDirectories::for_account(Path::new("/"), uid, identity)?;
    Ok(server
        .runtime
        .with_file_name(format!("{identity}-desktop-{uid}")))
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

    #[test]
    fn desktop_channel_directory_is_per_account_and_identity_beside_the_server_runtime() {
        let development = desktop_channel_directory(501, "spaceterm-development").unwrap();
        let server =
            ServerDirectories::for_account(Path::new("/home/owner"), 501, "spaceterm-development")
                .unwrap();
        assert_eq!(development.parent(), server.runtime.parent());
        assert_ne!(development, server.runtime);
        assert_ne!(
            development,
            desktop_channel_directory(501, "spaceterm").unwrap()
        );
        assert_ne!(
            development,
            desktop_channel_directory(502, "spaceterm-development").unwrap()
        );
        assert!(desktop_channel_directory(501, "other").is_err());
    }
}
