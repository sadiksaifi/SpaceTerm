use super::*;

/// Seconds launchd lets a job exit after SIGTERM when it unloads the job, before SIGKILL.
const EXIT_TIMEOUT: u64 = 20;

pub(super) struct LaunchAgent {
    directory: PathBuf,
    name: String,
    label: String,
    target: String,
    identity: String,
}
impl LaunchAgent {
    pub(super) fn new(paths: &Paths, identity: &str) -> Result<Self, ServerError> {
        let home = paths
            .data
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or(ServerError::StorageUnavailable)?;
        let label = format!("io.github.sadiksaifi.{identity}.server");
        // SAFETY: geteuid has no preconditions.
        let target = format!("gui/{}/{label}", unsafe { libc::geteuid() });
        Ok(Self {
            directory: home.join("Library/LaunchAgents"),
            name: format!("{label}.plist"),
            label,
            target,
            identity: identity.into(),
        })
    }
    fn contents(&self, runtime: &Path) -> Result<Vec<u8>, ServerError> {
        let executable = runtime.join("bin/spaceterm-server");
        let executable = executable.to_str().ok_or(ServerError::RuntimeUnavailable)?;
        let executable = executable
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;");
        Ok(format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{}</string>
<key>ProgramArguments</key><array><string>{executable}</string><string>service-serve</string><string>{}</string></array>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
<key>ProcessType</key><string>Interactive</string>
<key>LimitLoadToSessionType</key><string>Aqua</string>
<key>ExitTimeOut</key><integer>{EXIT_TIMEOUT}</integer>
<key>Umask</key><integer>63</integer>
</dict></plist>
"#, self.label, self.identity).into_bytes())
    }
    fn registration(&self) -> registration::Registration {
        registration::Registration {
            directory: self.directory.clone(),
            name: self.name.clone(),
        }
    }
    fn validate_observation(
        &self,
        runtime: &Path,
        removing: bool,
        bytes: &[u8],
    ) -> Result<bool, ServerError> {
        let expected = format!(
            "\"Program\" = \"{}\";",
            runtime
                .join("bin/spaceterm-server")
                .to_str()
                .ok_or(ServerError::UnsafeStorage)?
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
        );
        let text = std::str::from_utf8(bytes).map_err(|_| ServerError::ServiceUnavailable)?;
        if (removing && !self.registration().path().exists())
            || !text.lines().any(|line| line.trim() == expected)
        {
            return Err(ServerError::CleanupUnresolved);
        }
        running(bytes)
    }
    fn absent_job(&self, error: ServerError) -> Result<bool, ServerError> {
        // Distinguish an absent job from an inaccessible login domain.
        let domain = self
            .target
            .rsplit_once('/')
            .ok_or(ServerError::ServiceUnavailable)?
            .0;
        command("/bin/launchctl", &["print".into(), domain.into()]).map_err(|_| error)?;
        Ok(false)
    }
}
impl Supervisor for LaunchAgent {
    fn replace(&self, previous: &Path, candidate: &Path) -> Result<(), ServerError> {
        self.registration()
            .check(&self.contents(previous)?, false)
            .or_else(|_| self.registration().check(&self.contents(candidate)?, false))?;
        // Never reload a running job. Stop already delivered the sole destructive dispatch.
        if self.running()? {
            return Err(ServerError::ActivationUnresolved);
        }
        if let Ok(bytes) = command(
            "/bin/launchctl",
            &["list".into(), self.label.clone().into()],
        ) {
            self.validate_observation(previous, false, &bytes)
                .or_else(|_| self.validate_observation(candidate, false, &bytes))?;
            command(
                "/bin/launchctl",
                &["bootout".into(), self.target.clone().into()],
            )?;
        }
        self.registration()
            .replace(&self.contents(previous)?, &self.contents(candidate)?)
    }
    fn startup(&self) -> ServiceStartup {
        ServiceStartup::AtLogin
    }
    fn install(&self, runtime: &Path) -> Result<(), ServerError> {
        self.registration().install(&self.contents(runtime)?)
    }
    fn check(&self, runtime: &Path, removing: bool) -> Result<(), ServerError> {
        self.registration()
            .check(&self.contents(runtime)?, removing)?;
        if let Ok(bytes) = command(
            "/bin/launchctl",
            &["list".into(), self.label.clone().into()],
        ) {
            self.validate_observation(runtime, removing, &bytes)?;
        }
        Ok(())
    }
    fn observe(&self, runtime: &Path, removing: bool) -> Result<bool, ServerError> {
        self.registration()
            .check(&self.contents(runtime)?, removing)?;
        match command(
            "/bin/launchctl",
            &["list".into(), self.label.clone().into()],
        ) {
            Ok(bytes) => self.validate_observation(runtime, removing, &bytes),
            Err(error) => self.absent_job(error),
        }
    }
    fn prepare_start(&self) -> Result<(), ServerError> {
        Ok(())
    }
    fn start(&self) -> Result<(), ServerError> {
        command(
            "/bin/launchctl",
            &["enable".into(), self.target.clone().into()],
        )?;
        if self.running()? {
            return Ok(());
        }
        let domain = self
            .target
            .rsplit_once('/')
            .ok_or(ServerError::ServiceUnavailable)?
            .0;
        // An already loaded idle job needs kickstart, while a new registration needs bootstrap.
        if command(
            "/bin/launchctl",
            &["list".into(), self.label.clone().into()],
        )
        .is_ok()
        {
            command(
                "/bin/launchctl",
                &["kickstart".into(), self.target.clone().into()],
            )?;
        } else {
            command(
                "/bin/launchctl",
                &[
                    "bootstrap".into(),
                    domain.into(),
                    self.registration().path().into_os_string(),
                ],
            )?;
        }
        Ok(())
    }
    fn set_automatic_start(&self, enabled: bool) -> Result<(), ServerError> {
        command(
            "/bin/launchctl",
            &[
                if enabled { "enable" } else { "disable" }.into(),
                self.target.clone().into(),
            ],
        )?;
        Ok(())
    }
    fn stop(&self) -> Result<(), ServerError> {
        self.set_automatic_start(false)?;
        if self.running()? {
            command(
                "/bin/launchctl",
                &["kill".into(), "SIGTERM".into(), self.target.clone().into()],
            )?;
            let deadline = Instant::now() + self.stop_timeout();
            while self.running()? {
                if Instant::now() >= deadline {
                    return Err(ServerError::CleanupUnresolved);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        if command(
            "/bin/launchctl",
            &["list".into(), self.label.clone().into()],
        )
        .is_ok()
        {
            command(
                "/bin/launchctl",
                &["bootout".into(), self.target.clone().into()],
            )?;
        }
        Ok(())
    }
    /// Stop waits as long as launchd would before it kills the job at logout.
    fn stop_timeout(&self) -> Duration {
        Duration::from_secs(EXIT_TIMEOUT)
    }
    fn running(&self) -> Result<bool, ServerError> {
        let result = command(
            "/bin/launchctl",
            &["list".into(), self.label.clone().into()],
        );
        match result {
            Ok(bytes) => running(&bytes),
            Err(error) => self.absent_job(error),
        }
    }
    fn uninstall(&self, runtime: &Path) -> Result<(), ServerError> {
        self.registration().remove(&self.contents(runtime)?)
    }
}

fn running(bytes: &[u8]) -> Result<bool, ServerError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ServerError::ServiceUnavailable)?;
    Ok(text.lines().any(|line| {
        line.trim()
            .strip_prefix("\"PID\" = ")
            .and_then(|value| value.strip_suffix(';'))
            .and_then(|value| value.parse::<u32>().ok())
            .is_some_and(|pid| pid > 0)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn one_observation_validates_the_approved_program_before_reporting_running() {
        let agent = LaunchAgent {
            directory: PathBuf::from("/unavailable-fixture"),
            name: "fixture.plist".into(),
            label: "fixture".into(),
            target: "gui/fixture".into(),
            identity: "spaceterm-development".into(),
        };
        let runtime = Path::new("/approved-fixture");
        assert_eq!(
            agent.validate_observation(
                runtime,
                false,
                b"\"Program\" = \"/approved-fixture/bin/spaceterm-server\";\n\"PID\" = 7;\n"
            ),
            Ok(true)
        );
        assert_eq!(
            agent.validate_observation(
                runtime,
                false,
                b"\"Program\" = \"/other-fixture/bin/spaceterm-server\";\n\"PID\" = 7;\n"
            ),
            Err(ServerError::CleanupUnresolved)
        );
        assert_eq!(
            agent.validate_observation(
                runtime,
                false,
                b"\"Program\" = \"/approved-fixture/bin/spaceterm-server\";\n"
            ),
            Ok(false)
        );
    }

    struct Fixture {
        root: PathBuf,
        agent: LaunchAgent,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = command(
                "/bin/launchctl",
                &["bootout".into(), self.agent.target.clone().into()],
            );
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    #[ignore = "requires a GUI login domain; run mise run server:service:test:macos"]
    fn launchd_unload_allows_in_flight_cleanup_to_finish() {
        let suffix =
            u64::from_be_bytes(crate::protocol::random().unwrap()[..8].try_into().unwrap());
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("spaceterm-launchagent-{suffix:x}"));
        let label = format!("io.github.sadiksaifi.spaceterm.validation-{suffix:x}");
        // SAFETY: geteuid has no preconditions.
        let domain = format!("gui/{}", unsafe { libc::geteuid() });
        let fixture = Fixture {
            agent: LaunchAgent {
                directory: root.join("agents"),
                name: format!("{label}.plist"),
                target: format!("{domain}/{label}"),
                label,
                identity: "spaceterm-development".into(),
            },
            root,
        };
        let runtime = fixture.root.join("runtime");
        std::fs::create_dir_all(runtime.join("bin")).unwrap();
        let executable = runtime.join("bin/spaceterm-server");
        std::fs::write(
            &executable,
            "#!/bin/sh\ncd -- \"$(dirname -- \"$0\")/../..\" || exit 1\n\
             trap '/bin/sleep 1; printf clean > cleaned; exit 0' TERM\n\
             printf ready > ready\nwhile :; do /bin/sleep 1; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        fixture.agent.install(&runtime).unwrap();
        command(
            "/bin/launchctl",
            &[
                "bootstrap".into(),
                domain.into(),
                fixture.agent.registration().path().into_os_string(),
            ],
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fixture.root.join("ready").exists() {
            assert!(Instant::now() < deadline, "fixture did not become ready");
            std::thread::sleep(Duration::from_millis(20));
        }
        // Logout unloads the job directly, without the explicit Stop action's pre-unload wait.
        command(
            "/bin/launchctl",
            &["bootout".into(), fixture.agent.target.clone().into()],
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fixture.root.join("cleaned").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            fixture.root.join("cleaned").exists(),
            "launchd killed the job before its SIGTERM cleanup finished"
        );
    }
}
