//! OpenSSH account admission and a bounded stdio carrier for SpaceTerm Protocol.
use crate::ServerError;
use crate::{host, protocol, storage::Directory, transport::Transport};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd};

/// Nonsecret continuity fences supplied by an authenticated account route.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountBridgeIdentity {
    pub account: u32,
    pub build: String,
    pub server: [u8; 32],
}

/// Bounded account discovery. The helper declares its protocol; final admission negotiates
/// capabilities with the running server before any operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountDiscovery {
    pub schema: u32,
    pub account: u32,
    pub build: String,
    pub server: Option<[u8; 32]>,
    pub installed: bool,
    pub enabled: bool,
    pub running: bool,
    pub stopped: bool,
    pub protocol_majors: Vec<u16>,
    pub capabilities: Vec<String>,
    pub target: String,
    pub version: String,
    pub commit: String,
}

/// Discover the account's existing state without provisioning or starting a server.
pub fn discover(identity: &str) -> Result<AccountDiscovery, ServerError> {
    let paths = host::Paths::for_identity(identity)?;
    let server = match Directory::open(&paths.data, false).and_then(|d| d.read()) {
        Ok(record) => Some(record.server),
        Err(ServerError::NotSetUp) => None,
        Err(error) => return Err(error),
    };
    let status = crate::service::Service::for_identity(identity)?.status()?;
    Ok(AccountDiscovery {
        schema: 1,
        // SAFETY: geteuid has no preconditions.
        account: unsafe { libc::geteuid() },
        build: identity.into(),
        server,
        installed: status.installed,
        enabled: status.enabled,
        running: status.running,
        stopped: status.stopped,
        protocol_majors: vec![protocol::MAJOR],
        capabilities: vec!["workspace_policy".into()],
        target: String::new(),
        version: String::new(),
        commit: String::new(),
    })
}

/// A pollable owning read pipe from an OpenSSH process.
pub trait AccountRead: Read + AsRawFd + Send {}
impl<T: Read + AsRawFd + Send> AccountRead for T {}
/// A pollable owning write pipe to an OpenSSH process.
pub trait AccountWrite: Write + AsRawFd + Send {}
impl<T: Write + AsRawFd + Send> AccountWrite for T {}

/// Opens only the original validated SSH route. Implementations own SSH authorization policy.
#[cfg(feature = "client")]
pub trait AccountConnector: Send + Sync {
    fn connect(&self) -> Result<AccountBridge, ServerError>;
}

/// An account bridge whose process owner is released on refusal, interruption or final drop.
pub struct AccountBridge {
    read: Box<dyn AccountRead>,
    write: Box<dyn AccountWrite>,
    cleanup: Option<Box<dyn FnOnce() + Send>>,
    identity: AccountBridgeIdentity,
}

impl AccountBridge {
    /// Validate the remote preamble before any client operation. `cleanup` must cancel and
    /// reap the owned SSH process group; it also runs if construction fails.
    pub fn from_streams(
        read: Box<dyn AccountRead>,
        write: Box<dyn AccountWrite>,
        cleanup: Box<dyn FnOnce() + Send>,
        expected: &AccountBridgeIdentity,
    ) -> Result<Self, ServerError> {
        let mut bridge = Self {
            read,
            write,
            cleanup: Some(cleanup),
            identity: expected.clone(),
        };
        let observed: AccountBridgeIdentity = protocol::Wire::new(&mut bridge)?.read()?;
        if observed != *expected || observed.server == [0; 32] || observed.account == 0 {
            return Err(ServerError::AuthenticationRefused);
        }
        Ok(bridge)
    }
    pub fn identity(&self) -> &AccountBridgeIdentity {
        &self.identity
    }
}

impl Read for AccountBridge {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.read.read(bytes)
    }
}
impl Write for AccountBridge {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.write.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.write.flush()
    }
}
impl Transport for AccountBridge {
    fn read_fd(&self) -> std::os::fd::RawFd {
        self.read.as_raw_fd()
    }
    fn write_fd(&self) -> std::os::fd::RawFd {
        self.write.as_raw_fd()
    }
}

/// Carry protocol frames to the owning account's fixed private endpoint. Never starts a server.
pub fn run(identity: &str) -> Result<(), ServerError> {
    // SAFETY: isatty only observes the process's standard descriptors.
    if unsafe { libc::isatty(libc::STDIN_FILENO) != 0 || libc::isatty(libc::STDOUT_FILENO) != 0 } {
        return Err(ServerError::Protocol);
    }
    let paths = host::Paths::for_identity(identity)?;
    crate::service::check_admission(&paths)?;
    let server = Directory::open(&paths.data, false)?.read()?.server;
    let mut stream = host::connect(&paths)?;
    // SAFETY: geteuid has no preconditions.
    let identity = AccountBridgeIdentity {
        account: unsafe { libc::geteuid() },
        build: identity.into(),
        server,
    };
    let mut io = AccountBridge {
        // Protocol frames have no newline. Raw duplicate descriptors avoid stdout's line buffer.
        read: Box::new(std::fs::File::from(
            std::io::stdin()
                .as_fd()
                .try_clone_to_owned()
                .map_err(|_| ServerError::Protocol)?,
        )),
        write: Box::new(std::fs::File::from(
            std::io::stdout()
                .as_fd()
                .try_clone_to_owned()
                .map_err(|_| ServerError::Protocol)?,
        )),
        cleanup: None,
        identity: identity.clone(),
    };
    protocol::Wire::new(&mut io)?.write(&identity)?;
    relay(&mut io, &mut stream)
}

fn relay(
    io: &mut dyn Transport,
    stream: &mut std::os::unix::net::UnixStream,
) -> Result<(), ServerError> {
    loop {
        let mut fds = [
            libc::pollfd {
                fd: io.read_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: poll borrows two live descriptors and the writable array for this call.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(ServerError::Protocol);
        }
        for (index, fd) in fds.iter().enumerate() {
            if fd.revents & libc::POLLIN != 0 {
                if index == 0 {
                    let frame = protocol::Wire::terminal(io)?.read_frame()?;
                    protocol::Wire::terminal(stream)?.write_frame(&frame)?;
                } else {
                    let frame = protocol::Wire::terminal(stream)?.read_frame()?;
                    protocol::Wire::terminal(io)?.write_frame(&frame)?;
                }
            } else if fd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                return Ok(());
            }
        }
    }
}

impl Drop for AccountBridge {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::net::UnixStream,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    #[test]
    fn an_account_change_refuses_the_bridge_and_cleans_up_its_process() {
        let (client, mut peer) = UnixStream::pair().unwrap();
        let original = AccountBridgeIdentity {
            account: 501,
            build: "spaceterm-development".into(),
            server: [7; 32],
        };
        let mut changed = original.clone();
        changed.account = 502;
        let bytes = serde_json::to_vec(&changed).unwrap();
        peer.write_all(&(bytes.len() as u32).to_be_bytes()).unwrap();
        peer.write_all(&bytes).unwrap();
        let cleaned = Arc::new(AtomicBool::new(false));
        let cleanup = Arc::clone(&cleaned);
        let result = AccountBridge::from_streams(
            Box::new(client.try_clone().unwrap()),
            Box::new(client),
            Box::new(move || {
                cleanup.store(true, Ordering::Release);
            }),
            &original,
        );
        assert!(matches!(result, Err(ServerError::AuthenticationRefused)));
        assert!(cleaned.load(Ordering::Acquire));
    }

    #[test]
    fn a_server_or_build_change_is_refused_before_any_client_protocol_bytes() {
        for (build, server) in [("spaceterm", [7; 32]), ("spaceterm-development", [8; 32])] {
            let (client, mut peer) = UnixStream::pair().unwrap();
            let expected = AccountBridgeIdentity {
                account: 501,
                build: "spaceterm-development".into(),
                server: [7; 32],
            };
            let changed = AccountBridgeIdentity {
                account: 501,
                build: build.into(),
                server,
            };
            let bytes = serde_json::to_vec(&changed).unwrap();
            peer.write_all(&(bytes.len() as u32).to_be_bytes()).unwrap();
            peer.write_all(&bytes).unwrap();
            let result = AccountBridge::from_streams(
                Box::new(client.try_clone().unwrap()),
                Box::new(client),
                Box::new(|| {}),
                &expected,
            );
            assert!(matches!(result, Err(ServerError::AuthenticationRefused)));
            let mut byte = [0];
            assert_eq!(peer.read(&mut byte).unwrap(), 0);
        }
    }

    #[test]
    fn an_oversized_bridge_preamble_is_refused_without_reading_the_payload() {
        let (client, mut peer) = UnixStream::pair().unwrap();
        peer.write_all(&u32::MAX.to_be_bytes()).unwrap();
        let expected = AccountBridgeIdentity {
            account: 501,
            build: "spaceterm-development".into(),
            server: [7; 32],
        };
        let cleaned = Arc::new(AtomicBool::new(false));
        let cleanup = Arc::clone(&cleaned);
        let result = AccountBridge::from_streams(
            Box::new(client.try_clone().unwrap()),
            Box::new(client),
            Box::new(move || {
                cleanup.store(true, Ordering::Release);
            }),
            &expected,
        );
        assert!(matches!(result, Err(ServerError::Protocol)));
        assert!(cleaned.load(Ordering::Acquire));
    }
}
