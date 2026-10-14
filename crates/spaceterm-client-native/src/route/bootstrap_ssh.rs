//! Retained OpenSSH multiplex owner and fixed signed-account helper transport.
//! No public method accepts a command, executable, local path or environment from the remote peer.

use super::captured_ssh::{ChildOwner, bounded_output};
use super::*;
use crate::{
    bootstrap::production::{
        BootstrapHostConnection, BootstrapHostFactory, BootstrapRemoteConnection,
    },
    native::artifacts::ServerArtifacts,
};
use spaceterm_client::{
    journal::JournalSshCapture,
    pairing::{bootstrap::*, credentials::PairingFuture},
};
use spaceterm_core::pairing::PairingFailure;
use spaceterm_distribution::VerifiedAccountServerArtifact;
use spaceterm_host::{
    bootstrap::{
        command::{MAX_PROBE_BYTES, read_only_probe_script},
        linger::{
            BootstrapLingerFailure, LingerFuture, LingerInvocation, ScopedLingerAuthentication,
        },
        probe::ReadOnlyHostProbe,
        process::{BootstrapProbeFailure, BootstrapProbeFuture},
        wire::*,
    },
    transport::{PeerFacts, TransportFuture, TransportRoute},
};
use std::{
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    process::{Command, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

pub struct NativeBootstrapHostFactory {
    routes: Arc<NativeRouteConnector>,
    artifacts: Arc<ServerArtifacts>,
    build: BuildChannel,
}
impl NativeBootstrapHostFactory {
    pub fn new(routes: Arc<NativeRouteConnector>, build: BuildChannel) -> Self {
        Self {
            routes,
            artifacts: Arc::new(ServerArtifacts::default()),
            build,
        }
    }
    pub fn with_artifacts(
        routes: Arc<NativeRouteConnector>,
        build: BuildChannel,
        artifacts: Arc<ServerArtifacts>,
    ) -> Self {
        Self {
            routes,
            artifacts,
            build,
        }
    }
}
impl BootstrapHostFactory for NativeBootstrapHostFactory {
    fn open(
        &self,
        destination: String,
        generation: u64,
    ) -> PairingFuture<'_, Result<BootstrapHostConnection, BootstrapFailure>> {
        Box::pin(async move {
            if generation == 0 {
                return Err(fail(PairingFailure::InvalidRequest));
            }
            let configuration = self
                .routes
                .ssh
                .as_ref()
                .ok_or_else(|| fail(PairingFailure::UnsupportedTarget))?;
            let capture = configuration
                .capture(&destination)
                .await
                .map_err(|_| fail(PairingFailure::RouteChanged))?;
            let config = configuration.bootstrap_configuration(&capture)?;
            let master = tokio::task::spawn_blocking(move || Master::start(config))
                .await
                .map_err(|_| fail(PairingFailure::Offline))??;
            let master = Arc::new(master);
            let probe = master.probe().await?;
            let uid = probe_uid(&probe)?;
            let host = ReadOnlyHostProbe::parse(&probe, uid)
                .map_err(|_| fail(PairingFailure::UnsupportedTarget))?;
            // A window counter is not an authentication-session identity.
            // Keep identical aliases on different retained masters distinct.
            let mut session = [0; 32];
            ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut session)
                .map_err(|_| fail(PairingFailure::CredentialUnavailable))?;
            let route = BootstrapRoute::capture(
                session,
                hash(capture.configuration(), b"account"),
                hash(capture.configuration(), b"configuration"),
                uid,
                self.build,
            )?;
            let preview = BootstrapHostFacts {
                route,
                account_uid: uid,
                os: host.os,
                architecture: host.architecture,
                os_version: host.os_version,
                libc: host.libc,
                kernel: host.kernel,
                supervision: host.supervision,
                registration: BootstrapRegistration::Missing,
                storage: BootstrapPrerequisite::Available,
                installation: BootstrapPrerequisite::Available,
                linger: host.linger,
                sudo: if host.sudo
                    == spaceterm_core::bootstrap::BootstrapSudoAvailability::Available
                {
                    BootstrapSudo::ResponseRequired
                } else {
                    BootstrapSudo::Unavailable
                },
                authorization_agent: host.sudo
                    == spaceterm_core::bootstrap::BootstrapSudoAvailability::Available,
                background_items_allowed: true,
            };
            let platform = preview.supported_platform()?;
            preview.prerequisites(true)?;
            // Nothing downloaded executes before platform/libc/manager/authorization preflight.
            // Staging is isolated, temporary account code, never sudo or a parallel service.
            let source = self.artifacts.clone();
            let build = build_identity(route.build());
            let artifact = tokio::task::spawn_blocking(move || {
                source.download_for_target(build, platform, None)
            })
            .await
            .map_err(|_| fail(PairingFailure::Offline))?
            .map_err(|_| BootstrapFailure {
                class: BootstrapFailureClass::BootstrapTrustUnavailable,
                phase: BootstrapPhase::Planned,
            })?;
            let staged = Arc::new(StagedArtifact {
                bytes: artifact.bytes().to_vec(),
                entry: helper_entry(route.build(), platform),
                digest: artifact.artifact_sha256(),
            });
            let channel = master.stage(staged.clone(), uid).await?;
            let connection = Arc::new(Remote {
                master,
                route,
                destination,
                staged,
                channel: Mutex::new(Some(channel)),
                account_lock: tokio::sync::Mutex::new(()),
                original: capture,
                routes: self.routes.clone(),
            });
            Ok(BootstrapHostConnection {
                connection,
                artifact: Some(artifact),
            })
        })
    }
}
impl SshRouteConfiguration {
    fn bootstrap_configuration(
        &self,
        capture: &JournalSshCapture,
    ) -> Result<Configuration, BootstrapFailure> {
        let mut file =
            tempfile::NamedTempFile::new().map_err(|_| fail(PairingFailure::StorageUnavailable))?;
        // Top-level and every jump host share the captured authentication policy.
        writeln!(
            file,
            "Host *\n BatchMode {}",
            if self.askpass.is_some() { "no" } else { "yes" }
        )
        .map_err(|_| fail(PairingFailure::StorageUnavailable))?;
        file.write_all(capture.configuration().as_bytes())
            .map_err(|_| fail(PairingFailure::StorageUnavailable))?;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o400))
            .map_err(|_| fail(PairingFailure::StorageUnavailable))?;
        let directory =
            tempfile::tempdir().map_err(|_| fail(PairingFailure::StorageUnavailable))?;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
            .map_err(|_| fail(PairingFailure::StorageUnavailable))?;
        Ok(Configuration {
            executable: self.executable.clone(),
            home: self.home.clone(),
            environment: self.environment.clone(),
            file,
            directory,
            askpass: self.askpass.clone(),
        })
    }
}
struct Configuration {
    executable: PathBuf,
    home: PathBuf,
    environment: Vec<(OsString, OsString)>,
    file: tempfile::NamedTempFile,
    directory: tempfile::TempDir,
    askpass: Option<Arc<dyn TunnelAskPass>>,
}
struct Master {
    configuration: Configuration,
    child: Mutex<ChildOwner>,
    socket: PathBuf,
}
impl Master {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.configuration.executable);
        command
            .args(["-F"])
            .arg(self.configuration.file.path())
            .args(["-S"])
            .arg(&self.socket)
            .args([
                "-o",
                "ControlMaster=no",
                "-o",
                "BatchMode=yes",
                "-o",
                "ClearAllForwardings=yes",
                "-o",
                "PermitLocalCommand=no",
                "-o",
                "RemoteCommand=none",
                "-o",
                "ProxyCommand=/usr/bin/false",
            ])
            .env_clear()
            .env("HOME", &self.configuration.home)
            .envs(self.configuration.environment.iter().cloned())
            .current_dir(&self.configuration.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        command
    }
    fn start(configuration: Configuration) -> Result<Self, BootstrapFailure> {
        let socket = configuration.directory.path().join("master");
        let mut command = Command::new(&configuration.executable);
        command
            .args(["-F"])
            .arg(configuration.file.path())
            .args(["-M", "-S"])
            .arg(&socket)
            .args([
                "-N",
                "-o",
                "ControlPersist=no",
                "-o",
                "ClearAllForwardings=yes",
                "-o",
                "PermitLocalCommand=no",
                "-o",
                "RemoteCommand=none",
                "--",
                "st-route-0",
            ])
            .env_clear()
            .env("HOME", &configuration.home)
            .envs(configuration.environment.iter().cloned())
            .current_dir(&configuration.home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let handoff = configuration
            .askpass
            .as_ref()
            .map(|askpass| askpass.handoff())
            .transpose()
            .map_err(|_| fail(PairingFailure::AuthorizationUnavailable))?;
        if let Some(handoff) = &handoff {
            handoff.configure(&mut command);
        }
        let child = ChildOwner(Some(
            spaceterm_host::process::spawn(&mut command)
                .map_err(|_| fail(PairingFailure::Offline))?,
        ));
        let master = Self {
            configuration,
            child: Mutex::new(child),
            socket,
        };
        let until = Instant::now() + DIAL_DEADLINE;
        loop {
            master.revalidate()?;
            if master.socket.exists() {
                let mut check = master.command();
                check
                    .args(["-O", "check", "--", "st-route-0"])
                    .stdin(Stdio::null());
                if bounded_output(&mut check, until).is_ok() {
                    return Ok(master);
                }
            }
            if Instant::now() >= until {
                return Err(fail(PairingFailure::TimedOut));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn revalidate(&self) -> Result<(), BootstrapFailure> {
        if self
            .child
            .lock()
            .map_err(|_| fail(PairingFailure::RecoveryRequired))?
            .exited()
            .map_err(|_| fail(PairingFailure::Offline))?
        {
            return Err(fail(PairingFailure::RouteChanged));
        }
        Ok(())
    }
    async fn probe(self: &Arc<Self>) -> Result<Vec<u8>, BootstrapFailure> {
        let master = self.clone();
        tokio::task::spawn_blocking(move || {
            master.revalidate()?;
            let mut command = master.command();
            // Only a product-owned constant reaches the remote shell.
            let script = std::str::from_utf8(read_only_probe_script())
                .map_err(|_| fail(PairingFailure::InvalidRequest))?;
            command.args([
                "-T",
                "--",
                "st-route-0",
                &format!("/bin/sh -c {}", shell_literal(script)),
            ]);
            let bytes = bounded_output(&mut command, Instant::now() + Duration::from_secs(15))
                .map_err(|_| fail(PairingFailure::Offline))?;
            if bytes.len() > MAX_PROBE_BYTES {
                return Err(fail(PairingFailure::LimitExceeded));
            }
            master.revalidate()?;
            Ok(bytes)
        })
        .await
        .map_err(|_| fail(PairingFailure::Offline))?
    }
    async fn stage(
        self: &Arc<Self>,
        artifact: Arc<StagedArtifact>,
        uid: u32,
    ) -> Result<Arc<Channel>, BootstrapFailure> {
        let master = self.clone();
        tokio::task::spawn_blocking(move || {
            master.revalidate()?;
            let script = format!("set -efu\nPATH=/usr/bin:/bin:/usr/sbin:/sbin\nexport PATH\numask 077\nd=$(/usr/bin/mktemp -d /tmp/spaceterm-bootstrap.XXXXXXXX)\ntrap 'rm -rf -- \"$d\"' EXIT HUP INT TERM\n/usr/bin/head -c {} >\"$d/archive\"\nif [ -x /usr/bin/sha256sum ]; then hash=$(/usr/bin/sha256sum \"$d/archive\"); else hash=$(/usr/bin/shasum -a 256 \"$d/archive\"); fi\n[ \"${{hash%% *}}\" = {} ] || exit 1\n/usr/bin/tar -xzf \"$d/archive\" -C \"$d\"\ncd \"$d/spaceterm-server\"\n{} bootstrap-account\n", artifact.bytes.len(), hex(&artifact.digest), shell_literal(artifact.entry));
            let mut command = master.command();
            command.args(["-T", "--", "st-route-0", &format!("/bin/sh -c {}", shell_literal(&script))]);
            let channel = Arc::new(Channel::spawn(&mut command, uid)?);
            channel.write(&artifact.bytes)?;
            master.revalidate()?;
            Ok(channel)
        }).await.map_err(|_| fail(PairingFailure::Offline))?
    }
}
struct StagedArtifact {
    bytes: Vec<u8>,
    entry: &'static str,
    digest: [u8; 32],
}
struct Remote {
    master: Arc<Master>,
    route: BootstrapRoute,
    destination: String,
    staged: Arc<StagedArtifact>,
    channel: Mutex<Option<Arc<Channel>>>,
    account_lock: tokio::sync::Mutex<()>,
    original: JournalSshCapture,
    routes: Arc<NativeRouteConnector>,
}
impl Remote {
    async fn check_capture(&self) -> Result<(), BootstrapFailure> {
        self.revalidate()?;
        let actual = self
            .routes
            .ssh
            .as_ref()
            .ok_or_else(|| fail(PairingFailure::RouteChanged))?
            .capture(&self.destination)
            .await
            .map_err(|_| fail(PairingFailure::RouteChanged))?;
        if actual != self.original {
            return Err(fail(PairingFailure::RouteChanged));
        }
        Ok(())
    }
}
impl BootstrapRemoteConnection for Remote {
    fn route(&self) -> BootstrapRoute {
        self.route
    }
    fn destination(&self) -> &str {
        &self.destination
    }
    fn revalidate(&self) -> Result<(), BootstrapFailure> {
        self.master.revalidate()
    }
    fn probe(&self) -> BootstrapProbeFuture<'_, Vec<u8>> {
        Box::pin(async move {
            self.check_capture()
                .await
                .map_err(|_| BootstrapProbeFailure::RouteChanged)?;
            self.master
                .probe()
                .await
                .map_err(|_| BootstrapProbeFailure::Offline)
        })
    }
    fn account<'a>(
        &'a self,
        request: AccountRequest,
        artifact: Option<&'a VerifiedAccountServerArtifact>,
    ) -> PairingFuture<'a, Result<AccountReply, BootstrapFailure>> {
        Box::pin(async move {
            let _guard = self.account_lock.lock().await;
            self.check_capture().await?;
            let readonly = matches!(
                request,
                AccountRequest::Inspect
                    | AccountRequest::Resolve { .. }
                    | AccountRequest::RetainedOwner { .. }
                    | AccountRequest::OwnerBinding { .. }
            );
            let bytes =
                serde_json::to_vec(&request).map_err(|_| fail(PairingFailure::InvalidRequest))?;
            let channel = self
                .channel
                .lock()
                .map_err(|_| fail(PairingFailure::RecoveryRequired))?
                .clone();
            let channel = match channel {
                Some(channel) => channel,
                None if readonly => {
                    let channel = self
                        .master
                        .stage(self.staged.clone(), self.route.account_uid())
                        .await?;
                    *self
                        .channel
                        .lock()
                        .map_err(|_| fail(PairingFailure::RecoveryRequired))? =
                        Some(channel.clone());
                    channel
                }
                None => return Err(fail(PairingFailure::RecoveryRequired)),
            };
            let result = async {
                channel
                    .send(&bytes)
                    .await
                    .map_err(|_| fail(PairingFailure::RecoveryRequired))?;
                if let Some(artifact) = artifact {
                    tokio::task::block_in_place(|| {
                        crate::bootstrap::write_artifact(artifact, ArtifactWriter(&channel))
                    })
                    .map_err(|_| fail(PairingFailure::RecoveryRequired))?;
                }
                let reply = channel
                    .recv()
                    .await
                    .map_err(|_| fail(PairingFailure::RecoveryRequired))?
                    .ok_or_else(|| fail(PairingFailure::RecoveryRequired))?;
                self.revalidate()?;
                serde_json::from_slice(&reply)
                    .map_err(|_| fail(PairingFailure::InvalidDurableState))
            }
            .await;
            if result.is_err() {
                // Retire this exact failed command. A later original-request lookup may
                // reopen a helper only on the same authenticated master, never reenroll.
                self.channel
                    .lock()
                    .map_err(|_| fail(PairingFailure::RecoveryRequired))?
                    .take();
            }
            result
        })
    }
    fn owner_channel(
        &self,
    ) -> PairingFuture<'_, Result<Arc<dyn MessageTransport>, PairingFailure>> {
        Box::pin(async move {
            self.check_capture()
                .await
                .map_err(|_| PairingFailure::RouteChanged)?;
            let channel = self
                .master
                .stage(self.staged.clone(), self.route.account_uid())
                .await
                .map_err(|_| PairingFailure::Offline)?;
            let request = serde_json::to_vec(&AccountRequest::OwnerChannel)
                .map_err(|_| PairingFailure::InvalidRequest)?;
            channel
                .send(&request)
                .await
                .map_err(|_| PairingFailure::Offline)?;
            Ok(channel as Arc<dyn MessageTransport>)
        })
    }
    fn prompt(&self) -> PairingFuture<'_, Result<Zeroizing<Vec<u8>>, BootstrapFailure>> {
        Box::pin(async move {
            self.check_capture().await?;
            let askpass = self
                .master
                .configuration
                .askpass
                .as_ref()
                .ok_or_else(|| fail(PairingFailure::AuthorizationUnavailable))?
                .handoff()
                .map_err(|_| fail(PairingFailure::AuthorizationUnavailable))?;
            tokio::task::spawn_blocking(move || {
                let mut command = askpass.prompt_command();
                command
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .process_group(0);
                let mut bytes = Zeroizing::new(
                    bounded_output(&mut command, Instant::now() + Duration::from_secs(120))
                        .map_err(|_| fail(PairingFailure::AuthorizationDenied))?,
                );
                if bytes.last() == Some(&b'\n') {
                    bytes.pop();
                }
                if bytes.is_empty()
                    || bytes.len() > 16 * 1024
                    || bytes.iter().any(|b| *b < 0x20 || *b == 0x7f)
                {
                    return Err(fail(PairingFailure::AuthorizationDenied));
                }
                Ok(bytes)
            })
            .await
            .map_err(|_| fail(PairingFailure::AuthorizationUnavailable))?
        })
    }
    fn enable_linger<'a>(
        &'a self,
        invocation: &'a LingerInvocation,
        authentication: &'a ScopedLingerAuthentication<'a>,
    ) -> LingerFuture<'a, ()> {
        Box::pin(async move {
            if invocation.account_uid() != self.route.account_uid() {
                return Err(BootstrapLingerFailure::RouteChanged);
            }
            self.check_capture()
                .await
                .map_err(|_| BootstrapLingerFailure::RouteChanged)?;
            let mut command = self.master.command();
            let fixed = format!(
                "exec /usr/bin/sudo -k -p '[SpaceTerm linger password]' -- /usr/bin/loginctl enable-linger -- {} 2>&1",
                self.route.account_uid()
            );
            command.args(["-tt", "--", "st-route-0", &fixed]);
            let channel = Arc::new(
                Channel::spawn(&mut command, self.route.account_uid())
                    .map_err(|_| BootstrapLingerFailure::Unavailable)?,
            );
            let until = Instant::now() + Duration::from_secs(120);
            let mut output = Zeroizing::new(Vec::new());
            let mut answered = 0;
            loop {
                self.revalidate()
                    .map_err(|_| BootstrapLingerFailure::RouteChanged)?;
                let chunk = channel
                    .read_available()
                    .map_err(|_| BootstrapLingerFailure::Unavailable)?;
                if output.len() + chunk.len() > 4096 {
                    return Err(BootstrapLingerFailure::LimitExceeded);
                }
                output.extend_from_slice(&chunk);
                let marker = b"[SpaceTerm linger password]";
                if output.windows(marker.len()).any(|part| part == marker) {
                    if answered >= 3 {
                        return Err(BootstrapLingerFailure::Denied);
                    }
                    let mut response = authentication.respond(self.route.account_uid()).await?;
                    self.revalidate()
                        .map_err(|_| BootstrapLingerFailure::RouteChanged)?;
                    response.push(b'\n');
                    channel
                        .write(&response)
                        .map_err(|_| BootstrapLingerFailure::Unavailable)?;
                    answered += 1;
                    output.clear();
                }
                if channel
                    .exited()
                    .map_err(|_| BootstrapLingerFailure::Unavailable)?
                {
                    return channel.finish().map_err(|_| BootstrapLingerFailure::Denied);
                }
                if Instant::now() >= until {
                    return Err(BootstrapLingerFailure::TimedOut);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    }
}

struct ArtifactWriter<'a>(&'a Channel);
impl Write for ArtifactWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for chunk in bytes.chunks(TRANSFER_CHUNK) {
            self.0
                .write(&(chunk.len() as u32).to_be_bytes())
                .and_then(|_| self.0.write(chunk))
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
struct Channel {
    child: Mutex<ChildOwner>,
    input: Mutex<Option<std::process::ChildStdin>>,
    output: Mutex<std::process::ChildStdout>,
    uid: u32,
}
impl Channel {
    fn spawn(command: &mut Command, uid: u32) -> Result<Self, BootstrapFailure> {
        let mut child = ChildOwner(Some(
            spaceterm_host::process::spawn(command).map_err(|_| fail(PairingFailure::Offline))?,
        ));
        let input = child
            .0
            .as_mut()
            .and_then(|c| c.stdin.take())
            .ok_or_else(|| fail(PairingFailure::Offline))?;
        let output = child
            .0
            .as_mut()
            .and_then(|c| c.stdout.take())
            .ok_or_else(|| fail(PairingFailure::Offline))?;
        nonblocking(input.as_raw_fd())?;
        nonblocking(output.as_raw_fd())?;
        Ok(Self {
            child: Mutex::new(child),
            input: Mutex::new(Some(input)),
            output: Mutex::new(output),
            uid,
        })
    }
    fn write(&self, mut bytes: &[u8]) -> Result<(), BootstrapFailure> {
        let mut input = self
            .input
            .lock()
            .map_err(|_| fail(PairingFailure::RecoveryRequired))?;
        let input = input
            .as_mut()
            .ok_or_else(|| fail(PairingFailure::Offline))?;
        let until = Instant::now() + Duration::from_secs(120);
        while !bytes.is_empty() {
            match input.write(bytes) {
                Ok(0) => return Err(fail(PairingFailure::Offline)),
                Ok(count) => bytes = &bytes[count..],
                Err(error) if retry_io(&error) => {}
                Err(_) => return Err(fail(PairingFailure::Offline)),
            }
            if Instant::now() >= until {
                return Err(fail(PairingFailure::TimedOut));
            }
            if !bytes.is_empty() {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        Ok(())
    }
    fn read_exact(&self, mut bytes: &mut [u8]) -> Result<(), BootstrapFailure> {
        let mut output = self
            .output
            .lock()
            .map_err(|_| fail(PairingFailure::RecoveryRequired))?;
        let until = Instant::now() + Duration::from_secs(120);
        while !bytes.is_empty() {
            match output.read(bytes) {
                Ok(0) => return Err(fail(PairingFailure::Offline)),
                Ok(count) => bytes = &mut bytes[count..],
                Err(error) if retry_io(&error) => {}
                Err(_) => return Err(fail(PairingFailure::Offline)),
            }
            if Instant::now() >= until {
                return Err(fail(PairingFailure::TimedOut));
            }
            if !bytes.is_empty() {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        Ok(())
    }
    fn read_available(&self) -> Result<Zeroizing<Vec<u8>>, BootstrapFailure> {
        let mut bytes = Zeroizing::new(vec![0; 1024]);
        match self
            .output
            .lock()
            .map_err(|_| fail(PairingFailure::RecoveryRequired))?
            .read(&mut bytes)
        {
            Ok(count) => {
                bytes.truncate(count);
                Ok(bytes)
            }
            Err(error) if retry_io(&error) => {
                bytes.clear();
                Ok(bytes)
            }
            Err(_) => Err(fail(PairingFailure::Offline)),
        }
    }
    fn exited(&self) -> Result<bool, BootstrapFailure> {
        self.child
            .lock()
            .map_err(|_| fail(PairingFailure::RecoveryRequired))?
            .exited()
            .map_err(|_| fail(PairingFailure::Offline))
    }
    fn finish(&self) -> Result<(), BootstrapFailure> {
        let mut owner = self
            .child
            .lock()
            .map_err(|_| fail(PairingFailure::RecoveryRequired))?;
        owner.retire();
        let status = owner
            .0
            .as_mut()
            .ok_or_else(|| fail(PairingFailure::Offline))?
            .wait()
            .map_err(|_| fail(PairingFailure::Offline))?;
        owner.0.take();
        if status.success() {
            Ok(())
        } else {
            Err(fail(PairingFailure::AuthorizationDenied))
        }
    }
}
impl MessageTransport for Channel {
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
            // Blocking descriptors are owned and deadline-bounded; no spawned reader outlives this call.
            tokio::task::block_in_place(|| {
                self.write(&(bytes.len() as u32).to_be_bytes())?;
                self.write(bytes)
            })
            .map_err(|_| TransportError::Unavailable)
        })
    }
    fn recv(&self) -> TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            tokio::task::block_in_place(|| {
                let mut prefix = [0; 4];
                self.read_exact(&mut prefix)?;
                let count = u32::from_be_bytes(prefix) as usize;
                if count == 0 || count > FRAME_LIMIT {
                    return Err(fail(PairingFailure::LimitExceeded));
                }
                let mut bytes = Zeroizing::new(vec![0; count]);
                self.read_exact(&mut bytes)?;
                Ok(Some(std::mem::take(&mut *bytes)))
            })
            .map_err(|_| TransportError::Unavailable)
        })
    }
    fn close(&self) -> TransportFuture<'_, ()> {
        Box::pin(async move {
            tokio::task::block_in_place(|| {
                self.input
                    .lock()
                    .map_err(|_| fail(PairingFailure::RecoveryRequired))?
                    .take();
                let until = Instant::now() + Duration::from_secs(15);
                while !self.exited()? {
                    if Instant::now() >= until {
                        return Err(fail(PairingFailure::TimedOut));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                self.finish()
            })
            .map_err(|_| TransportError::Unavailable)
        })
    }
}
fn nonblocking(fd: std::os::fd::RawFd) -> Result<(), BootstrapFailure> {
    // SAFETY: the caller retains the descriptor for the entire flag update.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(fail(PairingFailure::Offline));
    }
    Ok(())
}
fn retry_io(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
    )
}
fn fail(class: PairingFailure) -> BootstrapFailure {
    BootstrapFailure::new(class, BootstrapPhase::OpenSshAuthenticated)
}
fn shell_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn hash(configuration: &str, domain: &[u8]) -> [u8; 32] {
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    hash.update(domain);
    hash.update(configuration.as_bytes());
    let mut digest = [0; 32];
    digest.copy_from_slice(hash.finish().as_ref());
    digest
}
fn probe_uid(bytes: &[u8]) -> Result<u32, BootstrapFailure> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| text.strip_prefix("STBP2\nuid="))
        .and_then(|text| text.split_once('\n'))
        .and_then(|(uid, _)| uid.parse().ok())
        .filter(|uid| !matches!(*uid, 0 | u32::MAX))
        .ok_or_else(|| fail(PairingFailure::InvalidRequest))
}

fn build_identity(build: BuildChannel) -> spaceterm_host::startup::BuildIdentity {
    match build {
        BuildChannel::Release => spaceterm_host::startup::BuildIdentity::Release,
        BuildChannel::Preflight => spaceterm_host::startup::BuildIdentity::Preflight,
        BuildChannel::Development => spaceterm_host::startup::BuildIdentity::Development,
    }
}
fn helper_entry(
    build: BuildChannel,
    platform: spaceterm_distribution::ServerPlatform,
) -> &'static str {
    match (platform, build) {
        (spaceterm_distribution::ServerPlatform::LinuxX86_64, _) => "./bin/spaceterm-server",
        (_, BuildChannel::Release) => "./SpaceTerm Server.app/Contents/MacOS/spaceterm-server",
        (_, BuildChannel::Preflight) => {
            "./SpaceTerm Preflight Server.app/Contents/MacOS/spaceterm-server"
        }
        (_, BuildChannel::Development) => {
            "./SpaceTerm Development Server.app/Contents/MacOS/spaceterm-server"
        }
    }
}

#[cfg(test)]
mod tests;
