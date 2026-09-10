use crate::domain::{RemoteDirectoryIdentity, RemoteUser};
use crate::ssh::command::ValidatedRemoteLoginShell;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteWorkspaceAccountError {
    InvalidUser,
    #[cfg(test)]
    InvalidLoginShell,
}

/// Account facts discovered during remote home initialization and reused by manual directory pinning.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct RemoteWorkspaceAccount {
    user: RemoteUser,
    home_identity: RemoteDirectoryIdentity,
    login_shell: ValidatedRemoteLoginShell,
}

impl fmt::Debug for RemoteWorkspaceAccount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteWorkspaceAccount(<redacted>)")
    }
}

impl RemoteWorkspaceAccount {
    #[cfg(test)]
    pub(crate) fn new(
        user: String,
        home_identity: RemoteDirectoryIdentity,
        login_shell: String,
    ) -> Result<Self, RemoteWorkspaceAccountError> {
        let user = RemoteUser::new(user).map_err(|_| RemoteWorkspaceAccountError::InvalidUser)?;
        let login_shell = ValidatedRemoteLoginShell::new(login_shell)
            .map_err(|_| RemoteWorkspaceAccountError::InvalidLoginShell)?;
        Ok(Self {
            user,
            home_identity,
            login_shell,
        })
    }

    pub(crate) fn from_validated_login_shell(
        user: String,
        home_identity: RemoteDirectoryIdentity,
        login_shell: ValidatedRemoteLoginShell,
    ) -> Result<Self, RemoteWorkspaceAccountError> {
        let user = RemoteUser::new(user).map_err(|_| RemoteWorkspaceAccountError::InvalidUser)?;
        Ok(Self {
            user,
            home_identity,
            login_shell,
        })
    }

    pub(crate) fn user(&self) -> &str {
        self.user.as_str()
    }

    pub(crate) const fn remote_user(&self) -> &RemoteUser {
        &self.user
    }

    pub(crate) const fn home_identity(&self) -> &RemoteDirectoryIdentity {
        &self.home_identity
    }

    pub(crate) const fn login_shell(&self) -> &ValidatedRemoteLoginShell {
        &self.login_shell
    }
}
