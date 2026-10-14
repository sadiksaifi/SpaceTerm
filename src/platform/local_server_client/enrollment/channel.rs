//! The deliberate stopped-owner command reuses the installed, signed server.
//! It has no service-manager Start capability and owns only its one child.

use super::*;
use spaceterm_client::ports::ClientFuture;
use spaceterm_host::{
    account_service::RegisteredServer,
    server_installation::{InstalledServerExecutable, ServerInstallationStore},
    transport::{PeerFacts, TransportError, TransportFuture, TransportRoute},
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

pub(super) trait OwnerChannelProvider: Send + Sync {
    fn open<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ClientFuture<'a, Arc<dyn MessageTransport>>;
}

pub(super) struct NativeOwnerChannel(pub(super) NativeClientProvider);
impl OwnerChannelProvider for NativeOwnerChannel {
    fn open<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ClientFuture<'a, Arc<dyn MessageTransport>> {
        Box::pin(async move {
            if registration.desired_state() == DesiredServerState::Stopped {
                let native = self.0.clone();
                let pin = registration.server_pin();
                let generation = registration.generation();
                return tokio::task::spawn_blocking(move || {
                    let registration = native
                        .management()
                        .supervisor()
                        .discover()
                        .map_err(classify)?
                        .ok_or_else(|| failure(ClientFailureClass::StaleAuthority))?;
                    if registration.server_pin() != pin
                        || registration.generation() != generation
                        || registration.desired_state() != DesiredServerState::Stopped
                    {
                        return Err(failure(ClientFailureClass::StaleAuthority));
                    }
                    let executable = ServerInstallationStore::open_existing(&registration)
                        .and_then(|store| {
                            store.retain_compatible_approved(
                                spaceterm_host::organization_store::schema::SCHEMA_VERSION,
                            )
                        })
                        .map_err(classify)?;
                    let mut command = tokio::process::Command::new(executable.path());
                    command
                        .arg("owner-enrollment")
                        .env_clear()
                        .current_dir(registration.data_directory())
                        .stdin(std::process::Stdio::piped())
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::null())
                        .kill_on_drop(true);
                    let mut child = command
                        .spawn()
                        .map_err(|_| failure(ClientFailureClass::Unavailable))?;
                    let input = child
                        .stdin
                        .take()
                        .ok_or_else(|| failure(ClientFailureClass::Unavailable))?;
                    let output = child
                        .stdout
                        .take()
                        .ok_or_else(|| failure(ClientFailureClass::Unavailable))?;
                    Ok(Arc::new(CommandChannel {
                        child: tokio::sync::Mutex::new(child),
                        input: tokio::sync::Mutex::new(Some(input)),
                        output: tokio::sync::Mutex::new(output),
                        _executable: executable,
                        uid: registration.key().account_uid,
                    }) as Arc<dyn MessageTransport>)
                })
                .await
                .map_err(|_| failure(ClientFailureClass::Unavailable))?;
            }
            let endpoint = registration.endpoint().map_err(classify)?;
            let directory = endpoint
                .path()
                .parent()
                .ok_or_else(|| failure(ClientFailureClass::UnsafeEndpoint))?;
            let owner = PrivateIpcDirectory::open(directory)
                .and_then(|directory| directory.endpoint("owner"))
                .map_err(spaceterm_client_native::route::classify)?;
            let until = tokio::time::Instant::now() + DEADLINE;
            loop {
                match IpcTransport::connect(
                    &owner,
                    MessageLimit::new(FRAME_LIMIT)
                        .map_err(spaceterm_client_native::route::classify)?,
                )
                .await
                {
                    Ok(transport) => return Ok(Arc::new(transport) as Arc<dyn MessageTransport>),
                    Err(TransportError::Unavailable) if tokio::time::Instant::now() < until => {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    Err(error) => return Err(spaceterm_client_native::route::classify(error)),
                }
            }
        })
    }
}

struct CommandChannel {
    child: tokio::sync::Mutex<tokio::process::Child>,
    input: tokio::sync::Mutex<Option<tokio::process::ChildStdin>>,
    output: tokio::sync::Mutex<tokio::process::ChildStdout>,
    _executable: InstalledServerExecutable,
    uid: u32,
}
impl MessageTransport for CommandChannel {
    fn peer_facts(&self) -> PeerFacts {
        PeerFacts {
            route: TransportRoute::PrivateIpc,
            local_uid: Some(self.uid),
            local_pid: None,
            pinned_server_key: None,
        }
    }
    fn send<'a>(&'a self, bytes: &'a [u8]) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            if bytes.is_empty() || bytes.len() > FRAME_LIMIT {
                return Err(TransportError::MessageTooLarge);
            }
            let mut input = self.input.lock().await;
            let input = input.as_mut().ok_or(TransportError::Closed)?;
            input
                .write_all(&(bytes.len() as u32).to_be_bytes())
                .await
                .map_err(|_| TransportError::Closed)?;
            input
                .write_all(bytes)
                .await
                .map_err(|_| TransportError::Closed)
        })
    }
    fn recv(&self) -> TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            let mut output = self.output.lock().await;
            let length = output
                .read_u32()
                .await
                .map_err(|_| TransportError::Closed)? as usize;
            if length == 0 || length > FRAME_LIMIT {
                return Err(TransportError::MessageTooLarge);
            }
            let mut bytes = Zeroizing::new(vec![0; length]);
            output
                .read_exact(&mut bytes)
                .await
                .map_err(|_| TransportError::Closed)?;
            Ok(Some(std::mem::take(&mut *bytes)))
        })
    }
    fn close(&self) -> TransportFuture<'_, ()> {
        Box::pin(async move {
            self.input.lock().await.take();
            let mut child = self.child.lock().await;
            match tokio::time::timeout(DEADLINE, child.wait()).await {
                Ok(Ok(status)) if status.success() => Ok(()),
                Ok(_) => Err(TransportError::Unavailable),
                Err(_) => {
                    child
                        .kill()
                        .await
                        .map_err(|_| TransportError::Unavailable)?;
                    child
                        .wait()
                        .await
                        .map_err(|_| TransportError::Unavailable)?;
                    Err(TransportError::HandshakeExpired)
                }
            }
        })
    }
}
