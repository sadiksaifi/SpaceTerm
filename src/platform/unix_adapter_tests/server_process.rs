use super::*;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

#[path = "../../../crates/spaceterm-server-host/examples/support/repository.rs"]
#[expect(
    dead_code,
    reason = "the SSH fixture reuses owning-server mutation and inventory helpers"
)]
mod repository;
#[path = "../../../crates/spaceterm-server-host/examples/support/terminal_semantics.rs"]
mod terminal_semantics;
#[path = "../../../crates/spaceterm-server-host/examples/support/worktree.rs"]
#[expect(
    dead_code,
    reason = "the SSH fixture uses journaled writes and exact removal review"
)]
mod worktree_fixture;
#[test]
fn installer_executes_the_verified_archive_layout_and_cleans_its_temporary_files() {
    // This fixture exercises the generated shell and archive layout without service effects.
    let root = std::env::temp_dir().join(format!("st-install-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut bytes = Vec::new();
    {
        let compressed = flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::default());
        let mut archive = tar::Builder::new(compressed);
        let helper = b"#!/bin/sh\ncase $1 in ssh-enable-linger) exit 0;; ssh-enable-linger-password) IFS= read -r password; test \"$password\" = fixture;; ssh-install) test -f \"$3\" && test -f \"$4\";; *) exit 91;; esac\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(helper.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, "server/bin/spaceterm-server", &helper[..])
            .unwrap();
        archive.into_inner().unwrap().finish().unwrap();
    }
    let artifact = VerifiedArtifact {
        digest: hex_digest(&bytes),
        bytes,
        signature: vec![1; 64],
        version: "test".into(),
        platform: ("Linux".into(), "x86_64".into()),
    };
    for (password, input, expected_code) in [
        (false, None, 0),
        (true, Some("fixture"), 0),
        (true, Some("wrong"), 77),
    ] {
        let script = crate::platform::unix_ssh_process::UnixSshProcessAdapter
            .install(
                identity(),
                artifact.bytes.len(),
                &artifact.signature,
                &artifact.digest,
                password,
            )
            .replace("${TMPDIR:-/tmp}", root.to_str().unwrap())
            .replace("if [ \"$(uname -s)\" = Linux ]; then", "if true; then");
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut writer = child.stdin.take().unwrap();
        if let Some(input) = input {
            writeln!(writer, "{input}").unwrap();
        }
        writer.write_all(&artifact.bytes).unwrap();
        drop(writer);
        assert_eq!(child.wait().unwrap().code(), Some(expected_code));
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }
    std::fs::remove_dir(root).unwrap();
}

use crate::ssh::askpass::{
    AskPassAttempt, AskPassAttemptObservation, AskPassBrokerLease, AskPassLease, AskPassUnavailable,
};
struct NoPrompt;
impl AskPassLease for NoPrompt {
    fn entries(&self) -> Vec<(&'static str, &std::ffi::OsStr)> {
        vec![
            ("SSH_ASKPASS", std::ffi::OsStr::new("/usr/bin/false")),
            ("SSH_ASKPASS_REQUIRE", std::ffi::OsStr::new("force")),
            ("DISPLAY", std::ffi::OsStr::new(":0")),
        ]
    }
    fn cancel(&self) {}
    fn capability(&self) -> &[u8] {
        b"native-no-prompt"
    }
}
impl AskPassAttemptFactory for NoPrompt {
    fn start_attempt(&self, _: &AppPaths) -> Result<AskPassAttempt, AskPassUnavailable> {
        Ok(AskPassAttempt {
            lease: AskPassBrokerLease::new(Arc::new(NoPrompt)),
            observation: AskPassAttemptObservation::default(),
        })
    }
}
#[derive(Clone, Default)]
struct ObservedAdapter {
    spawns: Arc<std::sync::atomic::AtomicUsize>,
    last_exit: Arc<std::sync::atomic::AtomicI32>,
}
impl SshProcessAdapter for ObservedAdapter {
    type Process = crate::platform::unix_ssh_process::UnixSshProcess;
    fn spawn(
        &self,
        request: super::super::process::SshProcessSpawnRequest,
    ) -> Result<
        super::super::process::SpawnedSshProcess<Self::Process>,
        super::super::process::SshProcessMechanismError,
    > {
        self.spawns
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let result = crate::platform::unix_ssh_process::UnixSshProcessAdapter.spawn(request);
        if let Err(error) = &result {
            eprintln!("native spawn classification: {error:?}");
        }
        result
    }
    fn try_status(
        &self,
        process: &mut Self::Process,
    ) -> Result<
        Option<super::super::process::ProcessExit>,
        super::super::process::SshProcessMechanismError,
    > {
        let status =
            crate::platform::unix_ssh_process::UnixSshProcessAdapter.try_status(process)?;
        if let Some(exit) = status {
            self.last_exit.store(
                exit.code().unwrap_or(-1),
                std::sync::atomic::Ordering::SeqCst,
            );
        }
        Ok(status)
    }
    fn signal(
        &self,
        process: &mut Self::Process,
        signal: super::super::process::ProcessSignal,
    ) -> Result<(), super::super::process::SshProcessMechanismError> {
        crate::platform::unix_ssh_process::UnixSshProcessAdapter.signal(process, signal)
    }
    fn reap(
        &self,
        process: Self::Process,
    ) -> Result<(), super::super::process::SshProcessMechanismError> {
        crate::platform::unix_ssh_process::UnixSshProcessAdapter.reap(process)
    }
}
struct NativeBroker;
impl crate::ssh::askpass::AskPassPresenter for NativeBroker {
    fn cancel_active(&self) {}
    fn present(
        &self,
        _: crate::ssh::askpass::prompt::AskPassRequest,
        _: &std::sync::atomic::AtomicBool,
    ) -> Result<
        crate::ssh::askpass::AskPassProtocolReply,
        crate::ssh::askpass::AskPassPresentationFailure,
    > {
        Err(crate::ssh::askpass::AskPassPresentationFailure::Unavailable)
    }
}
impl AskPassAttemptFactory for NativeBroker {
    fn start_attempt(&self, paths: &AppPaths) -> Result<AskPassAttempt, AskPassUnavailable> {
        let attempt = crate::ssh::askpass::start_attempt_with_presenter(
            paths,
            std::env::current_exe().unwrap(),
            &crate::platform::unix_askpass_transport::UnixAskPassLocalIpc,
            Arc::new(NativeBroker),
        );
        eprintln!("native broker bound: {}", attempt.is_ok());
        attempt
    }
}
/// One explicitly selected account, test-owned client transport state and real adapters.
struct NativeFixture {
    destination: SshDestination,
    /// What the person running this expects of the account: stopped, missing or ready.
    expected: String,
    /// The disposable client state this fixture owns and removes.
    root: PathBuf,
    /// The disposable client's own private storage, which retains its records.
    paths: Arc<AppPaths>,
    adapter: ObservedAdapter,
    backend: NativeSshConnectBackend<ObservedAdapter>,
}
fn native_fixture(cx: &mut gpui::TestAppContext, name: &str) -> NativeFixture {
    native_fixture_with_artifacts(
        cx,
        name,
        SshServerArtifacts::new(
            std::env::var_os("SPACETERM_SSH_SERVER_ARTIFACTS").map(PathBuf::from),
        ),
    )
}

fn native_fixture_with_artifacts(
    cx: &mut gpui::TestAppContext,
    name: &str,
    artifacts: SshServerArtifacts,
) -> NativeFixture {
    use crate::platform::{
        app_directories::{AppDirectories, AppDirectoryEnvironment},
        unix_local_socket::UnixControlSocketProbe,
        unix_secure_filesystem::UnixSecureFilesystem,
    };
    let destination = SshDestination::new(
        std::env::var("SPACETERM_SSH_NATIVE_DESTINATION").expect("explicit destination required"),
    )
    .unwrap();
    let expected =
        std::env::var("SPACETERM_SSH_NATIVE_EXPECT").unwrap_or_else(|_| "stopped".into());
    let root = std::fs::canonicalize("/tmp")
        .unwrap()
        .join(format!("sts-{name}-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let env = AppDirectoryEnvironment {
        home: Some(home.clone().into()),
        xdg_config_home: Some(root.join("config").into()),
        xdg_data_home: Some(root.join("data").into()),
        xdg_state_home: Some(root.join("state").into()),
        xdg_cache_home: Some(root.join("cache").into()),
        xdg_runtime_dir: Some(root.join("runtime").into()),
        ..Default::default()
    };
    let actual_paths = std::env::var_os("SPACETERM_SSH_NATIVE_REAL_PATHS").as_deref()
        == Some(std::ffi::OsStr::new("1"));
    let directories = if actual_paths {
        assert!(
            std::env::var_os("SPACETERM_SSH_NATIVE_CONFIG").is_none(),
            "actual configuration is read only"
        );
        AppDirectories::resolve(identity()).unwrap()
    } else {
        AppDirectories::resolve_xdg(identity(), &env, Some(root.clone())).unwrap()
    };
    let paths = Arc::new(
        AppPaths::from_directories(directories, 103, Arc::new(UnixSecureFilesystem)).unwrap(),
    );
    paths
        .ensure_secure_root(crate::platform::app_directories::AppDirectoryRoot::Config)
        .unwrap();
    if let Some(configuration) = std::env::var_os("SPACETERM_SSH_NATIVE_CONFIG") {
        std::fs::write(
            paths.managed_ssh_config(),
            std::fs::read(configuration).unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(
            paths.managed_ssh_config(),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let adapter = ObservedAdapter::default();
    let fixture_paths = Arc::clone(&paths);
    let backend = NativeSshConnectBackend::new(NativeSshServerConfig {
        paths,
        home,
        startup: StartupSshEnvironment::from_environment(
            |key| std::env::var_os(key),
            "/usr/bin:/bin".into(),
        )
        .unwrap(),
        executable: OpenSshExecutable::new("/usr/bin/ssh".into()).unwrap(),
        adapter: adapter.clone(),
        socket_probe: Arc::new(UnixControlSocketProbe),
        askpass: if actual_paths {
            Arc::new(NativeBroker)
        } else {
            Arc::new(NoPrompt)
        },
        executor: cx.background_executor.clone(),
        artifacts,
        filesystem: Arc::new(
            crate::platform::unix_host_config_filesystem::UnixHostConfigFilesystem,
        ),
        bootstrap: Arc::new(crate::platform::unix_ssh_process::UnixSshProcessAdapter),
    });
    NativeFixture {
        destination,
        expected,
        root,
        paths: fixture_paths,
        adapter,
        backend,
    }
}

fn terminal_command(
    connection: &mut spaceterm_server::pane::PaneConnection,
    lease: spaceterm_server::pane::Lease,
    text: &str,
) -> Result<(), spaceterm_server::ServerError> {
    use spaceterm_server::pane::{FocusTarget, InteractiveInput, Key, Request};
    let key = |physical, logical: &str, text| Key {
        physical,
        action: 0,
        logical: logical.into(),
        text,
        unshifted: None,
        modifiers: 0,
        consumed: 0,
        option_as_alt: false,
    };
    connection.exchange(&Request::Focus {
        lease,
        generation: 1,
        target: FocusTarget::Terminal,
    })?;
    connection.exchange(&Request::Input {
        lease,
        generation: 1,
        input: InteractiveInput::Key(key(
            crate::runtime::PhysicalKey::Unidentified as u32,
            "text-input",
            Some(text.into()),
        )),
    })?;
    connection.exchange(&Request::Input {
        lease,
        generation: 1,
        input: InteractiveInput::Key(key(
            crate::runtime::PhysicalKey::Enter as u32,
            "enter",
            None,
        )),
    })?;
    Ok(())
}

/// Observes only a fixture's static witness or its bounded shell exit status.
fn terminal_fixture_command(
    connection: &mut spaceterm_server::pane::PaneConnection,
    lease: spaceterm_server::pane::Lease,
    command: &str,
    stage: &'static str,
    witness: &'static str,
) -> Result<Box<spaceterm_server::pane::Screen>, terminal_semantics::Failure> {
    let status = format!("ST-FIXTURE-{stage}-EXIT=");
    let command = format!("{command}; printf '\\n{status}%d\\n' \"$?\"");
    terminal_semantics::stage(stage, terminal_command(connection, lease, &command))?;
    let lines = |screen: &spaceterm_server::pane::Screen| {
        screen
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
    };
    let output = terminal_semantics::screen(connection, lease, |screen| {
        lines(screen).iter().any(|line| {
            line.trim_end() == witness
                || line
                    .trim_end()
                    .strip_prefix(&status)
                    .is_some_and(|code| code.parse::<u8>().is_ok())
        })
    })
    .map_err(|failure| failure.at(stage))?;
    let lines = lines(&output);
    if lines.iter().any(|line| line.trim_end() == witness) {
        return Ok(output);
    }
    if let Some(code) = lines.iter().find_map(|line| {
        line.trim_end()
            .strip_prefix(&status)
            .and_then(|code| code.parse::<u8>().ok())
    }) {
        eprintln!("SSH fixture shell failure: stage={stage}, exit={code}");
        return Err(terminal_semantics::Failure::from("fixture-shell-command-failed").at(stage));
    }
    Err(terminal_semantics::Failure::from("fixture-shell-witness-missing").at(stage))
}

/// Every mutation and shell command targets a Workspace this invocation created on this route.
/// A retained original prevents cleanup until that exact write has been resolved.
fn native_server_owned_work(
    server: &spaceterm_server::LocalServer,
) -> Result<(), terminal_semantics::Failure> {
    use spaceterm_server::{
        hierarchy::{CloseTarget, Mutation, Outcome},
        pane::{PaneSize, Reply, Request},
        worktree::{Branch, BranchName, Operation},
    };
    use terminal_semantics::stage as api;
    if std::env::var("SPACETERM_DEVELOPMENT_CLIENT").as_deref() != Ok("ssh-native-semantics") {
        return Err("SSH fixture requires its dedicated development client identity".into());
    }
    let journal = api("ssh-open-fixture-journal", server.shared_journal())?;
    if journal.pending().is_some() {
        return Err("SSH fixture has an unresolved original; preserve it before new work".into());
    }
    drop(journal);
    let mut connection = api("ssh-connect-pane", server.connect_shared_pane())?;
    let baseline = api(
        "ssh-baseline-inventory",
        repository::inventory(&mut connection),
    )?
    .0;
    let created = api(
        "ssh-create-workspace",
        worktree_fixture::journaled_mutation(
            server,
            &mut connection,
            Mutation::CreateWorkspace {
                name: "SSH native semantics".into(),
                directory: None,
            },
        ),
    )?;
    let Outcome::Accepted { target: workspace } = created.outcome else {
        return Err("SSH native fixture Workspace creation refused".into());
    };
    let mut remote_root = None;
    let mut root_identity = None;
    let exercise = (|| -> Result<_, terminal_semantics::Failure> {
        let current = api(
            "ssh-fixture-inventory",
            repository::inventory(&mut connection),
        )?
        .0;
        let pane = current
            .workspaces
            .iter()
            .find(|work| work.id == workspace)
            .and_then(|work| work.tabs.first())
            .ok_or("SSH fixture Pane missing")?
            .root_pane;
        let Reply::Attached { lease } = api(
            "ssh-select-fixture-pane",
            connection.exchange(&Request::SelectPane {
                pane,
                explicit: true,
                style: None,
                size: PaneSize {
                    columns: 80,
                    rows: 24,
                    cell_width: 8,
                    cell_height: 18,
                },
            }),
        )?
        else {
            return Err("SSH fixture Pane did not start".into());
        };
        let report = terminal_semantics::exercise(&mut connection, lease, terminal_command)?;
        if !connection.worktree_writes() {
            return Err("SSH Worktree capability missing".into());
        }
        // Only the owning server's shell creates or accesses these paths. They never enter local I/O.
        terminal_fixture_command(
            &mut connection,
            lease,
            r#"st_561_root=$(mktemp -d /tmp/st561.XXXXXXXX) && printf 'ST-ROOT-CREATED\n'"#,
            "ssh-root-mkdir",
            "ST-ROOT-CREATED",
        )?;
        // A cd inside command substitution captures the integration's chpwd OSC output.
        // Resolve the server path without changing the shell's directory or invoking hooks.
        terminal_fixture_command(
            &mut connection,
            lease,
            r#"st_561_root=$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$st_561_root") && printf 'ST-ROOT-CANONICAL\n'"#,
            "ssh-root-canonicalize",
            "ST-ROOT-CANONICAL",
        )?;
        let output = terminal_fixture_command(
            &mut connection,
            lease,
            r#"st_561_identity=$(python3 -c 'import os,sys; s=os.stat(sys.argv[1],follow_symlinks=False); print("%d:%d"%(s.st_dev,s.st_ino))' "$st_561_root") && printf 'ST-FIXTURE-ROOT=%s\nST-FIXTURE-IDENTITY=%s\nST-ROOT-READY\n' "$st_561_root" "$st_561_identity""#,
            "ssh-root-identity",
            "ST-ROOT-READY",
        )?;
        let lines = output
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let root = lines
            .iter()
            .find_map(|line| line.trim_end().strip_prefix("ST-FIXTURE-ROOT="))
            .ok_or("SSH fixture root identity missing")?;
        let path = PathBuf::from(root);
        if !matches!(
            path.parent().and_then(|p| p.to_str()),
            Some("/tmp" | "/private/tmp")
        ) || !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| {
                name.starts_with("st561.")
                    && name.len() == 14
                    && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')
            })
        {
            return Err("SSH fixture root shape changed".into());
        }
        let identity = lines
            .iter()
            .find_map(|line| line.trim_end().strip_prefix("ST-FIXTURE-IDENTITY="))
            .and_then(|value| value.split_once(':'))
            .and_then(|(device, inode)| {
                Some((device.parse::<u64>().ok()?, inode.parse::<u64>().ok()?))
            })
            .ok_or("SSH fixture filesystem identity missing")?;
        remote_root = Some((path.clone(), lease));
        root_identity = Some(identity);
        terminal_fixture_command(
            &mut connection,
            lease,
            r#"GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 git -C "$st_561_root" -c init.defaultBranch=main init -q && git -C "$st_561_root" config core.hooksPath /dev/null && git -C "$st_561_root" -c commit.gpgSign=false -c user.name=Fixture -c user.email=fixture@invalid commit -q --allow-empty -m fixture && printf 'ST-REPOSITORY-READY\n'"#,
            "ssh-repository-ready",
            "ST-REPOSITORY-READY",
        )?;
        let pinned = api(
            "ssh-pin-fixture-workspace",
            worktree_fixture::journaled_mutation(
                server,
                &mut connection,
                Mutation::PinWorkspace {
                    workspace,
                    version: 0,
                    directory: Some(path.clone()),
                },
            ),
        )?;
        if !matches!(pinned.outcome, Outcome::Accepted { .. }) {
            return Err("SSH fixture pin refused".into());
        }
        let worktree = path.join("created");
        let created = api(
            "ssh-create-fixture-worktree",
            worktree_fixture::journaled_mutation(
                server,
                &mut connection,
                Mutation::Worktree(Operation::Create {
                    workspace,
                    root: path.clone(),
                    path: worktree.clone(),
                    branch: Branch::New {
                        name: "native-created".into(),
                        base: BranchName::Local("main".into()),
                    },
                }),
            ),
        )?;
        if !matches!(created.outcome, Outcome::Accepted { .. }) {
            return Err("SSH Worktree creation refused".into());
        }
        let published = api(
            "ssh-observe-created-worktree",
            repository::await_facts(&mut connection, &path, |facts| {
                facts
                    .worktrees
                    .as_ref()
                    .is_some_and(|list| list.worktrees.iter().any(|row| row.root == worktree))
            }),
        )?;
        if !published
            .workspaces
            .iter()
            .filter(|work| work.id == workspace)
            .flat_map(|work| &work.tabs)
            .any(|tab| {
                tab.worktree.as_ref() == Some(&worktree)
                    && tab.worktree_repository.as_ref() == Some(&path)
            })
        {
            return Err("SSH Worktree Tab did not belong to its owning server".into());
        }
        terminal_fixture_command(
            &mut connection,
            lease,
            r#"test -d "$st_561_root/created" && printf 'ST-WORKTREE-EXISTS\n'"#,
            "ssh-worktree-exists",
            "ST-WORKTREE-EXISTS",
        )?;
        let removal = api(
            "ssh-review-worktree-removal",
            worktree_fixture::review(&mut connection, &path, &worktree),
        )?;
        let removed = api(
            "ssh-remove-fixture-worktree",
            worktree_fixture::journaled_mutation(
                server,
                &mut connection,
                Mutation::Worktree(removal),
            ),
        )?;
        if !matches!(removed.outcome, Outcome::Accepted { .. }) {
            return Err("SSH Worktree removal refused".into());
        }
        api(
            "ssh-observe-removed-worktree",
            repository::await_facts(&mut connection, &path, |facts| {
                facts
                    .worktrees
                    .as_ref()
                    .is_some_and(|list| list.worktrees.iter().all(|row| row.root != worktree))
            }),
        )?;
        terminal_fixture_command(
            &mut connection,
            lease,
            r#"test ! -e "$st_561_root/created" && printf 'ST-WORKTREE-REMOVED\n'"#,
            "ssh-worktree-removed",
            "ST-WORKTREE-REMOVED",
        )?;
        Ok(report)
    })();
    if let Err(failure) = &exercise {
        eprintln!("SSH fixture exercise failed: {failure}");
    }
    if api("ssh-cleanup-journal-check", server.shared_journal())?
        .pending()
        .is_some()
    {
        return Err("SSH fixture cleanup waits for original-operation recovery".into());
    }
    // Recheck the exact remote filesystem object before descriptor-safe recursive removal.
    // A changed target is retained, and an uncertain write never reaches this cleanup.
    let directory_cleanup = if let (Some((path, lease)), Some((device, inode))) =
        (remote_root, root_identity)
    {
        let root = path.to_str().ok_or("SSH fixture root encoding changed")?;
        let cleanup = format!(
            r#"python3 -c 'import os,shutil,stat,sys; p=sys.argv[1]; parent=os.open(os.path.dirname(p),os.O_RDONLY|os.O_DIRECTORY); name=os.path.basename(p); fd=os.open(name,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=parent); s=os.fstat(fd); assert (s.st_dev,s.st_ino)==({device},{inode}) and shutil.rmtree.avoids_symlink_attacks; [shutil.rmtree(n,dir_fd=fd) if stat.S_ISDIR(os.stat(n,dir_fd=fd,follow_symlinks=False).st_mode) else os.unlink(n,dir_fd=fd) for n in os.listdir(fd)]; s=os.stat(name,dir_fd=parent,follow_symlinks=False); assert (s.st_dev,s.st_ino)==({device},{inode}); os.rmdir(name,dir_fd=parent); os.close(fd); os.close(parent); print("ST-ROOT-REMOVED")' '{root}' 2>/dev/null"#
        );
        terminal_fixture_command(
            &mut connection,
            lease,
            &cleanup,
            "ssh-root-removed",
            "ST-ROOT-REMOVED",
        )
        .map(drop)
    } else {
        Ok(())
    };
    let target = CloseTarget::Workspace { workspace };
    let current = api(
        "ssh-cleanup-inventory",
        repository::inventory(&mut connection),
    )?
    .0;
    let confirmation = current
        .close_confirmation(target)
        .ok_or("SSH fixture close target missing")?;
    let closed = api(
        "ssh-close-fixture-workspace",
        worktree_fixture::journaled_mutation(
            server,
            &mut connection,
            Mutation::Close {
                target,
                confirmation,
            },
        ),
    )?;
    if !matches!(closed.outcome, Outcome::Accepted { .. }) {
        return Err("SSH fixture close refused".into());
    }
    let after = api(
        "ssh-verify-baseline-inventory",
        repository::inventory(&mut connection),
    )?
    .0;
    if !baseline
        .workspaces
        .iter()
        .all(|before| after.workspaces.iter().any(|work| work.id == before.id))
        || !baseline.close.as_ref().is_some_and(|before| {
            after.close.as_ref().is_some_and(|after| {
                before
                    .sessions
                    .iter()
                    .all(|pane| after.sessions.contains(pane))
            })
        })
    {
        return Err("baseline server work changed during the SSH fixture".into());
    }
    let report = exercise?;
    directory_cleanup?;
    eprintln!(
        "PASS SSH semantic protocol: copied rows={}, accessible rows={}, graphics supported={}, graphics bytes={}, owning-server Worktree create/remove; fixture cleanup and baseline work retained",
        report.copied_rows,
        report.accessible_rows,
        report.graphics_supported,
        report.graphics_bytes
    );
    Ok(())
}

/// Uses the selected client's real installation layout, with this test build's normal trust.
/// This exercises the native backend; desktop presentation still needs separate GUI evidence.
fn native_acceptance_catalog() -> PathBuf {
    let client = std::env::var_os("SPACETERM_SSH_NATIVE_CLIENT")
        .map(PathBuf::from)
        .expect("select the actual prepared client executable");
    let client = client
        .canonicalize()
        .expect("the prepared client must exist");
    let output = std::process::Command::new(&client)
        .arg("--build-info")
        .stderr(std::process::Stdio::null())
        .output()
        .expect("the prepared client must report its build identity");
    assert!(output.status.success(), "client build identity unavailable");
    let info: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("client build identity must be JSON");
    assert_eq!(info["schema"], 1);
    assert_eq!(
        info["identity"],
        identity(),
        "compile the matching test identity"
    );
    assert_eq!(info["target"], env!("SPACETERM_TARGET"));
    assert_eq!(info["version"], env!("SPACETERM_VERSION"));
    assert_eq!(info["commit"], env!("SPACETERM_COMMIT"));
    let prefix = client.parent().and_then(|path| path.parent()).unwrap();
    #[cfg(target_os = "macos")]
    let catalog = prefix.join("Resources/server-artifacts");
    #[cfg(target_os = "linux")]
    let catalog = prefix.join("share/spaceterm/server-artifacts");
    // Installed clients pin their native candidate. Development's foreground local server
    // needs no candidate, but its SSH catalog still undergoes normal signature verification.
    if cfg!(spaceterm_packaged) {
        let pinned = info["server_candidate_sha256"]
            .as_str()
            .filter(|digest| digest.len() == 64)
            .expect("the packaged client must pin its native server candidate");
        assert_eq!(pinned, env!("SPACETERM_SERVER_SHA256"));
        let digest = std::fs::read_to_string(
            catalog
                .join(env!("SPACETERM_TARGET"))
                .join("artifact.sha256"),
        )
        .expect("the selected client must include its native catalog entry");
        assert_eq!(digest.trim(), pinned, "client and catalog must match");
    }
    catalog
}

/// The runner binds the owner's published inventory to both accounts and one compiled identity.
/// This authorizes installation only; the fixture performs no reset or service removal.
fn native_acceptance_scope() -> u32 {
    let scope = std::env::var("SPACETERM_SSH_NATIVE_SCOPE")
        .expect("set the owner-confirmed sdk:LOCAL_UID:sdk:REMOTE_UID:IDENTITY scope");
    let fields = scope.split(':').collect::<Vec<_>>();
    assert_eq!(fields.len(), 5, "invalid owner-confirmed acceptance scope");
    assert_eq!(fields[0], "sdk");
    assert_eq!(fields[2], "sdk");
    assert_eq!(
        fields[4],
        identity(),
        "confirm exactly the compiled identity"
    );
    let local_uid: u32 = fields[1].parse().expect("explicit source UID required");
    let remote_uid: u32 = fields[3]
        .parse()
        .expect("explicit destination UID required");
    assert!(local_uid != 0 && remote_uid != 0);
    for (option, expected) in [("-un", "sdk".to_owned()), ("-u", local_uid.to_string())] {
        let actual = std::process::Command::new("/usr/bin/id")
            .arg(option)
            .output()
            .unwrap();
        assert!(actual.status.success(), "source account proof unavailable");
        assert_eq!(actual.stdout, format!("{expected}\n").as_bytes());
    }
    remote_uid
}

fn native_acceptance_account(
    backend: &NativeSshConnectBackend<ObservedAdapter>,
    destination: &SshDestination,
    expected_uid: u32,
    cancellation: &SshCancellationToken,
) -> u32 {
    let route = backend
        .route(destination, ConnectMode::Requested, cancellation, &|_| {})
        .unwrap_or_else(|error| panic!("account preflight: {}", failure_name(&error)));
    // Read-only proof on the same authenticated route that will receive the installation.
    // Freshness belongs to real account state, independently of client XDG directories.
    // The parent owns exact service cleanup and its recorded inventory before this check.
    let output = route
        .run(
            &r#"python3 - <<'PY'
import os, pathlib, pwd, stat, subprocess, sys
uid = os.geteuid()
account = pwd.getpwuid(uid)
assert uid == EXPECTED_UID and uid != 0 and account.pw_name == 'sdk'
home = pathlib.Path(account.pw_dir)
assert os.environ.get('HOME') == str(home)
metadata = home.lstat()
assert stat.S_ISDIR(metadata.st_mode) and metadata.st_uid == uid
identity = 'EXPECTED_IDENTITY'
temporary = pathlib.Path('/private/tmp' if sys.platform == 'darwin' else '/tmp')
paths = (
    home / '.local/share' / identity / 'server/service.json',
    home / 'Library/LaunchAgents' / ('io.github.sadiksaifi.' + identity + '.server.plist'),
    home / '.config/systemd/user' / ('io.github.sadiksaifi.' + identity + '.server.service'),
    temporary / (identity + '-server-' + str(uid)) / 's',
)
assert all(not path.exists() and not path.is_symlink() for path in paths)
if sys.platform == 'darwin':
    subprocess.run(['/bin/launchctl', 'print', 'gui/' + str(uid)], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
else:
    assert sys.platform == 'linux'
    linger = subprocess.run(['/usr/bin/loginctl', 'show-user', str(uid), '--property=Linger', '--value'],
                            check=True, capture_output=True, timeout=5)
    assert linger.stdout == b'yes\n'
    assert pathlib.Path('/run/user/' + str(uid) + '/bus').is_socket()
print(uid)
PY"#
                .replace("EXPECTED_UID", &expected_uid.to_string())
                .replace("EXPECTED_IDENTITY", identity()),
            Vec::new(),
            OBSERVATION,
            cancellation,
        )
        .unwrap_or_else(|error| panic!("account preflight: {}", failure_name(&error)));
    assert!(
        output.exit.is_success(),
        "requires the confirmed sdk UID, absent selected service policy/registration/endpoint and native service manager"
    );
    std::str::from_utf8(&output.stdout)
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .expect("account preflight must identify the confirmed UID")
}

#[gpui::test]
#[ignore = "installs into an owner-confirmed sdk account and build identity with fresh service state; requires matching signed packages"]
fn native_account_server_install_acceptance(cx: &mut gpui::TestAppContext) {
    use spaceterm_server::{
        hierarchy::{CloseTarget, Mutation, Outcome},
        pane::{PaneSize, Reply, Request},
    };
    let expected_account = native_acceptance_scope();
    assert_eq!(
        std::env::var("SPACETERM_SSH_NATIVE_INSTALL").as_deref(),
        Ok("1"),
        "explicit installation authorization required"
    );
    assert_eq!(
        std::env::var("SPACETERM_SSH_NATIVE_EXPECT").as_deref(),
        Ok("missing")
    );
    for key in [
        "SPACETERM_SSH_NATIVE_REAL_PATHS",
        "SPACETERM_SSH_SERVER_ARTIFACTS",
    ] {
        assert!(
            std::env::var_os(key).is_none(),
            "acceptance requires test-owned client transport state and the real catalog"
        );
    }
    assert!(
        std::env::var_os("SPACETERM_SSH_NATIVE_CONFIG").is_some(),
        "task-only SSH configuration required"
    );
    assert_eq!(
        std::env::var("SPACETERM_DEVELOPMENT_CLIENT").as_deref(),
        Ok("ssh-native-install-acceptance"),
        "use a dedicated fixture client identity"
    );
    let NativeFixture {
        destination,
        root,
        backend,
        ..
    } = native_fixture_with_artifacts(
        cx,
        "accept",
        SshServerArtifacts::new(Some(native_acceptance_catalog())),
    );
    let cancellation = SshCancellationToken::default();
    let account =
        native_acceptance_account(&backend, &destination, expected_account, &cancellation);
    let observed = backend
        .observe(&destination, None, &cancellation, &|_| {})
        .unwrap_or_else(|error| panic!("fresh observation: {}", failure_name(&error)));
    assert_eq!(observed.availability, ServerAvailability::NotInstalled);
    let offer = match backend.connect(
        &destination,
        ConnectMode::Requested,
        None,
        &cancellation,
        &|_| {},
    ) {
        Err(SshConnectFailure::InstallationOffered(offer)) => offer,
        Err(error) => panic!("verified installation offer: {}", failure_name(&error)),
        Ok(_) => panic!("fresh account connected without offering installation"),
    };
    assert_eq!(offer.platform, observed.facts.platform);
    let connected = offer
        .installation
        .install(&cancellation, &|_| {})
        .unwrap_or_else(|error| panic!("accepted installation: {}", failure_name(&error)));
    assert_eq!(connected.continuity.account, account);
    assert_eq!(
        connected.facts.version.as_deref(),
        Some(offer.version.as_str())
    );
    let mut pane_connection = connected.server.connect_shared_pane().unwrap();
    let created = worktree_fixture::journaled_mutation(
        &connected.server,
        &mut pane_connection,
        Mutation::CreateWorkspace {
            name: "SSH install acceptance".into(),
            directory: None,
        },
    )
    .unwrap();
    let Outcome::Accepted { target: workspace } = created.outcome else {
        panic!("installed server refused a Workspace")
    };
    let current = repository::inventory(&mut pane_connection).unwrap().0;
    let pane = current
        .workspaces
        .iter()
        .find(|entry| entry.id == workspace)
        .unwrap()
        .tabs[0]
        .root_pane;
    let Reply::Attached { lease } = pane_connection
        .exchange(&Request::SelectPane {
            pane,
            explicit: true,
            style: None,
            size: PaneSize {
                columns: 80,
                rows: 24,
                cell_width: 8,
                cell_height: 18,
            },
        })
        .unwrap()
    else {
        panic!("installed server did not start the Workspace's Pane")
    };
    terminal_fixture_command(
        &mut pane_connection,
        lease,
        "st648_live=accepted; printf 'ST648-INSTALLED\\n'",
        "install-acceptance",
        "ST648-INSTALLED",
    )
    .unwrap();
    let before = repository::inventory(&mut pane_connection).unwrap().0;
    let live = before
        .close
        .as_ref()
        .unwrap()
        .sessions
        .iter()
        .find(|entry| entry.id == pane)
        .unwrap();
    assert!(
        live.session.is_some(),
        "the usable Pane must own a live Terminal Session"
    );
    drop(pane_connection);
    backend.disconnect(&destination);
    // A new native backend has no candidate, while retaining this fixture's client identity.
    // It must discover the approved runtime and leave its existing shell alive.
    let config = &backend.config;
    let reused_backend = NativeSshConnectBackend::new(NativeSshServerConfig {
        paths: Arc::clone(&config.paths),
        home: config.home.clone(),
        startup: config.startup.clone(),
        executable: config.executable.clone(),
        adapter: config.adapter.clone(),
        socket_probe: Arc::clone(&config.socket_probe),
        askpass: Arc::clone(&config.askpass),
        executor: config.executor.clone(),
        artifacts: SshServerArtifacts::new(None),
        filesystem: Arc::clone(&config.filesystem),
        bootstrap: Arc::clone(&config.bootstrap),
    });
    let reused = reused_backend
        .connect(
            &destination,
            ConnectMode::Requested,
            Some(&connected.continuity),
            &cancellation,
            &|_| {},
        )
        .unwrap_or_else(|error| panic!("reuse without a catalog: {}", failure_name(&error)));
    assert_eq!(reused.continuity, connected.continuity);
    let mut pane_connection = reused.server.connect_shared_pane().unwrap();
    let after = repository::inventory(&mut pane_connection).unwrap().0;
    assert!(after.workspaces.iter().any(|entry| entry.id == workspace));
    let after_close = after.close.as_ref().unwrap();
    assert_eq!(after_close.boot, before.close.as_ref().unwrap().boot);
    assert!(
        after_close.sessions.contains(live),
        "reconnect replaced the live Terminal Session"
    );
    let Reply::Attached { lease } = pane_connection
        .exchange(&Request::SelectPane {
            pane,
            explicit: true,
            style: None,
            size: PaneSize {
                columns: 80,
                rows: 24,
                cell_width: 8,
                cell_height: 18,
            },
        })
        .unwrap()
    else {
        panic!("reused Workspace's Pane unavailable")
    };
    terminal_fixture_command(
        &mut pane_connection,
        lease,
        "test \"$st648_live\" = accepted && printf 'ST648-REUSED\\n'",
        "install-reuse",
        "ST648-REUSED",
    )
    .unwrap();
    let confirmation = after
        .close_confirmation(CloseTarget::Workspace { workspace })
        .unwrap();
    let closed = worktree_fixture::journaled_mutation(
        &reused.server,
        &mut pane_connection,
        Mutation::Close {
            target: CloseTarget::Workspace { workspace },
            confirmation,
        },
    )
    .unwrap();
    assert!(matches!(closed.outcome, Outcome::Accepted { .. }));
    drop(pane_connection);
    reused_backend.disconnect(&destination);
    drop(reused_backend);
    drop(backend);
    std::fs::remove_dir_all(root).unwrap();
    eprintln!(
        "PASS native SSH backend: identity={}, source={}, remote={}/{}, accepted install, usable Workspace, same live shell reused without a catalog; server retained for owner cleanup",
        identity(),
        env!("SPACETERM_TARGET"),
        connected.facts.platform.0,
        connected.facts.platform.1,
    );
}

/// Explicit env selects a disposable account. This test never invokes Stop or Start.
#[gpui::test]
#[ignore = "requires an explicitly selected disposable OpenSSH destination and signed artifacts"]
fn native_account_server_connect(cx: &mut gpui::TestAppContext) {
    let NativeFixture {
        destination,
        expected,
        root,
        adapter,
        backend,
        ..
    } = native_fixture(cx, "connect");
    let cancellation = SshCancellationToken::default();
    // Check Status reads the account's state without preparing, starting or installing anything.
    let observed = backend.observe(&destination, None, &cancellation, &|phase| {
        eprintln!("native check phase: {phase:?}")
    });
    backend.disconnect(&destination);
    match (expected.as_str(), &observed) {
        ("stopped", Err(SshConnectFailure::Stopped(_))) => {}
        ("missing", Ok(observation))
            if observation.availability == ServerAvailability::NotInstalled => {}
        ("ready", Ok(observation)) if observation.availability == ServerAvailability::Running => {
            assert!(observation.facts.version.is_some());
        }
        (_, Err(error)) => panic!("unexpected check classification: {}", failure_name(error)),
        _ => panic!("unexpected check availability"),
    }
    let mut result = backend.connect(
        &destination,
        ConnectMode::Requested,
        None,
        &cancellation,
        &|phase| eprintln!("native connect phase: {phase:?}"),
    );
    if let Err(SshConnectFailure::InstallationOffered(offer)) = result {
        if std::env::var_os("SPACETERM_SSH_NATIVE_INSTALL").as_deref()
            == Some(std::ffi::OsStr::new("1"))
        {
            result = offer.installation.install(&cancellation, &|_| {});
        } else {
            result = Err(SshConnectFailure::InstallationOffered(offer));
        }
    }
    eprintln!(
        "native observations: spawns={}, last_exit={}",
        adapter.spawns.load(std::sync::atomic::Ordering::SeqCst),
        adapter.last_exit.load(std::sync::atomic::Ordering::SeqCst)
    );
    match (expected.as_str(), result) {
        ("stopped", Err(SshConnectFailure::Stopped(_))) => {}
        ("missing", Err(SshConnectFailure::InstallationOffered(_))) => {}
        ("ready", Ok(connected)) => {
            assert_eq!(connected.server.refresh().unwrap().protocol_major, 1);
            native_server_owned_work(&connected.server)
                .unwrap_or_else(|failure| panic!("{failure}"));
            let reused = backend
                .connect(
                    &destination,
                    ConnectMode::QuietContinuation,
                    Some(&connected.continuity),
                    &cancellation,
                    &|_| {},
                )
                .unwrap_or_else(|_| panic!("existing route did not quietly reconnect"));
            assert_eq!(reused.server.refresh().unwrap().protocol_major, 1);
            let observed = adapter.spawns.load(std::sync::atomic::Ordering::SeqCst);
            // The long semantic exercise can outlive the original health channel's idle
            // deadline. Quiet continuation just admitted this lease's fresh transport.
            for _ in 0..4 {
                assert!(reused.lease.is_alive());
            }
            assert_eq!(
                adapter.spawns.load(std::sync::atomic::Ordering::SeqCst),
                observed,
                "idle health observations must reuse their authenticated transport"
            );
            // Kill only this test's master. Eligible direct software keys recover automatically.
            backend.disconnect(&destination);
            assert!(
                !connected.lease.is_alive(),
                "closed original route must become unavailable"
            );
            assert!(
                !reused.lease.is_alive(),
                "closed reused route must become unavailable"
            );
            let recovered = backend.connect(
                &destination,
                ConnectMode::QuietContinuation,
                Some(&connected.continuity),
                &cancellation,
                &|_| {},
            );
            if std::env::var_os("SPACETERM_SSH_NATIVE_QUIET").as_deref()
                == Some(std::ffi::OsStr::new("1"))
            {
                if let Err(error) = recovered {
                    panic!(
                        "eligible direct route did not reconnect after master loss: {}",
                        failure_name(&error)
                    );
                }
            } else {
                assert!(
                    matches!(recovered, Err(SshConnectFailure::NeedsPerson(_))),
                    "unproven route attempted background authentication"
                );
            }
        }
        (_, Err(error)) => panic!("unexpected classification: {}", failure_name(&error)),
        _ => panic!("unexpected connected result"),
    }
    backend.disconnect(&destination);
    let cancelled = SshCancellationToken::cancelled();
    assert!(matches!(
        backend.connect(
            &destination,
            ConnectMode::Requested,
            None,
            &cancelled,
            &|_| {}
        ),
        Err(SshConnectFailure::Cancelled)
    ));
    drop(backend);
    let _ = std::fs::remove_dir_all(root);
}
/// Explicit env selects a disposable account whose server is ready. This test reads one remote
/// maintenance observation and dispatches nothing: it never stops, restarts, uninstalls, stages,
/// updates or starts a server.
#[gpui::test]
#[ignore = "requires an explicitly selected disposable OpenSSH destination with a ready server"]
fn native_account_server_maintenance_observe(cx: &mut gpui::TestAppContext) {
    use crate::ssh::server_maintenance::{
        MaintenanceAuthority, MaintenanceTarget, SshMaintenanceBackend, SshMaintenanceFailure,
    };
    let NativeFixture {
        destination,
        expected,
        root,
        adapter,
        backend,
        ..
    } = native_fixture(cx, "maintenance");
    let cancellation = SshCancellationToken::default();
    // Only a connection admitted here can bind an original. Recovering an operation of a stopped
    // or uninstalled server belongs to the retained records in issue #560.
    assert_eq!(
        expected, "ready",
        "remote maintenance observation needs a ready server"
    );
    let connected = backend
        .connect(
            &destination,
            ConnectMode::Requested,
            None,
            &cancellation,
            &|phase| eprintln!("native connect phase: {phase:?}"),
        )
        .unwrap_or_else(|error| panic!("unexpected classification: {}", failure_name(&error)));
    let target = MaintenanceTarget {
        destination: destination.clone(),
        continuity: connected.continuity.clone(),
    };

    let facts = SshMaintenanceBackend::observe(
        &backend,
        &target,
        MaintenanceAuthority::Requested,
        &cancellation,
        &|phase| eprintln!("native maintenance phase: {phase:?}"),
    )
    .unwrap_or_else(|error| panic!("unexpected maintenance classification: {error:?}"))
    .facts;
    eprintln!(
        "native maintenance facts: status={:?}, startup={:?}, approved={:?}, activation={:?}",
        facts.status,
        facts.startup,
        facts.approved.as_ref().map(|build| &build.version),
        facts.activation.as_ref().map(|activation| activation.phase),
    );
    assert_eq!(
        (facts.server, facts.account, facts.build.as_str()),
        (
            connected.continuity.server,
            connected.continuity.account,
            connected.continuity.build.as_str()
        ),
        "the observation must come from the server the original bound"
    );
    assert!(facts.status.installed && !facts.status.stopped);

    // Background continuation reuses the admitted route without asking for authority.
    SshMaintenanceBackend::observe(
        &backend,
        &target,
        MaintenanceAuthority::Quiet,
        &cancellation,
        &|_| {},
    )
    .unwrap_or_else(|error| panic!("quiet continuation did not reuse the route: {error:?}"));

    // A server that is not the one bound pauses instead of being read.
    let changed = MaintenanceTarget {
        destination: destination.clone(),
        continuity: crate::ssh::server_connection::SshServerContinuity {
            server: [0xff; 32],
            ..connected.continuity.clone()
        },
    };
    assert!(
        matches!(
            SshMaintenanceBackend::observe(
                &backend,
                &changed,
                MaintenanceAuthority::Requested,
                &cancellation,
                &|_| {},
            ),
            Err(SshMaintenanceFailure::Changed(_))
        ),
        "a changed server must refuse before it is read"
    );

    eprintln!(
        "native maintenance observations: spawns={}, last_exit={}",
        adapter.spawns.load(std::sync::atomic::Ordering::SeqCst),
        adapter.last_exit.load(std::sync::atomic::Ordering::SeqCst)
    );
    SshMaintenanceBackend::release(&backend, &destination);
    drop(connected);
    backend.disconnect(&destination);
    drop(backend);
    let _ = std::fs::remove_dir_all(root);
}

/// Explicit env selects a disposable account whose server is ready, and a second explicit flag
/// authorizes the destructive steps. This test stops, starts, updates and resolves originals on
/// that one account. It installs no server, touches no other account and never reverses Stop
/// except through the explicit Start it performs itself.
///
/// `SPACETERM_SSH_SERVER_ARTIFACTS` must hold a verified candidate that differs from the
/// account's approved build, so the update step has an exact artifact to stage.
#[gpui::test]
#[ignore = "requires an explicitly authorized disposable OpenSSH destination with a ready server"]
fn native_account_server_maintenance_originals(cx: &mut gpui::TestAppContext) {
    use crate::ssh::server_maintenance::{
        MaintenanceAuthority, MaintenanceTarget, OperationSubmission, SshMaintenanceBackend,
        SubmittedKind,
    };
    use spaceterm_server::service::maintenance::{Action, MaintenanceRefusal, OperationResolution};
    assert_eq!(
        std::env::var_os("SPACETERM_SSH_NATIVE_MAINTAIN").as_deref(),
        Some(std::ffi::OsStr::new("1")),
        "destructive remote maintenance needs its own explicit authorization"
    );
    let NativeFixture {
        destination,
        expected,
        root,
        adapter,
        backend,
        ..
    } = native_fixture(cx, "originals");
    let cancellation = SshCancellationToken::default();
    assert_eq!(
        expected, "ready",
        "binding an original needs a ready server"
    );
    let connected = backend
        .connect(
            &destination,
            ConnectMode::Requested,
            None,
            &cancellation,
            &|_| {},
        )
        .unwrap_or_else(|error| panic!("unexpected classification: {}", failure_name(&error)));
    let target = MaintenanceTarget {
        destination: destination.clone(),
        continuity: connected.continuity.clone(),
    };
    let observe = |authority| {
        SshMaintenanceBackend::observe(&backend, &target, authority, &cancellation, &|_| {})
            .map(|observation| observation.facts)
    };
    // Each run submits fresh originals. Reusing one identity would make the service replay its
    // retained outcome instead of dispatching, which is the guarantee this test relies on
    // elsewhere rather than something it should trip over here.
    let next_operation = || {
        let mut operation = [0; 32];
        getrandom::fill(&mut operation).expect("an operation identity");
        operation
    };

    // Describing Stop changes nothing: the server is still running afterwards.
    let stop = SshMaintenanceBackend::prepare(
        &backend,
        &target,
        Action::Stop,
        MaintenanceAuthority::Requested,
        &cancellation,
        &|phase| eprintln!("native prepare phase: {phase:?}"),
    )
    .unwrap_or_else(|error| panic!("describing Stop failed: {error:?}"));
    assert!(stop.running && !stop.stopped);
    assert!(
        observe(MaintenanceAuthority::Requested)
            .unwrap()
            .status
            .running
    );

    // The described Stop runs once and its result is validated, not assumed.
    let stopped_original = next_operation();
    let outcome = SshMaintenanceBackend::apply(
        &backend,
        &target,
        &OperationSubmission {
            operation: stopped_original,
            kind: SubmittedKind::Service(Action::Stop),
            confirmation: Some(stop.confirmation.clone()),
        },
        &cancellation,
        &|phase| eprintln!("native stop phase: {phase:?}"),
    )
    .unwrap_or_else(|error| panic!("Stop failed: {error:?}"));
    eprintln!("native stop outcome: {:?}", outcome.resolution);
    assert_eq!(outcome.resolution, OperationResolution::Completed);
    assert!(
        outcome
            .status
            .is_some_and(|status| status.stopped && !status.running)
    );

    // Resolving the same original reports it again and dispatches nothing, so a server the
    // person stopped stays stopped rather than being stopped a second time.
    let resolved = SshMaintenanceBackend::resolve(
        &backend,
        &target,
        stopped_original,
        &crate::ssh::server_maintenance::SubmittedKind::Service(
            spaceterm_server::service::maintenance::Action::Stop,
        ),
        MaintenanceAuthority::Quiet,
        &cancellation,
        &|_| {},
    )
    .unwrap_or_else(|error| panic!("resolving the original failed: {error:?}"));
    assert_eq!(resolved.operation, stopped_original);
    assert_eq!(resolved.resolution, OperationResolution::Completed);
    // Reading a stopped server still works, so its result stays understandable afterwards.
    let after = observe(MaintenanceAuthority::Requested).unwrap();
    assert!(
        after.status.stopped && !after.status.running && !after.status.enabled,
        "Stop must be durable: {:?}",
        after.status
    );

    // Explicit Start is the only operation that reverses that Stop.
    let started = SshMaintenanceBackend::apply(
        &backend,
        &target,
        &OperationSubmission {
            operation: next_operation(),
            kind: SubmittedKind::Start,
            confirmation: None,
        },
        &cancellation,
        &|phase| eprintln!("native start phase: {phase:?}"),
    )
    .unwrap_or_else(|error| panic!("Start failed: {error:?}"));
    assert_eq!(started.resolution, OperationResolution::Completed);
    let running = observe(MaintenanceAuthority::Requested).unwrap();
    assert!(running.status.running && !running.status.stopped);

    // The confirmation taken while the server was running before cannot stop the work that
    // started afterwards, even under a new original.
    let stale = SshMaintenanceBackend::apply(
        &backend,
        &target,
        &OperationSubmission {
            operation: next_operation(),
            kind: SubmittedKind::Service(Action::Stop),
            confirmation: Some(stop.confirmation.clone()),
        },
        &cancellation,
        &|_| {},
    )
    .unwrap_or_else(|error| panic!("a stale confirmation must be refused, not lost: {error:?}"));
    assert_eq!(
        stale.resolution,
        OperationResolution::Refused(MaintenanceRefusal::Changed),
        "a confirmation bound to an earlier run must not stop later work"
    );
    assert!(
        observe(MaintenanceAuthority::Requested)
            .unwrap()
            .status
            .running
    );

    // Staging transfers the exact verified candidate and describes its activation. The approved
    // build and the running server are unchanged until the activation is performed.
    let approved = observe(MaintenanceAuthority::Requested)
        .unwrap()
        .approved
        .expect("an installed server names its approved build");
    let update = SshMaintenanceBackend::prepare_update(
        &backend,
        &target,
        MaintenanceAuthority::Requested,
        &cancellation,
        &|phase| eprintln!("native staging phase: {phase:?}"),
    )
    .unwrap_or_else(|error| panic!("staging failed: {error:?}"))
    .expect("the candidate must differ from the approved build");
    let candidate = update
        .candidate
        .clone()
        .expect("an update names its candidate");
    assert_ne!(candidate.digest, approved.digest);
    let staged = observe(MaintenanceAuthority::Requested).unwrap();
    assert_eq!(
        staged.approved.as_ref().map(|build| &build.digest),
        Some(&approved.digest)
    );
    assert!(
        staged.status.running,
        "staging leaves the approved runtime running"
    );

    let activated = SshMaintenanceBackend::apply(
        &backend,
        &target,
        &OperationSubmission {
            operation: next_operation(),
            kind: SubmittedKind::Update {
                candidate: candidate.digest.clone(),
            },
            confirmation: Some(update.confirmation.clone()),
        },
        &cancellation,
        &|phase| eprintln!("native activation phase: {phase:?}"),
    )
    .unwrap_or_else(|error| panic!("activation failed: {error:?}"));
    eprintln!("native activation outcome: {:?}", activated.resolution);
    assert_eq!(activated.resolution, OperationResolution::Completed);
    let updated = observe(MaintenanceAuthority::Requested).unwrap();
    assert_eq!(
        updated.approved.as_ref().map(|build| &build.digest),
        Some(&candidate.digest),
        "the account runs the exact candidate it approved"
    );
    assert_eq!(
        updated.server, connected.continuity.server,
        "activation preserves the server's identity"
    );

    eprintln!(
        "native originals observations: spawns={}, last_exit={}",
        adapter.spawns.load(std::sync::atomic::Ordering::SeqCst),
        adapter.last_exit.load(std::sync::atomic::Ordering::SeqCst)
    );
    SshMaintenanceBackend::release(&backend, &destination);
    drop(connected);
    backend.disconnect(&destination);
    drop(backend);
    let _ = std::fs::remove_dir_all(root);
}

/// Explicit env selects a disposable account whose server is ready, and the same destructive
/// authorization as the originals test. This test loses the client that submitted an original,
/// twice, and reads what a new client makes of the retained records each time.
///
/// It leaves the account's server stopped, because that is what the original it finishes asked
/// for. Start it again explicitly when the run is over.
#[gpui::test]
#[ignore = "requires an explicitly authorized disposable OpenSSH destination with a ready server"]
fn native_remote_original_finishes_after_the_client_is_lost(cx: &mut gpui::TestAppContext) {
    use crate::ssh::server_connection::{
        RemoteConnectionStatus, SshConnectBackend, SshConnectionIntentStore, SshServerConnections,
    };
    use crate::ssh::server_maintenance::{SshMaintenanceBackend, SubmittedKind};
    use crate::ssh::server_operation_records::PrivateRemoteOperationRecords;
    use crate::ssh::server_operations::{
        RemoteMaintenanceRequest, RemoteOperationHandle, RemoteOperationState,
        RemoteOperationStore, RemoteServerOperations, SshServerOperations,
    };
    use spaceterm_server::service::maintenance::Action;
    assert_eq!(
        std::env::var_os("SPACETERM_SSH_NATIVE_MAINTAIN").as_deref(),
        Some(std::ffi::OsStr::new("1")),
        "destructive remote maintenance needs its own explicit authorization"
    );
    let NativeFixture {
        destination,
        expected,
        root,
        paths,
        backend,
        ..
    } = native_fixture(cx, "relaunch");
    assert_eq!(
        expected, "ready",
        "binding an original needs a ready server"
    );
    let backend = Arc::new(backend);
    let maintenance: Arc<dyn SshMaintenanceBackend> = backend.clone();
    let records = Arc::new(PrivateRemoteOperationRecords::new(Arc::clone(&paths)));
    let intents =
        Arc::new(crate::ssh::server_intents::PrivateSshConnectionIntents::new(Arc::clone(&paths)));
    let connections = SshServerConnections::new(
        Arc::clone(&backend) as Arc<dyn SshConnectBackend>,
        intents.clone() as Arc<dyn SshConnectionIntentStore>,
    );
    let until = |mut ready: Box<dyn FnMut() -> bool>| {
        let deadline = Instant::now() + Duration::from_secs(90);
        while Instant::now() < deadline {
            if ready() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    };
    let client = |records: &Arc<PrivateRemoteOperationRecords>| {
        SshServerOperations::new(
            Arc::clone(&maintenance),
            Arc::clone(records) as Arc<dyn RemoteOperationStore>,
            connections.clone(),
        )
    };
    let connect = || {
        connections.connect(destination.clone());
        let watched = connections.clone();
        let watching = destination.clone();
        assert!(
            until(Box::new(move || matches!(
                watched.status(&watching),
                RemoteConnectionStatus::Connected { .. }
            ))),
            "the connection that admits the identity did not establish"
        );
    };
    let describe_stop = |owner: &Arc<SshServerOperations>| {
        owner.describe(
            &destination,
            RemoteMaintenanceRequest::Service(Action::Stop),
        );
        let watched = Arc::clone(owner);
        let watching = destination.clone();
        assert!(
            until(Box::new(move || matches!(
                watched.state(&watching),
                RemoteOperationState::Described { .. }
            ))),
            "the service did not describe the Stop"
        );
        match owner.state(&destination) {
            RemoteOperationState::Described { handle, .. } => handle,
            state => panic!("a described change names its handle: {state:?}"),
        }
    };
    let lose_the_route = || {
        maintenance.release(&destination);
        connections.disconnect(&destination);
        SshConnectBackend::disconnect(backend.as_ref(), &destination);
    };
    let settled = |owner: &Arc<SshServerOperations>| {
        let watched = Arc::clone(owner);
        let watching = destination.clone();
        assert!(
            until(Box::new(move || matches!(
                watched.state(&watching),
                RemoteOperationState::Resolved { .. }
                    | RemoteOperationState::Unconfirmed { .. }
                    | RemoteOperationState::Failed { .. }
            ))),
            "the original was never settled"
        );
        owner.state(&destination)
    };

    connect();

    // An original whose dispatch never left this client is never invented by the service.
    let submitting = client(&records);
    let handle = describe_stop(&submitting);
    submitting.confirm(handle);
    // Nothing is dispatched until the record that resolves it is retained, so waiting for that
    // record makes losing the client afterwards a real interruption.
    let retained = records.clone();
    assert!(
        until(Box::new(move || {
            retained.load().is_ok_and(|records| records.len() == 1)
        })),
        "the submitted original was never retained"
    );
    drop(submitting);
    lose_the_route();

    let resuming = client(&records);
    resuming.resume_retained();
    let quiet = Arc::clone(&resuming);
    let watching = destination.clone();
    assert!(
        until(Box::new(move || !matches!(
            quiet.state(&watching),
            RemoteOperationState::Unread
        ))),
        "the new client did not read its retained original"
    );
    assert!(
        matches!(
            resuming.state(&destination),
            RemoteOperationState::NeedsPerson { .. }
        ),
        "background continuation must wait for the person, not ask: {:?}",
        resuming.state(&destination)
    );
    resuming.resume(&destination);
    eprintln!("native undispatched original: {:?}", settled(&resuming));
    drop(resuming);

    // The server is untouched, because that original never reached it.
    connect();
    let running = client(&records);
    let handle = describe_stop(&running);

    // This original completes, and the client that submitted it is lost before the person can
    // acknowledge the result.
    running.confirm(handle);
    let watched = Arc::clone(&running);
    let watching = destination.clone();
    assert!(
        until(Box::new(move || matches!(
            watched.state(&watching),
            RemoteOperationState::Resolved { .. }
        ))),
        "the Stop was never validated: {:?}",
        running.state(&destination)
    );
    drop(running);
    lose_the_route();

    // A new client presents the retained result without asking any computer.
    let reopened = client(&records);
    reopened.resume_retained();
    let watched = Arc::clone(&reopened);
    let watching = destination.clone();
    assert!(
        until(Box::new(move || matches!(
            watched.state(&watching),
            RemoteOperationState::Resolved { .. }
        ))),
        "a reloaded result did not wait for acknowledgement: {:?}",
        reopened.state(&destination)
    );
    let RemoteOperationState::Resolved { handle, kind, .. } = reopened.state(&destination) else {
        unreachable!("matched above")
    };
    assert_eq!(kind, SubmittedKind::Service(Action::Stop));

    // Removing the saved connection keeps that result and never saves the connection again.
    connections.remove(&destination);
    let removed = Arc::clone(&reopened);
    let watching = destination.clone();
    assert!(
        until(Box::new(move || removed
            .entries()
            .iter()
            .any(|entry| entry.destination == watching && entry.removed))),
        "the removed connection's result stopped being presented"
    );
    let saved = intents.clone();
    assert!(
        until(Box::new(move || saved
            .load()
            .is_ok_and(|connections| connections.is_empty()))),
        "presenting a removed connection's original must not save it again"
    );

    // Acknowledging retires the record, so nothing is left to tell.
    reopened.acknowledge(RemoteOperationHandle(handle.0));
    let acknowledged = Arc::clone(&reopened);
    assert!(
        until(Box::new(move || acknowledged.entries().is_empty())),
        "an acknowledged result stayed"
    );
    assert!(records.load().unwrap().is_empty());

    drop(reopened);
    drop(connections);
    drop(backend);
    let _ = std::fs::remove_dir_all(root);
}

fn failure_name(error: &SshConnectFailure) -> &'static str {
    match error {
        SshConnectFailure::Cancelled => "cancelled",
        SshConnectFailure::Stopped(_) => "stopped",
        SshConnectFailure::Unavailable(UnavailableKind::LocalIpcUnavailable) => {
            "local IPC capacity"
        }
        SshConnectFailure::Unavailable(UnavailableKind::Network) => "network",
        SshConnectFailure::Unavailable(UnavailableKind::SshFailed) => "ssh failure",
        SshConnectFailure::Unavailable(UnavailableKind::ServiceUnavailable) => {
            "service unavailable"
        }
        SshConnectFailure::Unavailable(_) => "unavailable",
        SshConnectFailure::NeedsPerson(_) => "needs person",
        SshConnectFailure::Changed(_) => "continuity changed",
        SshConnectFailure::Refused(RefusalKind::MissingArtifact) => "missing artifact",
        SshConnectFailure::Refused(RefusalKind::InvalidArtifact) => "invalid artifact",
        SshConnectFailure::Refused(_) => "refused",
        SshConnectFailure::Incompatible(_) => "incompatible",
        SshConnectFailure::Unconfirmed(_) => "unconfirmed",
        SshConnectFailure::InstallationOffered(_) => "offered",
    }
}
