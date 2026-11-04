//! Uninstalling SpaceTerm Core from this machine: the CLI's `uninstall` and the service lane's
//! Uninstall for Remove Machine.
//!
//! Uninstall removes the account service, the managed CLI launcher and every installed version.
//! With [`Data::Keep`] the data root stays: Workspace organization, Machine Settings, pairings and
//! the CLI's saved machines. With [`Data::Delete`] the data root goes too. Remove Machine keeps
//! the data and revokes every pairing.
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

#[cfg(feature = "client")]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UninstallConfirmation {
    token: String,
    expires_at: u64,
    observation: String,
}

impl Service {
    /// Preview an uninstall or confirm its exact service, data choice and live Session
    /// observation. A preview is retained for ten minutes. Neither preview nor retention starts
    /// a server. A stopped Core uninstalls without a preview.
    #[cfg(feature = "client")]
    pub fn uninstall_cli(
        &self,
        confirmation: Option<&str>,
        choice: Data,
    ) -> Result<CliUninstall, CoreError> {
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
            // Nothing was ever set up. Only a launcher can remain.
            Err(CoreError::NotSetUp) => {
                self.remove_files()?;
                return Ok(CliUninstall::Completed);
            }
            Err(error) => return Err(error),
        };
        let operation = data.lock("service.lock", false)?;
        data.verify_path(&self.paths.data)?;
        let state = Self::state(&data)?;
        let status = self.passive_status()?;
        if status.running != initial.running {
            return Err(CoreError::Changed);
        }
        let sessions = if let Some(link) = link {
            if !matches!(link.health(), crate::connection::link::Health::Live) {
                return Err(CoreError::Changed);
            }
            let (_, revision) = link.next_inventory(0).map_err(|_| CoreError::Protocol)?;
            let close = revision
                .inventory
                .close
                .as_ref()
                .ok_or(CoreError::Protocol)?;
            close
                .sessions
                .iter()
                .filter_map(|pane| pane.session)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let observation =
            activation::confirmation_token(&(&self.identity, &state, &status, &sessions, choice))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| CoreError::StorageUnavailable)?
            .as_secs();
        if let Some(token) = confirmation {
            let preview = data
                .read_json::<UninstallConfirmation>("cli-uninstall.json")?
                .ok_or(CoreError::Changed)?;
            if preview.token != token
                || preview.observation != observation
                || now > preview.expires_at
            {
                return Err(CoreError::Changed);
            }
            data.remove(c"cli-uninstall.json")?;
        } else if status.running {
            let token = format!(
                "c1-{}",
                crate::protocol::random()?
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            );
            let expires_at = now + 600;
            data.write_json(
                c"cli-uninstall.json",
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
        drop(operation);
        drop(data);
        if choice == Data::Delete {
            self.remove_data_root()?;
        }
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

    /// Deletes the data root once nothing holds its locks.
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
