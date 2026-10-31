//! Opt-in provisioning acceptance through real OpenSSH, signed packages and paired QUIC.
#[path = "setup_consent.rs"]
mod consent_tests;

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

#[path = "../../../crates/spaceterm-cli/examples/support/repository.rs"]
#[expect(
    dead_code,
    reason = "acceptance uses inventory and owning-server mutation helpers"
)]
mod repository;
#[path = "../../../crates/spaceterm-cli/examples/support/mod.rs"]
#[expect(
    dead_code,
    reason = "acceptance uses Pane claims, terminal commands and directory requests"
)]
mod support;
#[path = "../../../crates/spaceterm-cli/examples/support/terminal_semantics.rs"]
mod terminal_semantics;
#[path = "../../../crates/spaceterm-cli/examples/support/worktree.rs"]
#[expect(
    dead_code,
    reason = "acceptance uses journaled writes and exact removal review"
)]
mod worktree_fixture;

/// Observes only a fixture's static witness or its bounded shell exit status.
fn terminal_fixture_command(
    pane: &support::Pane,
    command: &str,
    stage: &'static str,
    witness: &'static str,
) -> Result<support::Screen, terminal_semantics::Failure> {
    let status = format!("ST-FIXTURE-{stage}-EXIT=");
    let command = format!("{command}; printf '\\n{status}%d\\n' \"$?\"");
    terminal_semantics::stage(stage, pane.command(&command))?;
    let output = terminal_semantics::screen(pane, |screen| {
        screen.rows.iter().any(|line| {
            line.trim_end() == witness
                || line
                    .trim_end()
                    .strip_prefix(&status)
                    .is_some_and(|code| code.parse::<u8>().is_ok())
        })
    })
    .map_err(|failure| failure.at(stage))?;
    let lines = &output.rows;
    if lines.iter().any(|line| line.trim_end() == witness) {
        return Ok(output);
    }
    if let Some(code) = lines.iter().find_map(|line| {
        line.trim_end()
            .strip_prefix(&status)
            .and_then(|code| code.parse::<u8>().ok())
    }) {
        eprintln!("QUIC fixture shell failure: stage={stage}, exit={code}");
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
        pane::{PaneSize, directories},
        protocol,
        worktree::{Branch, BranchName, Operation},
    };
    use terminal_semantics::stage as api;
    if identity() != "spaceterm-preflight" {
        return Err("QUIC fixture requires the installed Preflight identity".into());
    }
    let journal = api("quic-open-fixture-journal", server.shared_journal())?;
    if journal.pending().is_some() {
        return Err("QUIC fixture has an unresolved original; preserve it before new work".into());
    }
    drop(journal);
    let mut connection = api("quic-connect-pane", server.link())?;
    let baseline = api(
        "quic-baseline-inventory",
        repository::inventory(&connection),
    )?
    .0;
    let created = api(
        "quic-create-workspace",
        worktree_fixture::journaled_mutation(
            server,
            &connection,
            Mutation::CreateWorkspace {
                name: "QUIC native semantics".into(),
                directory: None,
                control: spaceterm_server::hierarchy::CreateControl::Creator,
            },
        ),
    )?;
    let Outcome::Accepted { target: workspace } = created.outcome else {
        return Err("QUIC native fixture Workspace creation refused".into());
    };
    // The fixture Pane outlives the exercise, so cleanup can still run its shell commands.
    let mut fixture: Option<support::Pane> = None;
    let mut remote_root = None;
    let mut root_identity = None;
    let exercise = (|| -> Result<_, terminal_semantics::Failure> {
        let current = api("quic-fixture-inventory", repository::inventory(&connection))?.0;
        let pane = current
            .workspaces
            .iter()
            .find(|work| work.id == workspace)
            .and_then(|work| work.tabs.first())
            .ok_or("QUIC fixture Pane missing")?
            .root_pane;
        let size = PaneSize {
            columns: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 18,
        };
        let started = api(
            "quic-select-fixture-pane",
            support::select(&connection, pane, true, size),
        )?
        .attached()
        .ok_or("QUIC fixture Pane did not start")?;
        let shell = &*fixture.insert(started);
        let report = terminal_semantics::exercise(shell)?;
        terminal_fixture_command(
            shell,
            "st653_live=retained; printf 'ST653-LIVE\\n'",
            "quic-live-shell",
            "ST653-LIVE",
        )?;
        let before = api("quic-before-reconnect", repository::inventory(&connection))?.0;
        let reconnected = reconnect();
        if api("quic-original-client", server.client_scope())?
            != api("quic-reconnected-client", reconnected.client_scope())?
        {
            return Err("QUIC reconnect changed the paired Client Identity".into());
        }
        connection.close();
        connection = api("quic-reconnect-pane", reconnected.link())?;
        let after = api("quic-after-reconnect", repository::inventory(&connection))?.0;
        let before = before.close.ok_or("QUIC original live inventory missing")?;
        let after = after
            .close
            .ok_or("QUIC reconnected live inventory missing")?;
        if before.boot != after.boot
            || !before
                .sessions
                .iter()
                .all(|live| after.sessions.contains(live))
        {
            return Err("QUIC reconnect replaced existing Terminal Sessions".into());
        }
        let resumed = api(
            "quic-resume-fixture-pane",
            support::select(&connection, pane, true, size),
        )?
        .attached()
        .ok_or("QUIC fixture Pane did not resume")?;
        let shell = &*fixture.insert(resumed);
        terminal_fixture_command(
            shell,
            "test \"$st653_live\" = retained && printf 'ST653-REUSED\\n'",
            "quic-shell-reuse",
            "ST653-REUSED",
        )?;
        // Only the owning server's shell creates or accesses these paths. They never enter local I/O.
        terminal_fixture_command(
            shell,
            r#"st653_root=$(mktemp -d /tmp/st653.XXXXXXXX) && printf 'ST-ROOT-CREATED\n'"#,
            "quic-root-mkdir",
            "ST-ROOT-CREATED",
        )?;
        // A cd inside command substitution captures the integration's chpwd OSC output.
        // Resolve the server path without changing the shell's directory or invoking hooks.
        terminal_fixture_command(
            shell,
            r#"st653_root=$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$st653_root") && printf 'ST-ROOT-CANONICAL\n'"#,
            "quic-root-canonicalize",
            "ST-ROOT-CANONICAL",
        )?;
        let output = terminal_fixture_command(
            shell,
            r#"st653_identity=$(python3 -c 'import os,sys; s=os.stat(sys.argv[1],follow_symlinks=False); print("%d:%d"%(s.st_dev,s.st_ino))' "$st653_root") && printf 'ST-FIXTURE-ROOT=%s\nST-FIXTURE-IDENTITY=%s\nST-ROOT-READY\n' "$st653_root" "$st653_identity""#,
            "quic-root-identity",
            "ST-ROOT-READY",
        )?;
        let lines = &output.rows;
        let root = lines
            .iter()
            .find_map(|line| line.trim_end().strip_prefix("ST-FIXTURE-ROOT="))
            .ok_or("QUIC fixture root identity missing")?;
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
            return Err("QUIC fixture root shape changed".into());
        }
        let identity = lines
            .iter()
            .find_map(|line| line.trim_end().strip_prefix("ST-FIXTURE-IDENTITY="))
            .and_then(|value| value.split_once(':'))
            .and_then(|(device, inode)| {
                Some((device.parse::<u64>().ok()?, inode.parse::<u64>().ok()?))
            })
            .ok_or("QUIC fixture filesystem identity missing")?;
        remote_root = Some(path.clone());
        root_identity = Some(identity);
        let created_directory = path.join("created-directory");
        let value = directory_operation(
            &connection,
            protocol::Operation::DirectoryCreate(directories::Creation {
                path: created_directory.clone(),
            }),
        )?;
        if !matches!(value, directories::Value::Directory(ref value) if value == &created_directory)
        {
            return Err("QUIC directory creation returned another directory".into());
        }
        let value = directory_operation(
            &connection,
            protocol::Operation::DirectoryRead(directories::Operation::List { path: path.clone() }),
        )?;
        if !matches!(value, directories::Value::Listing { names, truncated: false } if names.iter().any(|name| name == "created-directory"))
        {
            return Err("QUIC directory browsing missed its created directory".into());
        }
        terminal_fixture_command(
            shell,
            r#"ln -s "$st653_root/created-directory" "$st653_root/alias" && printf 'ST-ALIAS-READY\n'"#,
            "quic-directory-alias",
            "ST-ALIAS-READY",
        )?;
        let value = directory_operation(
            &connection,
            protocol::Operation::DirectoryRead(directories::Operation::Match {
                path: path.join("alias"),
                candidates: vec![created_directory],
            }),
        )?;
        if !matches!(value, directories::Value::Match { index: Some(0) }) {
            return Err("QUIC directory comparison did not resolve the owning-server alias".into());
        }
        terminal_fixture_command(
            shell,
            r#"GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 git -C "$st653_root" -c init.defaultBranch=main init -q && git -C "$st653_root" config core.hooksPath /dev/null && git -C "$st653_root" -c commit.gpgSign=false -c user.name=Fixture -c user.email=fixture@invalid commit -q --allow-empty -m fixture && printf 'ST-REPOSITORY-READY\n'"#,
            "quic-repository-ready",
            "ST-REPOSITORY-READY",
        )?;
        let pinned = api(
            "quic-pin-fixture-workspace",
            worktree_fixture::journaled_mutation(
                server,
                &connection,
                Mutation::PinWorkspace {
                    workspace,
                    version: 0,
                    directory: Some(path.clone()),
                },
            ),
        )?;
        if !matches!(pinned.outcome, Outcome::Accepted { .. }) {
            return Err("QUIC fixture pin refused".into());
        }
        let worktree = path.join("created");
        let created = api(
            "quic-create-fixture-worktree",
            worktree_fixture::journaled_mutation(
                server,
                &connection,
                Mutation::Worktree(Operation::Create {
                    control: spaceterm_server::hierarchy::CreateControl::Creator,
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
            return Err("QUIC Worktree creation refused".into());
        }
        let published = api(
            "quic-observe-created-worktree",
            repository::await_facts(&connection, &path, |facts| {
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
            return Err("QUIC Worktree Tab did not belong to its owning server".into());
        }
        terminal_fixture_command(
            shell,
            r#"test -d "$st653_root/created" && printf 'ST-WORKTREE-EXISTS\n'"#,
            "quic-worktree-exists",
            "ST-WORKTREE-EXISTS",
        )?;
        let removal = api(
            "quic-review-worktree-removal",
            worktree_fixture::review(&connection, &path, &worktree),
        )?;
        let removed = api(
            "quic-remove-fixture-worktree",
            worktree_fixture::journaled_mutation(server, &connection, Mutation::Worktree(removal)),
        )?;
        if !matches!(removed.outcome, Outcome::Accepted { .. }) {
            return Err("QUIC Worktree removal refused".into());
        }
        api(
            "quic-observe-removed-worktree",
            repository::await_facts(&connection, &path, |facts| {
                facts
                    .worktrees
                    .as_ref()
                    .is_some_and(|list| list.worktrees.iter().all(|row| row.root != worktree))
            }),
        )?;
        terminal_fixture_command(
            shell,
            r#"test ! -e "$st653_root/created" && printf 'ST-WORKTREE-REMOVED\n'"#,
            "quic-worktree-removed",
            "ST-WORKTREE-REMOVED",
        )?;
        Ok(report)
    })();
    if let Err(failure) = &exercise {
        eprintln!("QUIC fixture exercise failed: {failure}");
    }
    if api("quic-cleanup-journal-check", server.shared_journal())?
        .pending()
        .is_some()
    {
        return Err("QUIC fixture cleanup waits for original-operation recovery".into());
    }
    // Recheck the exact remote filesystem object before descriptor-safe recursive removal.
    // A changed target is retained, and an uncertain write never reaches this cleanup.
    let directory_cleanup = if let (Some(path), Some(shell), Some((device, inode))) =
        (remote_root, fixture.as_ref(), root_identity)
    {
        let root = path.to_str().ok_or("QUIC fixture root encoding changed")?;
        let cleanup = format!(
            r#"python3 -c 'import os,shutil,stat,sys; p=sys.argv[1]; parent=os.open(os.path.dirname(p),os.O_RDONLY|os.O_DIRECTORY); name=os.path.basename(p); fd=os.open(name,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=parent); s=os.fstat(fd); assert (s.st_dev,s.st_ino)==({device},{inode}) and shutil.rmtree.avoids_symlink_attacks; os.fchdir(fd); [shutil.rmtree(n) if stat.S_ISDIR(os.stat(n,dir_fd=fd,follow_symlinks=False).st_mode) else os.unlink(n,dir_fd=fd) for n in os.listdir(fd)]; s=os.stat(name,dir_fd=parent,follow_symlinks=False); assert (s.st_dev,s.st_ino)==({device},{inode}); os.rmdir(name,dir_fd=parent); os.close(fd); os.close(parent); print("ST-ROOT-REMOVED")' '{root}' 2>/dev/null"#
        );
        terminal_fixture_command(shell, &cleanup, "quic-root-removed", "ST-ROOT-REMOVED").map(drop)
    } else {
        Ok(())
    };
    let target = CloseTarget::Workspace { workspace };
    let current = api("quic-cleanup-inventory", repository::inventory(&connection))?.0;
    let confirmation = current
        .close_confirmation(target)
        .ok_or("QUIC fixture close target missing")?;
    let closed = api(
        "quic-close-fixture-workspace",
        worktree_fixture::journaled_mutation(
            server,
            &connection,
            Mutation::Close {
                target,
                confirmation,
            },
        ),
    )?;
    if !matches!(closed.outcome, Outcome::Accepted { .. }) {
        return Err("QUIC fixture close refused".into());
    }
    let after = api(
        "quic-verify-baseline-inventory",
        repository::inventory(&connection),
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
        return Err("baseline server work changed during the QUIC fixture".into());
    }
    let report = exercise?;
    directory_cleanup?;
    eprintln!(
        "PASS QUIC semantic protocol: copied rows={}, accessible rows={}, graphics supported={}, graphics bytes={}, owning-server Worktree create/remove; fixture cleanup and baseline work retained",
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

#[derive(Default)]
struct ObservedPrompts {
    attempts: AtomicUsize,
}
impl AskPassAttemptFactory for ObservedPrompts {
    fn start_attempt(&self, paths: &AppPaths) -> Result<AskPassAttempt, AskPassUnavailable> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        NoPrompt.start_attempt(paths)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeOfferKind {
    Installation,
    Startup,
    Network,
}

struct NativeOfferObservation {
    kind: NativeOfferKind,
    address: Option<SetupAddress>,
}

struct NativeApprovals {
    permissions: (bool, bool, bool),
    pause_on_offer: bool,
    waiting: AtomicBool,
    release: AtomicBool,
    observed: Mutex<Vec<NativeOfferObservation>>,
    adapter: ObservedAdapter,
    prompts: Arc<ObservedPrompts>,
    expected_address: Mutex<Option<SetupAddress>>,
    master_baseline: usize,
}
impl NativeApprovals {
    fn new(adapter: &ObservedAdapter, permissions: (bool, bool, bool)) -> Arc<Self> {
        Arc::new(Self {
            permissions,
            pause_on_offer: false,
            waiting: AtomicBool::new(false),
            release: AtomicBool::new(false),
            observed: Mutex::default(),
            adapter: adapter.clone(),
            prompts: Arc::default(),
            expected_address: Mutex::default(),
            master_baseline: adapter.masters.load(Ordering::SeqCst),
        })
    }

    fn assert_one_authentication(&self) {
        assert_eq!(self.prompts.attempts.load(Ordering::SeqCst), 1);
        assert_eq!(
            self.adapter.masters.load(Ordering::SeqCst),
            self.master_baseline + 1
        );
    }
}
impl SetupApprovalHandler for NativeApprovals {
    fn approve(&self, offer: SetupOffer, cancellation: &CancellationToken) -> bool {
        self.assert_one_authentication();
        assert_eq!(
            self.adapter.active.load(Ordering::SeqCst),
            1,
            "approval must pause the same live setup SSH connection"
        );
        let (kind, address, accepted) = match offer {
            SetupOffer::Installation { .. } => {
                (NativeOfferKind::Installation, None, self.permissions.0)
            }
            SetupOffer::Startup => (NativeOfferKind::Startup, None, self.permissions.1),
            SetupOffer::Network { address, .. } => {
                if let Some(expected) = &*self.expected_address.lock().unwrap() {
                    assert!(
                        &address == expected,
                        "network approval changed the selected QUIC address"
                    );
                }
                (NativeOfferKind::Network, Some(address), self.permissions.2)
            }
        };
        self.observed
            .lock()
            .unwrap()
            .push(NativeOfferObservation { kind, address });
        if self.pause_on_offer {
            self.waiting.store(true, Ordering::SeqCst);
            while !self.release.load(Ordering::SeqCst) && !cancellation.is_cancelled() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        accepted && !cancellation.is_cancelled()
    }
}

#[derive(Clone, Default)]
struct ObservedAdapter {
    spawns: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    sealed: Arc<AtomicBool>,
    cancel_after_spawn: Arc<Mutex<Option<CancellationToken>>>,
    refused_start_observed: Arc<AtomicBool>,
    masters: Arc<AtomicUsize>,
    channels: Arc<AtomicUsize>,
    master_path: Arc<Mutex<Option<std::ffi::OsString>>>,
    master_configuration: Arc<Mutex<Option<PathBuf>>>,
}
impl ObservedAdapter {
    fn assert_closed(&self) {
        assert_eq!(
            self.active.load(Ordering::SeqCst),
            0,
            "setup retained an SSH process"
        );
        if let Some(path) = self.master_path.lock().unwrap().as_ref() {
            assert!(
                std::fs::symlink_metadata(path).is_err(),
                "setup retained its private master socket"
            );
        }
        if let Some(path) = self.master_configuration.lock().unwrap().as_ref() {
            assert!(
                std::fs::symlink_metadata(path).is_err(),
                "setup retained its private master configuration"
            );
        }
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
        let arguments = request.arguments();
        let options = arguments
            .windows(2)
            .filter(|pair| pair[0] == "-o")
            .map(|pair| pair[1].as_os_str())
            .collect::<Vec<_>>();
        let option = |value: &str| options.contains(&std::ffi::OsStr::new(value));
        let nonnetwork = arguments == [std::ffi::OsString::from("-V")]
            || arguments.iter().any(|argument| argument == "-G");
        if !nonnetwork {
            let control_path = options
                .iter()
                .find_map(|option| option.to_str()?.strip_prefix("ControlPath="))
                .expect("setup SSH must select its exact private master socket");
            if option("ControlMaster=yes") {
                self.masters.fetch_add(1, Ordering::SeqCst);
                *self.master_path.lock().unwrap() = Some(control_path.into());
                *self.master_configuration.lock().unwrap() = Some(
                    arguments
                        .windows(2)
                        .find(|pair| pair[0] == "-F")
                        .expect("setup master must use its attempt-owned configuration")[1]
                        .clone()
                        .into(),
                );
            } else {
                assert!(option("ControlMaster=no"));
                assert!(option("ProxyCommand=false"));
                assert!(option("ProxyJump=none"));
                assert!(option("BatchMode=yes"));
                assert!(
                    self.master_path.lock().unwrap().as_deref()
                        == Some(std::ffi::OsStr::new(control_path)),
                    "setup channel selected a different master socket"
                );
                self.channels.fetch_add(1, Ordering::SeqCst);
            }
        }
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
        let status =
            crate::platform::unix_ssh_process::UnixSshProcessAdapter.try_status(process)?;
        if status.is_some_and(|exit| exit.code() == Some(86)) {
            self.refused_start_observed.store(true, Ordering::SeqCst);
        }
        Ok(status)
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
            store: crate::direct_connections::credentials::native(identity()).unwrap(),
            keys: Mutex::default(),
            inserts: AtomicUsize::new(0),
            reads: AtomicUsize::new(0),
            fail_insert,
        })
    }
}
impl CredentialStore for ObservedCredentials {
    fn kind(&self) -> spaceterm_connections::StoreKind {
        self.store.kind()
    }
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
        Self::with_runtime_name(
            name,
            catalog,
            fail_credentials,
            std::ffi::OsStr::new("runtime"),
        )
    }
    fn with_runtime_name(
        name: &str,
        catalog: Option<PathBuf>,
        fail_credentials: bool,
        runtime_name: &std::ffi::OsStr,
    ) -> Self {
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
            xdg_runtime_dir: Some(root.join(runtime_name).into()),
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
            Arc::new(spaceterm_connections::records::PrivateRecords::new(
                Arc::new(crate::direct_connection_storage::DesktopRecordFile::new(
                    Arc::clone(&paths),
                )),
            )),
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
        self.backend_with_home(bootstrap, PathBuf::from(std::env::var_os("HOME").unwrap()))
    }
    fn backend_with_home(
        &self,
        bootstrap: Arc<dyn SshServerBootstrap>,
        home: PathBuf,
    ) -> NativeSshSetup<ObservedAdapter> {
        NativeSshSetup::new(
            NativeSshSetupConfig {
                paths: Arc::clone(&self.paths),
                filesystem: Arc::new(
                    crate::platform::unix_host_config_filesystem::UnixHostConfigFilesystem,
                ),
                bootstrap,
                home,
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

struct NativeDnsHome {
    path: PathBuf,
    identity: (u64, u64),
}
impl NativeDnsHome {
    fn from_environment(
        fixture: &NativeFixture,
        request: &SetupRequest,
        expected_address: &SetupAddress,
    ) -> Option<Self> {
        use std::io::Write;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

        let hostname = std::env::var_os("SPACETERM_SETUP_NATIVE_HOSTNAME_OVERRIDE")?;
        let hostname = hostname
            .to_str()
            .expect("the fixture HostName must be UTF-8");
        assert!(
            request.address.is_none(),
            "the DNS fixture must derive its address from OpenSSH"
        );
        let address = SetupAddress::parse(hostname).expect("select a valid DNS HostName");
        assert!(address.host().parse::<std::net::IpAddr>().is_err());
        assert_eq!(
            hostname.trim_end_matches('.').to_ascii_lowercase(),
            address.host(),
            "the HostName override accepts DNS only, without a port"
        );
        assert!(
            expected_address == &address,
            "EXPECT_ADDRESS must independently confirm the override's default QUIC address"
        );
        let literal_alias = |value: &str| {
            !value.is_empty()
                && value.len() <= 253
                && value.as_bytes()[0].is_ascii_alphanumeric()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        };
        let destination = request.destination.as_str();
        let alias = match destination.split_once('@') {
            Some((user, alias)) => {
                assert!(literal_alias(user), "select a literal SSH account name");
                alias
            }
            None => destination,
        };
        assert!(
            literal_alias(alias),
            "select a literal SSH alias without patterns or a port"
        );
        assert!(
            !alias.eq_ignore_ascii_case(address.host()),
            "the DNS fixture needs an alias distinct from its effective HostName"
        );
        let source = PathBuf::from(std::env::var_os("HOME").expect("select the actual SSH home"))
            .join(".ssh/config");
        assert!(
            source.is_absolute()
                && !source
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir)),
            "the existing user configuration must have an absolute literal path"
        );
        let include = source
            .to_str()
            .expect("the existing user configuration path must be UTF-8");
        assert!(
            !include.chars().any(|character| character.is_control()
                || matches!(character, '*' | '?' | '[' | ']' | '%' | '$' | '~')),
            "the existing user configuration path must not expand patterns or tokens"
        );
        assert!(
            std::fs::metadata(&source).is_ok_and(|metadata| metadata.is_file()),
            "the DNS fixture must include an existing user SSH configuration"
        );
        let include = include.replace('\\', "\\\\").replace('"', "\\\"");
        let path = fixture.root.join("dns-home");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        let home = Self {
            path,
            identity: (metadata.dev(), metadata.ino()),
        };
        let ssh = home.path.join(".ssh");
        std::fs::DirBuilder::new().mode(0o700).create(&ssh).unwrap();
        let mut config = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(ssh.join("config"))
            .unwrap();
        write!(
            config,
            "Host {alias}\n    HostName {}\nHost *\n    Include \"{include}\"\nHost *\n",
            address.host()
        )
        .unwrap();
        Some(home)
    }
}
impl Drop for NativeDnsHome {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        if std::fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.is_dir() && (metadata.dev(), metadata.ino()) == self.identity
        }) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn expected_state() -> String {
    let expected = std::env::var("SPACETERM_SETUP_NATIVE_EXPECT")
        .expect("select missing, ready or stopped account state");
    assert!(matches!(expected.as_str(), "missing" | "ready" | "stopped"));
    expected
}
fn native_request(approvals: Arc<NativeApprovals>) -> SetupRequest {
    assert_eq!(
        identity(),
        "spaceterm-preflight",
        "native setup fixtures require a compiled Preflight client and installed Preflight service"
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
        address: std::env::var_os("SPACETERM_SETUP_NATIVE_ADDRESS").map(|address| {
            SetupAddress::parse(address.to_str().expect("the setup address must be UTF-8")).unwrap()
        }),
        name: "setup-native-653".into(),
        prompts: approvals.prompts.clone(),
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
    let approvals = NativeApprovals::new(&fixture.adapter, (false, false, false));
    let backend = fixture.backend();
    let result = backend.setup(
        native_request(approvals.clone()),
        &CancellationToken::default(),
        &progress,
    );
    assert_eq!(result, Err(SetupFailure::Cancelled));
    let expected = match expected_state().as_str() {
        "missing" => NativeOfferKind::Installation,
        "stopped" => NativeOfferKind::Startup,
        "ready" => NativeOfferKind::Network,
        _ => unreachable!(),
    };
    let offers = approvals.observed.lock().unwrap();
    assert_eq!(offers.len(), 1);
    assert_eq!(offers[0].kind, expected);
    approvals.assert_one_authentication();
    fixture.assert_no_connection();
    assert_eq!(
        backend.process_counts().spawned,
        backend.process_counts().reaped
    );
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
        native_request(NativeApprovals::new(
            &fixture.adapter,
            (false, false, false),
        )),
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
        native_request(NativeApprovals::new(
            &fixture.adapter,
            (false, false, false),
        )),
        &cancellation,
        &progress,
    );
    assert_eq!(result, Err(SetupFailure::Cancelled));
    fixture.assert_no_connection();
    assert_eq!(backend.process_counts().spawned, 1);
    assert_eq!(backend.process_counts().active(), 0);
    assert_eq!(
        backend.process_counts().spawned,
        backend.process_counts().reaped
    );
    assert_eq!(fixture.credentials.inserts.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "cancels at the first approval on an explicitly selected Preflight service while its authenticated setup connection remains live"]
fn native_server_setup_cancellation_during_approval_closes_every_ssh_process() {
    let fixture = NativeFixture::new("cancel-approval", configured_catalog(), false);
    let mut approvals = NativeApprovals::new(&fixture.adapter, (false, false, false));
    Arc::get_mut(&mut approvals).unwrap().pause_on_offer = true;
    let cancellation = CancellationToken::default();
    let (progress, _) = fixture.progress();
    let backend = Arc::new(fixture.backend());
    let attempted = Arc::clone(&backend);
    let worker_cancellation = cancellation.clone();
    let request = native_request(approvals.clone());
    let thread =
        std::thread::spawn(move || attempted.setup(request, &worker_cancellation, &progress));
    let mut worker = SetupWorker {
        cancellation,
        thread: Some(thread),
    };
    wait_for("setup approval wait", || {
        approvals.waiting.load(Ordering::SeqCst)
    });
    assert_eq!(backend.process_counts().active(), 1);
    let result = worker.finish();
    assert_eq!(result, Err(SetupFailure::Cancelled));
    assert!(worker.cancellation.is_cancelled());
    assert_eq!(approvals.observed.lock().unwrap().len(), 1);
    approvals.assert_one_authentication();
    fixture.assert_no_connection();
    let counts = backend.process_counts();
    assert_eq!(counts.active(), 0);
    assert_eq!(counts.spawned, counts.reaped);
    assert_eq!(fixture.credentials.inserts.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "requires an explicitly selected running server; credential insertion is deliberately refused before enrollment"]
fn native_server_setup_reports_unavailable_credentials_without_publishing_a_connection() {
    assert_eq!(expected_state(), "ready");
    let fixture = NativeFixture::new("credentials", None, true);
    let (progress, phases) = fixture.progress();
    let backend = fixture.backend();
    let result = setup_with_approvals(&backend, &fixture.adapter, (false, false), &progress);
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

/// One directory request. Its reply arrives when the operation settles.
fn directory_operation(
    link: &spaceterm_server::connection::link::ServerLink,
    operation: spaceterm_server::protocol::Operation,
) -> Result<spaceterm_server::pane::directories::Value, terminal_semantics::Failure> {
    use spaceterm_server::protocol::ReplyBody;
    let pending = link
        .request(operation)
        .map_err(|_| "QUIC directory request failed")?;
    match pending.wait_for(Duration::from_secs(8)) {
        Some(Ok(ReplyBody::Directory(Ok(value)))) => Ok(value),
        Some(_) => Err("QUIC directory operation refused".into()),
        None => {
            pending.cancel();
            Err("QUIC directory operation exceeded its deadline".into())
        }
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

fn setup_with_approvals(
    backend: &impl SshSetupBackend,
    adapter: &ObservedAdapter,
    permissions: (bool, bool),
    progress: &SetupProgress,
) -> Result<crate::direct_connections::DirectRouteId, SetupFailure> {
    let approvals = NativeApprovals::new(adapter, (permissions.0, permissions.1, true));
    backend.setup(
        native_request(approvals),
        &CancellationToken::default(),
        progress,
    )
}

#[test]
fn disconnecting_a_saved_route_closes_its_retained_server_handle() {
    use spaceterm_server::connection::link::{Health, LinkError, LinkEvent, ServerLink};

    let (connections, routes) = spaceterm_connections::tests::connected_routes(&["Setup"]);
    let route = routes[0];
    let server = connections.server(&route).unwrap();
    let link = ServerLink::new(Arc::new(|_: LinkEvent| {}), Arc::new(|| {}), Instant::now());
    server.adopt(link.clone());
    assert!(server.refresh().is_ok());
    let pending = link
        .request(spaceterm_server::protocol::Operation::Direct(
            spaceterm_server::direct::Management::Status,
        ))
        .unwrap();

    connections.disconnect(&route);

    assert_eq!(link.health(), Health::Closed(LinkError::Closed));
    assert!(pending.wait().is_err());
    assert!(server.refresh().is_err());
    assert_eq!(
        connections.status(&route),
        Some(DirectRouteStatus::Disconnected)
    );
}

#[test]
#[ignore = "requires compiled and installed Preflight; one SSH authentication spans approved setup, then QUIC owns work, reconnect and exact pairing cleanup"]
fn native_server_setup_acceptance() {
    let mut fixture = NativeFixture::new("accept", configured_catalog(), false);
    let permissions = accepted_permissions();
    let approvals = NativeApprovals::new(&fixture.adapter, (permissions.0, permissions.1, true));
    let request = native_request(approvals.clone());
    let require_network_offer = request.address.is_none() || expected_state() == "missing";
    let expected_address = request.address.clone().unwrap_or_else(|| {
        let expected = std::env::var("SPACETERM_SETUP_NATIVE_EXPECT_ADDRESS")
            .expect("confirm the SSH alias's effective DNS HostName and QUIC port");
        let address = SetupAddress::parse(&expected).unwrap();
        assert!(
            address.host().parse::<std::net::IpAddr>().is_err(),
            "the default-address acceptance cell must retain a DNS name"
        );
        assert_ne!(
            request.destination.as_str().rsplit('@').next().unwrap(),
            address.host(),
            "select an SSH alias whose HostName differs from the alias"
        );
        address
    });
    let dns_home = NativeDnsHome::from_environment(&fixture, &request, &expected_address);
    *approvals.expected_address.lock().unwrap() = Some(expected_address.clone());
    let observed = Arc::clone(&approvals);
    let (base_progress, phases) = fixture.progress();
    let connecting = Arc::new(AtomicBool::new(false));
    let observed_connecting = Arc::clone(&connecting);
    let expected_before_quic = expected_address.clone();
    let progress = SetupProgress::new(move |phase| {
        base_progress.report(phase);
        if phase == SetupPhase::Connecting {
            observed.assert_one_authentication();
            let offers = observed.observed.lock().unwrap();
            if require_network_offer {
                assert!(
                    offers
                        .iter()
                        .any(|offer| { offer.address.as_ref() == Some(&expected_before_quic) }),
                    "the default-DNS or fresh-install cell requires a network offer before QUIC admission"
                );
            }
            observed_connecting.store(true, Ordering::SeqCst);
        }
    });
    let backend = match &dns_home {
        Some(home) => fixture.backend_with_home(
            Arc::new(crate::platform::unix_ssh_process::UnixSshProcessAdapter),
            home.path.clone(),
        ),
        None => fixture.backend(),
    };
    let route = backend
        .setup(request, &CancellationToken::default(), &progress)
        .expect("setup must finish with admitted saved QUIC");
    drop(dns_home);
    approvals.assert_one_authentication();
    assert!(connecting.load(Ordering::SeqCst));
    let offers = approvals.observed.lock().unwrap();
    let network_offers = offers
        .iter()
        .filter(|offer| offer.kind == NativeOfferKind::Network)
        .count();
    assert!(network_offers <= 1);
    if require_network_offer {
        assert_eq!(network_offers, 1);
    }
    match expected_state().as_str() {
        "missing" => assert!(
            offers
                .iter()
                .any(|offer| offer.kind == NativeOfferKind::Installation)
        ),
        "stopped" => assert!(
            offers
                .iter()
                .any(|offer| offer.kind == NativeOfferKind::Startup)
        ),
        _ => {}
    }
    drop(offers);
    assert_eq!(
        fixture.direct.status(&route),
        Some(DirectRouteStatus::Connected)
    );
    assert_eq!(fixture.direct.routes().len(), 1);
    assert_eq!(
        fixture.direct.route(&route).unwrap().endpoint,
        expected_address.url()
    );
    let paired = fixture.direct.route(&route).unwrap().paired;
    assert_ne!(paired.client, [0; 32]);
    let server = fixture.direct.server(&route).unwrap();
    assert!(
        !server.is_account_local(),
        "setup returned local file authority"
    );
    assert_eq!(server.refresh().unwrap().protocol_major, 4);
    let spawns = fixture.adapter.seal();
    let counts = backend.process_counts();
    assert_eq!(counts.active(), 0);
    assert_eq!(counts.spawned, counts.reaped);
    assert_eq!(counts.spawned, spawns);
    assert!(spawns > 0, "native acceptance did not exercise OpenSSH");
    assert!(fixture.adapter.channels.load(Ordering::SeqCst) > 0);
    assert_eq!(fixture.credentials.inserts.load(Ordering::SeqCst), 1);
    assert!(fixture.credentials.reads.load(Ordering::SeqCst) > 0);
    assert!(fixture.direct.pending_unpairs().is_empty());
    let phases = phases.lock().unwrap();
    assert_eq!(phases.last(), Some(&SetupPhase::Connecting));
    assert_eq!(
        phases
            .iter()
            .filter(|phase| **phase == SetupPhase::Authenticating)
            .count(),
        1
    );
    drop(phases);
    native_server_owned_work(&server, || {
        fixture.direct.disconnect(&route);
        assert!(
            server.refresh().is_err(),
            "the disconnected handle retained authority"
        );
        fixture.direct.connect(&route);
        wait_for("QUIC reconnect", || {
            fixture.direct.status(&route) == Some(DirectRouteStatus::Connected)
        });
        assert_eq!(fixture.direct.route(&route).unwrap().paired, paired);
        fixture.direct.server(&route).unwrap()
    })
    .unwrap_or_else(|failure| panic!("{failure}"));
    fixture.direct.disconnect(&route);
    let relaunched = DirectConnections::with_credentials(
        identity(),
        Arc::new(spaceterm_connections::records::PrivateRecords::new(
            Arc::new(crate::direct_connection_storage::DesktopRecordFile::new(
                Arc::clone(&fixture.paths),
            )),
        )),
        fixture.credentials.clone(),
    );
    relaunched.resume_retained();
    assert_eq!(relaunched.route(&route).unwrap().paired, paired);
    assert_eq!(
        relaunched.route(&route).unwrap().endpoint,
        expected_address.url()
    );
    relaunched.resume(&route);
    wait_for("saved QUIC relaunch", || {
        relaunched.status(&route) == Some(DirectRouteStatus::Connected)
    });
    let server = relaunched.server(&route).unwrap();
    assert_eq!(server.refresh().unwrap().protocol_major, 4);
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
#[ignore = "requires installed Preflight and reachable QUIC; overlaps read-only admission on one saved pairing, then revokes only that pairing"]
fn native_server_setup_refresh_overlaps_first_shared_pane_without_losing_admission() {
    use spaceterm_server::ServerError;
    use std::sync::{Barrier, mpsc};

    fn observed<T>(
        cycle: usize,
        stage: &'static str,
        started: Instant,
        result: Result<T, ServerError>,
    ) -> Result<T, ServerError> {
        eprintln!(
            "native.cycle={cycle} stage={stage} elapsed_ms={} error={:?}",
            started.elapsed().as_millis(),
            result.as_ref().err()
        );
        result
    }

    assert_eq!(expected_state(), "ready");
    let fixture = NativeFixture::new("overlap", configured_catalog(), false);
    let permissions = accepted_permissions();
    assert!(
        !permissions.0 && !permissions.1,
        "the overlap fixture requires a ready server without installation or startup authority"
    );
    let approvals = NativeApprovals::new(&fixture.adapter, (permissions.0, permissions.1, true));
    let request = native_request(approvals.clone());
    let requested_address = request.address.clone();
    let backend = fixture.backend();
    let (progress, _) = fixture.progress();
    let route = backend
        .setup(request, &CancellationToken::default(), &progress)
        .expect("setup must finish with admitted saved QUIC");
    approvals.assert_one_authentication();
    assert_eq!(
        fixture.direct.status(&route),
        Some(DirectRouteStatus::Connected)
    );
    assert_eq!(fixture.direct.routes().len(), 1);
    assert!(fixture.direct.pending_unpairs().is_empty());
    let saved = fixture.direct.route(&route).unwrap();
    if let Some(address) = requested_address {
        assert!(
            saved.endpoint == address.url(),
            "setup changed the requested endpoint"
        );
    }
    let server = fixture.direct.server(&route).unwrap();
    assert!(!server.is_account_local());
    let spawns = fixture.adapter.seal();
    let counts = backend.process_counts();
    assert_eq!(counts.active(), 0);
    assert_eq!(counts.spawned, counts.reaped);
    assert_eq!(counts.spawned, spawns);
    assert!(spawns > 0);
    assert_eq!(fixture.credentials.inserts.load(Ordering::SeqCst), 1);

    let started = Instant::now();
    let deadline = started + Duration::from_secs(45);
    let captured = server.clone();
    let (finished, completion) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let exercise = (|| -> Result<(), ServerError> {
            for cycle in 0..16 {
                // Six seconds of pacing crosses natural route health checks and
                // timestamp seconds without manufacturing an inner transport seam.
                let due = started + Duration::from_millis(cycle as u64 * 400);
                if let Some(wait) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                observed(
                    cycle,
                    "cycle-budget",
                    started,
                    if Instant::now() < deadline {
                        Ok(())
                    } else {
                        Err(ServerError::Protocol)
                    },
                )?;
                let barrier = Barrier::new(2);
                let (refresh_done, refresh_dropped) = mpsc::sync_channel(1);
                let (refreshed, connected) = std::thread::scope(|scope| {
                    let barrier = &barrier;
                    let captured = &captured;
                    let refresh = scope.spawn(move || {
                        barrier.wait();
                        let result = observed(cycle, "refresh", started, captured.refresh())
                            .and_then(|status| {
                                observed(
                                    cycle,
                                    "refresh-protocol",
                                    started,
                                    if status.protocol_major == 4 {
                                        Ok(())
                                    } else {
                                        Err(ServerError::Protocol)
                                    },
                                )
                            });
                        // refresh finished on the handle's shared connection.
                        let _ = refresh_done.send(());
                        result
                    });
                    let shared = scope.spawn(move || {
                        barrier.wait();
                        let link =
                            observed(cycle, "first-shared-connect", started, captured.link())?;
                        observed(
                            cycle,
                            "refresh-drop",
                            started,
                            refresh_dropped
                                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                                .map_err(|_| ServerError::Protocol),
                        )?;
                        // Protocol major 3 implies every capability, so a request that changes
                        // nothing checks the admitted connection.
                        let reply = observed(
                            cycle,
                            "sibling-round-trip",
                            started,
                            support::round_trip(&link),
                        );
                        // Close the shared connection, so the next cycle's refresh and connect
                        // overlap the first connection again.
                        link.close();
                        reply
                    });
                    (
                        refresh.join().unwrap_or(Err(ServerError::StartupFailed)),
                        shared.join().unwrap_or(Err(ServerError::StartupFailed)),
                    )
                });
                observed(cycle, "refresh-worker", started, refreshed)?;
                observed(cycle, "shared-worker", started, connected)?;
            }
            Ok(())
        })();
        drop(captured);
        let _ = finished.send(exercise);
    });
    let result = completion.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    let Ok(result) = result else {
        // The worker owns only read-only handles. Preserve the normal fixture's
        // saved pairing on failure; never retry or revoke under an in-flight call.
        drop(worker);
        observed::<()>(16, "wall-budget", started, Err(ServerError::Protocol))
            .unwrap_or_else(|error| panic!("{error:?}"));
        unreachable!();
    };
    observed(
        16,
        "exercise-worker",
        started,
        worker.join().map_err(|_| ServerError::StartupFailed),
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    observed(16, "exercise", started, result).unwrap_or_else(|error| panic!("{error:?}"));
    assert!(
        Instant::now() < deadline,
        "native overlap exceeded its wall budget"
    );
    let retained = fixture.direct.route(&route).unwrap();
    assert!(
        retained.paired == saved.paired,
        "overlap changed the saved pairing"
    );
    assert!(
        retained.endpoint == saved.endpoint,
        "overlap changed the saved endpoint"
    );
    assert_eq!(fixture.adapter.spawns.load(Ordering::SeqCst), spawns);
    assert_eq!(backend.process_counts(), counts);
    observed(
        16,
        "self-unpair",
        started,
        fixture
            .direct
            .unpair_self(&route)
            .map_err(ServerError::Direct),
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    while !fixture.direct.pending_unpairs().is_empty()
        || !fixture.credentials.keys.lock().unwrap().is_empty()
    {
        assert!(
            Instant::now() < deadline,
            "native overlap credential cleanup exceeded its wall budget"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(fixture.direct.routes().is_empty());
    assert!(
        server.refresh().is_err(),
        "self-unpair left an admitted handle"
    );
    fixture.adapter.assert_closed();
    assert_eq!(fixture.adapter.spawns.load(Ordering::SeqCst), spawns);
    assert_eq!(backend.process_counts(), counts);
}

#[test]
#[ignore = "requires a running disposable server and reachable QUIC; fails only the fixture's save directory after enrollment, then resolves its exact cleanup over QUIC"]
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
    let result = setup_with_approvals(&backend, &fixture.adapter, (false, false), &progress);
    assert_eq!(result, Err(SetupFailure::Unconfirmed(SetupOperation::Save)));
    fixture.assert_no_connection();
    let spawns = fixture.adapter.seal();
    let cleanup = fixture.direct.pending_unpairs();
    assert_eq!(cleanup.len(), 1);
    assert!(cleanup[0].enrollment_cleanup);
    unavailable.restore().unwrap();
    fixture.direct.resume_unpair(cleanup[0].key);
    wait_for("failed-save QUIC cleanup", || {
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
                "#!/bin/sh\n[ \"$1:$2\" = server:provision ] || exit 91\nshift 2\ncase $1 in ssh-enable-linger) if [ \"${{2-}}\" = --password-stdin ]; then IFS= read -r password; test \"$password\" = fixture; else exit {}; fi;; ssh-install) test -f \"$2\" && test -f \"$3\";; *) exit 91;; esac\n",
                if authorization_required { 1 } else { 0 },
            );
            let mut header = tar::Header::new_gnu();
            header.set_size(helper.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            let executable = spaceterm_server::service::Layout::host(identity())
                .unwrap()
                .executable();
            archive
                .append_data(
                    &mut header,
                    std::path::Path::new("server").join(executable),
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
    let result = setup_with_approvals(&backend, &fixture.adapter, (true, true), &progress);
    assert_eq!(
        result,
        Err(SetupFailure::Refused(RefusalKind::AuthorizationRefused))
    );
    fixture.assert_no_connection();
    assert_eq!(fixture.credentials.inserts.load(Ordering::SeqCst), 0);
    let approvals = NativeApprovals::new(&fixture.adapter, (false, false, false));
    let observed = backend.setup(
        native_request(approvals.clone()),
        &CancellationToken::default(),
        &progress,
    );
    assert!(
        observed == Err(SetupFailure::Cancelled)
            && approvals
                .observed
                .lock()
                .unwrap()
                .iter()
                .any(|offer| offer.kind == NativeOfferKind::Installation),
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
                    // Leave the real claim intact so only pinned QUIC can settle its cleanup.
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
#[ignore = "enrolls on a selected running server and drops its real SSH reply; cleanup may finish during setup or continue over pinned QUIC"]
fn native_server_setup_lost_enrollment_reply_settles_cleanup_without_ssh_recovery() {
    lost_enrollment_reply_cleanup(false);
}

#[test]
#[ignore = "enrolls on a selected running server, loses its enrollment reply and refuses SSH cancellation so pinned QUIC must settle cleanup"]
fn native_server_setup_lost_enrollment_reply_cleans_up_over_quic_without_ssh_recovery() {
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
    let result = setup_with_approvals(&backend, &fixture.adapter, (false, false), &progress);
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
    // Confirmed setup cleanup or QUIC cleanup may already have completed before return.
    wait_for(
        if refuse_cancellation {
            "lost-reply QUIC claim cancellation"
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

struct ReplacementDuringStartup;
impl SshServerBootstrap for ReplacementDuringStartup {
    fn locator(&self, identity: &str, action: &str, missing: bool) -> String {
        let script = crate::platform::unix_ssh_process::UnixSshProcessAdapter
            .locator(identity, action, missing);
        if action != "setup-start" {
            return script;
        }
        let prefix = script
            .rsplit_once("exec ")
            .expect("native locator ends with its selected executable")
            .0;
        let replace_and_start = r#"import json,os,signal,stat,subprocess,sys
identity_path,policy_path,binary,application=sys.argv[1:]
request=sys.stdin.buffer.read(16385)
assert len(request)<=16384
approved=json.loads(request)
assert approved['build']==application and approved['stopped']
fd=os.open(identity_path,os.O_RDWR|os.O_NOFOLLOW)
metadata=os.fstat(fd)
assert stat.S_ISREG(metadata.st_mode) and metadata.st_uid==os.geteuid() and metadata.st_nlink==1 and stat.S_IMODE(metadata.st_mode)==0o600
original=os.pread(fd,16385,0)
record=json.loads(original)
assert record['server']==approved['server']
policy=open(policy_path,'rb').read()
assert json.loads(policy)['stopped']
record['server'][0]^=1
assert any(record['server'])
replacement=json.dumps(record,separators=(',',':')).encode()
def write(data):
    os.lseek(fd,0,os.SEEK_SET)
    assert os.write(fd,data)==len(data)
    os.ftruncate(fd,len(data))
    os.fsync(fd)
def interrupted(signum,frame):
    raise InterruptedError()
for signum in (signal.SIGHUP,signal.SIGTERM,signal.SIGINT):
    signal.signal(signum,interrupted)
try:
    write(replacement)
    result=subprocess.run([binary,'server','provision','ssh-start'],input=request,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE,timeout=10)
    assert result.returncode!=0 and b'(changed)' in result.stderr
    assert open(policy_path,'rb').read()==policy
finally:
    current=os.stat(identity_path,follow_symlinks=False)
    assert (current.st_dev,current.st_ino)==(metadata.st_dev,metadata.st_ino)
    assert os.pread(fd,16385,0)==replacement
    write(original)
    os.close(fd)
assert open(identity_path,'rb').read()==original
raise SystemExit(86)
"#;
        let quoted = format!("'{}'", replace_and_start.replace('\'', "'\\''"));
        format!(
            "{prefix}python3 -c {quoted} \"$root/identity.json\" \"$policy\" \"$root/installations/$digest/bin/spaceterm\" '{identity}'\n"
        )
    }
    fn install(
        &self,
        identity: &str,
        length: usize,
        signature: &[u8],
        digest: &str,
        password: bool,
    ) -> String {
        crate::platform::unix_ssh_process::UnixSshProcessAdapter
            .install(identity, length, signature, digest, password)
    }
}

#[test]
#[ignore = "requires an owner-confirmed stopped disposable service; briefly replaces and restores only its server identity around the real conditional startup CLI"]
fn native_server_setup_refuses_a_replacement_at_startup_dispatch_without_clearing_stop() {
    assert_eq!(expected_state(), "stopped");
    assert_eq!(
        std::env::var("SPACETERM_SETUP_NATIVE_START_RACE").as_deref(),
        Ok("1"),
        "explicit temporary identity replacement authorization required"
    );
    let fixture = NativeFixture::new("start-race", None, true);
    let (progress, phases) = fixture.progress();
    let backend = fixture.backend_with_bootstrap(Arc::new(ReplacementDuringStartup));
    let result = setup_with_approvals(&backend, &fixture.adapter, (false, true), &progress);
    assert_eq!(
        result,
        Err(SetupFailure::Unconfirmed(SetupOperation::Start))
    );
    assert!(
        fixture
            .adapter
            .refused_start_observed
            .load(Ordering::SeqCst),
        "real conditional startup must refuse the changed server and preserve exact policy bytes"
    );
    fixture.assert_no_connection();
    assert_eq!(fixture.credentials.inserts.load(Ordering::SeqCst), 0);
    assert!(!phases.lock().unwrap().contains(&SetupPhase::Enrolling));
    let approvals = NativeApprovals::new(&fixture.adapter, (false, false, false));
    let still_stopped = backend.setup(
        native_request(approvals.clone()),
        &CancellationToken::default(),
        &progress,
    );
    assert_eq!(still_stopped, Err(SetupFailure::Cancelled));
    assert!(
        approvals
            .observed
            .lock()
            .unwrap()
            .iter()
            .any(|offer| offer.kind == NativeOfferKind::Startup)
    );
    fixture.adapter.assert_closed();
    assert_eq!(backend.process_counts().active(), 0);
}

#[test]
fn native_server_setup_refuses_unsafe_runtime_config_paths_before_spawning_ssh() {
    use std::os::unix::ffi::OsStringExt;
    for runtime in [
        std::ffi::OsString::from("runtime space"),
        std::ffi::OsString::from("runtime;touch shell-witness"),
        std::ffi::OsString::from_vec(b"runtime-\xff".to_vec()),
    ] {
        let fixture = NativeFixture::with_runtime_name("unsafe-config", None, true, &runtime);
        let backend = fixture.backend_with_home(
            Arc::new(crate::platform::unix_ssh_process::UnixSshProcessAdapter),
            fixture.root.clone(),
        );
        let result = backend.setup(
            SetupRequest {
                destination: SshDestination::new("fixture-target".into()).unwrap(),
                address: Some(SetupAddress::parse("192.0.2.1:9231").unwrap()),
                name: "Fixture".into(),
                prompts: Arc::new(NoPrompt),
                approvals: NativeApprovals::new(&fixture.adapter, (false, false, false)),
            },
            &CancellationToken::default(),
            &SetupProgress::new(|_| {}),
        );
        assert_eq!(
            result,
            Err(SetupFailure::Unavailable(UnavailableKind::SshFailed))
        );
        assert_eq!(backend.process_counts().spawned, 0);
        assert_eq!(fixture.adapter.spawns.load(Ordering::SeqCst), 0);
        assert!(!fixture.root.join("shell-witness").exists());
        fixture.assert_no_connection();
        assert_eq!(fixture.credentials.inserts.load(Ordering::SeqCst), 0);
    }
}

struct FixtureChild(Option<crate::platform::unix_ssh_process::UnixSshProcess>);
impl FixtureChild {
    fn spawn(executable: &str, arguments: Vec<std::ffi::OsString>, home: &std::path::Path) -> Self {
        use crate::ssh::process::{SshProcessSpawnRequest, SshProcessStdio};
        let request = SshProcessSpawnRequest::new(
            executable.into(),
            arguments,
            home.into(),
            vec![
                ("HOME".into(), home.as_os_str().into()),
                ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            ],
            SshProcessStdio::Null,
            SshProcessStdio::Null,
            SshProcessStdio::Null,
        );
        let spawned = crate::platform::unix_ssh_process::UnixSshProcessAdapter
            .spawn(request)
            .expect("fixture subprocess must launch");
        Self(Some(spawned.into_process()))
    }
    fn running(&mut self) -> bool {
        crate::platform::unix_ssh_process::UnixSshProcessAdapter
            .try_status(self.0.as_mut().unwrap())
            .unwrap()
            .is_none()
    }
}
impl Drop for FixtureChild {
    fn drop(&mut self) {
        if let Some(mut process) = self.0.take() {
            let adapter = crate::platform::unix_ssh_process::UnixSshProcessAdapter;
            let _ = adapter.signal(&mut process, ProcessSignal::Kill);
            let _ = adapter.reap(process);
        }
    }
}

fn fixture_ssh(
    config: &std::path::Path,
    home: &std::path::Path,
    arguments: &[std::ffi::OsString],
) -> crate::ssh::process::CapturedProcessOutput {
    use crate::ssh::{
        command::SshCommandSpec,
        process::{SshProcessEnvironment, run_captured_process},
    };
    // Configuration and master-control queries open no session, even when the config requests a fork.
    let mut selected = vec!["-N".into(), "-F".into(), config.as_os_str().into()];
    selected.extend_from_slice(arguments);
    let command = SshCommandSpec::for_test("/usr/bin/ssh".into(), selected);
    let environment = SshProcessEnvironment::new_without_authentication(home.into(), None).unwrap();
    run_captured_process(
        &crate::platform::unix_ssh_process::UnixSshProcessAdapter,
        &command,
        &environment,
        Vec::new(),
        16 * 1024,
        &CancellationToken::default(),
        Instant::now() + Duration::from_secs(3),
    )
    .expect("fixture SSH command must finish")
}

struct FixtureMasters {
    config: PathBuf,
    home: PathBuf,
}
impl Drop for FixtureMasters {
    fn drop(&mut self) {
        // Only this fixture's generated multiplexing sockets can be selected here.
        let Ok(entries) = std::fs::read_dir(&self.home) else {
            return;
        };
        for entry in entries.flatten().take(32) {
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("mux-"))
            {
                let arguments = [
                    "-S".into(),
                    entry.path().into_os_string(),
                    "-O".into(),
                    "exit".into(),
                    "fixture-hop".into(),
                ];
                let _ =
                    std::panic::catch_unwind(|| fixture_ssh(&self.config, &self.home, &arguments));
            }
        }
    }
}

struct BlockingDiscovery(PathBuf);
impl SshServerBootstrap for BlockingDiscovery {
    fn locator(&self, _: &str, action: &str, _: bool) -> String {
        assert_eq!(
            action, "ssh-discover",
            "the blocking fixture permits discovery only"
        );
        let witness = self.0.to_str().unwrap().replace('\'', "'\\''");
        format!(
            "printf authenticated > '{witness}'; while :; do printf . || exit; /bin/sleep 0.05; done\n"
        )
    }
    fn install(&self, _: &str, _: usize, _: &[u8], _: &str, _: bool) -> String {
        panic!("the blocking fixture never installs a server")
    }
}

struct StoppedDiscovery(PathBuf);
impl SshServerBootstrap for StoppedDiscovery {
    fn locator(&self, identity: &str, action: &str, _: bool) -> String {
        if action != "ssh-discover" {
            return "exit 79\n".into();
        }
        // The loopback SSH account is this fixture's owning account. No native service is touched.
        // SAFETY: geteuid has no preconditions.
        let account = unsafe { libc::geteuid() };
        let discovery = spaceterm_server::provisioning::AccountDiscovery {
            schema: 1,
            account,
            ssh_local_address: None,
            build: identity.into(),
            server: Some([19; 32]),
            installed: true,
            enabled: false,
            running: false,
            stopped: true,
            start_observation: Some(spaceterm_server::service::SetupStartObservation {
                account,
                build: identity.into(),
                server: [19; 32],
                installation: "a".repeat(64),
                version: "1.0.0".into(),
                commit: "b".repeat(40),
                stopped: true,
            }),
            protocol_majors: vec![spaceterm_server::protocol::MAJOR],
            capabilities: vec!["workspace_policy".into()],
            target: env!("SPACETERM_TARGET").into(),
            version: "1.0.0".into(),
            commit: "b".repeat(40),
        };
        let response = serde_json::to_string(&discovery)
            .unwrap()
            .replace('\'', "'\\''");
        let witness = self.0.to_str().unwrap().replace('\'', "'\\''");
        format!("printf x >> '{witness}'; printf '%s\\n' '{response}'\n")
    }
    fn install(&self, _: &str, _: usize, _: &[u8], _: &str, _: bool) -> String {
        panic!("the stopped-discovery fixture never installs a server")
    }
}

struct SetupWorker {
    cancellation: CancellationToken,
    thread: Option<
        std::thread::JoinHandle<Result<crate::direct_connections::DirectRouteId, SetupFailure>>,
    >,
}
impl SetupWorker {
    fn finish(&mut self) -> Result<crate::direct_connections::DirectRouteId, SetupFailure> {
        self.cancellation.cancel();
        self.join()
    }
    fn join(&mut self) -> Result<crate::direct_connections::DirectRouteId, SetupFailure> {
        self.thread
            .take()
            .unwrap()
            .join()
            .expect("native setup worker must finish")
    }
}
impl Drop for SetupWorker {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn fixture_account_name() -> String {
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::zeroed();
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0; 65536];
    // SAFETY: all output pointers refer to live writable storage for getpwuid_r.
    let status = unsafe {
        libc::getpwuid_r(
            libc::geteuid(),
            entry.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    assert_eq!(status, 0);
    assert!(!result.is_null(), "the fixture requires an owning account");
    // SAFETY: successful getpwuid_r initialized entry and its strings within the live buffer.
    let account = unsafe { std::ffi::CStr::from_ptr(entry.assume_init().pw_name) }
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        account
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
    );
    account
}

struct ProxyJumpFixture {
    // Drop every fixture process before removing its private files.
    unrelated: Mutex<FixtureChild>,
    _masters: FixtureMasters,
    _sshd: FixtureChild,
    config: PathBuf,
    setup: NativeFixture,
}
impl ProxyJumpFixture {
    fn new() -> Self {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        assert!(
            std::path::Path::new("/usr/sbin/sshd").is_file(),
            "native ProxyJump fixture requires /usr/sbin/sshd"
        );
        // SAFETY: geteuid has no preconditions.
        assert_ne!(
            unsafe { libc::geteuid() },
            0,
            "native setup runs as a nonroot account"
        );
        let fixture = NativeFixture::new("proxyjump", None, true);
        let root = &fixture.root;
        for name in ["host-key", "client-key"] {
            let status = std::process::Command::new("/usr/bin/ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(root.join(name))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("fixture requires ssh-keygen");
            assert!(status.success(), "fixture key generation failed");
        }
        let account = fixture_account_name();
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = reservation.local_addr().unwrap().port();
        let sshd_config = root.join("sshd-config");
        std::fs::write(&sshd_config, format!(
        "Port {port}\nListenAddress 127.0.0.1\nHostKey {}/host-key\nPidFile {}/sshd-pid\nAuthorizedKeysFile {}/client-key.pub\nAllowUsers {account}\nStrictModes no\nUsePAM no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nPubkeyAuthentication yes\nAllowTcpForwarding yes\nAllowAgentForwarding no\nX11Forwarding no\nPrintMotd no\nLogLevel QUIET\n",
        root.display(), root.display(), root.display())).unwrap();
        let public = std::fs::read_to_string(root.join("host-key.pub")).unwrap();
        let mut fields = public.split_whitespace();
        let algorithm = fields.next().unwrap();
        let key = fields.next().unwrap();
        std::fs::write(
            root.join("known-hosts"),
            format!("[127.0.0.1]:{port} {algorithm} {key}\n"),
        )
        .unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(root.join(".ssh"))
            .unwrap();
        let config = root.join(".ssh/config");
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&config)
            .unwrap();
        write!(file,
        "Host *\n ControlMaster auto\n ControlPath {}/mux-%C\n ControlPersist 600\n ForkAfterAuthentication yes\n BatchMode yes\n IdentitiesOnly yes\n IdentityFile {}/client-key\n UserKnownHostsFile {}/known-hosts\n GlobalKnownHostsFile /dev/null\n StrictHostKeyChecking yes\nHost fixture-hop fixture-target fixture-unrelated\n HostName 127.0.0.1\n Port {port}\n User {account}\nHost fixture-target\n ProxyJump fixture-hop\n",
        root.display(), root.display(), root.display()).unwrap();
        drop(file);
        drop(reservation);
        let mut sshd = FixtureChild::spawn(
            "/usr/sbin/sshd",
            vec![
                "-D".into(),
                "-e".into(),
                "-f".into(),
                sshd_config.into_os_string(),
            ],
            root,
        );
        wait_for("loopback sshd", || {
            assert!(
                sshd.running(),
                "fixture sshd exited; check native account and sshd prerequisites"
            );
            std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).is_ok()
        });
        let _masters = FixtureMasters {
            config: config.clone(),
            home: root.clone(),
        };
        let effective = fixture_ssh(&config, root, &["-G".into(), "fixture-target".into()]);
        assert!(effective.exit.is_success());
        let effective = String::from_utf8(effective.stdout).unwrap();
        assert!(effective.lines().any(|line| line == "controlmaster auto"));
        assert!(effective.lines().any(|line| line == "controlpersist 600"));
        assert!(
            effective
                .lines()
                .any(|line| line == "forkafterauthentication yes")
        );
        assert!(
            effective
                .lines()
                .any(|line| line == "proxyjump fixture-hop")
        );
        let user_socket = root.join("user-master");
        let user_master = FixtureChild::spawn(
            "/usr/bin/ssh",
            vec![
                "-F".into(),
                config.as_os_str().into(),
                "-S".into(),
                user_socket.as_os_str().into(),
                "-M".into(),
                "-N".into(),
                "-oControlPersist=no".into(),
                "-oForkAfterAuthentication=no".into(),
                "fixture-unrelated".into(),
            ],
            root,
        );
        let fixture = Self {
            unrelated: Mutex::new(user_master),
            _masters,
            _sshd: sshd,
            config,
            setup: fixture,
        };
        wait_for("unrelated fixture master", || {
            fixture.unrelated_master_running()
        });
        fixture
    }
    fn unrelated_master_running(&self) -> bool {
        if !self.unrelated.lock().unwrap().running() {
            return false;
        }
        fixture_ssh(
            &self.config,
            &self.setup.root,
            &[
                "-S".into(),
                self.setup.root.join("user-master").into_os_string(),
                "-O".into(),
                "check".into(),
                "fixture-unrelated".into(),
            ],
        )
        .exit
        .is_success()
    }
}

#[test]
#[ignore = "requires runnable /usr/sbin/sshd and ssh-keygen as the current nonroot account; uses only fixture keys, loopback listeners and private multiplexing sockets"]
fn native_server_setup_proxyjump_cancellation_leaves_no_persistent_hop_and_preserves_an_unrelated_master()
 {
    let harness = ProxyJumpFixture::new();
    let fixture = &harness.setup;
    let root = &fixture.root;
    let witness = root.join("proxy-authenticated");
    let backend = Arc::new(
        fixture.backend_with_home(Arc::new(BlockingDiscovery(witness.clone())), root.clone()),
    );
    let cancellation = CancellationToken::default();
    let worker_cancellation = cancellation.clone();
    let attempted = Arc::clone(&backend);
    let approvals = NativeApprovals::new(&fixture.adapter, (false, false, false));
    let prompts = approvals.prompts.clone();
    let thread = std::thread::spawn(move || {
        attempted.setup(
            SetupRequest {
                destination: SshDestination::new("fixture-target".into()).unwrap(),
                address: Some(SetupAddress::parse("192.0.2.1:9231").unwrap()),
                name: "ProxyJump fixture".into(),
                prompts,
                approvals,
            },
            &worker_cancellation,
            &SetupProgress::new(|_| {}),
        )
    });
    let mut worker = SetupWorker {
        cancellation,
        thread: Some(thread),
    };
    wait_for("authenticated ProxyJump discovery", || witness.exists());
    assert!(
        harness.unrelated_master_running(),
        "setup replaced the unrelated master"
    );
    assert_eq!(worker.finish(), Err(SetupFailure::Cancelled));
    fixture.assert_no_connection();
    let spawns = fixture.adapter.seal();
    let counts = backend.process_counts();
    assert_eq!(counts.active(), 0);
    assert_eq!(counts.spawned, counts.reaped);
    assert!(
        harness.unrelated_master_running(),
        "setup cancellation terminated the unrelated master"
    );
    assert!(
        !std::fs::read_dir(root).unwrap().flatten().any(|entry| entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("mux-"))),
        "setup or a ProxyJump hop left a persistent master socket"
    );
    assert_eq!(fixture.adapter.spawns.load(Ordering::SeqCst), spawns);
    assert_eq!(backend.process_counts(), counts);
}

#[test]
#[ignore = "requires runnable loopback sshd and fixture keys; closes only the attempt-owned master while a synthetic stopped-service approval waits"]
fn native_server_setup_missing_master_refuses_fresh_authentication_and_preserves_an_unrelated_master()
 {
    let harness = ProxyJumpFixture::new();
    let fixture = &harness.setup;
    let witness = fixture.root.join("discovery-count");
    let backend = Arc::new(fixture.backend_with_home(
        Arc::new(StoppedDiscovery(witness.clone())),
        fixture.root.clone(),
    ));
    let mut approvals = NativeApprovals::new(&fixture.adapter, (false, true, false));
    Arc::get_mut(&mut approvals).unwrap().pause_on_offer = true;
    let request = SetupRequest {
        destination: SshDestination::new("fixture-target".into()).unwrap(),
        address: Some(SetupAddress::parse("192.0.2.1:9231").unwrap()),
        name: "Missing setup master fixture".into(),
        prompts: approvals.prompts.clone(),
        approvals: approvals.clone(),
    };
    let cancellation = CancellationToken::default();
    let worker_cancellation = cancellation.clone();
    let attempted = Arc::clone(&backend);
    let (progress, phases) = fixture.progress();
    let thread =
        std::thread::spawn(move || attempted.setup(request, &worker_cancellation, &progress));
    let mut worker = SetupWorker {
        cancellation,
        thread: Some(thread),
    };
    wait_for("authenticated startup approval", || {
        approvals.waiting.load(Ordering::SeqCst)
    });
    let offers = approvals.observed.lock().unwrap();
    assert_eq!(offers.len(), 1);
    assert_eq!(offers[0].kind, NativeOfferKind::Startup);
    drop(offers);
    assert_eq!(std::fs::read(&witness).unwrap(), b"x");
    assert_eq!(backend.process_counts().active(), 1);
    assert_eq!(fixture.adapter.channels.load(Ordering::SeqCst), 1);
    assert!(harness.unrelated_master_running());
    let socket = fixture.adapter.master_path.lock().unwrap().clone().unwrap();
    let configuration = fixture
        .adapter
        .master_configuration
        .lock()
        .unwrap()
        .clone()
        .unwrap();
    let closed = fixture_ssh(
        &configuration,
        &fixture.root,
        &[
            "-S".into(),
            socket.clone(),
            "-o".into(),
            "ProxyCommand=false".into(),
            "-o".into(),
            "ProxyJump=none".into(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-O".into(),
            "exit".into(),
            "fixture-target".into(),
        ],
    );
    assert!(closed.exit.is_success(), "exact setup master did not close");
    wait_for("attempt-owned master socket removal", || {
        std::fs::symlink_metadata(&socket).is_err()
    });
    approvals.release.store(true, Ordering::SeqCst);
    assert!(matches!(
        worker.join(),
        Err(SetupFailure::Unavailable(
            UnavailableKind::HostUnreachable | UnavailableKind::ServiceUnavailable
        ))
    ));
    assert!(!worker.cancellation.is_cancelled());
    assert_eq!(std::fs::read(&witness).unwrap(), b"x");
    assert_eq!(fixture.adapter.channels.load(Ordering::SeqCst), 2);
    approvals.assert_one_authentication();
    fixture.assert_no_connection();
    assert_eq!(fixture.credentials.inserts.load(Ordering::SeqCst), 0);
    assert!(!phases.lock().unwrap().iter().any(|phase| matches!(
        phase,
        SetupPhase::Starting | SetupPhase::Enrolling | SetupPhase::Connecting
    )));
    let spawns = fixture.adapter.seal();
    let counts = backend.process_counts();
    assert_eq!(counts.active(), 0);
    assert_eq!(counts.spawned, counts.reaped);
    assert!(harness.unrelated_master_running());
    assert!(
        !std::fs::read_dir(&fixture.root)
            .unwrap()
            .flatten()
            .any(|entry| entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("mux-")))
    );
    assert_eq!(fixture.adapter.spawns.load(Ordering::SeqCst), spawns);
    assert_eq!(backend.process_counts(), counts);
}
