//! Effective OpenSSH route capture. The final tunnel reads a private snapshot,
//! never the alias/configuration that was checked immediately before spawning.

use super::*;
use spaceterm_client::journal::JournalSshCapture;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{OpenOptionsExt, PermissionsExt},
            process::CommandExt,
        },
    },
    process::{Child, Command, Stdio},
    time::Instant,
};

impl SshRouteConfiguration {
    pub(crate) async fn capture(
        &self,
        destination: &str,
    ) -> Result<JournalSshCapture, ClientFailure> {
        SshDestination::new(destination).map_err(classify)?;
        let executable = self.executable.clone();
        let managed = self.managed_config.clone();
        let home = self.home.clone();
        let environment = self.environment.clone();
        let destination = destination.to_owned();
        tokio::task::spawn_blocking(move || {
            capture(&executable, &managed, &home, &environment, &destination)
        })
        .await
        .map_err(|_| unavailable())?
    }

    pub(crate) async fn open_captured(
        &self,
        destination: &str,
        remote_socket: &str,
        limit: MessageLimit,
        original: &JournalSshCapture,
        foreground: Option<&CredentialUse>,
    ) -> Result<
        (
            Arc<dyn MessageTransport>,
            Arc<SshTunnel>,
            Arc<tempfile::NamedTempFile>,
        ),
        ClientFailure,
    > {
        let askpass = self.foreground_askpass(foreground)?;
        if self.capture(destination).await? != *original {
            return Err(changed());
        }
        // No user configuration is read after this comparison. A later alias,
        // Include, account or jump-host edit cannot redirect the spawned tunnel.
        let mut file = tempfile::NamedTempFile::new().map_err(|_| unavailable())?;
        // Jump processes read this same file. The parent's -o BatchMode alone
        // does not set their authentication policy.
        let batch = if askpass.is_some() { "no" } else { "yes" };
        write!(file, "Host *\n BatchMode {batch}\n").map_err(|_| unavailable())?;
        file.write_all(original.configuration().as_bytes())
            .map_err(|_| unavailable())?;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o400))
            .map_err(|_| unavailable())?;
        let file = Arc::new(file);
        let endpoint = self.endpoints.allocate()?;
        let command = SshTunnelCommand::new(
            self.executable.clone(),
            file.path().to_owned(),
            SshDestination::new("st-route-0").map_err(classify)?,
            endpoint,
            RemoteSocketPath::new(remote_socket).map_err(classify)?,
            if askpass.is_some() {
                AuthenticationMode::AskPass
            } else {
                AuthenticationMode::BatchMode
            },
        )
        .map_err(classify)?;
        let environment =
            SshEnvironment::new(self.home.clone(), self.environment.clone()).map_err(classify)?;
        let deadline = tokio::time::Instant::now() + DIAL_DEADLINE;
        let tunnel = Arc::new(
            SshTunnel::start(command, environment, askpass, DIAL_DEADLINE)
                .await
                .map_err(classify)?,
        );
        match tokio::time::timeout_at(deadline, IpcTransport::connect(tunnel.endpoint(), limit))
            .await
        {
            Ok(Ok(transport)) => Ok((Arc::new(transport), tunnel, file)),
            result => {
                tunnel.cancel().await.map_err(classify)?;
                Err(match result {
                    Ok(Err(error)) => classify(error),
                    _ => unavailable(),
                })
            }
        }
    }
}

fn changed() -> ClientFailure {
    ClientFailure::new(ClientFailureClass::StaleAuthority)
}

struct Effective {
    options: Vec<(String, String)>,
    jumps: Vec<String>,
}

fn capture(
    executable: &std::path::Path,
    managed: &std::path::Path,
    home: &std::path::Path,
    environment: &[(OsString, OsString)],
    destination: &str,
) -> Result<JournalSshCapture, ClientFailure> {
    let deadline = Instant::now() + DIAL_DEADLINE;
    // Freeze and inspect every Include before OpenSSH evaluates any Match.
    // Validation itself must not execute a mutable configuration's local code.
    let (_sources, managed) = configuration_snapshot(managed, home, deadline)?;
    let mut pending = vec![destination.to_owned()];
    let mut effective = Vec::new();
    let mut indices = BTreeMap::new();
    let mut bytes = 0usize;
    while effective.len() < pending.len() {
        if pending.len() > 8 {
            return Err(changed());
        }
        let destination = pending[effective.len()].clone();
        let (host, port) = if effective.is_empty() {
            // The top-level destination is literal OpenSSH argv. Only
            // ProxyJump entries use [account@]host[:port] jump syntax.
            (destination.as_str(), None)
        } else {
            jump_destination(&destination)?
        };
        let mut command = Command::new(executable);
        command.args([
            OsString::from("-G"),
            OsString::from("-F"),
            managed.as_os_str().to_owned(),
            OsString::from("-o"),
            OsString::from("BatchMode=yes"),
        ]);
        if let Some(port) = port {
            command.args(["-p", port]);
        }
        command
            .arg("--")
            .arg(host)
            .env_clear()
            .env("HOME", home)
            .envs(environment.iter().cloned())
            .current_dir(home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        let output = bounded_output(&mut command, deadline)?;
        bytes = bytes.checked_add(output.len()).ok_or_else(changed)?;
        if bytes > JournalSshCapture::MAX_BYTES {
            return Err(changed());
        }
        let output = String::from_utf8(output).map_err(|_| changed())?;
        let mut options = Vec::new();
        let mut jumps = Vec::new();
        for line in output.lines() {
            let (key, value) = line.split_once(' ').ok_or_else(changed)?;
            if !key.bytes().all(|b| b.is_ascii_alphanumeric())
                || value.is_empty()
                || value.bytes().any(|byte| byte.is_ascii_control())
            {
                return Err(changed());
            }
            match key.to_ascii_lowercase().as_str() {
                "host" | "localforward" | "remoteforward" | "dynamicforward" | "sendenv"
                | "setenv" | "remotecommand" => continue,
                "proxycommand" | "knownhostscommand" | "localcommand" if value != "none" => {
                    return Err(changed());
                }
                "canonicalizehostname" if !matches!(value, "false" | "no") => return Err(changed()),
                "proxyjump" if value != "none" => {
                    for jump in value.split(',') {
                        jump_destination(jump)?;
                        if jump == destination
                            || jumps.len() == 8
                            || jumps.iter().any(|old| old == jump)
                        {
                            return Err(changed());
                        }
                        jumps.push(jump.to_owned());
                        if !pending.iter().any(|entry| entry == jump) {
                            pending.push(jump.to_owned());
                        }
                    }
                    continue;
                }
                _ => {}
            }
            // Preserve OpenSSH's original-host token even though the immutable
            // configuration uses private aliases for resolved jump endpoints.
            let original_host = host.rsplit('@').next().ok_or_else(changed)?;
            options.push((key.to_owned(), value.replace("%n", original_host)));
        }
        for required in ["hostname", "user", "port"] {
            if options.iter().filter(|(key, _)| key == required).count() != 1 {
                return Err(changed());
            }
        }
        indices.insert(destination.clone(), effective.len());
        effective.push(Effective { options, jumps });
    }
    fn acyclic(
        index: usize,
        effective: &[Effective],
        indices: &BTreeMap<String, usize>,
        visits: &mut [u8],
    ) -> Result<(), ClientFailure> {
        match visits[index] {
            1 => return Err(changed()),
            2 => return Ok(()),
            _ => {}
        }
        visits[index] = 1;
        for jump in &effective[index].jumps {
            acyclic(
                *indices.get(jump).ok_or_else(changed)?,
                effective,
                indices,
                visits,
            )?;
        }
        visits[index] = 2;
        Ok(())
    }
    acyclic(0, &effective, &indices, &mut vec![0; effective.len()])?;
    let mut configuration = String::new();
    for (index, entry) in effective.iter().enumerate() {
        configuration.push_str(&format!("Host st-route-{index}\n"));
        for (key, value) in &entry.options {
            configuration.push_str(&format!(" {key} {value}\n"));
        }
        if !entry.jumps.is_empty() {
            let mut names = Vec::new();
            for jump in &entry.jumps {
                let index = indices.get(jump).ok_or_else(changed)?;
                names.push(format!("st-route-{index}"));
            }
            configuration.push_str(&format!(" ProxyJump {}\n", names.join(",")));
        }
    }
    JournalSshCapture::new(configuration)
}

/// Jump syntax is literal [account@]host[:port]. Arbitrary ProxyCommand and URI
/// routes are refused rather than reinterpreted or executed during a retry.
fn jump_destination(value: &str) -> Result<(&str, Option<&str>), ClientFailure> {
    SshDestination::new(value).map_err(classify)?;
    if value.contains('[') || value.contains(']') || value.contains('%') {
        return Err(changed());
    }
    if let Some((host, port)) = value.rsplit_once(':') {
        if host.contains(':') || port.parse::<u16>().ok().is_none_or(|port| port == 0) {
            return Err(changed());
        }
        Ok((host, Some(port)))
    } else {
        Ok((value, None))
    }
}

pub(super) struct ChildOwner(pub(super) Option<Child>);
impl ChildOwner {
    pub(super) fn exited(&self) -> Result<bool, ClientFailure> {
        let mut status = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: this unreaped child is still owned. WNOWAIT preserves its PID
        // reservation until the whole private process group has been retired.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.0.as_ref().ok_or_else(unavailable)?.id() as libc::id_t,
                status.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                Ok(false)
            } else {
                Err(unavailable())
            };
        }
        // SAFETY: successful waitid initialized status and its child PID field.
        Ok(unsafe { status.assume_init().si_pid() } != 0)
    }
    pub(super) fn retire(&mut self) {
        let Some(child) = self.0.as_ref() else {
            return;
        };
        // SAFETY: no code reaps this child before retiring its private group.
        // The still-owned PID cannot identify an unrelated replacement group.
        unsafe {
            libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
        }
    }
}
impl Drop for ChildOwner {
    fn drop(&mut self) {
        self.retire();
        if let Some(mut child) = self.0.take() {
            let _ = child.wait();
        }
    }
}
pub(super) fn bounded_output(
    command: &mut Command,
    deadline: Instant,
) -> Result<Vec<u8>, ClientFailure> {
    let mut child = ChildOwner(Some(
        spaceterm_host::process::spawn(command).map_err(|_| unavailable())?,
    ));
    let mut stdout = child
        .0
        .as_mut()
        .ok_or_else(unavailable)?
        .stdout
        .take()
        .ok_or_else(unavailable)?;
    let fd = stdout.as_raw_fd();
    // SAFETY: stdout owns this live descriptor. Only this owner reads it.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(unavailable());
    }
    let mut output = zeroize::Zeroizing::new(Vec::new());
    let mut buffer = zeroize::Zeroizing::new([0; 1024]);
    loop {
        match stdout.read(&mut *buffer) {
            Ok(0) => {
                if child.exited()? {
                    child.retire();
                    let status = child
                        .0
                        .as_mut()
                        .ok_or_else(unavailable)?
                        .wait()
                        .map_err(|_| unavailable())?;
                    child.0.take();
                    return if status.success() {
                        Ok(std::mem::take(&mut *output))
                    } else {
                        Err(changed())
                    };
                }
            }
            Ok(length) => {
                if output.len() + length > JournalSshCapture::MAX_BYTES {
                    return Err(changed());
                }
                output.extend_from_slice(&buffer[..length]);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return Err(unavailable()),
        }
        if Instant::now() >= deadline {
            return Err(unavailable());
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn configuration_snapshot(
    managed: &std::path::Path,
    home: &std::path::Path,
    deadline: Instant,
) -> Result<(tempfile::TempDir, PathBuf), ClientFailure> {
    struct Snapshot<'a> {
        directory: &'a std::path::Path,
        home: &'a std::path::Path,
        deadline: Instant,
        files: BTreeMap<PathBuf, PathBuf>,
        bytes: usize,
    }
    impl Snapshot<'_> {
        fn copy(
            &mut self,
            source: &std::path::Path,
            depth: usize,
        ) -> Result<PathBuf, ClientFailure> {
            if depth > 8 || Instant::now() >= self.deadline {
                return Err(changed());
            }
            let source = source.canonicalize().map_err(|_| changed())?;
            if let Some(path) = self.files.get(&source) {
                return Ok(path.clone());
            }
            if self.files.len() == 64 {
                return Err(changed());
            }
            let mut content = String::new();
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&source)
                .map_err(|_| changed())?;
            if !file.metadata().map_err(|_| changed())?.is_file() {
                return Err(changed());
            }
            file.take(256 * 1024 + 1)
                .read_to_string(&mut content)
                .map_err(|_| changed())?;
            self.bytes = self.bytes.checked_add(content.len()).ok_or_else(changed)?;
            if self.bytes > 256 * 1024 {
                return Err(changed());
            }
            let destination = self.directory.join(format!("config-{}", self.files.len()));
            self.files.insert(source, destination.clone());
            let mut frozen = String::new();
            for line in content.lines() {
                let trimmed = line.trim_start();
                let normalized = match trimmed
                    .find(|character: char| character.is_whitespace() || character == '=')
                {
                    Some(index) => {
                        let values = trimmed[index..].trim_start();
                        let values = values.strip_prefix('=').unwrap_or(values).trim_start();
                        format!("{} {}", &trimmed[..index], values)
                    }
                    _ => trimmed.to_owned(),
                };
                let words = shlex::split(&normalized).ok_or_else(changed)?;
                let Some(keyword) = words.first() else {
                    frozen.push('\n');
                    continue;
                };
                if keyword.eq_ignore_ascii_case("match")
                    && words[1..].iter().any(|word| {
                        word.trim_start_matches('!')
                            .split('=')
                            .next()
                            .is_some_and(|word| word.eq_ignore_ascii_case("exec"))
                    })
                {
                    return Err(changed());
                }
                if keyword.eq_ignore_ascii_case("include") {
                    for pattern in &words[1..] {
                        // These native Include forms need dynamic token expansion.
                        // Refuse them rather than approximate their route semantics.
                        if pattern.contains('%') || pattern.contains('$') {
                            return Err(changed());
                        }
                        let path = if let Some(relative) = pattern.strip_prefix("~/") {
                            self.home.join(relative)
                        } else if std::path::Path::new(pattern).is_absolute() {
                            PathBuf::from(pattern)
                        } else {
                            self.home.join(".ssh").join(pattern)
                        };
                        let pattern = path.to_str().ok_or_else(changed)?;
                        for included in glob::glob(pattern).map_err(|_| changed())? {
                            let included = included.map_err(|_| changed())?;
                            let copied = self.copy(&included, depth + 1)?;
                            let path = copied.to_str().ok_or_else(changed)?;
                            if path.chars().any(char::is_control) {
                                return Err(changed());
                            }
                            frozen.push_str(&format!(
                                "Include \"{}\"\n",
                                path.replace('\\', "\\\\").replace('\"', "\\\"")
                            ));
                        }
                    }
                } else {
                    frozen.push_str(line);
                    frozen.push('\n');
                }
            }
            std::fs::write(&destination, frozen).map_err(|_| unavailable())?;
            Ok(destination)
        }
    }
    let directory = tempfile::tempdir().map_err(|_| unavailable())?;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
        .map_err(|_| unavailable())?;
    let root = Snapshot {
        directory: directory.path(),
        home,
        deadline,
        files: BTreeMap::new(),
        bytes: 0,
    }
    .copy(managed, 0)?;
    Ok((directory, root))
}
