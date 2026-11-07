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
    pub(super) fn new(paths: &Paths, identity: &str) -> Result<Self, CoreError> {
        let home = paths
            .data
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or(CoreError::StorageUnavailable)?;
        let label = format!("com.peblon.{identity}.core");
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
    /// The executable of `runtime`, escaped for a property list string.
    fn escaped(&self, runtime: &Path) -> Result<String, CoreError> {
        let executable = runtime.join(self.executable()?);
        let executable = executable.to_str().ok_or(CoreError::RuntimeUnavailable)?;
        Ok(executable
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;"))
    }
    /// The plist key that attributes an agent to this identity's SpaceTerm Core bundle. Login
    /// Items & Extensions names the background item for that bundle. Without the key it falls back
    /// to the signing certificate's organization name. It attributes a plist path once, when it
    /// first registers that path, and keeps that attribution, so the key is there from the first
    /// write.
    fn association(&self) -> String {
        format!(
            "<key>AssociatedBundleIdentifiers</key><array><string>{}</string></array>",
            bundle_identifier(&self.identity)
        )
    }
    /// Registers `runtime`'s bundle with Launch Services before launchd loads an agent that runs
    /// it, so Login Items can resolve the agent's associated bundle.
    fn register_bundle(&self, runtime: &Path) -> Result<(), CoreError> {
        let layout =
            super::runtime::Layout::host(&self.identity).ok_or(CoreError::RuntimeUnavailable)?;
        if crate::launch_services::register(&runtime.join(layout.signed())) {
            Ok(())
        } else {
            Err(CoreError::ServiceUnavailable)
        }
    }
    /// The recovery agent's label, beside the server's.
    fn recovery_label(&self) -> String {
        format!("{}.recovery", self.label)
    }
    /// An agent that runs `runtime`'s recovery helper once when launchd loads it at the next
    /// login. SpaceTerm only writes it, so it never runs during the activation that armed it.
    fn recovery_contents(&self, runtime: &Path) -> Result<Vec<u8>, CoreError> {
        let executable = self.escaped(runtime)?;
        Ok(format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{}</string>
<key>ProgramArguments</key><array><string>{executable}</string><string>internal</string><string>helper</string><string>--identity</string><string>{}</string></array>
{}
<key>RunAtLoad</key><true/>
<key>ProcessType</key><string>Interactive</string>
<key>LimitLoadToSessionType</key><string>Aqua</string>
<key>Umask</key><integer>63</integer>
</dict></plist>
"#, self.recovery_label(), self.identity, self.association()).into_bytes())
    }
    fn contents(&self, runtime: &Path) -> Result<Vec<u8>, CoreError> {
        let executable = self.escaped(runtime)?;
        Ok(format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{}</string>
<key>ProgramArguments</key><array><string>{executable}</string><string>internal</string><string>serve</string><string>--service</string><string>--identity</string><string>{}</string></array>
{}
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
<key>ProcessType</key><string>Interactive</string>
<key>LimitLoadToSessionType</key><string>Aqua</string>
<key>ExitTimeOut</key><integer>{EXIT_TIMEOUT}</integer>
<key>Umask</key><integer>63</integer>
</dict></plist>
"#, self.label, self.identity, self.association()).into_bytes())
    }
    /// The executable the agent runs, relative to its runtime.
    fn executable(&self) -> Result<PathBuf, CoreError> {
        super::runtime::Layout::host(&self.identity)
            .map(super::runtime::Layout::executable)
            .ok_or(CoreError::RuntimeUnavailable)
    }
    pub(super) fn registration(&self) -> registration::Registration {
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
    ) -> Result<bool, CoreError> {
        let expected = format!(
            "\"Program\" = \"{}\";",
            runtime
                .join(self.executable()?)
                .to_str()
                .ok_or(CoreError::UnsafeStorage)?
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
        );
        let text = std::str::from_utf8(bytes).map_err(|_| CoreError::ServiceUnavailable)?;
        if (removing && !self.registration().path().exists())
            || !text.lines().any(|line| line.trim() == expected)
        {
            return Err(CoreError::CleanupUnresolved);
        }
        Ok(process(bytes)?.is_some())
    }
    fn absent_job(&self, error: CoreError) -> Result<bool, CoreError> {
        // Distinguish an absent job from an inaccessible login domain.
        let domain = self
            .target
            .rsplit_once('/')
            .ok_or(CoreError::ServiceUnavailable)?
            .0;
        command("/bin/launchctl", &["print".into(), domain.into()]).map_err(|_| error)?;
        Ok(false)
    }
}
impl Supervisor for LaunchAgent {
    fn arm_recovery(&self, runtime: &Path) -> Result<(), CoreError> {
        super::publish_entry(
            &self
                .directory
                .join(format!("{}.plist", self.recovery_label())),
            &self.recovery_contents(runtime)?,
        )
    }
    fn disarm_recovery(&self) -> Result<(), CoreError> {
        let label = self.recovery_label();
        if command("/bin/launchctl", &["list".into(), label.clone().into()]).is_ok() {
            // SAFETY: geteuid has no preconditions.
            let target = format!("gui/{}/{label}", unsafe { libc::geteuid() });
            command("/bin/launchctl", &["bootout".into(), target.into()])?;
        }
        super::remove_entry(&self.directory.join(format!("{label}.plist")))
    }
    fn replace(&self, previous: &Path, candidate: &Path) -> Result<(), CoreError> {
        self.registration()
            .check(&self.contents(previous)?, false)
            .or_else(|_| self.registration().check(&self.contents(candidate)?, false))?;
        // Never reload a running job. Stop already delivered the sole destructive dispatch.
        if self.running()? {
            return Err(CoreError::ActivationUnresolved);
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
        // Login Items attributed the plist's path at installation. Registering the candidate only
        // keeps Launch Services pointed at the installation that now holds the bundle.
        let _ = self.register_bundle(candidate);
        self.registration()
            .replace(&self.contents(previous)?, &self.contents(candidate)?)
    }
    fn startup(&self) -> ServiceStartup {
        ServiceStartup::AtLogin
    }
    fn login_item_off(&self) -> bool {
        login_items::requires_approval(&self.registration().path())
    }
    /// A failed registration fails the installation before the plist exists, because Login Items
    /// would otherwise keep the fallback name for the plist's path.
    fn install(&self, runtime: &Path) -> Result<(), CoreError> {
        self.register_bundle(runtime)?;
        self.registration().install(&self.contents(runtime)?)
    }
    fn check(&self, runtime: &Path, removing: bool) -> Result<(), CoreError> {
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
    fn observe(&self, runtime: &Path, removing: bool) -> Result<bool, CoreError> {
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
    fn prepare_start(&self) -> Result<(), CoreError> {
        Ok(())
    }
    fn start(&self) -> Result<(), CoreError> {
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
            .ok_or(CoreError::ServiceUnavailable)?
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
    fn set_automatic_start(&self, enabled: bool) -> Result<(), CoreError> {
        command(
            "/bin/launchctl",
            &[
                if enabled { "enable" } else { "disable" }.into(),
                self.target.clone().into(),
            ],
        )?;
        Ok(())
    }
    fn stop(&self) -> Result<(), CoreError> {
        self.set_automatic_start(false)?;
        if self.running()? {
            command(
                "/bin/launchctl",
                &["kill".into(), "SIGTERM".into(), self.target.clone().into()],
            )?;
            let deadline = Instant::now() + self.stop_timeout();
            while self.running()? {
                if Instant::now() >= deadline {
                    return Err(CoreError::CleanupUnresolved);
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
    fn running(&self) -> Result<bool, CoreError> {
        Ok(self.process()?.is_some())
    }
    fn process(&self) -> Result<Option<u32>, CoreError> {
        let result = command(
            "/bin/launchctl",
            &["list".into(), self.label.clone().into()],
        );
        match result {
            Ok(bytes) => process(&bytes),
            Err(error) => self.absent_job(error).map(|_| None),
        }
    }
    fn uninstall(&self, runtime: &Path) -> Result<(), CoreError> {
        self.registration().remove(&self.contents(runtime)?)
    }
}

fn process(bytes: &[u8]) -> Result<Option<u32>, CoreError> {
    let text = std::str::from_utf8(bytes).map_err(|_| CoreError::ServiceUnavailable)?;
    Ok(text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("\"PID\" = ")
            .and_then(|value| value.strip_suffix(';'))
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|pid| *pid > 0)
    }))
}

/// Login Items & Extensions, where macOS lists the agent as a background item the person turns
/// on or off.
mod login_items {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    use objc2_foundation::{NSString, NSURL};
    use std::path::Path;

    #[link(name = "ServiceManagement", kind = "framework")]
    unsafe extern "C" {}

    /// `SMAppServiceStatusRequiresApproval`: the item is registered and off in Login Items,
    /// because the person turned it off or has not approved it yet. System Settings shows both
    /// the same way.
    const REQUIRES_APPROVAL: isize = 2;

    /// Whether launchd won't run the agent at `plist` until the person turns it on.
    pub(super) fn requires_approval(plist: &Path) -> bool {
        let (Some(class), Some(path)) = (AnyClass::get(c"SMAppService"), plist.to_str()) else {
            return false;
        };
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        // SAFETY: `+[SMAppService statusForLegacyURL:]` takes a file URL and returns an
        // NSInteger status. It reads Background Task Management's record and changes nothing.
        let status: isize = unsafe { msg_send![class, statusForLegacyURL: &*url] };
        status == REQUIRES_APPROVAL
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_agent_is_labelled_for_the_identity_core_and_runs_internal_serve() {
        let paths = Paths {
            identity: "spaceterm-preflight".into(),
            data: "/Users/owner/.local/share/spaceterm-preflight/core".into(),
            runtime: "/private/tmp/spaceterm-preflight-core-501".into(),
        };
        let agent = LaunchAgent::new(&paths, "spaceterm-preflight").unwrap();
        assert_eq!(agent.label, "com.peblon.spaceterm-preflight.core");
        assert_eq!(agent.name, "com.peblon.spaceterm-preflight.core.plist");
        assert_eq!(
            agent.directory,
            Path::new("/Users/owner/Library/LaunchAgents")
        );
        let contents = String::from_utf8(agent.contents(Path::new("/runtime")).unwrap()).unwrap();
        assert!(contents.contains(
            "<string>/runtime/SpaceTerm Core Preflight.app/Contents/MacOS/spaceterm</string>\
             <string>internal</string><string>serve</string><string>--service</string>\
             <string>--identity</string><string>spaceterm-preflight</string>"
        ));
    }

    #[test]
    fn both_agents_name_the_identity_core_bundle_for_login_items() {
        let paths = Paths {
            identity: "spaceterm-preflight".into(),
            data: "/Users/owner/.local/share/spaceterm-preflight/core".into(),
            runtime: "/private/tmp/spaceterm-preflight-core-501".into(),
        };
        let agent = LaunchAgent::new(&paths, "spaceterm-preflight").unwrap();
        let association = "<key>AssociatedBundleIdentifiers</key>\
            <array><string>com.peblon.spaceterm-preflight.core</string></array>";
        for contents in [
            agent.contents(Path::new("/runtime")).unwrap(),
            agent.recovery_contents(Path::new("/runtime")).unwrap(),
        ] {
            assert!(String::from_utf8(contents).unwrap().contains(association));
        }
    }

    #[test]
    fn an_unregistered_bundle_leaves_the_agent_unpublished() {
        let root = std::env::temp_dir().join(format!(
            "spaceterm-unregistered-{:x}",
            u64::from_be_bytes(crate::protocol::random().unwrap()[..8].try_into().unwrap())
        ));
        let agent = LaunchAgent {
            directory: root.join("LaunchAgents"),
            name: "fixture.plist".into(),
            label: "fixture".into(),
            target: "gui/fixture".into(),
            identity: "spaceterm-development".into(),
        };
        let result = agent.install(&root.join("missing-installation"));
        let published = agent.registration().path().exists();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(result, Err(CoreError::ServiceUnavailable));
        assert!(!published);
    }

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
                b"\"Program\" = \"/approved-fixture/SpaceTerm Core Development.app/Contents/MacOS/spaceterm\";\n\"PID\" = 7;\n"
            ),
            Ok(true)
        );
        assert_eq!(
            agent.validate_observation(
                runtime,
                false,
                b"\"Program\" = \"/other-fixture/SpaceTerm Core Development.app/Contents/MacOS/spaceterm\";\n\"PID\" = 7;\n"
            ),
            Err(CoreError::CleanupUnresolved)
        );
        assert_eq!(
            agent.validate_observation(
                runtime,
                false,
                b"\"Program\" = \"/approved-fixture/SpaceTerm Core Development.app/Contents/MacOS/spaceterm\";\n"
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
    #[ignore = "requires a GUI login domain; run mise run core:service:test:macos"]
    fn launchd_unload_allows_in_flight_cleanup_to_finish() {
        let suffix =
            u64::from_be_bytes(crate::protocol::random().unwrap()[..8].try_into().unwrap());
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("spaceterm-launchagent-{suffix:x}"));
        let label = format!("com.peblon.spaceterm.validation-{suffix:x}");
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
        let executable = runtime.join(fixture.agent.executable().unwrap());
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(
            &executable,
            "#!/bin/sh\ncd -- \"$(dirname -- \"$0\")/../../../..\" || exit 1\n\
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
