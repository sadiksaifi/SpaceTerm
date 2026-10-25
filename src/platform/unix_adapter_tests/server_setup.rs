//! Opt-in provisioning acceptance through real OpenSSH, signed packages and paired WSS.
use super::*;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::cancellation::CancellationToken;
use crate::direct_connections::credentials::{CredentialError, CredentialStore, Interaction};
use crate::direct_connections::{DirectConnections, DirectRouteStatus};
use crate::ssh::askpass::{
    AskPassAttempt, AskPassAttemptFactory, AskPassAttemptObservation, AskPassBrokerLease,
    AskPassLease, AskPassUnavailable,
};
use crate::ssh::process::{
    ProcessExit, ProcessSignal, SpawnedSshProcess, SshProcessAdapter, SshProcessMechanismError,
    SshProcessSpawnRequest,
};

#[path = "../../../crates/spaceterm-server-host/examples/support/repository.rs"]
#[expect(
    dead_code,
    reason = "acceptance uses inventory and owning-server mutation helpers"
)]
mod repository;
#[path = "../../../crates/spaceterm-server-host/examples/support/terminal_semantics.rs"]
mod terminal_semantics;
#[path = "../../../crates/spaceterm-server-host/examples/support/worktree.rs"]
#[expect(
    dead_code,
    reason = "acceptance uses journaled writes and exact removal review"
)]
mod worktree_fixture;

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
        eprintln!("WSS fixture shell failure: stage={stage}, exit={code}");
        return Err(terminal_semantics::Failure::from("fixture-shell-command-failed").at(stage));
    }
    Err(terminal_semantics::Failure::from("fixture-shell-witness-missing").at(stage))
}

/// Every mutation and shell command targets a Workspace this invocation created on this route.
/// A retained original prevents cleanup until that exact write has been resolved.
fn native_server_owned_work(
    server: &spaceterm_server::ServerConnection,
    reconnect: impl FnOnce() -> spaceterm_server::ServerConnection,
) -> Result<(), terminal_semantics::Failure> {
    use spaceterm_server::{
        hierarchy::{CloseTarget, Mutation, Outcome},
        pane::{PaneSize, Reply, Request},
        worktree::{Branch, BranchName, Operation},
    };
    use terminal_semantics::stage as api;
    if std::env::var("SPACETERM_DEVELOPMENT_CLIENT").as_deref() != Ok("setup-native-653") {
        return Err("WSS fixture requires its dedicated development client identity".into());
    }
    let journal = api("wss-open-fixture-journal", server.shared_journal())?;
    if journal.pending().is_some() {
        return Err("WSS fixture has an unresolved original; preserve it before new work".into());
    }
    drop(journal);
    let mut connection = api("wss-connect-pane", server.connect_shared_pane())?;
    let baseline = api(
        "wss-baseline-inventory",
        repository::inventory(&mut connection),
    )?
    .0;
    let created = api(
        "wss-create-workspace",
        worktree_fixture::journaled_mutation(
            server,
            &mut connection,
            Mutation::CreateWorkspace {
                name: "WSS native semantics".into(),
                directory: None,
            },
        ),
    )?;
    let Outcome::Accepted { target: workspace } = created.outcome else {
        return Err("WSS native fixture Workspace creation refused".into());
    };
    let mut remote_root = None;
    let mut root_identity = None;
    let exercise = (|| -> Result<_, terminal_semantics::Failure> {
        let current = api(
            "wss-fixture-inventory",
            repository::inventory(&mut connection),
        )?
        .0;
        let pane = current
            .workspaces
            .iter()
            .find(|work| work.id == workspace)
            .and_then(|work| work.tabs.first())
            .ok_or("WSS fixture Pane missing")?
            .root_pane;
        let Reply::Attached { mut lease } = api(
            "wss-select-fixture-pane",
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
            return Err("WSS fixture Pane did not start".into());
        };
        let report = terminal_semantics::exercise(&mut connection, lease, terminal_command)?;
        terminal_fixture_command(
            &mut connection,
            lease,
            "st653_live=retained; printf 'ST653-LIVE\\n'",
            "wss-live-shell",
            "ST653-LIVE",
        )?;
        let before = api(
            "wss-before-reconnect",
            repository::inventory(&mut connection),
        )?
        .0;
        let reconnected = reconnect();
        if api("wss-original-client", server.client_scope())?
            != api("wss-reconnected-client", reconnected.client_scope())?
        {
            return Err("WSS reconnect changed the paired Client Identity".into());
        }
        connection = api("wss-reconnect-pane", reconnected.connect_shared_pane())?;
        let after = api(
            "wss-after-reconnect",
            repository::inventory(&mut connection),
        )?
        .0;
        let before = before.close.ok_or("WSS original live inventory missing")?;
        let after = after
            .close
            .ok_or("WSS reconnected live inventory missing")?;
        if before.boot != after.boot
            || !before
                .sessions
                .iter()
                .all(|live| after.sessions.contains(live))
        {
            return Err("WSS reconnect replaced existing Terminal Sessions".into());
        }
        let Reply::Attached { lease: resumed } = api(
            "wss-resume-fixture-pane",
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
            return Err("WSS fixture Pane did not resume".into());
        };
        lease = resumed;
        terminal_fixture_command(
            &mut connection,
            lease,
            "test \"$st653_live\" = retained && printf 'ST653-REUSED\\n'",
            "wss-shell-reuse",
            "ST653-REUSED",
        )?;
        if !connection.worktree_writes() {
            return Err("WSS Worktree capability missing".into());
        }
        // Only the owning server's shell creates or accesses these paths. They never enter local I/O.
        terminal_fixture_command(
            &mut connection,
            lease,
            r#"st653_root=$(mktemp -d /tmp/st653.XXXXXXXX) && printf 'ST-ROOT-CREATED\n'"#,
            "wss-root-mkdir",
            "ST-ROOT-CREATED",
        )?;
        // A cd inside command substitution captures the integration's chpwd OSC output.
        // Resolve the server path without changing the shell's directory or invoking hooks.
        terminal_fixture_command(
            &mut connection,
            lease,
            r#"st653_root=$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$st653_root") && printf 'ST-ROOT-CANONICAL\n'"#,
            "wss-root-canonicalize",
            "ST-ROOT-CANONICAL",
        )?;
        let output = terminal_fixture_command(
            &mut connection,
            lease,
            r#"st653_identity=$(python3 -c 'import os,sys; s=os.stat(sys.argv[1],follow_symlinks=False); print("%d:%d"%(s.st_dev,s.st_ino))' "$st653_root") && printf 'ST-FIXTURE-ROOT=%s\nST-FIXTURE-IDENTITY=%s\nST-ROOT-READY\n' "$st653_root" "$st653_identity""#,
            "wss-root-identity",
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
            .ok_or("WSS fixture root identity missing")?;
        let path = PathBuf::from(root);
        if !matches!(
            path.parent().and_then(|p| p.to_str()),
            Some("/tmp" | "/private/tmp")
        ) || !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| {
                name.starts_with("st653.")
                    && name.len() == 14
                    && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')
            })
        {
            return Err("WSS fixture root shape changed".into());
        }
        let identity = lines
            .iter()
            .find_map(|line| line.trim_end().strip_prefix("ST-FIXTURE-IDENTITY="))
            .and_then(|value| value.split_once(':'))
            .and_then(|(device, inode)| {
                Some((device.parse::<u64>().ok()?, inode.parse::<u64>().ok()?))
            })
            .ok_or("WSS fixture filesystem identity missing")?;
        remote_root = Some((path.clone(), lease));
        root_identity = Some(identity);
        let created_directory = path.join("created-directory");
        let value = directory_operation(
            &mut connection,
            1,
            spaceterm_server::pane::Request::DirectoryCreate(
                spaceterm_server::pane::directories::CreationRequest::Begin {
                    id: 1,
                    path: created_directory.clone(),
                },
            ),
            true,
        )?;
        if !matches!(value, spaceterm_server::pane::directories::Value::Directory(ref value) if value == &created_directory)
        {
            return Err("WSS directory creation returned another directory".into());
        }
        let value = directory_operation(
            &mut connection,
            2,
            Request::DirectoryRead(spaceterm_server::pane::directories::Request::Begin {
                id: 2,
                operation: spaceterm_server::pane::directories::Operation::List {
                    path: path.clone(),
                },
            }),
            false,
        )?;
        if !matches!(value, spaceterm_server::pane::directories::Value::Listing { names, truncated: false } if names.iter().any(|name| name == "created-directory"))
        {
            return Err("WSS directory browsing missed its created directory".into());
        }
        terminal_fixture_command(
            &mut connection,
            lease,
            r#"ln -s "$st653_root/created-directory" "$st653_root/alias" && printf 'ST-ALIAS-READY\n'"#,
            "wss-directory-alias",
            "ST-ALIAS-READY",
        )?;
        let value = directory_operation(
            &mut connection,
            3,
            Request::DirectoryRead(spaceterm_server::pane::directories::Request::Begin {
                id: 3,
                operation: spaceterm_server::pane::directories::Operation::Match {
                    path: path.join("alias"),
                    candidates: vec![created_directory],
                },
            }),
            false,
        )?;
        if !matches!(
            value,
            spaceterm_server::pane::directories::Value::Match { index: Some(0) }
        ) {
            return Err("WSS directory comparison did not resolve the owning-server alias".into());
        }
        terminal_fixture_command(
            &mut connection,
            lease,
            r#"GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 git -C "$st653_root" -c init.defaultBranch=main init -q && git -C "$st653_root" config core.hooksPath /dev/null && git -C "$st653_root" -c commit.gpgSign=false -c user.name=Fixture -c user.email=fixture@invalid commit -q --allow-empty -m fixture && printf 'ST-REPOSITORY-READY\n'"#,
            "wss-repository-ready",
            "ST-REPOSITORY-READY",
        )?;
        let pinned = api(
            "wss-pin-fixture-workspace",
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
            return Err("WSS fixture pin refused".into());
        }
        let worktree = path.join("created");
        let created = api(
            "wss-create-fixture-worktree",
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
            return Err("WSS Worktree creation refused".into());
        }
        let published = api(
            "wss-observe-created-worktree",
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
            return Err("WSS Worktree Tab did not belong to its owning server".into());
        }
        terminal_fixture_command(
            &mut connection,
            lease,
            r#"test -d "$st653_root/created" && printf 'ST-WORKTREE-EXISTS\n'"#,
            "wss-worktree-exists",
            "ST-WORKTREE-EXISTS",
        )?;
        let removal = api(
            "wss-review-worktree-removal",
            worktree_fixture::review(&mut connection, &path, &worktree),
        )?;
        let removed = api(
            "wss-remove-fixture-worktree",
            worktree_fixture::journaled_mutation(
                server,
                &mut connection,
                Mutation::Worktree(removal),
            ),
        )?;
        if !matches!(removed.outcome, Outcome::Accepted { .. }) {
            return Err("WSS Worktree removal refused".into());
        }
        api(
            "wss-observe-removed-worktree",
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
            r#"test ! -e "$st653_root/created" && printf 'ST-WORKTREE-REMOVED\n'"#,
            "wss-worktree-removed",
            "ST-WORKTREE-REMOVED",
        )?;
        Ok(report)
    })();
    if let Err(failure) = &exercise {
        eprintln!("WSS fixture exercise failed: {failure}");
    }
    if api("wss-cleanup-journal-check", server.shared_journal())?
        .pending()
        .is_some()
    {
        return Err("WSS fixture cleanup waits for original-operation recovery".into());
    }
    // Recheck the exact remote filesystem object before descriptor-safe recursive removal.
    // A changed target is retained, and an uncertain write never reaches this cleanup.
    let directory_cleanup = if let (Some((path, lease)), Some((device, inode))) =
        (remote_root, root_identity)
    {
        let root = path.to_str().ok_or("WSS fixture root encoding changed")?;
        let cleanup = format!(
            r#"python3 -c 'import os,shutil,stat,sys; p=sys.argv[1]; parent=os.open(os.path.dirname(p),os.O_RDONLY|os.O_DIRECTORY); name=os.path.basename(p); fd=os.open(name,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=parent); s=os.fstat(fd); assert (s.st_dev,s.st_ino)==({device},{inode}) and shutil.rmtree.avoids_symlink_attacks; [shutil.rmtree(n,dir_fd=fd) if stat.S_ISDIR(os.stat(n,dir_fd=fd,follow_symlinks=False).st_mode) else os.unlink(n,dir_fd=fd) for n in os.listdir(fd)]; s=os.stat(name,dir_fd=parent,follow_symlinks=False); assert (s.st_dev,s.st_ino)==({device},{inode}); os.rmdir(name,dir_fd=parent); os.close(fd); os.close(parent); print("ST-ROOT-REMOVED")' '{root}' 2>/dev/null"#
        );
        terminal_fixture_command(
            &mut connection,
            lease,
            &cleanup,
            "wss-root-removed",
            "ST-ROOT-REMOVED",
        )
        .map(drop)
    } else {
        Ok(())
    };
    let target = CloseTarget::Workspace { workspace };
    let current = api(
        "wss-cleanup-inventory",
        repository::inventory(&mut connection),
    )?
    .0;
    let confirmation = current
        .close_confirmation(target)
        .ok_or("WSS fixture close target missing")?;
    let closed = api(
        "wss-close-fixture-workspace",
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
        return Err("WSS fixture close refused".into());
    }
    let after = api(
        "wss-verify-baseline-inventory",
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
        return Err("baseline server work changed during the WSS fixture".into());
    }
    let report = exercise?;
    directory_cleanup?;
    eprintln!(
        "PASS WSS semantic protocol: copied rows={}, accessible rows={}, graphics supported={}, graphics bytes={}, owning-server Worktree create/remove; fixture cleanup and baseline work retained",
        report.copied_rows,
        report.accessible_rows,
        report.graphics_supported,
        report.graphics_bytes
    );
    Ok(())
}

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
        b"setup-native-no-prompt"
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
    spawns: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    sealed: Arc<AtomicBool>,
    cancel_after_spawn: Arc<Mutex<Option<CancellationToken>>>,
}
impl ObservedAdapter {
    fn assert_closed(&self) {
        assert_eq!(
            self.active.load(Ordering::SeqCst),
            0,
            "setup retained an SSH process"
        );
    }
    fn seal(&self) -> usize {
        self.assert_closed();
        self.sealed.store(true, Ordering::SeqCst);
        self.spawns.load(Ordering::SeqCst)
    }
}
impl SshProcessAdapter for ObservedAdapter {
    type Process = crate::platform::unix_ssh_process::UnixSshProcess;
    fn spawn(
        &self,
        request: SshProcessSpawnRequest,
    ) -> Result<SpawnedSshProcess<Self::Process>, SshProcessMechanismError> {
        assert!(
            !self.sealed.load(Ordering::SeqCst),
            "remote use attempted SSH after setup"
        );
        self.spawns.fetch_add(1, Ordering::SeqCst);
        let process = crate::platform::unix_ssh_process::UnixSshProcessAdapter.spawn(request)?;
        self.active.fetch_add(1, Ordering::SeqCst);
        if let Some(cancellation) = self.cancel_after_spawn.lock().unwrap().take() {
            cancellation.cancel();
        }
        Ok(process)
    }
    fn try_status(
        &self,
        process: &mut Self::Process,
    ) -> Result<Option<ProcessExit>, SshProcessMechanismError> {
        crate::platform::unix_ssh_process::UnixSshProcessAdapter.try_status(process)
    }
    fn signal(
        &self,
        process: &mut Self::Process,
        signal: ProcessSignal,
    ) -> Result<(), SshProcessMechanismError> {
        crate::platform::unix_ssh_process::UnixSshProcessAdapter.signal(process, signal)
    }
    fn reap(&self, process: Self::Process) -> Result<(), SshProcessMechanismError> {
        crate::platform::unix_ssh_process::UnixSshProcessAdapter.reap(process)?;
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(())
    }
}

struct ObservedCredentials {
    store: Arc<dyn CredentialStore>,
    keys: Mutex<Vec<[u8; 32]>>,
    inserts: AtomicUsize,
    reads: AtomicUsize,
    fail_insert: bool,
}
impl ObservedCredentials {
    fn new(fail_insert: bool) -> Arc<Self> {
        Arc::new(Self {
            store: crate::direct_connections::credentials::native(identity()),
            keys: Mutex::default(),
            inserts: AtomicUsize::new(0),
            reads: AtomicUsize::new(0),
            fail_insert,
        })
    }
}
impl CredentialStore for ObservedCredentials {
    fn read(
        &self,
        key: &[u8; 32],
        interaction: Interaction,
    ) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, CredentialError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.store.read(key, interaction)
    }
    fn insert(&self, key: &[u8; 32], secret: &[u8]) -> Result<(), CredentialError> {
        self.inserts.fetch_add(1, Ordering::SeqCst);
        if self.fail_insert {
            return Err(CredentialError::Unavailable);
        }
        self.store.insert(key, secret)?;
        self.keys.lock().unwrap().push(*key);
        Ok(())
    }
    fn remove(&self, key: &[u8; 32], interaction: Interaction) -> Result<(), CredentialError> {
        self.store.remove(key, interaction)?;
        self.keys.lock().unwrap().retain(|saved| saved != key);
        Ok(())
    }
}

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);
struct NativeFixture {
    root: PathBuf,
    paths: Arc<AppPaths>,
    direct: DirectConnections,
    credentials: Arc<ObservedCredentials>,
    adapter: ObservedAdapter,
    catalog: Option<PathBuf>,
}
impl NativeFixture {
    fn new(name: &str, catalog: Option<PathBuf>, fail_credentials: bool) -> Self {
        use crate::platform::{
            app_directories::{AppDirectories, AppDirectoryEnvironment},
            unix_secure_filesystem::UnixSecureFilesystem,
        };
        let serial = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = crate::platform::unix_adapter_tests::short_temporary_root()
            .join(format!("st653-{name}-{}-{serial}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let environment = AppDirectoryEnvironment {
            home: None,
            xdg_config_home: Some(root.join("config").into()),
            xdg_data_home: Some(root.join("data").into()),
            xdg_state_home: Some(root.join("state").into()),
            xdg_cache_home: Some(root.join("cache").into()),
            xdg_runtime_dir: Some(root.join("runtime").into()),
            ..Default::default()
        };
        let directories =
            AppDirectories::resolve_xdg(identity(), &environment, Some(root.clone())).unwrap();
        let paths = Arc::new(
            AppPaths::from_directories(directories, 103, Arc::new(UnixSecureFilesystem)).unwrap(),
        );
        let credentials = ObservedCredentials::new(fail_credentials);
        let direct = DirectConnections::with_credentials(
            identity(),
            Arc::clone(&paths),
            credentials.clone(),
        );
        direct.resume_retained();
        Self {
            root,
            paths,
            direct,
            credentials,
            adapter: ObservedAdapter::default(),
            catalog,
        }
    }
    fn backend(&self) -> NativeSshSetup<ObservedAdapter> {
        self.backend_with_bootstrap(Arc::new(
            crate::platform::unix_ssh_process::UnixSshProcessAdapter,
        ))
    }
    fn backend_with_bootstrap(
        &self,
        bootstrap: Arc<dyn SshServerBootstrap>,
    ) -> NativeSshSetup<ObservedAdapter> {
        NativeSshSetup::new(
            NativeSshSetupConfig {
                paths: Arc::clone(&self.paths),
                filesystem: Arc::new(
                    crate::platform::unix_host_config_filesystem::UnixHostConfigFilesystem,
                ),
                bootstrap,
                home: PathBuf::from(std::env::var_os("HOME").unwrap()),
                startup: StartupSshEnvironment::from_environment(
                    |key| std::env::var_os(key),
                    "/usr/bin:/bin".into(),
                )
                .unwrap(),
                executable: OpenSshExecutable::new("/usr/bin/ssh".into()).unwrap(),
                adapter: self.adapter.clone(),
                artifacts: ServerArtifactCatalog::new(self.catalog.clone()),
            },
            self.direct.clone(),
        )
    }
    fn progress(&self) -> (SetupProgress, Arc<Mutex<Vec<SetupPhase>>>) {
        let phases = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&phases);
        let adapter = self.adapter.clone();
        (
            SetupProgress::new(move |phase| {
                if phase == SetupPhase::Connecting {
                    adapter.assert_closed();
                }
                observed.lock().unwrap().push(phase);
            }),
            phases,
        )
    }
    fn assert_no_connection(&self) {
        self.adapter.assert_closed();
        assert!(
            self.direct.routes().is_empty(),
            "failed setup published a connection"
        );
    }
}
impl Drop for NativeFixture {
    fn drop(&mut self) {
        if self.direct.routes().is_empty()
            && self.direct.pending_unpairs().is_empty()
            && self.credentials.keys.lock().unwrap().is_empty()
        {
            let _ = std::fs::remove_dir_all(&self.root);
        } else {
            eprintln!("Native setup fixture retained its exact connection and cleanup records.");
        }
    }
}

fn expected_state() -> String {
    let expected = std::env::var("SPACETERM_SETUP_NATIVE_EXPECT")
        .expect("select missing, ready or stopped account state");
    assert!(matches!(expected.as_str(), "missing" | "ready" | "stopped"));
    expected
}
fn native_request(approvals: SetupApprovals) -> SetupRequest {
    assert_ne!(
        identity(),
        "spaceterm",
        "native setup fixtures require a nonproduction identity"
    );
    assert_eq!(
        std::env::var("SPACETERM_SETUP_NATIVE_IDENTITY").as_deref(),
        Ok(identity()),
        "confirm the exact disposable account service identity"
    );
    SetupRequest {
        destination: SshDestination::new(
            std::env::var("SPACETERM_SETUP_NATIVE_DESTINATION")
                .expect("select an existing OpenSSH configuration destination"),
        )
        .unwrap(),
        endpoint: std::env::var("SPACETERM_SETUP_NATIVE_ENDPOINT")
            .expect("select the owner-confirmed concrete reachable IP:port")
            .parse()
            .unwrap(),
        name: "setup-native-653".into(),
        prompts: Arc::new(NoPrompt),
        approvals,
    }
}
fn configured_catalog() -> Option<PathBuf> {
    std::env::var_os("SPACETERM_SERVER_ARTIFACTS").map(PathBuf::from)
}
fn wait_for(stage: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "native setup did not finish {stage}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "observes an explicitly selected account's missing, ready or deliberately stopped server"]
fn native_server_setup_requires_consent() {
    let fixture = NativeFixture::new("consent", configured_catalog(), true);
    let (progress, phases) = fixture.progress();
    let result = fixture.backend().setup(
        native_request(SetupApprovals::default()),
        &CancellationToken::default(),
        &progress,
    );
    match expected_state().as_str() {
        "missing" => assert!(matches!(
            result,
            Err(SetupFailure::InstallationOffered { .. })
        )),
        "stopped" => assert!(matches!(result, Err(SetupFailure::StartupRequired { .. }))),
        "ready" => assert!(matches!(
            result,
            Err(SetupFailure::NetworkAuthorizationRequired { .. })
        )),
        _ => unreachable!(),
    }
    fixture.assert_no_connection();
    assert_eq!(fixture.credentials.inserts.load(Ordering::SeqCst), 0);
    assert!(!phases.lock().unwrap().iter().any(|phase| matches!(
        phase,
        SetupPhase::Installing | SetupPhase::Starting | SetupPhase::Enrolling
    )));
}

#[test]
#[ignore = "reads an explicitly selected missing account server and a deliberately damaged copy of its signed package"]
fn native_server_setup_rejects_a_damaged_signed_package_before_offering_installation() {
    assert_eq!(expected_state(), "missing");
    let mut fixture = NativeFixture::new("rejection", configured_catalog(), false);
    let source = fixture
        .catalog
        .as_ref()
        .expect("provide the prepared signed catalog");
    let target = std::env::var("SPACETERM_SETUP_NATIVE_TARGET")
        .expect("select the destination artifact target");
    assert!(matches!(
        target.as_str(),
        "aarch64-apple-darwin" | "x86_64-unknown-linux-gnu"
    ));
    let damaged = fixture.root.join("damaged-catalog");
    std::fs::create_dir_all(damaged.join(&target)).unwrap();
    for file in ["artifact.tar.gz", "artifact.sig", "artifact.sha256"] {
        std::fs::copy(
            source.join(&target).join(file),
            damaged.join(&target).join(file),
        )
        .unwrap();
    }
    let signature = damaged.join(&target).join("artifact.sig");
    let mut bytes = std::fs::read(&signature).unwrap();
    assert_eq!(bytes.len(), 64);
    bytes[0] ^= 1;
    std::fs::write(signature, bytes).unwrap();
    fixture.catalog = Some(damaged);
    let (progress, phases) = fixture.progress();
    let result = fixture.backend().setup(
        native_request(SetupApprovals::default()),
        &CancellationToken::default(),
        &progress,
    );
    assert_eq!(
        result,
        Err(SetupFailure::Refused(RefusalKind::InvalidArtifact))
    );
    fixture.assert_no_connection();
    assert!(!phases.lock().unwrap().contains(&SetupPhase::Installing));
    assert_eq!(fixture.credentials.inserts.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "cancels an explicitly selected account's first real SSH command before any service or enrollment effect"]
fn native_server_setup_cancellation_closes_every_ssh_process() {
    let fixture = NativeFixture::new("cancel", configured_catalog(), false);
    let cancellation = CancellationToken::default();
    *fixture.adapter.cancel_after_spawn.lock().unwrap() = Some(cancellation.clone());
    let (progress, _) = fixture.progress();
    let backend = fixture.backend();
    let result = backend.setup(
        native_request(SetupApprovals::default()),
        &cancellation,
        &progress,
    );
    assert_eq!(result, Err(SetupFailure::Cancelled));
    fixture.assert_no_connection();
    assert_eq!(backend.process_counts().spawned, 1);
    assert_eq!(backend.process_counts().active(), 0);
    assert_eq!(fixture.credentials.inserts.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "requires an explicitly selected running server; credential insertion is deliberately refused before enrollment"]
fn native_server_setup_reports_unavailable_credentials_without_publishing_a_connection() {
    assert_eq!(expected_state(), "ready");
    let fixture = NativeFixture::new("credentials", None, true);
    let (progress, phases) = fixture.progress();
    let backend = fixture.backend();
    let result = setup_with_approved_offers(&backend, (false, false), &progress);
    assert_eq!(
        result,
        Err(SetupFailure::Unavailable(
            UnavailableKind::CredentialUnavailable
        ))
    );
    fixture.assert_no_connection();
    assert_eq!(fixture.credentials.inserts.load(Ordering::SeqCst), 1);
    assert!(!phases.lock().unwrap().contains(&SetupPhase::Connecting));
}

fn directory_operation(
    connection: &mut spaceterm_server::pane::PaneConnection,
    id: u64,
    begin: spaceterm_server::pane::Request,
    creating: bool,
) -> Result<spaceterm_server::pane::directories::Value, terminal_semantics::Failure> {
    use spaceterm_server::pane::{Reply, Request, directories};
    let mut reply = connection
        .exchange(&begin)
        .map_err(|_| "WSS directory request failed")?;
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match reply {
            Reply::DirectoryRead(Ok(directories::Progress::Ready(value)))
            | Reply::DirectoryCreate(Ok(directories::Progress::Ready(value))) => return Ok(value),
            Reply::DirectoryRead(Ok(directories::Progress::Pending))
            | Reply::DirectoryCreate(Ok(directories::Progress::Pending)) => {}
            _ => return Err("WSS directory operation refused".into()),
        }
        if Instant::now() >= deadline {
            let _ = connection.exchange(&if creating {
                Request::DirectoryCreate(directories::CreationRequest::Cancel { id })
            } else {
                Request::DirectoryRead(directories::Request::Cancel { id })
            });
            return Err("WSS directory operation exceeded its deadline".into());
        }
        std::thread::sleep(Duration::from_millis(20));
        reply = connection
            .exchange(&if creating {
                Request::DirectoryCreate(directories::CreationRequest::Poll { id })
            } else {
                Request::DirectoryRead(directories::Request::Poll { id })
            })
            .map_err(|_| "WSS directory reply lost")?;
    }
}

fn accepted_permissions() -> (bool, bool) {
    let confirmed = |key| std::env::var(key).as_deref() == Ok("1");
    assert!(
        confirmed("SPACETERM_SETUP_NATIVE_EXPOSE"),
        "explicit network exposure authorization required"
    );
    let install = confirmed("SPACETERM_SETUP_NATIVE_INSTALL");
    let start = confirmed("SPACETERM_SETUP_NATIVE_START");
    match expected_state().as_str() {
        "missing" => assert!(install, "explicit installation authorization required"),
        "stopped" => assert!(start, "explicit startup authorization required"),
        _ => {}
    }
    (install, start)
}

fn setup_with_approved_offers(
    backend: &impl SshSetupBackend,
    permissions: (bool, bool),
    progress: &SetupProgress,
) -> Result<crate::direct_connections::DirectRouteId, SetupFailure> {
    let mut approvals = SetupApprovals::default();
    loop {
        match backend.setup(
            native_request(approvals.clone()),
            &CancellationToken::default(),
            progress,
        ) {
            Err(SetupFailure::InstallationOffered { approval, .. })
                if permissions.0 && approvals.install.is_none() =>
            {
                approvals.install = Some(approval)
            }
            Err(SetupFailure::StartupRequired { approval })
                if permissions.1 && approvals.start.is_none() =>
            {
                approvals.start = Some(approval)
            }
            Err(SetupFailure::NetworkAuthorizationRequired { approval, .. })
                if approvals.expose_network.is_none() =>
            {
                approvals.expose_network = Some(approval)
            }
            result => return result,
        }
    }
}

#[test]
#[ignore = "installs or starts only an owner-confirmed disposable service; writes a native credential, exposes its confirmed IP:port and removes only test-owned WSS work and pairing"]
fn native_server_setup_acceptance() {
    assert_eq!(
        std::env::var("SPACETERM_DEVELOPMENT_CLIENT").as_deref(),
        Ok("setup-native-653")
    );
    let mut fixture = NativeFixture::new("accept", configured_catalog(), false);
    let (progress, phases) = fixture.progress();
    let backend = fixture.backend();
    let route = setup_with_approved_offers(&backend, accepted_permissions(), &progress)
        .expect("setup must finish with admitted saved WSS");
    assert_eq!(
        fixture.direct.status(&route),
        Some(DirectRouteStatus::Connected)
    );
    assert_eq!(fixture.direct.routes().len(), 1);
    let paired = fixture.direct.route(&route).unwrap().paired;
    assert_ne!(paired.client, [0; 32]);
    let server = fixture.direct.server(&route).unwrap();
    assert!(
        !server.is_account_local(),
        "setup returned local file authority"
    );
    assert_eq!(server.refresh().unwrap().protocol_major, 1);
    let spawns = fixture.adapter.seal();
    let counts = backend.process_counts();
    assert_eq!(counts.active(), 0);
    assert_eq!(counts.spawned, counts.reaped);
    assert_eq!(counts.spawned, spawns);
    assert!(spawns > 0, "native acceptance did not exercise OpenSSH");
    assert_eq!(fixture.credentials.inserts.load(Ordering::SeqCst), 1);
    assert!(fixture.credentials.reads.load(Ordering::SeqCst) > 0);
    assert!(fixture.direct.pending_unpairs().is_empty());
    assert_eq!(phases.lock().unwrap().last(), Some(&SetupPhase::Connecting));
    native_server_owned_work(&server, || {
        fixture.direct.disconnect(&route);
        assert!(
            server.refresh().is_err(),
            "the disconnected handle retained authority"
        );
        fixture.direct.connect(&route);
        wait_for("WSS reconnect", || {
            fixture.direct.status(&route) == Some(DirectRouteStatus::Connected)
        });
        assert_eq!(fixture.direct.route(&route).unwrap().paired, paired);
        fixture.direct.server(&route).unwrap()
    })
    .unwrap_or_else(|failure| panic!("{failure}"));
    fixture.direct.disconnect(&route);
    let relaunched = DirectConnections::with_credentials(
        identity(),
        Arc::clone(&fixture.paths),
        fixture.credentials.clone(),
    );
    relaunched.resume_retained();
    assert_eq!(relaunched.route(&route).unwrap().paired, paired);
    relaunched.resume(&route);
    wait_for("saved WSS relaunch", || {
        relaunched.status(&route) == Some(DirectRouteStatus::Connected)
    });
    let server = relaunched.server(&route).unwrap();
    assert_eq!(server.refresh().unwrap().protocol_major, 1);
    assert!(
        server
            .paired_devices()
            .unwrap()
            .iter()
            .any(|device| device.pairing == paired.pairing && device.client == paired.client)
    );
    assert_eq!(fixture.adapter.spawns.load(Ordering::SeqCst), spawns);
    fixture.direct = relaunched;
    fixture.direct.unpair_self(&route).unwrap();
    wait_for("exact native credential cleanup", || {
        fixture.direct.pending_unpairs().is_empty()
            && fixture.credentials.keys.lock().unwrap().is_empty()
    });
    assert!(fixture.direct.routes().is_empty());
    assert!(
        server.refresh().is_err(),
        "self-unpair left an admitted runtime handle"
    );
    fixture.adapter.assert_closed();
    assert_eq!(fixture.adapter.spawns.load(Ordering::SeqCst), spawns);
    assert_eq!(backend.process_counts(), counts);
}

#[test]
#[ignore = "requires a running disposable server and reachable WSS; fails only the fixture's save directory after enrollment, then resolves its exact cleanup over WSS"]
fn native_server_setup_save_failure_keeps_cleanup_without_publishing_a_connection() {
    assert_eq!(expected_state(), "ready");
    let fixture = NativeFixture::new("save", None, false);
    let unavailable = Arc::new(UnavailableDirectory {
        path: fixture.paths.directories().data.clone(),
        parked: fixture.root.join("saved-data"),
    });
    let obstruction = Arc::clone(&unavailable);
    let adapter = fixture.adapter.clone();
    let progress = SetupProgress::new(move |phase| {
        if phase == SetupPhase::Connecting {
            adapter.assert_closed();
            obstruction.block();
        }
    });
    let backend = fixture.backend();
    let result = setup_with_approved_offers(&backend, (false, false), &progress);
    unavailable.restore().unwrap();
    assert_eq!(result, Err(SetupFailure::Unconfirmed(SetupOperation::Save)));
    fixture.assert_no_connection();
    let spawns = fixture.adapter.seal();
    let cleanup = fixture.direct.pending_unpairs();
    assert_eq!(cleanup.len(), 1);
    assert!(cleanup[0].enrollment_cleanup);
    fixture.direct.resume_unpair(cleanup[0].key);
    wait_for("failed-save WSS cleanup", || {
        fixture.direct.pending_unpairs().is_empty()
            && fixture.credentials.keys.lock().unwrap().is_empty()
    });
    assert_eq!(fixture.adapter.spawns.load(Ordering::SeqCst), spawns);
}

#[test]
fn installer_frames_linux_authorization_before_the_archive_and_cleans_temporary_files() {
    use std::io::Write;
    let serial = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let root = crate::platform::unix_adapter_tests::short_temporary_root()
        .join(format!("st653-install-{}-{serial}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    for (authorization_required, password, input, expected_code) in [
        (false, false, None, 0),
        (true, false, None, 77),
        (true, true, Some("fixture"), 0),
        (true, true, Some("wrong"), 77),
    ] {
        let mut bytes = Vec::new();
        {
            let compressed =
                flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::default());
            let mut archive = tar::Builder::new(compressed);
            let helper = format!(
                "#!/bin/sh\ncase $1 in ssh-enable-linger) exit {};; ssh-enable-linger-password) IFS= read -r password; test \"$password\" = fixture;; ssh-install) test -f \"$3\" && test -f \"$4\";; *) exit 91;; esac\n",
                if authorization_required { 1 } else { 0 },
            );
            let mut header = tar::Header::new_gnu();
            header.set_size(helper.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            archive
                .append_data(
                    &mut header,
                    "server/bin/spaceterm-server",
                    helper.as_bytes(),
                )
                .unwrap();
            archive.into_inner().unwrap().finish().unwrap();
        }
        let script = crate::platform::unix_ssh_process::UnixSshProcessAdapter
            .install(
                identity(),
                bytes.len(),
                &[1; 64],
                &hex_digest(&bytes),
                password,
            )
            .replace("${TMPDIR:-/tmp}", root.to_str().unwrap())
            .replace("if [ \"$(uname -s)\" = Linux ]; then", "if true; then");
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", &script])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut writer = child.stdin.take().unwrap();
        if let Some(input) = input {
            writeln!(writer, "{input}").unwrap();
        }
        writer.write_all(&bytes).unwrap();
        drop(writer);
        assert_eq!(child.wait().unwrap().code(), Some(expected_code));
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }
    std::fs::remove_dir(root).unwrap();
}

#[test]
#[ignore = "requires a missing Linux service with linger disabled; the fixture refuses administrator interaction and never grants authorization"]
fn native_server_setup_refuses_linux_installation_without_operating_system_authorization() {
    assert_eq!(expected_state(), "missing");
    assert_eq!(
        std::env::var("SPACETERM_SETUP_NATIVE_TARGET").as_deref(),
        Ok("x86_64-unknown-linux-gnu")
    );
    assert_eq!(
        std::env::var("SPACETERM_SETUP_NATIVE_AUTHORIZATION").as_deref(),
        Ok("unavailable")
    );
    let fixture = NativeFixture::new("authorization", configured_catalog(), false);
    let (progress, _) = fixture.progress();
    let backend = fixture.backend();
    let result = setup_with_approved_offers(&backend, (true, true), &progress);
    assert_eq!(
        result,
        Err(SetupFailure::Refused(RefusalKind::AuthorizationRefused))
    );
    fixture.assert_no_connection();
    assert_eq!(fixture.credentials.inserts.load(Ordering::SeqCst), 0);
    let observed = backend.setup(
        native_request(SetupApprovals::default()),
        &CancellationToken::default(),
        &progress,
    );
    assert!(
        matches!(observed, Err(SetupFailure::InstallationOffered { .. })),
        "authorization refusal must leave the account service uninstalled"
    );
    fixture.adapter.assert_closed();
}

struct LostEnrollmentReply {
    refuse_cancellation: bool,
    cancellation_requests: AtomicUsize,
}
impl SshServerBootstrap for LostEnrollmentReply {
    fn locator(&self, identity: &str, action: &str, missing: bool) -> String {
        let script = crate::platform::unix_ssh_process::UnixSshProcessAdapter
            .locator(identity, action, missing);
        match action.split_whitespace().next() {
            Some("setup-direct") => {
                // Lose the enrollment reply after the real server has processed the request.
                format!("{} >/dev/null\n", script.trim_end())
            }
            Some("setup-direct-cancel") => {
                self.cancellation_requests.fetch_add(1, Ordering::SeqCst);
                if self.refuse_cancellation {
                    // Leave the real claim intact so only pinned WSS can settle its cleanup.
                    "exit 75\n".into()
                } else {
                    script
                }
            }
            _ => script,
        }
    }
    fn install(
        &self,
        identity: &str,
        archive_length: usize,
        signature: &[u8],
        digest: &str,
        password: bool,
    ) -> String {
        crate::platform::unix_ssh_process::UnixSshProcessAdapter.install(
            identity,
            archive_length,
            signature,
            digest,
            password,
        )
    }
}

#[test]
#[ignore = "enrolls on a selected running server and drops its real SSH reply; cleanup may finish during setup or continue over pinned WSS"]
fn native_server_setup_lost_enrollment_reply_settles_cleanup_without_ssh_recovery() {
    lost_enrollment_reply_cleanup(false);
}

#[test]
#[ignore = "enrolls on a selected running server, loses its enrollment reply and refuses SSH cancellation so pinned WSS must settle cleanup"]
fn native_server_setup_lost_enrollment_reply_cleans_up_over_wss_without_ssh_recovery() {
    lost_enrollment_reply_cleanup(true);
}

fn lost_enrollment_reply_cleanup(refuse_cancellation: bool) {
    assert_eq!(expected_state(), "ready");
    let fixture = NativeFixture::new("lost-reply", None, false);
    let (progress, _) = fixture.progress();
    let bootstrap = Arc::new(LostEnrollmentReply {
        refuse_cancellation,
        cancellation_requests: AtomicUsize::new(0),
    });
    let backend = fixture.backend_with_bootstrap(bootstrap.clone());
    let result = setup_with_approved_offers(&backend, (false, false), &progress);
    assert_eq!(
        result,
        Err(SetupFailure::Unconfirmed(SetupOperation::Enrollment))
    );
    fixture.assert_no_connection();
    assert_eq!(fixture.credentials.inserts.load(Ordering::SeqCst), 1);
    assert_eq!(bootstrap.cancellation_requests.load(Ordering::SeqCst), 1);
    let spawns = fixture.adapter.seal();
    let counts = backend.process_counts();
    assert_eq!(counts.active(), 0);
    assert_eq!(counts.spawned, counts.reaped);
    // Confirmed setup cleanup or WSS cleanup may already have completed before return.
    wait_for(
        if refuse_cancellation {
            "lost-reply WSS claim cancellation"
        } else {
            "lost-reply claim cleanup"
        },
        || {
            fixture.direct.pending_unpairs().is_empty()
                && fixture.credentials.keys.lock().unwrap().is_empty()
        },
    );
    assert_eq!(fixture.adapter.spawns.load(Ordering::SeqCst), spawns);
    assert_eq!(backend.process_counts(), counts);
    fixture.assert_no_connection();
}

/// Keeps the fixture's original record and directory identity intact through a failed save.
struct UnavailableDirectory {
    path: PathBuf,
    parked: PathBuf,
}
impl UnavailableDirectory {
    fn block(&self) {
        std::fs::rename(&self.path, &self.parked).unwrap();
        std::fs::write(&self.path, b"fixture storage unavailable").unwrap();
    }
    fn restore(&self) -> std::io::Result<()> {
        if self.parked.exists() {
            std::fs::remove_file(&self.path)?;
            std::fs::rename(&self.parked, &self.path)?;
        }
        Ok(())
    }
}
impl Drop for UnavailableDirectory {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
