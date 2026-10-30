//! The transient service helper. It runs one admitted service operation outside the server it
//! stops or replaces: `spaceterm server provision service-run --operation ID --identity X`.
//!
//! The helper must outlive the service manager's Stop of the server that launched it. On Linux
//! it therefore runs in its own transient user unit, outside the server unit's control group. On
//! macOS it starts in a new session, outside the launchd job's process group. It inherits no
//! server descriptor, and it acts only on its own ledger record.
use super::Service;
use super::control::{Action as ControlAction, Controller};
use super::operations::{Action, HELPER_LOCK, Ledger, OperationId, Record, State};
use crate::ServerError;
use crate::storage::Directory;
use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::time::Duration;

/// The helper's arguments after the executable, as the CLI parses them.
fn arguments(identity: &str, op: &OperationId) -> Vec<OsString> {
    [
        "server",
        "provision",
        "service-run",
        "--operation",
        &op.to_string(),
        "--identity",
        identity,
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

/// Starts the helper for `op` from the approved runtime, detached from this server. The server
/// runs the approved runtime, so its own executable is the helper's.
pub fn launch(identity: &str, op: &OperationId) -> Result<(), ServerError> {
    let program = std::env::current_exe().map_err(|_| ServerError::RuntimeUnavailable)?;
    let mut argv = vec![program.into_os_string()];
    argv.extend(arguments(identity, op));
    spawn_detached(&format!("{identity}-service-{}", op.short()), &argv)
}

/// The helper's own work: takes the helper lock, claims its record, runs the same control a local
/// command runs and records the outcome. A record settled without it is left alone.
pub fn run(identity: &str, op: &OperationId) -> Result<(), ServerError> {
    let paths = crate::host::Paths::for_identity(identity)?;
    let data = Directory::open(&paths.data, false)?;
    let _helper = data
        .try_lock(HELPER_LOCK)?
        .ok_or(ServerError::ServiceBusy)?;
    let mut ledger = Ledger::in_directory(Directory::open(&paths.data, false)?);
    let Some(record) = ledger.claim(op)? else {
        return Ok(());
    };
    let state = perform(identity, &record).unwrap_or_else(|error| match error {
        ServerError::ActivationUnresolved | ServerError::CleanupUnresolved => State::Unresolved,
        error => State::Failed {
            code: super::lane_codes::code(&error).to_owned(),
        },
    });
    ledger.mark(op, state)?;
    Ok(())
}

fn perform(identity: &str, record: &Record) -> Result<State, ServerError> {
    let service = Service::for_identity(identity)?;
    if service.passive_server_id()? != Some(record.server) {
        return Err(ServerError::Changed);
    }
    let controller = Controller::new(service.clone());
    let phase = match record.action {
        Action::Stop | Action::Restart => {
            let action = if record.action == Action::Stop {
                ControlAction::Stop
            } else {
                ControlAction::Restart
            };
            controller.confirm(controller.prepare(action)?)?;
            return Ok(State::Done);
        }
        Action::Update => {
            let digest = record.candidate.ok_or(ServerError::CandidateMissing)?;
            let digest: String = digest.iter().map(|v| format!("{v:02x}")).collect();
            let directory = paths_installation(identity, &digest)?;
            let (version, commit) = super::runtime::describe(&directory, identity)?;
            let candidate = super::RetainedInstallation {
                directory,
                version,
                commit,
            };
            let Some(approval) = service.prepare_activation(&candidate)? else {
                return Ok(State::Done);
            };
            service.activate(approval)?.phase
        }
        Action::Return => service.return_to_previous(service.prepare_return()?)?.phase,
    };
    Ok(activation_outcome(phase))
}

/// The ledger outcome an activation phase proves.
pub(crate) fn activation_outcome(phase: super::activation::ActivationPhase) -> State {
    use super::activation::ActivationPhase;
    match phase {
        ActivationPhase::Completed { .. } => State::Done,
        ActivationPhase::Failed { .. } => State::Failed {
            code: "startup_failed".to_owned(),
        },
        ActivationPhase::Activating | ActivationPhase::Uncertain => State::Unresolved,
    }
}

fn paths_installation(identity: &str, digest: &str) -> Result<std::path::PathBuf, ServerError> {
    Ok(crate::host::Paths::for_identity(identity)?
        .data
        .join("installations")
        .join(digest))
}

/// Starts `argv` outside this process's service unit or job, without waiting for it.
#[cfg(target_os = "linux")]
fn spawn_detached(unit: &str, argv: &[OsString]) -> Result<(), ServerError> {
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let mut command = std::process::Command::new("/usr/bin/systemd-run");
    command
        .args(["--user", "--collect", "--quiet", "--no-ask-password"])
        .arg(format!("--unit={unit}"))
        .arg("--")
        .args(argv)
        .env_clear()
        .env("XDG_RUNTIME_DIR", format!("/run/user/{uid}"))
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path=/run/user/{uid}/bus"),
        );
    super::command::execute(&mut command, Duration::from_secs(20)).map(drop)
}

/// macOS has no `POSIX_SPAWN_SETSID` in `libc` yet. The value is from `<sys/spawn.h>`.
#[cfg(target_os = "macos")]
const POSIX_SPAWN_SETSID: libc::c_int = 0x0400;

/// Starts `argv` outside this process's service unit or job, without waiting for it.
#[cfg(target_os = "macos")]
fn spawn_detached(_unit: &str, argv: &[OsString]) -> Result<(), ServerError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let strings = argv
        .iter()
        .map(|v| CString::new(v.as_bytes()).map_err(|_| ServerError::RuntimeUnavailable))
        .collect::<Result<Vec<_>, _>>()?;
    let program = strings.first().ok_or(ServerError::RuntimeUnavailable)?;
    let mut pointers: Vec<*mut libc::c_char> =
        strings.iter().map(|v| v.as_ptr().cast_mut()).collect();
    pointers.push(std::ptr::null_mut());
    let environment = [
        c"PATH=/usr/bin:/bin:/usr/sbin:/sbin".as_ptr().cast_mut(),
        std::ptr::null_mut(),
    ];
    // SAFETY: every pointer refers to a live terminated string or to the attribute and file
    // action objects, which are initialized before use and destroyed exactly once below.
    unsafe {
        let mut attributes: libc::posix_spawnattr_t = std::ptr::null_mut();
        let mut actions: libc::posix_spawn_file_actions_t = std::ptr::null_mut();
        if libc::posix_spawnattr_init(&mut attributes) != 0 {
            return Err(ServerError::ServiceUnavailable);
        }
        if libc::posix_spawn_file_actions_init(&mut actions) != 0 {
            libc::posix_spawnattr_destroy(&mut attributes);
            return Err(ServerError::ServiceUnavailable);
        }
        // A new session leaves the job's process group. CLOEXEC_DEFAULT closes every descriptor
        // the file actions do not name, so no server socket, lock or PTY reaches the helper.
        let flags =
            POSIX_SPAWN_SETSID | libc::POSIX_SPAWN_CLOEXEC_DEFAULT | libc::POSIX_SPAWN_SETSIGDEF;
        let mut all: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&mut all);
        let mut result = libc::posix_spawnattr_setflags(&mut attributes, flags as libc::c_short);
        if result == 0 {
            result = libc::posix_spawnattr_setsigdefault(&mut attributes, &all);
        }
        for fd in 0..3 {
            if result == 0 {
                result = libc::posix_spawn_file_actions_addopen(
                    &mut actions,
                    fd,
                    c"/dev/null".as_ptr(),
                    if fd == 0 {
                        libc::O_RDONLY
                    } else {
                        libc::O_WRONLY
                    },
                    0,
                );
            }
        }
        let mut pid = 0;
        if result == 0 {
            result = libc::posix_spawn(
                &mut pid,
                program.as_ptr(),
                &actions,
                &attributes,
                pointers.as_ptr(),
                environment.as_ptr(),
            );
        }
        libc::posix_spawn_file_actions_destroy(&mut actions);
        libc::posix_spawnattr_destroy(&mut attributes);
        if result != 0 {
            return Err(ServerError::ServiceUnavailable);
        }
        // Reap the helper if it exits while this server still runs. Otherwise launchd adopts it.
        let _ = std::thread::Builder::new()
            .name("spaceterm-service-helper".into())
            .spawn(move || {
                let mut status = 0;
                while libc::waitpid(pid, &mut status, 0) == -1
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                }
            });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! The native probe proves the property the helper exists for: a helper launched by a
    //! service job survives that job's Stop, including a Stop the helper itself dispatches. The
    //! test binary plays every role. The job and helper roles run only when their fixture
    //! directory is named on the command line, so an ordinary `--ignored` run skips them.
    use super::*;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    const JOB: &str = "service::helper::tests::probe_job";
    const HELPER: &str = "service::helper::tests::probe_helper";

    #[test]
    fn helper_arguments_name_the_hidden_step_and_its_record() {
        let op = OperationId([0xab; 16]);
        assert_eq!(
            arguments("spaceterm-preflight", &op),
            [
                "server",
                "provision",
                "service-run",
                "--operation",
                &"ab".repeat(16),
                "--identity",
                "spaceterm-preflight"
            ]
            .map(OsString::from)
        );
    }

    /// The fixture directory named by a role's command line.
    fn fixture() -> Option<PathBuf> {
        std::env::args()
            .skip(1)
            .find(|v| v.starts_with('/') && v.contains("spaceterm-helper-probe-"))
            .map(PathBuf::from)
    }

    /// This process's ID and session, as `PID SESSION`.
    fn identities() -> String {
        // SAFETY: getsid has no preconditions.
        format!("{} {}", std::process::id(), unsafe { libc::getsid(0) })
    }

    fn read_identities(path: &Path) -> (i32, i32) {
        let text = std::fs::read_to_string(path).unwrap();
        let (pid, session) = text.split_once(' ').unwrap();
        (pid.parse().unwrap(), session.parse().unwrap())
    }

    fn role(name: &str, root: &Path) -> Vec<OsString> {
        vec![
            std::env::current_exe().unwrap().into_os_string(),
            name.into(),
            root.as_os_str().to_owned(),
            "--exact".into(),
            "--ignored".into(),
            "--test-threads=1".into(),
        ]
    }

    fn label(root: &Path) -> String {
        root.file_name().unwrap().to_str().unwrap().to_owned()
    }

    fn wait_for(path: &Path, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while !path.exists() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        true
    }

    /// The fixture service: launches the helper, then runs until it is stopped.
    #[test]
    #[ignore = "a role of helper_outlives_the_stop_of_the_job_that_launched_it"]
    fn probe_job() {
        let Some(root) = fixture() else { return };
        extern "C" fn exit_cleanly(_: libc::c_int) {
            // SAFETY: _exit is async-signal-safe.
            unsafe { libc::_exit(0) }
        }
        // The server exits successfully on SIGTERM, so launchd's KeepAlive does not restart it.
        // SAFETY: the handler only calls an async-signal-safe function.
        unsafe {
            libc::signal(
                libc::SIGTERM,
                exit_cleanly as *const () as libc::sighandler_t,
            )
        };
        spawn_detached(&format!("{}-helper", label(&root)), &role(HELPER, &root)).unwrap();
        std::fs::write(root.join("launched"), identities()).unwrap();
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    /// The fixture helper: stops the job that launched it the way Stop does, then reports.
    #[test]
    #[ignore = "a role of helper_outlives_the_stop_of_the_job_that_launched_it"]
    fn probe_helper() {
        let Some(root) = fixture() else { return };
        std::fs::write(root.join("helper"), identities()).unwrap();
        wait_for(&root.join("launched"), Duration::from_secs(10));
        stop_job(&root);
        // Outlive the job's teardown by a margin before reporting.
        std::thread::sleep(Duration::from_secs(3));
        std::fs::write(root.join("survived"), b"").unwrap();
    }

    #[cfg(target_os = "macos")]
    fn target(root: &Path) -> String {
        // SAFETY: geteuid has no preconditions.
        format!("gui/{}/{}", unsafe { libc::geteuid() }, label(root))
    }

    #[cfg(target_os = "macos")]
    fn launchctl(args: &[&str]) -> Result<Vec<u8>, ServerError> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        super::super::command("/bin/launchctl", &args)
    }

    #[cfg(target_os = "macos")]
    fn job_pid(root: &Path) -> Option<u32> {
        let bytes = launchctl(&["list", &label(root)]).ok()?;
        String::from_utf8_lossy(&bytes).lines().find_map(|line| {
            line.trim()
                .strip_prefix("\"PID\" = ")?
                .strip_suffix(';')?
                .parse()
                .ok()
        })
    }

    /// Stop as the LaunchAgent supervisor does: SIGTERM, wait for the exit, then bootout.
    #[cfg(target_os = "macos")]
    fn stop_job(root: &Path) {
        launchctl(&["disable", &target(root)]).unwrap();
        launchctl(&["kill", "SIGTERM", &target(root)]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while job_pid(root).is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        launchctl(&["bootout", &target(root)]).unwrap();
        std::fs::write(root.join("stopped"), b"").unwrap();
    }

    #[cfg(target_os = "macos")]
    fn start_job(root: &Path) {
        let program: Vec<String> = role(JOB, root)
            .iter()
            .map(|v| {
                v.to_str()
                    .unwrap()
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
            })
            .map(|v| format!("<string>{v}</string>"))
            .collect();
        // The same session limits and exit timeout as the server's LaunchAgent. The plist stays
        // in the fixture directory, so no login item is registered.
        let plist = root.join(format!("{}.plist", label(root)));
        std::fs::write(
            &plist,
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{}</string>
<key>ProgramArguments</key><array>{}</array>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
<key>ProcessType</key><string>Interactive</string>
<key>LimitLoadToSessionType</key><string>Aqua</string>
<key>ExitTimeOut</key><integer>20</integer>
</dict></plist>
"#,
                label(root),
                program.join("")
            ),
        )
        .unwrap();
        // SAFETY: geteuid has no preconditions.
        let domain = format!("gui/{}", unsafe { libc::geteuid() });
        launchctl(&["bootstrap", &domain, plist.to_str().unwrap()]).unwrap();
    }

    #[cfg(target_os = "macos")]
    fn remove_job(root: &Path) {
        let _ = launchctl(&["bootout", &target(root)]);
        let _ = launchctl(&["enable", &target(root)]);
    }

    #[cfg(target_os = "linux")]
    fn systemctl(args: &[&str]) -> Result<Vec<u8>, ServerError> {
        let mut command = std::process::Command::new("/usr/bin/systemctl");
        command.args(["--user", "--no-pager"]).args(args);
        super::super::command::execute(&mut command, Duration::from_secs(20))
    }

    /// Whether the job unit still runs, by the user-service supervisor's own rule: an active
    /// state, a main process, or any task left in its control group.
    #[cfg(target_os = "linux")]
    fn job_active(root: &Path) -> bool {
        let Ok(bytes) = systemctl(&[
            "show",
            &format!("{}.service", label(root)),
            "--property=ActiveState,MainPID,TasksCurrent",
        ]) else {
            return true;
        };
        let text = String::from_utf8_lossy(&bytes);
        let value = |key: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
                .unwrap_or_default()
                .to_owned()
        };
        !matches!(value("ActiveState").as_str(), "inactive" | "failed")
            || !matches!(value("TasksCurrent").as_str(), "0" | "[not set]")
            || value("MainPID") != "0"
    }

    /// Stop as the user-service supervisor does: a nonblocking stop, then wait for inactivity.
    #[cfg(target_os = "linux")]
    fn stop_job(root: &Path) {
        systemctl(&["--no-block", "stop", &format!("{}.service", label(root))]).unwrap();
        // A helper left in the job's control group would keep it running for this whole wait.
        let deadline = Instant::now() + Duration::from_secs(30);
        while job_active(root) {
            if Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        std::fs::write(root.join("stopped"), b"").unwrap();
    }

    /// The fixture job runs in a transient unit with the server unit's kill policy.
    #[cfg(target_os = "linux")]
    fn start_job(root: &Path) {
        let mut command = std::process::Command::new("/usr/bin/systemd-run");
        command
            .args(["--user", "--quiet", "--collect"])
            .arg(format!("--unit={}", label(root)))
            .args([
                "--property=KillMode=mixed",
                "--property=SendSIGKILL=no",
                "--property=TimeoutStopSec=infinity",
                "--",
            ])
            .args(role(JOB, root));
        super::super::command::execute(&mut command, Duration::from_secs(20)).unwrap();
    }

    #[cfg(target_os = "linux")]
    fn remove_job(root: &Path) {
        for unit in [label(root), format!("{}-helper", label(root))] {
            let _ = systemctl(&["stop", &format!("{unit}.service")]);
        }
    }

    struct Probe(PathBuf);
    impl Drop for Probe {
        fn drop(&mut self) {
            remove_job(&self.0);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    #[ignore = "starts a fixture service job; run mise run server:admin:native"]
    fn helper_outlives_the_stop_of_the_job_that_launched_it() {
        let suffix =
            u64::from_be_bytes(crate::protocol::random().unwrap()[..8].try_into().unwrap());
        let parent = std::env::temp_dir().canonicalize().unwrap();
        #[cfg(target_os = "macos")]
        let name =
            format!("io.github.sadiksaifi.spaceterm.validation.spaceterm-helper-probe-{suffix:x}");
        #[cfg(target_os = "linux")]
        let name = format!("spaceterm-helper-probe-{suffix:x}");
        let probe = Probe(parent.join(name));
        std::fs::create_dir(&probe.0).unwrap();
        start_job(&probe.0);
        assert!(
            wait_for(&probe.0.join("launched"), Duration::from_secs(20)),
            "the job never launched its helper"
        );
        let (job, job_session) = read_identities(&probe.0.join("launched"));
        assert!(
            wait_for(&probe.0.join("helper"), Duration::from_secs(20)),
            "the helper never started"
        );
        let (_, helper_session) = read_identities(&probe.0.join("helper"));
        assert_ne!(
            helper_session, job_session,
            "the helper shares the job's session"
        );
        assert!(
            wait_for(&probe.0.join("stopped"), Duration::from_secs(60)),
            "the job never stopped"
        );
        // SAFETY: kill with signal 0 only checks that the process exists.
        assert_ne!(
            unsafe { libc::kill(job, 0) },
            0,
            "the job survived its Stop"
        );
        assert!(
            wait_for(&probe.0.join("survived"), Duration::from_secs(20)),
            "the helper did not survive the Stop of its job"
        );
    }
}
