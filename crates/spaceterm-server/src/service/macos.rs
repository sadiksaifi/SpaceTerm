use super::*;

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
<key>ExitTimeOut</key><integer>20</integer>
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
}
impl Supervisor for LaunchAgent {
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
            let expected = format!(
                "\"Program\" = \"{}\";",
                runtime
                    .join("bin/spaceterm-server")
                    .to_str()
                    .ok_or(ServerError::UnsafeStorage)?
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
            );
            let text = std::str::from_utf8(&bytes).map_err(|_| ServerError::ServiceUnavailable)?;
            if removing && !self.registration().path().exists() {
                return Err(ServerError::CleanupUnresolved);
            }
            if !text.lines().any(|line| line.trim() == expected) {
                return Err(ServerError::CleanupUnresolved);
            }
        }
        Ok(())
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
    fn disable(&self) -> Result<(), ServerError> {
        command(
            "/bin/launchctl",
            &["disable".into(), self.target.clone().into()],
        )?;
        if self.running()? {
            command(
                "/bin/launchctl",
                &["kill".into(), "SIGTERM".into(), self.target.clone().into()],
            )?;
            let deadline = Instant::now() + Duration::from_secs(20);
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
    fn running(&self) -> Result<bool, ServerError> {
        let result = command(
            "/bin/launchctl",
            &["list".into(), self.label.clone().into()],
        );
        match result {
            Ok(bytes) => {
                let text =
                    std::str::from_utf8(&bytes).map_err(|_| ServerError::ServiceUnavailable)?;
                Ok(text.lines().any(|line| {
                    line.trim()
                        .strip_prefix("\"PID\" = ")
                        .and_then(|v| v.strip_suffix(';'))
                        .and_then(|v| v.parse::<u32>().ok())
                        .is_some_and(|pid| pid > 0)
                }))
            }
            Err(error) => {
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
    }
    fn uninstall(&self, runtime: &Path) -> Result<(), ServerError> {
        self.registration().remove(&self.contents(runtime)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

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
