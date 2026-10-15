use crate::{
    AuthenticatedStatus, ServerError,
    host::{self, Paths},
    protocol,
    storage::Directory,
};
use serde::{Deserialize, Serialize};
use std::{
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    seed: [u8; 32],
    pin: [u8; 32],
}
impl Drop for Credential {
    fn drop(&mut self) {
        self.seed.zeroize();
    }
}

/// Resolving this handle never reads or writes storage and never starts a process.
#[derive(Clone)]
pub struct LocalServer {
    paths: Paths,
    identity: String,
    account: String,
    versions: Vec<u16>,
    shared_pane: std::sync::Arc<crate::pane::SharedPaneConnection>,
}
impl LocalServer {
    /// A single worker owns the journal lock for this exact native device credential and pin.
    pub fn shared_journal(&self) -> Result<crate::hierarchy::ClientJournal, ServerError> {
        let credential = self.credential()?.ok_or(ServerError::NotPaired)?;
        let directory =
            Directory::open(&self.paths.data.join("clients").join(&self.account), true)?;
        crate::hierarchy::ClientJournal::open(
            directory,
            credential.pin,
            protocol::public(&protocol::key(&credential.seed)?),
        )
    }
    pub fn for_identity(directory_name: &str) -> Result<Self, ServerError> {
        // Development may run two independent desktop clients against one private server.
        // Each uses native credential storage and explicit local pairing, never a secret file.
        let mut account = "desktop".to_owned();
        let mut versions = vec![2, 1];
        if directory_name == "spaceterm-development" {
            if let Some(client) = std::env::var_os("SPACETERM_DEVELOPMENT_CLIENT") {
                let client = client.to_str().ok_or(ServerError::Unsupported)?;
                account = development_account(client)?;
            }
            if let Some(major) = std::env::var_os("SPACETERM_DEVELOPMENT_PROTOCOL_MAJOR") {
                versions = match major.to_str() {
                    Some("1") => vec![1],
                    Some("2") => vec![2],
                    _ => return Err(ServerError::IncompatibleProtocol),
                };
            }
        }
        Ok(Self {
            paths: Paths::for_identity(directory_name)?,
            identity: directory_name.to_owned(),
            account,
            versions,
            shared_pane: std::sync::Arc::default(),
        })
    }
    /// Independent Development desktop identity, retained only in native credential storage.
    pub fn for_development_client(name: &str, protocol_major: u16) -> Result<Self, ServerError> {
        let mut server = Self::for_identity("spaceterm-development")?;
        server.account = development_account(name)?;
        server.versions = match protocol_major {
            1 | 2 => vec![protocol_major],
            _ => return Err(ServerError::IncompatibleProtocol),
        };
        Ok(server)
    }
    fn entry(&self) -> Result<keyring::Entry, ServerError> {
        // Explicit native builders exclude keyring's mock backend even when target features change.
        #[cfg(target_os = "macos")]
        let builder = keyring::macos::default_credential_builder();
        #[cfg(target_os = "linux")]
        let builder = keyring::secret_service::default_credential_builder();
        let credential = builder
            .build(
                None,
                &format!("io.github.sadiksaifi.{}.local-server", self.identity),
                &self.account,
            )
            .map_err(|_| ServerError::CredentialUnavailable)?;
        Ok(keyring::Entry::new_with_credential(credential))
    }
    fn credential(&self) -> Result<Option<Credential>, ServerError> {
        match self.entry()?.get_secret() {
            Ok(bytes) => {
                let bytes = Zeroizing::new(bytes);
                serde_json::from_slice(&bytes)
                    .map(Some)
                    .map_err(|_| ServerError::CredentialUnavailable)
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(ServerError::CredentialUnavailable),
        }
    }
    fn authenticate(&self, credential: &Credential) -> Result<AuthenticatedStatus, ServerError> {
        let mut stream = host::connect(&self.paths)?;
        let mut status = protocol::authenticate_client(
            &mut stream,
            &protocol::key(&credential.seed)?,
            &credential.pin,
            &self.versions,
        )?;
        let mut connection = crate::pane::PaneConnection::new(stream, false);
        // The status-only server predates optional terminal operations and closes after proof.
        status.development_pane = matches!(
            connection.exchange(&crate::pane::Request::Capabilities),
            Ok(crate::pane::Reply::Capabilities {
                development_pane: true,
                ..
            })
        );
        Ok(status)
    }

    /// Authenticates a terminal transport without setup or implicit Start Server.
    pub fn connect_pane(&self) -> Result<crate::pane::PaneConnection, ServerError> {
        if self.identity != "spaceterm-development" {
            return Err(ServerError::Unsupported);
        }
        let credential = self.credential()?.ok_or(ServerError::NotPaired)?;
        let mut stream = host::connect(&self.paths)?;
        protocol::authenticate_client(
            &mut stream,
            &protocol::key(&credential.seed)?,
            &credential.pin,
            &self.versions,
        )?;
        crate::pane::PaneConnection::negotiate(stream)
    }
    /// Multiplexes shared inventory and Pane requests on one authenticated transport.
    /// Clones of this native client share its connection, but independent desktops do not.
    pub fn connect_shared_pane(&self) -> Result<crate::pane::PaneConnection, ServerError> {
        self.shared_pane.connect(|| self.connect_pane())
    }
    /// Reads the saved native credential and authenticates an existing server. Never starts it.
    pub fn refresh(&self) -> Result<AuthenticatedStatus, ServerError> {
        self.authenticate(&self.credential()?.ok_or(ServerError::NotPaired)?)
    }
    /// Local owning-account authority is granted only by this explicit setup action.
    pub fn setup_and_start(&self) -> Result<AuthenticatedStatus, ServerError> {
        // Locked or unavailable storage is refused before touching server identity.
        self.credential()?;
        let data = Directory::open(&self.paths.data, true)?;
        let _setup = data.lock("setup.lock", false)?;
        let existing = self.credential()?;
        // A surviving native pin is evidence of an existing identity. Missing server storage
        // must not silently replace it during repair.
        let mut record = if existing.is_some() {
            data.read().map_err(|error| {
                if error == ServerError::NotPaired {
                    ServerError::AuthenticationRefused
                } else {
                    error
                }
            })?
        } else {
            data.read_or_create()?
        };
        let pin = protocol::public(&protocol::key(&record.seed)?);
        let credential = match existing {
            Some(credential) if credential.pin == pin => credential,
            Some(_) => return Err(ServerError::AuthenticationRefused),
            None => {
                let credential = Credential {
                    seed: protocol::random()?,
                    pin,
                };
                let bytes = Zeroizing::new(
                    serde_json::to_vec(&credential)
                        .map_err(|_| ServerError::CredentialUnavailable)?,
                );
                self.entry()?
                    .set_secret(&bytes)
                    .map_err(|_| ServerError::CredentialUnavailable)?;
                // Require a successful native read before publishing paired-device authority.
                let saved = self
                    .credential()?
                    .ok_or(ServerError::CredentialUnavailable)?;
                if saved.seed != credential.seed || saved.pin != credential.pin {
                    return Err(ServerError::CredentialUnavailable);
                }
                credential
            }
        };
        let device = protocol::public(&protocol::key(&credential.seed)?);
        if !record.paired.contains(&device) {
            if record.paired.len() >= 64 {
                return Err(ServerError::StorageUnavailable);
            }
            record.paired.push(device);
            data.write(&record)?;
        }
        // Runtime restoration and directory publication use this same retained record.
        // Release setup serialization before waiting for the server to admit a connection.
        drop(_setup);
        match self.authenticate(&credential) {
            Ok(status) => return Ok(status),
            Err(ServerError::NotRunning) => {}
            Err(error) => return Err(error),
        }
        // Each application stages this binary beside the desktop executable. No PATH
        // lookup, shell command, retained service, or remote-provided executable is involved.
        let executable = std::env::current_exe()
            .map_err(|_| ServerError::StartupFailed)?
            .parent()
            .ok_or(ServerError::StartupFailed)?
            .join("spaceterm-server");
        let mut command = Command::new(executable);
        command
            .args(["serve", &self.identity])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: setsid is async-signal-safe and this closure performs no allocation or locking.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let mut child = command.spawn().map_err(|_| ServerError::StartupFailed)?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.authenticate(&credential) {
                Ok(status) => return Ok(status),
                Err(ServerError::NotRunning) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                Err(ServerError::NotRunning) => return Err(ServerError::StartupFailed),
                Err(error) => return Err(error),
            }
        }
    }
}

fn development_account(name: &str) -> Result<String, ServerError> {
    if name.is_empty()
        || name.len() > 32
        || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return Err(ServerError::Unsupported);
    }
    Ok(format!("desktop-{name}"))
}
