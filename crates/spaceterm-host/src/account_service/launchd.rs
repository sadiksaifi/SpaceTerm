use super::{
    LingerState, RegisteredServer, ServiceFailure, ServiceFuture, ServiceManager, SupervisorState,
    commands::{
        Account, ConfigurationFile, ServiceCommand, ServiceCommandRunner, ServiceInstallations,
        checked_executable, runner,
    },
};
use crate::startup::{BuildIdentity, DesiredServerState};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) fn label(identity: BuildIdentity) -> &'static str {
    match identity {
        BuildIdentity::Release => "io.github.sadiksaifi.spaceterm.server",
        BuildIdentity::Preflight => "io.github.sadiksaifi.spaceterm-preflight.server",
        BuildIdentity::Development => "io.github.sadiksaifi.spaceterm-development.server",
    }
}

pub(super) fn render(
    identity: BuildIdentity,
    desired: DesiredServerState,
    executable: &Path,
) -> Result<String, ServiceFailure> {
    let executable = checked_executable(executable)?;
    let escaped = executable
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;");
    let (load, keep) = match desired {
        DesiredServerState::Running => {
            ("<true/>", "<dict><key>SuccessfulExit</key><false/></dict>")
        }
        DesiredServerState::Stopped => ("<false/>", "<false/>"),
    };
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key><string>{}</string>\n  <key>ProgramArguments</key>\n  <array><string>{escaped}</string><string>serve</string></array>\n  <key>RunAtLoad</key>{load}\n  <key>KeepAlive</key>{keep}\n  <key>ProcessType</key><string>Background</string>\n</dict>\n</plist>\n",
        label(identity)
    ))
}

/// Login-session LaunchAgent manager. It never falls back to the system domain.
/// Executable approval belongs to the injected signed installation provider.
pub struct LaunchdServiceManager {
    pub(super) account: Account,
    pub(super) runner: Arc<dyn ServiceCommandRunner>,
    pub(super) installations: Arc<dyn ServiceInstallations>,
}
impl LaunchdServiceManager {
    /// Resolves the actual account and compiled identity without service effects.
    pub fn current(installations: Arc<dyn ServiceInstallations>) -> Result<Self, ServiceFailure> {
        if !cfg!(target_os = "macos") {
            return Err(ServiceFailure::UnsupportedHost);
        }
        Ok(Self {
            account: Account::current()?,
            runner: runner(),
            installations,
        })
    }
    fn domain(&self) -> String {
        format!("gui/{}", self.account.key.account_uid)
    }
    fn target(&self) -> String {
        format!(
            "{}/{}",
            self.domain(),
            label(self.account.key.build_identity)
        )
    }
    pub(super) fn path(&self) -> PathBuf {
        self.account
            .home
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", label(self.account.key.build_identity)))
    }
    async fn call(
        &self,
        args: Vec<std::ffi::OsString>,
    ) -> Result<super::commands::ServiceCommandOutput, ServiceFailure> {
        self.runner
            .run(ServiceCommand::new("/bin/launchctl", args))
            .await
    }
    async fn domain_available(&self) -> Result<(), ServiceFailure> {
        let reply = self
            .runner
            .run(
                ServiceCommand::new("/bin/launchctl", ["print".into(), self.domain().into()])
                    .discard_stdout(),
            )
            .await?;
        if reply.code == 0 {
            Ok(())
        } else {
            Err(ServiceFailure::LoginSessionUnavailable)
        }
    }
    async fn suppressed(&self) -> Result<bool, ServiceFailure> {
        let reply = self
            .call(vec!["print-disabled".into(), self.domain().into()])
            .await?;
        reply.success()?;
        disabled(&reply.bytes, label(self.account.key.build_identity))
    }
    pub(super) async fn start_service(&self) -> Result<(), ServiceFailure> {
        self.domain_available().await?;
        if self.suppressed().await? {
            return Err(ServiceFailure::Suppressed);
        }
        let reply = self
            .call(vec!["print".into(), self.target().into()])
            .await?;
        match reply.code {
            0 => {}
            // launchctl's ESRCH means the fixed service is not bootstrapped.
            3 | 113 => self
                .call(vec![
                    "bootstrap".into(),
                    self.domain().into(),
                    self.path().into_os_string(),
                ])
                .await?
                .success()?,
            _ => return Err(ServiceFailure::SupervisorUnavailable),
        }
        self.call(vec!["kickstart".into(), self.target().into()])
            .await?
            .success()
    }
    fn configuration(&self, create: bool) -> Result<Option<ConfigurationFile>, ServiceFailure> {
        ConfigurationFile::open(
            &self.account.home,
            &["Library", "LaunchAgents"],
            &format!("{}.plist", label(self.account.key.build_identity)),
            create,
        )
    }
}
impl ServiceManager for LaunchdServiceManager {
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
            self.domain_available().await?;
            if self.suppressed().await? {
                return Ok(
                    if registration.desired_state() == DesiredServerState::Stopped {
                        SupervisorState::Disabled
                    } else {
                        SupervisorState::Suppressed
                    },
                );
            }
            let output = self
                .call(vec!["print".into(), self.target().into()])
                .await?;
            if matches!(output.code, 3 | 113) {
                return Ok(
                    if registration.desired_state() == DesiredServerState::Stopped {
                        SupervisorState::Disabled
                    } else {
                        SupervisorState::Inactive
                    },
                );
            }
            output.success()?;
            state(&output.bytes)
        })
    }
    fn scoped_processes<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ServiceFuture<'a, Vec<u32>> {
        Box::pin(async move {
            self.account.check(registration)?;
            self.domain_available().await?;
            let output = self
                .call(vec!["print".into(), self.target().into()])
                .await?;
            if matches!(output.code, 3 | 113) {
                return Ok(Vec::new());
            }
            output.success()?;
            process_scope(&output.bytes)
        })
    }
    fn linger(&self) -> ServiceFuture<'_, Option<LingerState>> {
        Box::pin(async { Ok(None) })
    }
    fn configure<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.account.check(registration)?;
            self.domain_available().await?;
            if registration.desired_state() == DesiredServerState::Stopped {
                // Suppression precedes graceful retirement and never loads unavailable code.
                return self
                    .call(vec!["disable".into(), self.target().into()])
                    .await?
                    .success();
            }
            let path = self.installations.approved_executable(registration)?;
            let content = render(
                self.account.key.build_identity,
                registration.desired_state(),
                &path,
            )?;
            self.configuration(true)?
                .ok_or(ServiceFailure::StorageUnavailable)?
                .publish(content.as_bytes())?;
            self.call(vec!["enable".into(), self.target().into()])
                .await?
                .success()
        })
    }
    fn start<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.account.check(registration)?;
            if registration.desired_state() != DesiredServerState::Running {
                return Err(ServiceFailure::RequestConflict);
            }
            self.verify_installation(registration)?;
            self.start_service().await
        })
    }
    fn stop<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.account.check(registration)?;
            self.domain_available().await?;
            let output = self
                .call(vec!["bootout".into(), self.target().into()])
                .await?;
            if matches!(output.code, 3 | 113) {
                Ok(())
            } else {
                output.success()
            }
        })
    }
    fn uninstall<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.account.check(registration)?;
            if let Some(file) = self.configuration(false)? {
                file.remove()?;
            }
            self.installations.uninstall(registration)
        })
    }
    fn verify_installation(&self, registration: &RegisteredServer) -> Result<(), ServiceFailure> {
        self.account.check(registration)?;
        checked_executable(&self.installations.approved_executable(registration)?)?;
        Ok(())
    }
}

pub(super) fn state(bytes: &[u8]) -> Result<SupervisorState, ServiceFailure> {
    if bytes.len() > 16 * 1024 {
        return Err(ServiceFailure::LimitExceeded);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ServiceFailure::SupervisorUnavailable)?;
    // print nests environment and other dictionaries. Only the first-level state
    // of the requested service is an observation; an environment value grants none.
    let mut depth = 0usize;
    let mut result = None;
    for line in text.lines() {
        let line = line.trim();
        if depth == 1
            && let Some(value) = line.strip_prefix("state = ")
        {
            if result.is_some() {
                return Err(ServiceFailure::SupervisorUnavailable);
            }
            result = Some(match value {
                "running" => SupervisorState::Running,
                "spawn scheduled" | "waiting" => SupervisorState::Starting,
                "exited" | "not running" => SupervisorState::Inactive,
                _ => return Err(ServiceFailure::SupervisorUnavailable),
            });
        }
        if line.ends_with('{') {
            depth += 1;
        }
        if line == "}" {
            depth = depth
                .checked_sub(1)
                .ok_or(ServiceFailure::SupervisorUnavailable)?;
        }
    }
    result.ok_or(ServiceFailure::SupervisorUnavailable)
}

// Only the requested label in the account domain's disabled dictionary is authority.
// Nested values and other services never classify this LaunchAgent.
fn disabled(bytes: &[u8], target: &str) -> Result<bool, ServiceFailure> {
    if bytes.len() > 16 * 1024 {
        return Err(ServiceFailure::LimitExceeded);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ServiceFailure::SupervisorUnavailable)?;
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    if lines.next() != Some("disabled services = {") {
        return Err(ServiceFailure::SupervisorUnavailable);
    }
    let prefix = format!("\"{target}\" => ");
    let mut result = None;
    let mut depth = 1usize;
    let mut closed = false;
    for line in lines {
        if closed {
            return Err(ServiceFailure::SupervisorUnavailable);
        }
        if depth == 1
            && let Some(value) = line.strip_prefix(&prefix)
        {
            if result.is_some() {
                return Err(ServiceFailure::SupervisorUnavailable);
            }
            result = Some(match value {
                "true" | "disabled" => true,
                "false" | "enabled" => false,
                _ => return Err(ServiceFailure::SupervisorUnavailable),
            });
        }
        if line.ends_with('{') {
            depth += 1;
        }
        if line == "}" {
            depth = depth
                .checked_sub(1)
                .ok_or(ServiceFailure::SupervisorUnavailable)?;
            closed = depth == 0;
        }
    }
    if !closed {
        return Err(ServiceFailure::SupervisorUnavailable);
    }
    Ok(result.unwrap_or(false))
}

fn process_scope(bytes: &[u8]) -> Result<Vec<u32>, ServiceFailure> {
    let observed_state = state(bytes)?;
    let text = std::str::from_utf8(bytes).map_err(|_| ServiceFailure::CleanupUnresolved)?;
    let mut depth = 0usize;
    let mut pid = None;
    for line in text.lines().map(str::trim) {
        if depth == 1
            && let Some(value) = line.strip_prefix("pid = ")
        {
            if pid.is_some() {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            pid = Some(
                value
                    .parse::<u32>()
                    .ok()
                    .filter(|pid| *pid != 0 && *pid <= i32::MAX as u32)
                    .ok_or(ServiceFailure::CleanupUnresolved)?,
            );
        }
        if line.ends_with('{') {
            depth += 1;
        }
        if line == "}" {
            depth = depth
                .checked_sub(1)
                .ok_or(ServiceFailure::CleanupUnresolved)?;
        }
    }
    if depth != 0 || (observed_state == SupervisorState::Running && pid.is_none()) {
        return Err(ServiceFailure::CleanupUnresolved);
    }
    Ok(pid.into_iter().collect())
}
