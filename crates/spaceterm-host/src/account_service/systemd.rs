use super::{
    LingerState, RegisteredServer, ServiceFailure, ServiceFuture, ServiceManager, SupervisorState,
    commands::{
        Account, ConfigurationFile, ServiceCommand, ServiceCommandOutput, ServiceCommandRunner,
        ServiceInstallations, checked_executable, runner,
    },
};
use crate::startup::{BuildIdentity, DesiredServerState};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

pub(super) fn unit(identity: BuildIdentity) -> &'static str {
    match identity {
        BuildIdentity::Release => "spaceterm-server.service",
        BuildIdentity::Preflight => "spaceterm-preflight-server.service",
        BuildIdentity::Development => "spaceterm-development-server.service",
    }
}
pub(super) fn render(
    identity: BuildIdentity,
    desired: DesiredServerState,
    executable: &Path,
) -> Result<String, ServiceFailure> {
    let executable = checked_executable(executable)?;
    // systemd 249's string_is_safe rejects quotes/backslashes after unescaping
    // ExecStart. Quoting cannot make these executable names admissible.
    if executable.contains(['"', '\'', '\\']) {
        return Err(ServiceFailure::InvalidRecord);
    }
    // ':' disables argv environment expansion. The executable path is resolved
    // separately by systemd and must retain literal dollars, not doubled dollars.
    let escaped = executable.replace('%', "%%");
    let description = match identity {
        BuildIdentity::Release => "SpaceTerm Server",
        BuildIdentity::Preflight => "SpaceTerm Preflight Server",
        BuildIdentity::Development => "SpaceTerm Development Server",
    };
    let restart = match desired {
        DesiredServerState::Running => "on-failure",
        DesiredServerState::Stopped => "no",
    };
    // Signal only the main process so normal Stop can drain started Git writes.
    // Unlike mixed/control-group with SendSIGKILL=no, process mode lets a new
    // main process start after a crash with surviving children. Startup must
    // complete exact previous-instance cleanup before readiness.
    Ok(format!(
        "[Unit]\nDescription={description}\nStartLimitIntervalSec=0\n\n[Service]\nType=simple\nExecStart=:\"{escaped}\" serve\nRestart={restart}\nRestartSec=2s\nKillSignal=SIGTERM\nKillMode=process\nSendSIGKILL=no\nTimeoutStopSec=infinity\n\n[Install]\nWantedBy=default.target\n"
    ))
}

#[derive(Clone, Copy)]
pub(super) enum Operation {
    Reload,
    Enable,
    Disable,
    Start,
    Stop,
    Show,
    UnitPath,
    ProcessScope,
}

/// systemd user-service manager with account-resolved bus reachability.
/// It does not use caller DBUS, HOME or XDG overrides and never invokes system units.
pub struct SystemdServiceManager {
    pub(super) account: Account,
    pub(super) runner: Arc<dyn ServiceCommandRunner>,
    pub(super) installations: Arc<dyn ServiceInstallations>,
    pub(super) configuration_root: OnceLock<PathBuf>,
}
impl SystemdServiceManager {
    /// Resolves the actual account and compiled identity without service effects.
    pub fn current(installations: Arc<dyn ServiceInstallations>) -> Result<Self, ServiceFailure> {
        if !cfg!(target_os = "linux") {
            return Err(ServiceFailure::UnsupportedHost);
        }
        Ok(Self {
            account: Account::current()?,
            runner: runner(),
            installations,
            configuration_root: OnceLock::new(),
        })
    }
    pub(super) fn command(&self, operation: Operation) -> ServiceCommand {
        let mut args = vec![
            "--user".into(),
            "--no-pager".into(),
            "--no-ask-password".into(),
        ];
        let name = unit(self.account.key.build_identity);
        let suffix: Vec<_> = match operation {
            Operation::Reload => vec!["daemon-reload"],
            Operation::Enable => vec!["enable", "--", name],
            Operation::Disable => vec!["disable", "--", name],
            Operation::Start => vec!["--no-block", "start", "--", name],
            Operation::Stop => vec!["--no-block", "stop", "--", name],
            Operation::Show => vec![
                "show",
                "--property=LoadState,ActiveState,SubState,UnitFileState",
                "--",
                name,
            ],
            Operation::ProcessScope => vec![
                "show",
                "--property=LoadState,ActiveState,MainPID,ControlGroup",
                "--",
                name,
            ],
            Operation::UnitPath => vec!["show", "--property=UnitPath", "--value"],
        };
        args.extend(suffix.into_iter().map(Into::into));
        let mut command =
            ServiceCommand::new("/usr/bin/systemctl", args).user_manager(&self.account);
        if let Some(root) = self.configuration_root.get() {
            command.manager_configuration(root);
        }
        command
    }
    pub(super) async fn call(
        &self,
        operation: Operation,
    ) -> Result<ServiceCommandOutput, ServiceFailure> {
        self.runner.run(self.command(operation)).await
    }
    async fn stop_or_disable(&self, operation: Operation) -> Result<(), ServiceFailure> {
        let reply = self.call(operation).await?;
        if reply.code == 0 {
            return Ok(());
        }
        let observation = self.call(Operation::Show).await?;
        if state(observation.code, &observation.bytes)? == SupervisorState::NotInstalled {
            Ok(())
        } else {
            Err(ServiceFailure::SupervisorUnavailable)
        }
    }
    fn check_runtime(&self) -> Result<(), ServiceFailure> {
        // The user manager's runtime root is fixed by logind, not a caller's environment.
        crate::private_filesystem::Directory::open(&self.account.runtime)?
            .ok_or(ServiceFailure::SupervisorUnavailable)?
            .verify()?;
        Ok(())
    }
    async fn resolve_configuration(&self) -> Result<(), ServiceFailure> {
        if self.configuration_root.get().is_some() {
            return Ok(());
        }
        let reply = self.call(Operation::UnitPath).await?;
        reply.success()?;
        let text =
            std::str::from_utf8(&reply.bytes).map_err(|_| ServiceFailure::SupervisorUnavailable)?;
        let paths = unit_paths(text)?;
        // UnitPath orders the manager's user configuration before data/search
        // directories. Its user.control entry identifies that configuration root
        // even when the root is not yet present. Runtime overrides are not config.
        let control = paths.iter().map(Path::new).find(|path| {
            path.ends_with("systemd/user.control") && !path.starts_with(&self.account.runtime)
        });
        let directory = if let Some(control) = control {
            let directory = control.with_file_name("user");
            if !paths.iter().any(|path| Path::new(path) == directory) {
                return Err(ServiceFailure::SupervisorUnavailable);
            }
            directory
        } else {
            paths
                .iter()
                .map(Path::new)
                .find(|path| {
                    path.ends_with("systemd/user")
                        && !path.starts_with(&self.account.runtime)
                        && path.parent().and_then(Path::parent).is_some_and(|root| {
                            root.starts_with(&self.account.home)
                                || std::fs::symlink_metadata(root)
                                    .is_ok_and(|facts| facts.uid() == self.account.key.account_uid)
                        })
                })
                .ok_or(ServiceFailure::SupervisorUnavailable)?
                .to_owned()
        };
        let root = directory
            .parent()
            .and_then(Path::parent)
            .ok_or(ServiceFailure::SupervisorUnavailable)?;
        self.configuration_at(root, false)?;
        // A concurrent observation cannot replace the first admitted manager fact.
        self.configuration_root.get_or_init(|| root.to_owned());
        Ok(())
    }
    fn configuration_at(
        &self,
        root: &Path,
        create: bool,
    ) -> Result<Option<ConfigurationFile>, ServiceFailure> {
        let (anchor, relative) = match root.strip_prefix(&self.account.home) {
            Ok(relative) => (self.account.home.as_path(), relative),
            Err(_) => (root, Path::new("")),
        };
        let mut components = relative
            .iter()
            .map(|part| part.to_str().ok_or(ServiceFailure::InvalidRecord))
            .collect::<Result<Vec<_>, _>>()?;
        components.extend(["systemd", "user"]);
        ConfigurationFile::open(
            anchor,
            &components,
            unit(self.account.key.build_identity),
            create,
        )
    }
    fn configuration(&self, create: bool) -> Result<Option<ConfigurationFile>, ServiceFailure> {
        self.configuration_at(
            self.configuration_root
                .get()
                .ok_or(ServiceFailure::SupervisorUnavailable)?,
            create,
        )
    }
}
impl ServiceManager for SystemdServiceManager {
    fn status<'a>(
        &'a self,
        registration: Option<&'a RegisteredServer>,
    ) -> ServiceFuture<'a, SupervisorState> {
        Box::pin(async move {
            let Some(registration) = registration else {
                return Ok(SupervisorState::NotInstalled);
            };
            self.account.check(registration)?;
            if !registration.is_installed() {
                return Ok(SupervisorState::NotInstalled);
            }
            self.check_runtime()?;
            let output = self.call(Operation::Show).await?;
            state(output.code, &output.bytes)
        })
    }
    fn scoped_processes<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ServiceFuture<'a, Vec<u32>> {
        Box::pin(async move {
            self.account.check(registration)?;
            self.check_runtime()?;
            let output = self.call(Operation::ProcessScope).await?;
            let (main, cgroup) = process_scope(&output.bytes)?;
            // systemctl show uses exit 4 for a confirmed nonexistent unit.
            // Other failures, including loss of the user bus, prove no absence.
            if output.code != 0
                && !(output.code == 4
                    && std::str::from_utf8(&output.bytes)
                        .is_ok_and(|text| text.lines().any(|line| line == "LoadState=not-found")))
            {
                return Err(ServiceFailure::SupervisorUnavailable);
            }
            #[cfg(target_os = "linux")]
            {
                super::native_processes::systemd_scope(main, cgroup)
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (main, cgroup);
                Err(ServiceFailure::UnsupportedHost)
            }
        })
    }
    fn linger(&self) -> ServiceFuture<'_, Option<LingerState>> {
        Box::pin(async move {
            super::linger::observe(self.runner.as_ref(), self.account.key.account_uid)
                .await
                .map(Some)
        })
    }
    fn configure<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.account.check(registration)?;
            self.check_runtime()?;
            self.resolve_configuration().await?;
            if registration.desired_state() == DesiredServerState::Stopped {
                return self.stop_or_disable(Operation::Disable).await;
            }
            let executable = self.installations.approved_executable(registration)?;
            let content = render(
                self.account.key.build_identity,
                registration.desired_state(),
                &executable,
            )?;
            self.configuration(true)?
                .ok_or(ServiceFailure::StorageUnavailable)?
                .publish(content.as_bytes())?;
            self.call(Operation::Reload).await?.success()?;
            self.call(Operation::Enable).await?.success()
        })
    }
    fn start<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.account.check(registration)?;
            if registration.desired_state() != DesiredServerState::Running {
                return Err(ServiceFailure::RequestConflict);
            }
            self.check_runtime()?;
            self.verify_installation(registration)?;
            self.call(Operation::Start).await?.success()
        })
    }
    fn stop<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.account.check(registration)?;
            self.check_runtime()?;
            self.stop_or_disable(Operation::Stop).await
        })
    }
    fn uninstall<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.account.check(registration)?;
            self.check_runtime()?;
            self.resolve_configuration().await?;
            self.stop_or_disable(Operation::Disable).await?;
            if let Some(file) = self.configuration(false)? {
                file.remove()?;
            }
            self.call(Operation::Reload).await?.success()?;
            self.installations.uninstall(registration)
        })
    }
    fn verify_installation(&self, registration: &RegisteredServer) -> Result<(), ServiceFailure> {
        self.account.check(registration)?;
        checked_executable(&self.installations.approved_executable(registration)?)?;
        Ok(())
    }
}

pub(super) fn state(code: i32, bytes: &[u8]) -> Result<SupervisorState, ServiceFailure> {
    if bytes.len() > 16 * 1024 {
        return Err(ServiceFailure::LimitExceeded);
    }
    if !matches!(code, 0 | 4) {
        return Err(ServiceFailure::SupervisorUnavailable);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ServiceFailure::SupervisorUnavailable)?;
    let mut values = [None; 4];
    for line in text.lines() {
        let (key, value) = line
            .split_once('=')
            .ok_or(ServiceFailure::SupervisorUnavailable)?;
        let index = match key {
            "LoadState" => 0,
            "ActiveState" => 1,
            "SubState" => 2,
            "UnitFileState" => 3,
            _ => return Err(ServiceFailure::SupervisorUnavailable),
        };
        if values[index].replace(value).is_some() {
            return Err(ServiceFailure::SupervisorUnavailable);
        }
    }
    let [Some(load), Some(active), Some(sub), Some(enabled)] = values else {
        return Err(ServiceFailure::SupervisorUnavailable);
    };
    match load {
        "not-found"
            if matches!(active, "inactive" | "failed") && matches!(sub, "dead" | "failed") =>
        {
            return Ok(SupervisorState::NotInstalled);
        }
        "not-found" => {}
        "masked" => return Ok(SupervisorState::Suppressed),
        "loaded" if code == 0 => {}
        _ => return Err(ServiceFailure::SupervisorUnavailable),
    }
    match (active, sub) {
        ("active", "running") => Ok(SupervisorState::Running),
        ("activating", _) => Ok(SupervisorState::Starting),
        ("deactivating", _) => Ok(SupervisorState::Stopping),
        ("inactive" | "failed", _) if enabled == "disabled" => Ok(SupervisorState::Disabled),
        ("inactive" | "failed", _)
            if matches!(enabled, "enabled" | "enabled-runtime" | "static") =>
        {
            Ok(SupervisorState::Inactive)
        }
        _ => Err(ServiceFailure::SupervisorUnavailable),
    }
}

fn unit_paths(text: &str) -> Result<Vec<String>, ServiceFailure> {
    if text.len() > 16 * 1024 {
        return Err(ServiceFailure::LimitExceeded);
    }
    // systemctl's strv printer uses shell_maybe_quote(str, 0): double quotes,
    // shell-special backslash escapes and C/octal control escapes. POSIX shlex
    // does not decode that format. This bounded decoder never invokes a shell.
    let mut input = text.bytes().peekable();
    let mut result = Vec::new();
    loop {
        while input.peek().is_some_and(u8::is_ascii_whitespace) {
            input.next();
        }
        if input.peek().is_none() {
            return Ok(result);
        }
        if result.len() == 256 {
            return Err(ServiceFailure::LimitExceeded);
        }
        let quoted = input.peek() == Some(&b'"');
        if quoted {
            input.next();
        }
        let mut closed = !quoted;
        let mut bytes = Vec::new();
        while let Some(byte) = input.next() {
            match byte {
                b'"' if quoted => {
                    closed = true;
                    break;
                }
                b'"' => return Err(ServiceFailure::SupervisorUnavailable),
                byte if !quoted && byte.is_ascii_whitespace() => break,
                b'\\' => {
                    let escaped = input.next().ok_or(ServiceFailure::SupervisorUnavailable)?;
                    let byte = match escaped {
                        b'\\' | b'"' | b'`' | b'$' => escaped,
                        b'x' => {
                            let a = input.next().and_then(|b| (b as char).to_digit(16));
                            let b = input.next().and_then(|b| (b as char).to_digit(16));
                            let (Some(a), Some(b)) = (a, b) else {
                                return Err(ServiceFailure::SupervisorUnavailable);
                            };
                            (a * 16 + b) as u8
                        }
                        b'0'..=b'3' => {
                            let a = input.next().and_then(|b| (b as char).to_digit(8));
                            let b = input.next().and_then(|b| (b as char).to_digit(8));
                            let (Some(a), Some(b)) = (a, b) else {
                                return Err(ServiceFailure::SupervisorUnavailable);
                            };
                            ((escaped - b'0') as u32 * 64 + a * 8 + b) as u8
                        }
                        // All named C escapes encode controls rejected by validate_path.
                        _ => return Err(ServiceFailure::SupervisorUnavailable),
                    };
                    bytes.push(byte);
                }
                byte => bytes.push(byte),
            }
        }
        if !closed || (quoted && input.peek().is_some_and(|b| !b.is_ascii_whitespace())) {
            return Err(ServiceFailure::SupervisorUnavailable);
        }
        let path = String::from_utf8(bytes).map_err(|_| ServiceFailure::SupervisorUnavailable)?;
        super::registration::validate_path(Path::new(&path))
            .map_err(|_| ServiceFailure::SupervisorUnavailable)?;
        result.push(path);
    }
}

fn process_scope(bytes: &[u8]) -> Result<(u32, &str), ServiceFailure> {
    if bytes.len() > 16 * 1024 {
        return Err(ServiceFailure::LimitExceeded);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ServiceFailure::CleanupUnresolved)?;
    let mut load = None;
    let mut active = None;
    let mut main = None;
    let mut group = None;
    for line in text.lines() {
        let (key, value) = line
            .split_once('=')
            .ok_or(ServiceFailure::CleanupUnresolved)?;
        let slot = match key {
            "LoadState" => &mut load,
            "ActiveState" => &mut active,
            "MainPID" => &mut main,
            "ControlGroup" => &mut group,
            _ => return Err(ServiceFailure::CleanupUnresolved),
        };
        if slot.replace(value).is_some() {
            return Err(ServiceFailure::CleanupUnresolved);
        }
    }
    let load = load.ok_or(ServiceFailure::CleanupUnresolved)?;
    let active = active.ok_or(ServiceFailure::CleanupUnresolved)?;
    let main = main
        .and_then(|pid| pid.parse::<u32>().ok())
        .filter(|pid| *pid <= i32::MAX as u32)
        .ok_or(ServiceFailure::CleanupUnresolved)?;
    let group = group.ok_or(ServiceFailure::CleanupUnresolved)?;
    if !matches!(load, "loaded" | "not-found")
        || !matches!(
            active,
            "active" | "inactive" | "failed" | "activating" | "deactivating"
        )
        || (load == "not-found" && (main != 0 || !group.is_empty() || active != "inactive"))
        || (matches!(active, "active" | "activating" | "deactivating")
            && main == 0
            && group.is_empty())
    {
        return Err(ServiceFailure::CleanupUnresolved);
    }
    Ok((main, group))
}
