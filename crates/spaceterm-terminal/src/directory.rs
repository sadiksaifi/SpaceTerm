use crate::local_filesystem::LocalDirectoryIdentity;
use std::{
    fmt,
    path::{Path, PathBuf},
};
use thiserror::Error;
/// A Terminal Session's directory, retaining its machine boundary.
#[derive(Clone, Eq, PartialEq)]
pub enum CurrentDirectory {
    Local(std::path::PathBuf),
    Remote(RemoteDirectory),
}

impl std::fmt::Debug for CurrentDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Local(_) => "CurrentDirectory::Local",
            Self::Remote(_) => "CurrentDirectory::Remote",
        })
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
/// Validation failures for values that cross the local-to-remote domain boundary.
///
/// Rejected strings must not reach OpenSSH arguments, remote utility commands, or local path APIs.
pub enum RemoteWorkspaceValueError {
    #[error("Remote user must be one non-empty, whitespace-free token")]
    User,
    #[error("SSH destination must be one non-option, control-free token")]
    Destination,
    #[error("Remote Directory must be an absolute or ~/ path without control characters")]
    StartingDirectory,
    #[error("Physical remote directory identity must be an absolute control-free path")]
    DirectoryIdentity,
}

/// The validated account name discovered from a connected remote destination.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RemoteUser(String);

impl fmt::Debug for RemoteUser {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteUser(<redacted>)")
    }
}

impl RemoteUser {
    pub fn new(value: String) -> Result<Self, RemoteWorkspaceValueError> {
        if value.is_empty()
            || value
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(RemoteWorkspaceValueError::User);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The exact OpenSSH destination token selected by the user.
///
/// Equality is spelling-sensitive so different configured aliases remain distinct even when they
/// currently resolve to the same host. The token is passed as one validated OpenSSH argument.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SshDestination(String);

impl fmt::Debug for SshDestination {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SshDestination(<redacted>)")
    }
}

impl SshDestination {
    pub fn new(value: String) -> Result<Self, RemoteWorkspaceValueError> {
        if value.is_empty()
            || value.starts_with('-')
            || value
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(RemoteWorkspaceValueError::Destination);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the machine this destination names, without its account prefix.
    pub fn host(&self) -> &str {
        self.0.rsplit_once('@').map_or(&self.0, |(_, host)| host)
    }
}

/// The exact user-visible spelling of a directory selected on a remote destination.
///
/// This remote value is preserved for display and shell startup. It is not local filesystem
/// authority and must never be converted to `PathBuf` or passed to a local path API.
#[derive(Clone, Eq, PartialEq)]
pub struct RemoteDirectory(String);

impl fmt::Debug for RemoteDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteDirectory(<redacted>)")
    }
}

impl RemoteDirectory {
    pub fn new(value: String) -> Result<Self, RemoteWorkspaceValueError> {
        let supported_form = value.starts_with('/') || value == "~" || value.starts_with("~/");
        if !supported_form || value.chars().any(char::is_control) {
            return Err(RemoteWorkspaceValueError::StartingDirectory);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_home_spelling(&self, home: &RemoteDirectoryIdentity) -> bool {
        let directory = self.0.trim_end_matches('/');
        directory == "~" || directory == home.as_str().trim_end_matches('/')
    }
}

/// The physical absolute directory returned by the remote `pwd -P` validation protocol.
///
/// Its normalized path-like spelling is an opaque remote identity used for connection and pin
/// revalidation. It must never become a local `PathBuf` or local filesystem identity.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RemoteDirectoryIdentity(String);

impl fmt::Debug for RemoteDirectoryIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteDirectoryIdentity(<redacted>)")
    }
}

impl RemoteDirectoryIdentity {
    pub fn new(value: String) -> Result<Self, RemoteWorkspaceValueError> {
        let normalized = value == "/"
            || value.strip_prefix('/').is_some_and(|relative| {
                !relative.is_empty()
                    && relative.split('/').all(|component| {
                        !component.is_empty() && component != "." && component != ".."
                    })
            });
        if !normalized || value.chars().any(char::is_control) {
            return Err(RemoteWorkspaceValueError::DirectoryIdentity);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Eq)]
/// A validated local directory and its local filesystem identity.
///
/// Local launch and pin operations retain this authority when passing directories to native APIs.
pub struct ValidatedLocalDirectory {
    path: PathBuf,
    identity: LocalDirectoryIdentity,
}

impl PartialEq for ValidatedLocalDirectory {
    fn eq(&self, other: &Self) -> bool {
        self.path.as_os_str() == other.path.as_os_str() && self.identity == other.identity
    }
}

impl fmt::Debug for ValidatedLocalDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ValidatedLocalDirectory(<redacted>)")
    }
}

impl ValidatedLocalDirectory {
    pub fn new(path: PathBuf, identity: LocalDirectoryIdentity) -> Self {
        Self { path, identity }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn identity(&self) -> LocalDirectoryIdentity {
        self.identity.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ssh_destination(value: &str) -> SshDestination {
        SshDestination::new(value.to_owned()).unwrap()
    }
    fn remote_directory(value: &str) -> RemoteDirectory {
        RemoteDirectory::new(value.to_owned()).unwrap()
    }
    fn remote_identity(value: &str) -> RemoteDirectoryIdentity {
        RemoteDirectoryIdentity::new(value.to_owned()).unwrap()
    }
    #[test]
    fn remote_values_validate_without_changing_user_visible_spelling() {
        let destination = ssh_destination("root@fedora@orb");
        let directory = remote_directory("~/Projects/Space Term/");
        let identity = remote_identity("/home/root/Projects/Space Term");

        assert_eq!(
            (destination.as_str(), directory.as_str(), identity.as_str(),),
            (
                "root@fedora@orb",
                "~/Projects/Space Term/",
                "/home/root/Projects/Space Term",
            )
        );
        assert!(SshDestination::new("bad destination".to_owned()).is_err());
        assert!(RemoteDirectory::new("relative/path".to_owned()).is_err());
        assert!(RemoteDirectoryIdentity::new("~/not-physical".to_owned()).is_err());
    }

    #[test]
    fn remote_physical_identity_requires_normalized_absolute_form() {
        for invalid in [
            "",
            "relative/path",
            "//srv/project",
            "/srv//project",
            "/srv/./project",
            "/srv/team/../project",
            "/srv/project/",
        ] {
            assert!(
                RemoteDirectoryIdentity::new(invalid.to_owned()).is_err(),
                "{invalid:?} is not normalized `pwd -P` output"
            );
        }

        assert_eq!(remote_identity("/").as_str(), "/");
        assert_eq!(
            remote_identity("/srv/Space Term").as_str(),
            "/srv/Space Term"
        );
    }

    #[test]
    fn ssh_destination_host_should_omit_the_account() {
        let host = |value: &str| {
            SshDestination::new(value.to_owned())
                .unwrap()
                .host()
                .to_owned()
        };

        assert_eq!(host("orb"), "orb");
        assert_eq!(host("tester@orb"), "orb");
        assert_eq!(host("tester@example.com@orb"), "orb");
    }
}
