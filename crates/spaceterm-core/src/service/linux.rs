//! The owning account's systemd user manager and scoped logind authorization.
use super::*;
use std::os::unix::fs::DirBuilderExt;

const UNIT_PROPERTIES: &str = "--property=LoadState,ActiveState,Result,MainPID,TasksCurrent,FragmentPath,DropInPaths,NeedDaemonReload";

pub(super) struct UserService {
    directory: PathBuf,
    name: String,
    identity: String,
    uid: u32,
}
impl UserService {
    pub(super) fn new(paths: &Paths, identity: &str) -> Result<Self, CoreError> {
        let home = paths
            .data
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or(CoreError::StorageUnavailable)?;
        Ok(Self {
            directory: home.join(".config/systemd/user"),
            name: format!("io.github.sadiksaifi.{identity}.core.service"),
            identity: identity.into(),
            // SAFETY: geteuid has no preconditions; Paths rejects root and mismatched accounts.
            uid: unsafe { libc::geteuid() },
        })
    }
    fn registration(&self) -> registration::Registration {
        registration::Registration {
            directory: self.directory.clone(),
            name: self.name.clone(),
        }
    }
    /// The executable of `runtime`, quoted for an `ExecStart` line.
    fn exec(&self, runtime: &Path) -> Result<String, CoreError> {
        let executable = runtime.join(
            super::runtime::Layout::host(&self.identity)
                .ok_or(CoreError::RuntimeUnavailable)?
                .executable(),
        );
        let executable = executable.to_str().ok_or(CoreError::RuntimeUnavailable)?;
        // Unit strings have C escapes, percent specifiers, and (in ExecStart) dollar expansion.
        let mut quoted = String::new();
        for byte in executable.bytes() {
            match byte {
                b'%' => quoted.push_str("%%"),
                b'$' => quoted.push_str("$$"),
                b'\\' => quoted.push_str("\\\\"),
                b'"' => quoted.push_str("\\\""),
                0x20..=0x7e => quoted.push(char::from(byte)),
                value => {
                    use std::fmt::Write;
                    let _ = write!(quoted, "\\x{value:02x}");
                }
            }
        }
        Ok(quoted)
    }
    /// The recovery entry's unit name, beside the server's.
    fn recovery_name(&self) -> String {
        format!(
            "io.github.sadiksaifi.{}.core-recovery.service",
            self.identity
        )
    }
    /// A oneshot unit that runs `runtime`'s recovery helper at the next start of the user
    /// manager. The helper may wait for a readiness deadline and a return, so no start timeout
    /// applies.
    fn recovery_contents(&self, runtime: &Path) -> Result<Vec<u8>, CoreError> {
        let quoted = self.exec(runtime)?;
        Ok(format!("[Unit]\nDescription=SpaceTerm Core update recovery ({})\n\n[Service]\nType=oneshot\nExecStart=\"{quoted}\" internal helper --identity {}\nTimeoutStartSec=infinity\nUMask=0077\n\n[Install]\nWantedBy=default.target\n", self.identity, self.identity).into_bytes())
    }
    fn contents(&self, runtime: &Path) -> Result<Vec<u8>, CoreError> {
        let quoted = self.exec(runtime)?;
        // Only the main process gets SIGTERM. It closes admissions, drains started Git writes,
        // and shuts down its Terminal Sessions before exiting. No manager SIGKILL bypasses that.
        Ok(format!("[Unit]\nDescription=SpaceTerm Core ({})\nStartLimitIntervalSec=60\nStartLimitBurst=3\n\n[Service]\nType=exec\nExecStart=\"{quoted}\" internal serve --service --identity {}\nRestart=on-failure\nRestartSec=2\nTimeoutStopSec=infinity\nKillMode=mixed\nSendSIGKILL=no\nUMask=0077\n\n[Install]\nWantedBy=default.target\n", self.identity, self.identity).into_bytes())
    }
    fn manager(&self, program: &str, args: &[OsString]) -> Result<Vec<u8>, CoreError> {
        let mut command = Command::new(program);
        command
            .arg("--user")
            .args(args)
            .env("XDG_RUNTIME_DIR", format!("/run/user/{}", self.uid))
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path=/run/user/{}/bus", self.uid),
            );
        execute(&mut command)
    }
    fn systemctl(&self, args: &[&str]) -> Result<Vec<u8>, CoreError> {
        let mut arguments = vec!["--no-pager".into(), "--no-ask-password".into()];
        arguments.extend(args.iter().map(OsString::from));
        self.manager("/usr/bin/systemctl", &arguments)
    }
    fn observation(&self) -> Result<Vec<(String, String)>, CoreError> {
        parse_observation(&self.systemctl(&["show", &self.name, UNIT_PROPERTIES])?)
    }
    fn linger(&self) -> Result<(), CoreError> {
        let bytes = command(
            "/usr/bin/loginctl",
            &[
                "show-user".into(),
                self.uid.to_string().into(),
                "--property=Linger".into(),
                "--value".into(),
            ],
        )?;
        match bytes.as_slice() {
            b"yes\n" => Ok(()),
            b"no\n" => Err(CoreError::AuthorizationRequired),
            _ => Err(CoreError::ServiceUnavailable),
        }
    }
    /// Publish or remove just our expected startup link, preserving changed or foreign entries.
    fn startup(&self, enabled: bool) -> Result<(), CoreError> {
        let path = self.directory.join("default.target.wants");
        if !enabled
            && !path
                .try_exists()
                .map_err(|_| CoreError::StorageUnavailable)?
        {
            return Ok(());
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&path)
            .map_err(|_| CoreError::StorageUnavailable)?;
        let directory = owned_directory(&path)?;
        let name = CString::new(self.name.as_bytes()).map_err(|_| CoreError::UnsafeStorage)?;
        let target = self.registration().path();
        let target =
            CString::new(target.as_os_str().as_bytes()).map_err(|_| CoreError::UnsafeStorage)?;
        let mut before: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: live directory, terminated name, and correctly sized output are retained.
        let found = unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                name.as_ptr(),
                &mut before,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0;
        if !found {
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound {
                return Err(CoreError::StorageUnavailable);
            }
            if !enabled {
                return Ok(());
            }
            verify_directory_path(&directory, &path)?;
            // SAFETY: symlinkat exclusively creates a named link without following or replacing it.
            if unsafe { libc::symlinkat(target.as_ptr(), directory.as_raw_fd(), name.as_ptr()) }
                != 0
            {
                return Err(CoreError::CleanupUnresolved);
            }
        } else {
            if before.st_mode & libc::S_IFMT != libc::S_IFLNK
                || before.st_uid != self.uid
                || before.st_nlink != 1
            {
                return Err(CoreError::CleanupUnresolved);
            }
            let mut bytes = vec![0u8; 8192];
            // SAFETY: writable buffer and directory/name remain live for readlinkat.
            let count = unsafe {
                libc::readlinkat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if count < 0 || bytes.get(..count as usize) != Some(target.as_bytes()) {
                return Err(CoreError::CleanupUnresolved);
            }
            if enabled {
                return Ok(());
            }
            verify_directory_path(&directory, &path)?;
            let mut now: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: revalidate the exact link immediately before removing its directory entry.
            if unsafe {
                libc::fstatat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    &mut now,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
                || (before.st_dev, before.st_ino) != (now.st_dev, now.st_ino)
            {
                return Err(CoreError::CleanupUnresolved);
            }
            // SAFETY: unlinkat removes only the revalidated expected symlink beneath our directory.
            if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(CoreError::StorageUnavailable);
            }
        }
        directory
            .sync_all()
            .map_err(|_| CoreError::StorageUnavailable)
    }
}
fn parse_observation(bytes: &[u8]) -> Result<Vec<(String, String)>, CoreError> {
    let text = std::str::from_utf8(bytes).map_err(|_| CoreError::ServiceUnavailable)?;
    text.lines()
        .map(|line| {
            line.split_once('=')
                .map(|(key, value)| (key.into(), value.into()))
                .ok_or(CoreError::ServiceUnavailable)
        })
        .collect()
}
fn property<'a>(observation: &'a [(String, String)], key: &str) -> Result<&'a str, CoreError> {
    observation
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
        .ok_or(CoreError::ServiceUnavailable)
}

fn loaded_definition(state: &[(String, String)], registration: &Path) -> Result<bool, CoreError> {
    if property(state, "LoadState")? == "not-found" {
        return Ok(false);
    }
    if property(state, "LoadState")? != "loaded"
        || Path::new(property(state, "FragmentPath")?) != registration
        || !property(state, "DropInPaths")?.is_empty()
        || property(state, "NeedDaemonReload")? != "no"
    {
        return Err(CoreError::CleanupUnresolved);
    }
    Ok(true)
}

/// Start after the owning service has validated its registration, runtime and account authority.
fn start_unit(
    name: &str,
    registration: &Path,
    mut systemctl: impl FnMut(&[&str]) -> Result<Vec<u8>, CoreError>,
) -> Result<(), CoreError> {
    let state = parse_observation(&systemctl(&["show", name, UNIT_PROPERTIES])?)?;
    if loaded_definition(&state, registration)?
        && (property(&state, "ActiveState")? == "failed"
            || property(&state, "Result")? == "start-limit-hit")
    {
        // Inactive units can be garbage-collected before reset-failed, which then refuses an
        // unloaded unit. Reset only an observed failure; Start loads fresh registrations itself.
        systemctl(&["reset-failed", name])?;
    }
    systemctl(&["start", name])?;
    Ok(())
}

fn linger_command(uid: u32, authorization: LingerAuthorization) -> Command {
    let mut process = Command::new(if authorization == LingerAuthorization::Desktop {
        "/usr/bin/pkexec"
    } else {
        "/usr/bin/sudo"
    });
    process.args(["/usr/bin/loginctl", "enable-linger", &uid.to_string()]);
    linger_stdio(&mut process, authorization);
    process
}

fn linger_stdio(process: &mut Command, authorization: LingerAuthorization) {
    if authorization == LingerAuthorization::Desktop {
        process
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    }
}

impl Supervisor for UserService {
    fn arm_recovery(&self, runtime: &Path) -> Result<(), CoreError> {
        let unit = self.directory.join(self.recovery_name());
        super::publish_entry(&unit, &self.recovery_contents(runtime)?)?;
        let wants = self.directory.join("default.target.wants");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&wants)
            .map_err(|_| CoreError::StorageUnavailable)?;
        let link = wants.join(self.recovery_name());
        if std::fs::read_link(&link).ok().as_deref() != Some(unit.as_path()) {
            super::remove_entry(&link)?;
            std::os::unix::fs::symlink(&unit, &link).map_err(|_| CoreError::StorageUnavailable)?;
        }
        self.systemctl(&["daemon-reload"])?;
        Ok(())
    }
    fn disarm_recovery(&self) -> Result<(), CoreError> {
        super::remove_entry(
            &self
                .directory
                .join("default.target.wants")
                .join(self.recovery_name()),
        )?;
        super::remove_entry(&self.directory.join(self.recovery_name()))?;
        self.systemctl(&["daemon-reload"])?;
        Ok(())
    }
    fn replace(&self, previous: &Path, candidate: &Path) -> Result<(), CoreError> {
        self.registration()
            .check(&self.contents(previous)?, false)
            .or_else(|_| self.registration().check(&self.contents(candidate)?, false))?;
        if self.running()? {
            return Err(CoreError::ActivationUnresolved);
        }
        let observation = self.observation()?;
        if property(&observation, "LoadState")? != "not-found"
            && (property(&observation, "LoadState")? != "loaded"
                || Path::new(property(&observation, "FragmentPath")?) != self.registration().path()
                || !property(&observation, "DropInPaths")?.is_empty())
        {
            return Err(CoreError::CleanupUnresolved);
        }
        self.registration()
            .replace(&self.contents(previous)?, &self.contents(candidate)?)?;
        self.systemctl(&["daemon-reload"])?;
        self.check(candidate, false)
    }
    /// At boot with linger, otherwise from login to logout.
    fn startup(&self) -> ServiceStartup {
        if self.linger().is_ok() {
            ServiceStartup::AtBoot
        } else {
            ServiceStartup::AtLogin
        }
    }
    fn check(&self, runtime: &Path, removing: bool) -> Result<(), CoreError> {
        self.registration()
            .check(&self.contents(runtime)?, removing)?;
        let state = self.observation()?;
        if !loaded_definition(&state, &self.registration().path())? {
            return Ok(());
        }
        let object = format!(
            "/org/freedesktop/systemd1/unit/{}",
            self.name
                .bytes()
                .map(|byte| {
                    if byte.is_ascii_alphanumeric() {
                        char::from(byte).to_string()
                    } else {
                        format!("_{byte:02x}")
                    }
                })
                .collect::<String>()
        );
        let bytes = self.manager(
            "/usr/bin/busctl",
            &[
                "--json=short".into(),
                "get-property".into(),
                "org.freedesktop.systemd1".into(),
                object.into(),
                "org.freedesktop.systemd1.Service".into(),
                "ExecStart".into(),
            ],
        )?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| CoreError::ServiceUnavailable)?;
        let entries = value
            .get("data")
            .and_then(|v| v.as_array())
            .ok_or(CoreError::ServiceUnavailable)?;
        let executable = runtime.join(
            super::runtime::Layout::host(&self.identity)
                .ok_or(CoreError::RuntimeUnavailable)?
                .executable(),
        );
        let expected = executable.to_str().ok_or(CoreError::RuntimeUnavailable)?;
        if entries.len() != 1
            || entries[0].get(0).and_then(|v| v.as_str()) != Some(expected)
            || entries[0].get(1)
                != Some(&serde_json::json!([
                    expected,
                    "internal",
                    "serve",
                    "--service",
                    "--identity",
                    self.identity
                ]))
        {
            return Err(CoreError::CleanupUnresolved);
        }
        Ok(())
    }
    fn install(&self, runtime: &Path) -> Result<(), CoreError> {
        self.registration().install(&self.contents(runtime)?)?;
        self.systemctl(&["daemon-reload"])?;
        self.check(runtime, false)
    }
    /// Starting needs no linger. Without it the account's user manager, and SpaceTerm Core with
    /// it, runs while the user is logged in and stops at logout. Remote setup and pairing
    /// require linger themselves.
    fn prepare_start(&self) -> Result<(), CoreError> {
        Ok(())
    }
    fn linger_authorized(&self) -> Result<Option<bool>, CoreError> {
        match self.linger() {
            Ok(()) => Ok(Some(true)),
            Err(CoreError::AuthorizationRequired) => Ok(Some(false)),
            Err(error) => Err(error),
        }
    }
    fn start(&self) -> Result<(), CoreError> {
        start_unit(&self.name, &self.registration().path(), |args| {
            self.systemctl(args)
        })
    }
    fn set_automatic_start(&self, enabled: bool) -> Result<(), CoreError> {
        self.startup(enabled)?;
        self.systemctl(&["daemon-reload"])?;
        Ok(())
    }
    fn stop(&self) -> Result<(), CoreError> {
        self.set_automatic_start(false)?;
        self.systemctl(&["--no-block", "stop", &self.name])?;
        Ok(())
    }
    /// systemd never limits this unit's stop: it sets `TimeoutStopSec=infinity` and
    /// `SendSIGKILL=no`. This is SpaceTerm's own cap, equal to systemd's stock default
    /// `DefaultTimeoutStopSec=90s`. The server keeps stopping after the cap.
    fn stop_timeout(&self) -> Duration {
        Duration::from_secs(90)
    }
    fn uninstall(&self, runtime: &Path) -> Result<(), CoreError> {
        self.registration().remove(&self.contents(runtime)?)?;
        self.systemctl(&["daemon-reload"])?;
        Ok(())
    }
    fn running(&self) -> Result<bool, CoreError> {
        let state = self.observation()?;
        if property(&state, "LoadState")? == "not-found" {
            return Ok(false);
        }
        if !matches!(property(&state, "ActiveState")?, "inactive" | "failed") {
            return Ok(true);
        }
        let tasks = property(&state, "TasksCurrent")?;
        if !matches!(tasks, "0" | "[not set]") {
            return Ok(true);
        }
        Ok(property(&state, "MainPID")? != "0")
    }
    fn process(&self) -> Result<Option<u32>, CoreError> {
        let state = self.observation()?;
        Ok(property(&state, "MainPID")?
            .parse::<u32>()
            .ok()
            .filter(|pid| *pid > 0))
    }
    fn enable_linger(&self, authorization: LingerAuthorization) -> Result<(), CoreError> {
        match self.linger() {
            Ok(()) => return Ok(()),
            Err(CoreError::AuthorizationRequired) => {}
            Err(error) => return Err(error),
        }
        let mut process = linger_command(self.uid, authorization);
        let mut child = process
            .spawn()
            .map_err(|_| CoreError::AuthorizationRequired)?;
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return self.linger(),
                Ok(Some(_)) => return Err(CoreError::AuthorizationRequired),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CoreError::AuthorizationRequired);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unit_is_named_for_the_identity_core_and_runs_internal_serve() {
        let paths = Paths {
            identity: "spaceterm-preflight".into(),
            data: "/home/owner/.local/share/spaceterm-preflight/core".into(),
            runtime: "/tmp/spaceterm-preflight-core-1000".into(),
        };
        let service = UserService::new(&paths, "spaceterm-preflight").unwrap();
        assert_eq!(
            service.name,
            "io.github.sadiksaifi.spaceterm-preflight.core.service"
        );
        assert_eq!(
            service.directory,
            Path::new("/home/owner/.config/systemd/user")
        );
        let contents = String::from_utf8(service.contents(Path::new("/runtime")).unwrap()).unwrap();
        assert!(contents.contains("Description=SpaceTerm Core (spaceterm-preflight)\n"));
        assert!(contents.contains(
            "ExecStart=\"/runtime/bin/spaceterm\" internal serve --service --identity spaceterm-preflight\n"
        ));
    }

    const UNIT: &str = "spaceterm-test.service";
    const DEFINITION: &str = "/owned/spaceterm-test.service";

    /// Substitute only the systemctl process boundary. The real start policy consumes its output.
    struct StartManager {
        observation: Result<Vec<u8>, CoreError>,
        reset: Result<(), CoreError>,
        start: Result<(), CoreError>,
        calls: Vec<Vec<String>>,
    }
    impl StartManager {
        fn new(active: &str, result: &str) -> Self {
            Self {
                observation: Ok(format!("LoadState=loaded\nActiveState={active}\nResult={result}\nMainPID=0\nTasksCurrent=0\nFragmentPath={DEFINITION}\nDropInPaths=\nNeedDaemonReload=no\n").into_bytes()),
                // A fresh inactive unit can be garbage-collected between show and reset-failed.
                reset: if active == "inactive" && result == "success" { Err(CoreError::ServiceUnavailable) } else { Ok(()) },
                start: Ok(()),
                calls: Vec::new(),
            }
        }
        fn run(&mut self) -> Result<(), CoreError> {
            start_unit(UNIT, Path::new(DEFINITION), |args| {
                self.calls
                    .push(args.iter().map(|arg| (*arg).to_owned()).collect());
                assert_eq!(args.get(1), Some(&UNIT));
                match args[0] {
                    "show" => self.observation.clone(),
                    "reset-failed" => self.reset.map(|()| Vec::new()),
                    "start" => self.start.map(|()| Vec::new()),
                    command => panic!("unexpected manager command: {command}"),
                }
            })
        }
        fn commands(&self) -> Vec<&str> {
            self.calls.iter().map(|args| args[0].as_str()).collect()
        }
    }

    #[test]
    fn fresh_inactive_registration_starts_without_resetting_an_unloaded_unit() {
        let mut manager = StartManager::new("inactive", "success");
        assert_eq!(manager.run(), Ok(()));
        assert_eq!(manager.commands(), ["show", "start"]);
    }

    #[test]
    fn failed_and_start_limited_units_are_reset_before_one_start() {
        for (active, result) in [
            ("failed", "exit-code"),
            ("failed", "start-limit-hit"),
            ("inactive", "start-limit-hit"),
        ] {
            let mut manager = StartManager::new(active, result);
            assert_eq!(manager.run(), Ok(()));
            assert_eq!(manager.commands(), ["show", "reset-failed", "start"]);
        }
    }

    #[test]
    fn a_required_reset_failure_is_returned_without_starting() {
        let mut manager = StartManager::new("failed", "start-limit-hit");
        manager.reset = Err(CoreError::ServiceUnavailable);
        assert_eq!(manager.run(), Err(CoreError::ServiceUnavailable));
        assert_eq!(manager.commands(), ["show", "reset-failed"]);
    }

    #[test]
    fn observation_and_start_failures_are_returned_without_retry() {
        let mut manager = StartManager::new("inactive", "success");
        manager.observation = Err(CoreError::ServiceUnavailable);
        assert_eq!(manager.run(), Err(CoreError::ServiceUnavailable));
        assert_eq!(manager.commands(), ["show"]);
        let mut manager = StartManager::new("inactive", "success");
        manager.start = Err(CoreError::StartupFailed);
        assert_eq!(manager.run(), Err(CoreError::StartupFailed));
        assert_eq!(manager.commands(), ["show", "start"]);
    }

    #[test]
    fn changed_loaded_definitions_are_neither_reset_nor_started() {
        for changed in [
            String::from(
                "LoadState=loaded\nActiveState=failed\nResult=start-limit-hit\nFragmentPath=/foreign/unit.service\nDropInPaths=\nNeedDaemonReload=no\n",
            ),
            format!(
                "LoadState=loaded\nActiveState=failed\nResult=start-limit-hit\nFragmentPath={DEFINITION}\nDropInPaths=/foreign/override.conf\nNeedDaemonReload=no\n"
            ),
            format!(
                "LoadState=loaded\nActiveState=failed\nResult=start-limit-hit\nFragmentPath={DEFINITION}\nDropInPaths=\nNeedDaemonReload=yes\n"
            ),
        ] {
            let mut manager = StartManager::new("failed", "start-limit-hit");
            manager.observation = Ok(changed.into_bytes());
            assert_eq!(manager.run(), Err(CoreError::CleanupUnresolved));
            assert_eq!(manager.commands(), ["show"]);
        }
    }

    #[test]
    fn an_unloaded_registration_starts_without_resetting_manager_state() {
        let mut manager = StartManager::new("inactive", "success");
        manager.observation = Ok(b"LoadState=not-found\n".to_vec());
        assert_eq!(manager.run(), Ok(()));
        assert_eq!(manager.commands(), ["show", "start"]);
    }

    #[test]
    fn startup_link_cleanup_preserves_a_replacement_and_removes_only_its_own_link() {
        let root = std::env::temp_dir().join(format!(
            "sts-unit-{:x}",
            u64::from_be_bytes(crate::protocol::random().unwrap()[..8].try_into().unwrap())
        ));
        Directory::open(&root, true).unwrap();
        let service = UserService {
            directory: root.join("units"),
            name: "spaceterm-test.service".into(),
            identity: "spaceterm-development".into(),
            uid: unsafe { libc::geteuid() },
        };
        service.startup(true).unwrap();
        let link = service
            .directory
            .join("default.target.wants")
            .join(&service.name);
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            service.registration().path()
        );
        service.startup(false).unwrap();
        assert!(!link.exists());
        std::os::unix::fs::symlink("/foreign.service", &link).unwrap();
        assert_eq!(service.startup(false), Err(CoreError::CleanupUnresolved));
        assert_eq!(service.startup(true), Err(CoreError::CleanupUnresolved));
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            Path::new("/foreign.service")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
