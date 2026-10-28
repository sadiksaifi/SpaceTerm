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
    pub(super) fn new(paths: &Paths, identity: &str) -> Result<Self, ServerError> {
        let home = paths
            .data
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or(ServerError::StorageUnavailable)?;
        Ok(Self {
            directory: home.join(".config/systemd/user"),
            name: format!("io.github.sadiksaifi.{identity}.server.service"),
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
    fn contents(&self, runtime: &Path) -> Result<Vec<u8>, ServerError> {
        let executable = runtime.join("bin/spaceterm-server");
        let executable = executable.to_str().ok_or(ServerError::RuntimeUnavailable)?;
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
        // Only the main process gets SIGTERM. It closes admissions, drains started Git writes,
        // and shuts down its Terminal Sessions before exiting. No manager SIGKILL bypasses that.
        Ok(format!("[Unit]\nDescription=SpaceTerm account server ({})\nStartLimitIntervalSec=60\nStartLimitBurst=3\n\n[Service]\nType=exec\nExecStart=\"{quoted}\" service-serve {}\nRestart=on-failure\nRestartSec=2\nTimeoutStopSec=infinity\nKillMode=mixed\nSendSIGKILL=no\nUMask=0077\n\n[Install]\nWantedBy=default.target\n", self.identity, self.identity).into_bytes())
    }
    fn manager(&self, program: &str, args: &[OsString]) -> Result<Vec<u8>, ServerError> {
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
    fn systemctl(&self, args: &[&str]) -> Result<Vec<u8>, ServerError> {
        let mut arguments = vec!["--no-pager".into(), "--no-ask-password".into()];
        arguments.extend(args.iter().map(OsString::from));
        self.manager("/usr/bin/systemctl", &arguments)
    }
    fn observation(&self) -> Result<Vec<(String, String)>, ServerError> {
        parse_observation(&self.systemctl(&["show", &self.name, UNIT_PROPERTIES])?)
    }
    fn linger(&self) -> Result<(), ServerError> {
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
            b"no\n" => Err(ServerError::AuthorizationRequired),
            _ => Err(ServerError::ServiceUnavailable),
        }
    }
    /// Publish or remove just our expected startup link, preserving changed or foreign entries.
    fn startup(&self, enabled: bool) -> Result<(), ServerError> {
        let path = self.directory.join("default.target.wants");
        if !enabled
            && !path
                .try_exists()
                .map_err(|_| ServerError::StorageUnavailable)?
        {
            return Ok(());
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&path)
            .map_err(|_| ServerError::StorageUnavailable)?;
        let directory = owned_directory(&path)?;
        let name = CString::new(self.name.as_bytes()).map_err(|_| ServerError::UnsafeStorage)?;
        let target = self.registration().path();
        let target =
            CString::new(target.as_os_str().as_bytes()).map_err(|_| ServerError::UnsafeStorage)?;
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
                return Err(ServerError::StorageUnavailable);
            }
            if !enabled {
                return Ok(());
            }
            verify_directory_path(&directory, &path)?;
            // SAFETY: symlinkat exclusively creates a named link without following or replacing it.
            if unsafe { libc::symlinkat(target.as_ptr(), directory.as_raw_fd(), name.as_ptr()) }
                != 0
            {
                return Err(ServerError::CleanupUnresolved);
            }
        } else {
            if before.st_mode & libc::S_IFMT != libc::S_IFLNK
                || before.st_uid != self.uid
                || before.st_nlink != 1
            {
                return Err(ServerError::CleanupUnresolved);
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
                return Err(ServerError::CleanupUnresolved);
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
                return Err(ServerError::CleanupUnresolved);
            }
            // SAFETY: unlinkat removes only the revalidated expected symlink beneath our directory.
            if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(ServerError::StorageUnavailable);
            }
        }
        directory
            .sync_all()
            .map_err(|_| ServerError::StorageUnavailable)
    }
}
fn parse_observation(bytes: &[u8]) -> Result<Vec<(String, String)>, ServerError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ServerError::ServiceUnavailable)?;
    text.lines()
        .map(|line| {
            line.split_once('=')
                .map(|(key, value)| (key.into(), value.into()))
                .ok_or(ServerError::ServiceUnavailable)
        })
        .collect()
}
fn property<'a>(observation: &'a [(String, String)], key: &str) -> Result<&'a str, ServerError> {
    observation
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
        .ok_or(ServerError::ServiceUnavailable)
}

fn loaded_definition(state: &[(String, String)], registration: &Path) -> Result<bool, ServerError> {
    if property(state, "LoadState")? == "not-found" {
        return Ok(false);
    }
    if property(state, "LoadState")? != "loaded"
        || Path::new(property(state, "FragmentPath")?) != registration
        || !property(state, "DropInPaths")?.is_empty()
        || property(state, "NeedDaemonReload")? != "no"
    {
        return Err(ServerError::CleanupUnresolved);
    }
    Ok(true)
}

/// Start after the owning service has validated its registration, runtime and account authority.
fn start_unit(
    name: &str,
    registration: &Path,
    mut systemctl: impl FnMut(&[&str]) -> Result<Vec<u8>, ServerError>,
) -> Result<(), ServerError> {
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
    match authorization {
        LingerAuthorization::RemoteNoninteractive => {
            process.args(["-n", "--"]);
        }
        LingerAuthorization::RemoteStdin => {
            process.args(["-S", "-p", "", "--"]);
        }
        LingerAuthorization::Terminal | LingerAuthorization::Desktop => {}
    }
    process.args(["/usr/bin/loginctl", "enable-linger", &uid.to_string()]);
    linger_stdio(&mut process, authorization);
    process
}

fn linger_stdio(process: &mut Command, authorization: LingerAuthorization) {
    match authorization {
        LingerAuthorization::Terminal => {}
        LingerAuthorization::Desktop
        | LingerAuthorization::RemoteNoninteractive
        | LingerAuthorization::RemoteStdin => {
            process.stdin(if authorization == LingerAuthorization::RemoteStdin {
                Stdio::inherit()
            } else {
                Stdio::null()
            });
            process.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
}

impl Supervisor for UserService {
    fn replace(&self, previous: &Path, candidate: &Path) -> Result<(), ServerError> {
        self.registration()
            .check(&self.contents(previous)?, false)
            .or_else(|_| self.registration().check(&self.contents(candidate)?, false))?;
        if self.running()? {
            return Err(ServerError::ActivationUnresolved);
        }
        let observation = self.observation()?;
        if property(&observation, "LoadState")? != "not-found"
            && (property(&observation, "LoadState")? != "loaded"
                || Path::new(property(&observation, "FragmentPath")?) != self.registration().path()
                || !property(&observation, "DropInPaths")?.is_empty())
        {
            return Err(ServerError::CleanupUnresolved);
        }
        self.registration()
            .replace(&self.contents(previous)?, &self.contents(candidate)?)?;
        self.systemctl(&["daemon-reload"])?;
        self.check(candidate, false)
    }
    fn startup(&self) -> ServiceStartup {
        ServiceStartup::AtBoot
    }
    fn check(&self, runtime: &Path, removing: bool) -> Result<(), ServerError> {
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
            serde_json::from_slice(&bytes).map_err(|_| ServerError::ServiceUnavailable)?;
        let entries = value
            .get("data")
            .and_then(|v| v.as_array())
            .ok_or(ServerError::ServiceUnavailable)?;
        let executable = runtime.join("bin/spaceterm-server");
        let expected = executable.to_str().ok_or(ServerError::RuntimeUnavailable)?;
        if entries.len() != 1
            || entries[0].get(0).and_then(|v| v.as_str()) != Some(expected)
            || entries[0].get(1)
                != Some(&serde_json::json!([
                    expected,
                    "service-serve",
                    self.identity
                ]))
        {
            return Err(ServerError::CleanupUnresolved);
        }
        Ok(())
    }
    fn install(&self, runtime: &Path) -> Result<(), ServerError> {
        self.registration().install(&self.contents(runtime)?)?;
        self.systemctl(&["daemon-reload"])?;
        self.check(runtime, false)
    }
    fn prepare_start(&self) -> Result<(), ServerError> {
        self.linger()
    }
    fn start(&self) -> Result<(), ServerError> {
        start_unit(&self.name, &self.registration().path(), |args| {
            self.systemctl(args)
        })
    }
    fn set_automatic_start(&self, enabled: bool) -> Result<(), ServerError> {
        self.startup(enabled)?;
        self.systemctl(&["daemon-reload"])?;
        Ok(())
    }
    fn stop(&self) -> Result<(), ServerError> {
        self.set_automatic_start(false)?;
        self.systemctl(&["--no-block", "stop", &self.name])?;
        Ok(())
    }
    /// The unit never times out its stop, so systemd's default stop timeout,
    /// `DefaultTimeoutStopSec=90s`, bounds the wait instead.
    fn stop_timeout(&self) -> Duration {
        Duration::from_secs(90)
    }
    fn uninstall(&self, runtime: &Path) -> Result<(), ServerError> {
        self.registration().remove(&self.contents(runtime)?)?;
        self.systemctl(&["daemon-reload"])?;
        Ok(())
    }
    fn running(&self) -> Result<bool, ServerError> {
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
    fn enable_linger(&self, authorization: LingerAuthorization) -> Result<(), ServerError> {
        match self.linger() {
            Ok(()) => return Ok(()),
            Err(ServerError::AuthorizationRequired) => {}
            Err(error) => return Err(error),
        }
        let mut process = linger_command(self.uid, authorization);
        let mut child = process
            .spawn()
            .map_err(|_| ServerError::AuthorizationRequired)?;
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return self.linger(),
                Ok(Some(_)) => return Err(ServerError::AuthorizationRequired),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ServerError::AuthorizationRequired);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNIT: &str = "spaceterm-test.service";
    const DEFINITION: &str = "/owned/spaceterm-test.service";

    /// Substitute only the systemctl process boundary. The real start policy consumes its output.
    struct StartManager {
        observation: Result<Vec<u8>, ServerError>,
        reset: Result<(), ServerError>,
        start: Result<(), ServerError>,
        calls: Vec<Vec<String>>,
    }
    impl StartManager {
        fn new(active: &str, result: &str) -> Self {
            Self {
                observation: Ok(format!("LoadState=loaded\nActiveState={active}\nResult={result}\nMainPID=0\nTasksCurrent=0\nFragmentPath={DEFINITION}\nDropInPaths=\nNeedDaemonReload=no\n").into_bytes()),
                // A fresh inactive unit can be garbage-collected between show and reset-failed.
                reset: if active == "inactive" && result == "success" { Err(ServerError::ServiceUnavailable) } else { Ok(()) },
                start: Ok(()),
                calls: Vec::new(),
            }
        }
        fn run(&mut self) -> Result<(), ServerError> {
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
        manager.reset = Err(ServerError::ServiceUnavailable);
        assert_eq!(manager.run(), Err(ServerError::ServiceUnavailable));
        assert_eq!(manager.commands(), ["show", "reset-failed"]);
    }

    #[test]
    fn observation_and_start_failures_are_returned_without_retry() {
        let mut manager = StartManager::new("inactive", "success");
        manager.observation = Err(ServerError::ServiceUnavailable);
        assert_eq!(manager.run(), Err(ServerError::ServiceUnavailable));
        assert_eq!(manager.commands(), ["show"]);
        let mut manager = StartManager::new("inactive", "success");
        manager.start = Err(ServerError::StartupFailed);
        assert_eq!(manager.run(), Err(ServerError::StartupFailed));
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
            assert_eq!(manager.run(), Err(ServerError::CleanupUnresolved));
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
    fn remote_linger_authorization_limits_sudo_to_the_requested_account_and_prompt_mode() {
        for (authorization, expected) in [
            (
                LingerAuthorization::RemoteNoninteractive,
                vec!["-n", "--", "/usr/bin/loginctl", "enable-linger", "501"],
            ),
            (
                LingerAuthorization::RemoteStdin,
                vec![
                    "-S",
                    "-p",
                    "",
                    "--",
                    "/usr/bin/loginctl",
                    "enable-linger",
                    "501",
                ],
            ),
        ] {
            let command = linger_command(501, authorization);
            assert_eq!(command.get_program(), "/usr/bin/sudo");
            assert_eq!(command.get_args().collect::<Vec<_>>(), expected);
        }
    }

    #[test]
    fn only_explicit_remote_password_authorization_inherits_standard_input() {
        for authorization in ["noninteractive", "password"] {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "service::linux::tests::remote_linger_input_child",
                ])
                .env("SPACETERM_TEST_LINGER_INPUT", authorization)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(b"authorized-fixture\n")
                .unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }

    #[test]
    #[ignore = "runs only inside the isolated remote stdin policy test"]
    fn remote_linger_input_child() {
        let authorization = match std::env::var("SPACETERM_TEST_LINGER_INPUT")
            .unwrap()
            .as_str()
        {
            "noninteractive" => LingerAuthorization::RemoteNoninteractive,
            "password" => LingerAuthorization::RemoteStdin,
            _ => panic!("invalid fixture mode"),
        };
        let mut process = Command::new("/bin/sh");
        process.args([
            "-c",
            "IFS= read -r value && test \"$value\" = authorized-fixture",
        ]);
        linger_stdio(&mut process, authorization);
        assert_eq!(
            process.status().unwrap().success(),
            authorization == LingerAuthorization::RemoteStdin
        );
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
        assert_eq!(service.startup(false), Err(ServerError::CleanupUnresolved));
        assert_eq!(service.startup(true), Err(ServerError::CleanupUnresolved));
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            Path::new("/foreign.service")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(all(test, feature = "client"))]
mod vm;
