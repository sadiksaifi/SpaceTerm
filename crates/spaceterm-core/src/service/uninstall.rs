//! Uninstalling SpaceTerm Core from this machine: the CLI's `uninstall` and the service lane's
//! Uninstall for Remove Machine.
//!
//! Uninstall removes the account service, the managed CLI launcher and every installed version.
//! With [`Data::Keep`] the data root stays: Workspace organization, Machine Settings, pairings and
//! the CLI's saved machines. With [`Data::Delete`] the data root goes too, still under the service
//! lock. Remove Machine keeps the data and revokes every pairing.
use super::*;

/// What uninstall does with the data root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Data {
    Keep,
    Delete,
}

/// An uninstall preview carries only bounded service effects and a nonsecret approval token.
#[cfg(feature = "client")]
#[derive(Debug)]
pub enum CliUninstall {
    Preview {
        token: String,
        expires_at: u64,
        sessions: Vec<crate::hierarchy::Id>,
    },
    Completed,
}

/// A preview that always waits for confirmation, even while SpaceTerm Core is stopped.
#[cfg(feature = "client")]
#[derive(Debug)]
pub struct UninstallPreview {
    pub token: String,
    pub expires_at: u64,
    pub sessions: Vec<crate::hierarchy::Id>,
}

/// What one uninstall call asks for.
#[cfg(feature = "client")]
#[derive(Clone, Copy)]
enum Request<'a> {
    /// Preview only, whether or not SpaceTerm Core runs.
    Preview,
    /// Uninstall with the token of an earlier preview.
    Confirm(&'a str),
}

#[cfg(feature = "client")]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UninstallConfirmation {
    token: String,
    expires_at: u64,
    observation: String,
}

/// How long a preview stays confirmable.
#[cfg(feature = "client")]
const PREVIEW_SECONDS: u64 = 600;

#[cfg(feature = "client")]
fn token() -> Result<String, CoreError> {
    Ok(format!(
        "c1-{}",
        crate::protocol::random()?
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

/// Whether `token` has the form [`token`] issues.
#[cfg(feature = "client")]
fn issued(token: &str) -> bool {
    token.strip_prefix("c1-").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

#[cfg(feature = "client")]
fn now() -> Result<u64, CoreError> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| CoreError::StorageUnavailable)?
        .as_secs())
}

/// The live Terminal Session IDs, read under the server's service lock. With `fence`, the
/// server refuses every later Session start until `link` closes.
#[cfg(feature = "client")]
fn sessions(
    link: &crate::connection::link::CoreLink,
    fence: bool,
) -> Result<Vec<crate::hierarchy::Id>, CoreError> {
    if !matches!(link.health(), crate::connection::link::Health::Live) {
        return Err(CoreError::Changed);
    }
    match link
        .request(crate::protocol::Operation::Sessions { fence })
        .map_err(|_| CoreError::Changed)?
        .wait()
    {
        Ok(crate::protocol::ReplyBody::Sessions(mut sessions)) => {
            sessions.sort_unstable();
            Ok(sessions)
        }
        Ok(_) => Err(CoreError::Protocol),
        Err(_) => Err(CoreError::Changed),
    }
}

impl Service {
    /// Preview an uninstall, or confirm its exact service, data choice and live Session
    /// observation with the token of an earlier preview. Without a token it only previews, also
    /// while SpaceTerm Core is stopped. A preview is retained for ten minutes. Neither preview
    /// nor retention starts a server.
    #[cfg(feature = "client")]
    pub fn uninstall_cli(
        &self,
        confirmation: Option<&str>,
        choice: Data,
    ) -> Result<CliUninstall, CoreError> {
        self.uninstall_cli_forgetting(confirmation, choice, || Ok(()))
    }

    /// [`Self::uninstall_cli`], where a confirmed [`Data::Delete`] first runs `forget`: it
    /// removes what the data root's records name outside the root, such as credential items.
    /// `forget` runs under the service lock once the confirmation holds, before anything is
    /// removed. When it fails, nothing is removed and its error is returned, so the same
    /// confirmation, or a later one, finds the same records and can finish.
    #[cfg(feature = "client")]
    pub fn uninstall_cli_forgetting<E: From<CoreError>>(
        &self,
        confirmation: Option<&str>,
        choice: Data,
        forget: impl FnOnce() -> Result<(), E>,
    ) -> Result<CliUninstall, E> {
        let request = confirmation.map_or(Request::Preview, Request::Confirm);
        let forget = || match choice {
            Data::Delete => forget(),
            Data::Keep => Ok(()),
        };
        self.uninstall_with(request, choice, forget)
    }

    /// Preview an uninstall without removing anything, whether or not SpaceTerm Core runs. The
    /// desktop confirms first and then passes the token to [`Self::uninstall_cli`]. Any change
    /// to the service, its data choice or its live Sessions in between fails that call with
    /// `Changed`.
    #[cfg(feature = "client")]
    pub fn preview_uninstall(&self, choice: Data) -> Result<UninstallPreview, CoreError> {
        match self.uninstall_with(Request::Preview, choice, || Ok::<_, CoreError>(()))? {
            CliUninstall::Preview {
                token,
                expires_at,
                sessions,
            } => Ok(UninstallPreview {
                token,
                expires_at,
                sessions,
            }),
            CliUninstall::Completed => Err(CoreError::Protocol),
        }
    }

    #[cfg(feature = "client")]
    fn uninstall_with<E: From<CoreError>>(
        &self,
        request: Request<'_>,
        choice: Data,
        forget: impl FnOnce() -> Result<(), E>,
    ) -> Result<CliUninstall, E> {
        // Admission waits for the service lock, so establish this passive link first.
        let initial = self.passive_status()?;
        let link = if initial.running {
            Some(
                crate::MachineConnection::for_cli(&self.identity, crate::CliRoute::Local, None)?
                    .link()?,
            )
        } else {
            None
        };
        let data = match Directory::open(&self.paths.data, false) {
            Ok(data) => data,
            // Nothing was ever set up. Only a launcher can remain. A confirmation then needs
            // no retained preview, only a token a preview could have issued: if setup happens
            // in between, the confirmation finds no preview there.
            Err(CoreError::NotSetUp) if matches!(request, Request::Preview) => {
                return Ok(CliUninstall::Preview {
                    token: token()?,
                    expires_at: now()? + PREVIEW_SECONDS,
                    sessions: Vec::new(),
                });
            }
            Err(CoreError::NotSetUp) => {
                if let Request::Confirm(token) = request
                    && !issued(token)
                {
                    return Err(CoreError::Changed.into());
                }
                forget()?;
                self.remove_files()?;
                return Ok(CliUninstall::Completed);
            }
            Err(error) => return Err(error.into()),
        };
        // The CLI shares its state root while it uninstalls. Waiting here could hold off an
        // activation whose candidate migrates that state before it is ready.
        let operation = data
            .try_lock("service.lock")?
            .ok_or(CoreError::ServiceBusy)?;
        data.verify_path(&self.paths.data)?;
        let state = Self::state(&data)?;
        let status = self.passive_status()?;
        if status.running != initial.running {
            return Err(CoreError::Changed.into());
        }
        // A confirmation fences Session starts in the same step that reads the Sessions it
        // compares, and `link` holds that fence until the removal below finished.
        let sessions = match &link {
            Some(link) => sessions(link, matches!(request, Request::Confirm(_)))?,
            None => Vec::new(),
        };
        let observation =
            activation::confirmation_token(&(&self.identity, &state, &status, &sessions, choice))?;
        let now = now()?;
        if let Request::Confirm(token) = request {
            let preview = data
                .read_json::<UninstallConfirmation>("uninstall.json")?
                .ok_or(CoreError::Changed)?;
            if preview.token != token
                || preview.observation != observation
                || now > preview.expires_at
            {
                return Err(CoreError::Changed.into());
            }
            // Before the preview is spent, so the same confirmation can retry a failed `forget`.
            forget()?;
            data.remove(c"uninstall.json")?;
        } else {
            let token = token()?;
            let expires_at = now + PREVIEW_SECONDS;
            data.write_json(
                c"uninstall.json",
                &UninstallConfirmation {
                    token: token.clone(),
                    expires_at,
                    observation,
                },
            )?;
            return Ok(CliUninstall::Preview {
                token,
                expires_at,
                sessions,
            });
        }
        self.remove_locked(&data, state.is_some(), status.running)?;
        drop(link);
        // The root goes while the service lock is still held, so no setup that waits for the
        // lock can publish into it first.
        if choice == Data::Delete {
            self.remove_data_root()?;
        }
        drop(operation);
        drop(data);
        Ok(CliUninstall::Completed)
    }

    /// Remove Machine's uninstall, run by the helper after it stopped the Core: revokes every
    /// pairing, then removes the service, the launcher and every installed version. The data
    /// root stays.
    pub(super) fn remove_machine(&self) -> Result<(), CoreError> {
        let data = Directory::open(&self.paths.data, false)?;
        let _operation = data.lock("service.lock", false)?;
        data.verify_path(&self.paths.data)?;
        crate::direct::forget(&data)?;
        let installed = Self::state(&data)?.is_some();
        self.remove_locked(&data, installed, false)
    }

    /// The service lock must be held. Removes the registration, the launcher, the installed
    /// versions and the service record, in that order, so a partial failure leaves a service
    /// that a repeated uninstall finishes.
    fn remove_locked(
        &self,
        data: &Directory,
        installed: bool,
        running: bool,
    ) -> Result<(), CoreError> {
        if installed {
            self.apply_control_locked(data, control::Action::Uninstall, Some(running))?;
        }
        self.remove_files()?;
        data.remove(c"service.json")
    }

    /// Removes the managed launcher, then the installed versions.
    fn remove_files(&self) -> Result<(), CoreError> {
        let installations = self.paths.data.join("installations");
        runtime::launcher::remove(self.cli_home()?, &self.identity, &installations)?;
        match std::fs::symlink_metadata(&installations) {
            Ok(metadata) if metadata.is_dir() => {
                std::fs::remove_dir_all(&installations).map_err(|_| CoreError::StorageUnavailable)
            }
            Ok(_) => Err(CoreError::UnsafeStorage),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(CoreError::StorageUnavailable),
        }
    }

    /// Deletes the data root. The caller holds the service lock inside it.
    #[cfg(feature = "client")]
    fn remove_data_root(&self) -> Result<(), CoreError> {
        match std::fs::symlink_metadata(&self.paths.data) {
            Ok(metadata) if metadata.is_dir() => {
                std::fs::remove_dir_all(&self.paths.data).map_err(|_| CoreError::StorageUnavailable)
            }
            Ok(_) => Err(CoreError::UnsafeStorage),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(CoreError::StorageUnavailable),
        }
    }
}
