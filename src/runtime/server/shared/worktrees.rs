//! Worktree writes reuse the retained mutation journal. A started write has no read cancellation.
use super::*;
use crate::cancellation::CancellationToken;
use crate::repository_status::{
    ProgramError, ProgramExit, ProgramRequest, RepositoryProgramRunner, RepositoryToolDiscovery,
};
use crate::worktrees::git::{LocalWorktreeGit, RemovalExpectation, WorktreeRemoveError};
use spaceterm_server::{
    hierarchy::{Mutation, Outcome},
    worktree::*,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

mod preparing;
mod reads;

/// The old local factory's scoped launch rule, validated on the owning server.
pub(super) fn starting_directory(
    workspace: &spaceterm_server::hierarchy::Workspace,
    source: Option<Id>,
    root: &Path,
) -> Result<PathBuf, Refusal> {
    let source = source
        .map(|id| {
            workspace
                .tabs
                .iter()
                .flat_map(|t| &t.panes)
                .find(|p| p.id == id)
                .ok_or(Refusal::Missing)
        })
        .transpose()?;
    let filesystem = local_filesystem();
    if let Some(pin) = &workspace.pinned_directory {
        filesystem
            .validate_directory(pin)
            .map_err(|_| Refusal::DirectoryUnavailable)?;
    }
    let start = workspace
        .pinned_directory
        .as_deref()
        .filter(|pin| filesystem.physically_contains(root, pin))
        .or_else(|| {
            source
                .map(crate::runtime::server::repositories::pane_directory)
                .filter(|cwd| filesystem.physically_contains(root, cwd))
        })
        .unwrap_or(root);
    filesystem
        .validate_directory(start)
        .map(|directory| directory.path().to_owned())
        .map_err(|_| Refusal::DirectoryUnavailable)
}

enum JobResult {
    Prepared(Result<preparing::Prepared, Error>),
    Finished(Result<Option<PathBuf>, Error>),
}

struct WriteRunner(Option<std::fs::File>);
impl RepositoryProgramRunner for WriteRunner {
    fn run(
        &self,
        request: &ProgramRequest,
        stdout: &mut dyn FnMut(&[u8]),
        token: &CancellationToken,
    ) -> Result<ProgramExit, ProgramError> {
        let mut request = request.clone();
        let write = request
            .arguments
            .windows(2)
            .any(|args| args[0] == "worktree" && (args[1] == "add" || args[1] == "remove"));
        if write {
            request.deadline = None;
            // Drain all output without allowing a noisy filter to cancel a started write.
            request.stdout_limit = None;
            crate::platform::unix_repository_program::UnixRepositoryProgramRunner::new().run_locked(
                &request,
                &mut |_| {},
                &CancellationToken::default(),
                self.0.as_ref(),
            )
        } else {
            request.deadline = Some(Instant::now() + Duration::from_secs(5));
            crate::platform::unix_repository_program::UnixRepositoryProgramRunner::new().run_locked(
                &request,
                stdout,
                token,
                self.0.as_ref(),
            )
        }
    }
}
fn git(
    lock: Option<std::fs::File>,
    token: &CancellationToken,
) -> Result<(LocalWorktreeGit, PathBuf), Error> {
    #[cfg(target_os = "macos")]
    let discovery = crate::platform::macos_repository_tools::MacosRepositoryToolDiscovery::new();
    #[cfg(target_os = "linux")]
    let discovery = crate::platform::linux_repository_tools::LinuxRepositoryToolDiscovery::new(
        std::env::var_os("PATH").as_deref(),
    );
    let tools = discovery.discover();
    let home = crate::platform::app_directories::AppDirectoryEnvironment::capture()
        .home
        .map(PathBuf::from)
        .ok_or(Error::Unavailable)?;
    let home = std::fs::canonicalize(home).map_err(|_| Error::Unavailable)?;
    let runner: Arc<dyn RepositoryProgramRunner> = Arc::new(WriteRunner(lock));
    let (status, _) = crate::repository_status::tool_check::check_tools(
        runner.as_ref(),
        &crate::repository_status::ToolInventory {
            git: tools.git.clone(),
            github_cli: None,
        },
        &home,
        &[],
        Instant::now() + Duration::from_secs(5),
        token,
    );
    let (Some(path), crate::repository_status::GitToolStatus::Ready(version)) = (tools.git, status)
    else {
        return Err(Error::Unavailable);
    };
    Ok((
        LocalWorktreeGit::new(runner, path, home.clone(), version),
        home,
    ))
}
#[derive(Clone)]
struct Approval {
    device: Id,
    review: Removal,
    expected: RemovalExpectation,
    record: crate::worktrees::listing::WorktreeRecord,
    expires: Instant,
}
pub(super) struct Writes {
    approvals: Vec<Approval>,
    reads: reads::Reads,
    worker: Option<std::thread::JoinHandle<JobResult>>,
    operation: Option<(Id, u64)>,
}
impl Writes {
    pub(super) fn new() -> Self {
        Self {
            approvals: Vec::new(),
            reads: reads::Reads::default(),
            worker: None,
            operation: None,
        }
    }
    /// Whether a Worktree write is being prepared or run.
    pub(super) fn busy(&self) -> bool {
        self.worker.is_some()
    }
}
impl Drop for Writes {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn list(
    git: &LocalWorktreeGit,
    root: &Path,
    token: &CancellationToken,
) -> Result<crate::worktrees::WorktreeSnapshot, Error> {
    let directory = local_filesystem()
        .validate_directory(root)
        .map_err(|_| Error::Unavailable)?;
    let list = git
        .list(directory.path(), token)
        .map_err(|_| Error::Unavailable)?
        .ok_or(Error::Missing)?;
    if list.repository.main_root() != root {
        return Err(Error::Changed);
    }
    Ok(list)
}
fn affected_worktree_tabs(inventory: &Inventory, root: &Path, path: &Path) -> Vec<AffectedTab> {
    inventory.affected_worktree_tabs_in(root, path, |directory| {
        directory.starts_with(path) || local_filesystem().physically_contains(path, directory)
    })
}
pub(in crate::runtime::server) fn pinned_in_worktree<'a>(
    pins: impl IntoIterator<Item = &'a Path>,
    path: &Path,
) -> Result<bool, Error> {
    for pin in pins {
        if pin.starts_with(path) {
            return Ok(true);
        }
        let pin = match local_filesystem().validate_directory(pin) {
            Ok(pin) => pin,
            Err(LocalFilesystemError::Missing | LocalFilesystemError::NotDirectory) => continue,
            Err(_) => return Err(Error::Unavailable),
        };
        if local_filesystem().physically_contains(path, pin.path()) {
            return Ok(true);
        }
    }
    Ok(false)
}
impl SharedWorkspaceHost {
    fn worktree_lock(
        &self,
    ) -> Result<Option<spaceterm_server::organization::WorktreeLock>, Refusal> {
        self.journal
            .as_ref()
            .map(|journal| journal.worktree_lock())
            .transpose()
            .map(Option::flatten)
            .map_err(|_| Refusal::StorageUnavailable)
    }
    /// Commits a Worktree step and waits until it is retained. Git starts only after its record
    /// is durable, so recovery after a crash finds every write that may have begun.
    fn commit_retained(&mut self, next: State) -> Result<(), Refusal> {
        let revision = self.commit(next)?;
        self.retained(revision)
    }
    pub(super) fn cancel_worktree_reads(&mut self, connection: Id) {
        self.writes.reads.cancel_connection(connection);
    }
    pub(super) fn cancel_worktree_read(&mut self, connection: Id, id: RequestId) {
        self.writes.reads.cancel((connection, id));
    }
    /// When the earliest running Worktree read outlasts its deadline.
    pub(super) fn worktree_reads_due(&self) -> Option<Instant> {
        self.writes.reads.due()
    }
    /// Every Worktree read started from now on wakes `wake` when it settles.
    pub(super) fn wake_worktree_reads(
        &mut self,
        wake: Arc<dyn spaceterm_server::connection::Wake>,
    ) {
        self.writes.reads.set_wake(wake);
    }
    pub(super) fn stop_worktree_reads(&mut self) {
        self.writes.reads.stop();
    }
    /// Answers `read` at once, or starts it as read `id` of `connection` and returns `None`.
    pub(super) fn worktree_read(
        &mut self,
        device: Id,
        connection: Id,
        id: RequestId,
        read: Read,
    ) -> Result<Option<ReadReply>, Error> {
        if let Read::Settings = read {
            return Ok(Some(ReadReply::Settings(
                self.state.worktree_settings.clone(),
            )));
        }
        if matches!(read, Read::CheckRemoval { .. }) && !self.state.pending_worktrees().is_empty() {
            return Err(Error::Unavailable);
        }
        let mut inventory = self.inventory();
        inventory.repositories = None;
        let settings = self.state.worktree_settings.clone();
        self.writes
            .reads
            .begin((connection, id), device, read, inventory, settings)?;
        Ok(None)
    }
    /// The replies of Worktree reads that settled. A removal check that settled records its
    /// approval first, so the removal it reviewed can follow its reply.
    pub(super) fn settled_worktree_reads(&mut self) -> Vec<crate::runtime::server::SettledRead> {
        self.writes.approvals.retain(|a| a.expires > Instant::now());
        let settled = self.writes.reads.settled();
        settled
            .into_iter()
            .map(|((connection, id), prepared)| {
                let reply = prepared.and_then(|prepared| {
                    if let Some(approval) = prepared.approval {
                        let device = approval.device;
                        self.writes.approvals.retain(|a| a.device != device);
                        if self.writes.approvals.len() >= 32 {
                            return Err(Error::Unavailable);
                        }
                        self.writes.approvals.push(approval);
                    }
                    Ok(prepared.reply)
                });
                crate::runtime::server::SettledRead {
                    connection,
                    id,
                    reply: ReplyBody::Worktree(reply),
                }
            })
            .collect()
    }
    pub(super) fn begin_worktree(
        &mut self,
        device: Id,
        sequence: u64,
        operation: Operation,
    ) -> Result<Answer, Refusal> {
        if !self.state.pending_worktrees().is_empty() {
            return Ok(Answer::Pending);
        }
        let lock = self.worktree_lock()?;
        let mut next = self.state.clone();
        next.apply(
            device,
            sequence,
            Mutation::Worktree(operation.clone()),
            &self.prototype.directory,
            |_| false,
        )?;
        if let Operation::SetLocation { version, template } = operation {
            let checked = (|| {
                let current = next.worktree_settings.clone();
                if version != current.version {
                    return Err(Error::Changed);
                }
                crate::server_settings::validate_location_template(&template)?;
                next.worktree_settings = Settings {
                    template,
                    version: current.version.checked_add(1).ok_or(Error::Unavailable)?,
                };
                Ok(())
            })();
            next.finish_worktree(
                device,
                sequence,
                match checked {
                    Ok(()) => Outcome::Accepted { target: device },
                    Err(error) => Outcome::WorktreeFailed(error),
                },
            )?;
            let revision = self.commit(next)?;
            return Ok(Answer::Outcome {
                outcome: Box::new(
                    self.state
                        .outcome(device, sequence)
                        .ok_or(Refusal::Missing)?
                        .clone(),
                ),
                revision,
            });
        }
        let approval = match &operation {
            Operation::Remove { confirmation, .. } => self
                .writes
                .approvals
                .iter()
                .find(|a| a.device == device && a.review.confirmation == *confirmation)
                .cloned(),
            _ => None,
        };
        let mut inventory = self.inventory();
        inventory.repositories = None;
        self.commit_retained(next)?;
        self.writes.operation = Some((device, sequence));
        self.writes.worker = Some(
            std::thread::Builder::new()
                .name("spaceterm-worktree-preflight".into())
                .spawn(move || {
                    // From here Git children inherit the lock, so it fences recovery until they exit.
                    let lock =
                        lock.map(spaceterm_server::organization::WorktreeLock::into_inheritable);
                    JobResult::Prepared(preparing::prepare(operation, approval, inventory, lock))
                })
                .map_err(|_| Refusal::Capacity)?,
        );
        Ok(Answer::Pending)
    }
    fn start_prepared(
        &mut self,
        device: Id,
        sequence: u64,
        prepared: preparing::Prepared,
    ) -> Result<(), Refusal> {
        self.synchronize()?;
        let mut next = self.state.clone();
        match prepared {
            preparing::Prepared::Open {
                workspace,
                source,
                root,
                path,
                control,
            } => {
                let creator = control.creator(device, &self.inventory());
                let selected = next
                    .inventory
                    .workspaces
                    .iter()
                    .find(|w| w.id == workspace)
                    .ok_or(Refusal::Missing)
                    .and_then(|w| starting_directory(w, source, &path))
                    .and_then(|start| next.add_worktree_tab(creator, workspace, root, path, start));
                let outcome = match selected {
                    Ok(target) => Outcome::Accepted { target },
                    Err(_) => Outcome::WorktreeFailed(Error::Unavailable),
                };
                next.finish_worktree(device, sequence, outcome)?;
                self.commit_retained(next)?;
                self.writes.operation = None;
                if let Some(record) = self.state.outcome(device, sequence).cloned() {
                    self.reveal(&record);
                }
            }
            preparing::Prepared::Write(write) => {
                let checked = (|| {
                    if let Some(removal) = &write.removal {
                        removal.apply(device, &self.inventory(), &mut next)?;
                    }
                    if let Some(Mutation::Worktree(Operation::Create {
                        workspace,
                        root,
                        path,
                        ..
                    })) = self
                        .state
                        .outcome(device, sequence)
                        .map(|record| &record.mutation)
                    {
                        // Reserve capacity before Git starts, without publishing a speculative Tab.
                        next.clone()
                            .add_worktree_tab(
                                Some(device),
                                *workspace,
                                root.clone(),
                                path.clone(),
                                path.clone(),
                            )
                            .map_err(|_| Error::Unavailable)?;
                    }
                    Ok(())
                })();
                if let Err(error) = checked {
                    return self.settle_worktree(device, sequence, Err(error));
                }
                // Durable closure precedes ending reviewed Sessions and starting the Git write.
                self.commit_retained(next)?;
                self.synchronize()?;
                self.writes.worker = Some(
                    std::thread::Builder::new()
                        .name("spaceterm-worktree-write".into())
                        .spawn(move || JobResult::Finished((write.run)(write.git)))
                        .map_err(|_| Refusal::Capacity)?,
                );
            }
        }
        Ok(())
    }
    pub(super) fn finish_worktrees(&mut self, wait: bool) -> Result<(), Refusal> {
        self.recover()?;
        while self
            .writes
            .worker
            .as_ref()
            .is_some_and(|worker| wait || worker.is_finished())
        {
            let result = self
                .writes
                .worker
                .take()
                .ok_or(Refusal::Failed)?
                .join()
                .unwrap_or(JobResult::Finished(Err(Error::Interrupted)));
            let (device, sequence) = self.writes.operation.ok_or(Refusal::Failed)?;
            match result {
                JobResult::Prepared(Ok(prepared)) => {
                    self.start_prepared(device, sequence, prepared)?
                }
                JobResult::Prepared(Err(error)) => {
                    self.settle_worktree(device, sequence, Err(error))?
                }
                JobResult::Finished(result) => self.settle_worktree(device, sequence, result)?,
            }
            if !wait {
                break;
            }
        }
        if self.writes.worker.is_none() {
            for (device, record) in self.state.pending_worktrees() {
                // Only the inherited lock fences recovery; unavailable Git cannot strand IPC.
                let _lock = self.worktree_lock()?;
                self.settle_worktree(device, record.sequence, Err(Error::Interrupted))?;
            }
        }
        // A revoked device's record waits for its Worktree outcome, however that settled.
        for device in std::mem::take(&mut self.forgetting) {
            self.forget(device);
        }
        Ok(())
    }
    fn settle_worktree(
        &mut self,
        device: Id,
        sequence: u64,
        result: Result<Option<PathBuf>, Error>,
    ) -> Result<(), Refusal> {
        let operation = self
            .state
            .outcome(device, sequence)
            .ok_or(Refusal::Missing)?
            .mutation
            .clone();
        let mut next = self.state.clone();
        let outcome = match result {
            Err(error) => Outcome::WorktreeFailed(error),
            Ok(path) => {
                if let Mutation::Worktree(Operation::Create {
                    workspace,
                    root,
                    control,
                    ..
                }) = operation
                {
                    let creator = control.creator(device, &self.inventory());
                    match path.and_then(|path| {
                        next.add_worktree_tab(creator, workspace, root, path.clone(), path)
                            .ok()
                    }) {
                        Some(target) => Outcome::Accepted { target },
                        None => Outcome::WorktreeFailed(Error::Interrupted),
                    }
                } else {
                    Outcome::Accepted { target: device }
                }
            }
        };
        next.finish_worktree(device, sequence, outcome)?;
        self.commit_retained(next)?;
        self.writes.operation = None;
        if let Some(record) = self.state.outcome(device, sequence).cloned() {
            self.reveal(&record);
        }
        self.repositories.refresh();
        Ok(())
    }
}

fn removal_error(error: WorktreeRemoveError) -> Error {
    match error {
        WorktreeRemoveError::Replaced | WorktreeRemoveError::Changed => Error::Changed,
        _ => Error::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::server::{
        repository_tests::{GitFixture, finish_write, git_facts, review_removal},
        tests::{create_shared, host, screen, shared_inventory, size},
    };

    mod close_fallback {
        include!("worktrees/close_fallback_tests.rs");
    }

    #[test]
    fn created_worktree_tabs_apply_creation_control_and_reveal_to_the_origin() {
        use crate::runtime::server::tests::{effects, focus};
        for (name, policy, expected) in [
            ("origin", 0, Some([1; 32])),
            ("unclaimed", 1, None),
            ("creator", 2, Some([3; 32])),
        ] {
            let git = GitFixture::new();
            let mut prototype = host();
            prototype.directory = git.0.clone();
            let mut host = SharedWorkspaceHost::new(prototype);
            let workspace = create_shared(&mut host, 1);
            let initial = shared_inventory(&mut host);
            let origin = initial.workspaces[0].tabs[0].root_pane;
            let Reply::Attached { lease } = host.request(
                [1; 32],
                [1; 32],
                Request::SelectPane {
                    pane: origin,
                    explicit: true,
                    size: size(),
                    style: None,
                },
            ) else {
                panic!("the desktop starts the origin Pane")
            };
            focus(&mut host, lease, 1, FocusTarget::Terminal);
            let control = match policy {
                0 => CreateControl::OriginController { pane: origin },
                1 => CreateControl::Unclaimed,
                _ => CreateControl::Creator,
            };
            assert!(matches!(
                host.request(
                    [3; 32],
                    [3; 32],
                    Request::Mutate {
                        sequence: 1,
                        mutation: Mutation::Worktree(Operation::Create {
                            workspace,
                            root: git.0.clone(),
                            path: git.0.join(name),
                            branch: Branch::New {
                                name: name.into(),
                                base: BranchName::Local("main".into())
                            },
                            control,
                        }),
                    }
                ),
                Reply::MutationPending
            ));
            host.finish_worktrees(true).unwrap();
            let record = host.state.outcome([3; 32], 1).unwrap();
            let Outcome::Accepted { target } = record.outcome else {
                panic!("Worktree creation succeeds")
            };
            let updated = shared_inventory(&mut host);
            let tabs = &updated.workspaces[0].tabs;
            assert_eq!(tabs.len(), initial.workspaces[0].tabs.len() + 1);
            let tab = tabs.iter().find(|t| t.id == target).unwrap();
            let pane = tab.panes.iter().find(|p| p.id == tab.root_pane).unwrap();
            assert_eq!(pane.creator, expected);
            let reveals: Vec<_> = effects(&mut host, lease, usize::from(policy == 0))
                .into_iter()
                .filter_map(|request| match request.effect {
                    pane::effects::Effect::Reveal(reveal) => Some(reveal),
                    _ => None,
                })
                .collect();
            if policy == 0 {
                assert_eq!(reveals.len(), 1);
                assert_eq!((reveals[0].tab, reveals[0].pane), (tab.id, pane.id));
            } else {
                assert!(reveals.is_empty());
            }
            let selected = host.request(
                [1; 32],
                [1; 32],
                Request::SelectPane {
                    pane: pane.id,
                    explicit: true,
                    size: size(),
                    style: None,
                },
            );
            if policy == 2 {
                assert!(matches!(selected, Reply::Takeover { .. }));
            } else {
                assert!(matches!(selected, Reply::Attached { .. }));
            }
            host.shutdown().unwrap();
        }
    }

    /// Revokes the device while `operation` is in progress, and checks that its record is
    /// forgotten once the operation settles.
    fn revoke_during(git: &GitFixture, operation: impl FnOnce(Id) -> Operation) {
        let mut prototype = host();
        prototype.directory = git.0.clone();
        let mut host = SharedWorkspaceHost::new(prototype);
        let workspace = create_shared(&mut host, 1);
        assert!(matches!(
            host.request(
                [1; 32],
                [1; 32],
                Request::Mutate {
                    sequence: 2,
                    mutation: Mutation::Worktree(operation(workspace)),
                }
            ),
            Reply::MutationPending
        ));
        host.revoked([1; 32]);
        assert!(
            host.state.outcome([1; 32], 2).is_some(),
            "the running operation keeps its record"
        );
        host.finish_worktrees(true).unwrap();
        assert!(host.state.outcome([1; 32], 1).is_none());
        assert!(host.state.outcome([1; 32], 2).is_none());
        assert_eq!(host.state.next_sequence([1; 32]), 1);
        assert!(host.forgetting.is_empty());
        host.shutdown().unwrap();
    }

    #[test]
    fn a_device_revoked_during_its_worktree_write_is_forgotten_once_the_write_settles() {
        use spaceterm_server::worktree::{Branch, BranchName};
        let git = GitFixture::new();
        revoke_during(&git, |workspace| Operation::Create {
            control: spaceterm_server::hierarchy::CreateControl::Creator,
            workspace,
            root: git.0.clone(),
            path: git.0.join("created"),
            branch: Branch::New {
                name: "created".into(),
                base: BranchName::Local("main".into()),
            },
        });
    }

    #[test]
    fn a_device_revoked_while_opening_a_worktree_tab_is_forgotten_once_it_opens() {
        let git = GitFixture::new();
        let tree = git.0.join("linked");
        git.git(&["worktree", "add", "-b", "feature", tree.to_str().unwrap()]);
        revoke_during(&git, |workspace| Operation::OpenTab {
            workspace,
            source: None,
            root: git.0.clone(),
            path: tree.clone(),
            control: spaceterm_server::hierarchy::CreateControl::Creator,
        });
    }

    #[test]
    fn splitting_a_worktree_pane_keeps_its_scope_when_the_pin_or_cwd_is_outside() {
        use spaceterm_server::hierarchy::Axis;
        let git = GitFixture::new();
        let tree = git.0.join("linked");
        git.git(&["worktree", "add", "-b", "feature", tree.to_str().unwrap()]);
        let mut prototype = host();
        prototype.directory = git.0.clone();
        let mut host = SharedWorkspaceHost::new(prototype);
        let workspace = create_shared(&mut host, 1);
        assert!(
            matches!(host.request([1; 32], [1; 32], Request::Mutate { sequence: 2, mutation: Mutation::PinWorkspace { workspace, version: 0, directory: Some(git.0.clone()) } }), Reply::Outcome(record) if matches!(record.outcome, Outcome::Accepted { .. }))
        );
        let Outcome::Accepted { target: tab } = finish_write(
            &mut host,
            3,
            Operation::OpenTab {
                workspace,
                root: git.0.clone(),
                path: tree.clone(),
                source: None,
                control: spaceterm_server::hierarchy::CreateControl::Creator,
            },
        ) else {
            panic!("Worktree Tab");
        };
        let source = host.state.inventory.workspaces[0]
            .tabs
            .iter()
            .find(|t| t.id == tab)
            .unwrap()
            .root_pane;
        host.state.confirm_directory(source, git.0.clone());
        let Reply::Outcome(record) = host.request(
            [1; 32],
            [1; 32],
            Request::Mutate {
                sequence: 4,
                mutation: Mutation::SplitPane {
                    workspace,
                    tab,
                    source,
                    axis: Axis::Right,
                    control: spaceterm_server::hierarchy::CreateControl::Creator,
                },
            },
        ) else {
            panic!("split");
        };
        let Outcome::Accepted { target: pane } = record.outcome else {
            panic!("accepted split");
        };
        assert_eq!(host.state.pane(pane).unwrap().2.starting_directory, tree);
        assert!(
            host.panes.is_empty(),
            "membership creates no implicit shell"
        );
        let Reply::Attached { lease } = host.request(
            [1; 32],
            [2; 32],
            Request::SelectPane {
                pane,
                explicit: true,
                size: PaneSize {
                    columns: 240,
                    ..size()
                },
                style: None,
            },
        ) else {
            panic!("explicit split shell");
        };
        crate::runtime::server::tests::command(
            &mut host,
            2,
            lease,
            "printf '\\nSPLIT-CWD:%s\\n' \"$PWD\"\n",
        );
        crate::runtime::server::tests::await_text(
            &mut host,
            lease,
            &format!("SPLIT-CWD:{}", tree.display()),
        );
        let pin = git.0.join("lost-pin");
        std::fs::create_dir(&pin).unwrap();
        assert!(matches!(host.request([1; 32], [1; 32], Request::Mutate {
            sequence: 5, mutation: Mutation::PinWorkspace { workspace, version: 1, directory: Some(pin.clone()) },
        }), Reply::Outcome(record) if matches!(record.outcome, Outcome::Accepted { .. })));
        std::fs::remove_dir(&pin).unwrap();
        let mutation = Mutation::SplitPane {
            workspace,
            tab,
            source,
            axis: Axis::Down,
            control: spaceterm_server::hierarchy::CreateControl::Creator,
        };
        assert!(
            matches!(host.request([1; 32], [1; 32], Request::Mutate { sequence: 6, mutation: mutation.clone() }), Reply::Outcome(record) if record.outcome == Outcome::Invalid)
        );
        std::fs::create_dir(&pin).unwrap();
        assert!(
            matches!(host.request([1; 32], [1; 32], Request::Mutate { sequence: 6, mutation: mutation.clone() }), Reply::Outcome(record) if record.outcome == Outcome::Invalid),
            "repair cannot change the original outcome"
        );
        assert!(
            matches!(host.request([1; 32], [1; 32], Request::Mutate { sequence: 7, mutation }), Reply::Outcome(record) if matches!(record.outcome, Outcome::Accepted { .. })),
            "a fresh edit can proceed"
        );
        host.shutdown().unwrap();
    }

    #[test]
    fn confirmed_worktree_close_ends_its_shell_and_preserves_git_and_the_outside_tab() {
        use crate::runtime::server::tests::{await_text, command};
        use spaceterm_server::hierarchy::CloseTarget;
        let git = GitFixture::new();
        let tree = git.0.join("linked");
        git.git(&["worktree", "add", "-b", "feature", tree.to_str().unwrap()]);
        std::fs::write(tree.join("kept"), b"kept").unwrap();
        let mut prototype = host();
        prototype.directory = git.0.clone();
        let mut host = SharedWorkspaceHost::new(prototype);
        let workspace = create_shared(&mut host, 1);
        let outside = host.state.inventory.workspaces[0].root_tab;
        let Outcome::Accepted { target: tab } = finish_write(
            &mut host,
            2,
            Operation::OpenTab {
                workspace,
                root: git.0.clone(),
                path: tree.clone(),
                source: None,
                control: spaceterm_server::hierarchy::CreateControl::Creator,
            },
        ) else {
            panic!("open Worktree");
        };
        assert!(host.panes.is_empty(), "creating membership starts no shell");
        let pane = host.state.inventory.workspaces[0]
            .tabs
            .iter()
            .find(|t| t.id == tab)
            .unwrap()
            .root_pane;
        let Reply::Attached { lease } = host.request(
            [1; 32],
            [2; 32],
            Request::SelectPane {
                pane,
                explicit: true,
                size: PaneSize {
                    columns: 240,
                    ..size()
                },
                style: None,
            },
        ) else {
            panic!("explicit shell");
        };
        command(
            &mut host,
            2,
            lease,
            "printf '\\nSOURCE-CWD:%s\\n' \"$PWD\"\n",
        );
        await_text(&mut host, lease, &format!("SOURCE-CWD:{}", tree.display()));
        let target = CloseTarget::Worktree { workspace, tab };
        let confirmation = shared_inventory(&mut host)
            .close_confirmation(target)
            .unwrap();
        assert!(confirmation.panes.iter().any(|pane| pane.session.is_some()));
        assert!(
            matches!(host.request([3; 32], [3; 32], Request::Mutate { sequence: 1, mutation: Mutation::Close { target, confirmation } }), Reply::Outcome(record) if record.outcome == Outcome::Accepted { target: workspace })
        );
        host.tick(Instant::now());
        assert_eq!(
            host.state.inventory.workspaces[0]
                .tabs
                .iter()
                .map(|tab| tab.id)
                .collect::<Vec<_>>(),
            [outside]
        );
        assert_eq!(std::fs::read(tree.join("kept")).unwrap(), b"kept");
        git.git(&["show-ref", "--verify", "refs/heads/feature"]);
        git.git(&["worktree", "list", "--porcelain"]);
        assert!(
            host.panes
                .get(&pane)
                .is_none_or(|host| host.pane.as_ref().is_none_or(|pane| pane.ended))
        );
        host.shutdown().unwrap();
    }

    #[test]
    fn worktree_tabs_use_the_source_directory_and_contained_pin_before_the_root() {
        let git = GitFixture::new();
        let tree = git.0.join("linked");
        git.git(&["worktree", "add", "-b", "feature", tree.to_str().unwrap()]);
        let cwd = tree.join("src");
        let pin = tree.join("pinned");
        std::fs::create_dir(&cwd).unwrap();
        std::fs::create_dir(&pin).unwrap();
        let mut prototype = host();
        prototype.directory = git.0.clone();
        let mut host = SharedWorkspaceHost::new(prototype);
        let workspace = create_shared(&mut host, 1);
        let source = host.state.inventory.workspaces[0].tabs[0].root_pane;
        host.state.confirm_directory(source, cwd.clone());
        for (index, (pinned, expected)) in [
            (None, cwd.clone()),
            (Some(pin.clone()), pin),
            (Some(git.0.clone()), cwd),
        ]
        .into_iter()
        .enumerate()
        {
            let pin_sequence = 2 + index as u64 * 2;
            assert!(matches!(host.request([1; 32], [1; 32], Request::Mutate {
                sequence: pin_sequence,
                mutation: Mutation::PinWorkspace { workspace, version: index as u64, directory: pinned },
            }), Reply::Outcome(record) if matches!(record.outcome, Outcome::Accepted { .. })));
            let Outcome::Accepted { target } = finish_write(
                &mut host,
                pin_sequence + 1,
                Operation::OpenTab {
                    workspace,
                    root: git.0.clone(),
                    path: tree.clone(),
                    source: Some(source),
                    control: spaceterm_server::hierarchy::CreateControl::Creator,
                },
            ) else {
                panic!("open Worktree Tab");
            };
            let workspace = &host.state.inventory.workspaces[0];
            let tab = workspace.tabs.iter().find(|tab| tab.id == target).unwrap();
            assert_eq!(
                (tab.worktree.as_ref(), &tab.panes[0].starting_directory),
                (Some(&tree), &expected)
            );
            if index == 0 {
                let pane = tab.root_pane;
                assert!(host.panes.is_empty(), "New Tab starts no implicit shell");
                let Reply::Attached { lease } = host.request(
                    [1; 32],
                    [2; 32],
                    Request::SelectPane {
                        pane,
                        explicit: true,
                        size: PaneSize {
                            columns: 240,
                            ..size()
                        },
                        style: None,
                    },
                ) else {
                    panic!("explicit new Tab shell");
                };
                crate::runtime::server::tests::command(
                    &mut host,
                    2,
                    lease,
                    "printf '\\nTAB-CWD:%s\\n' \"$PWD\"\n",
                );
                crate::runtime::server::tests::await_text(
                    &mut host,
                    lease,
                    &format!("TAB-CWD:{}", expected.display()),
                );
            }
        }
        host.shutdown().unwrap();
    }

    struct FilterGate(PathBuf);
    impl Drop for FilterGate {
        fn drop(&mut self) {
            let _ = std::fs::write(self.0.join("release"), "");
        }
    }
    fn blocked_removal_fixture() -> (GitFixture, PathBuf, FilterGate) {
        let git = GitFixture::new();
        std::fs::write(git.0.join("payload"), "original\n").unwrap();
        std::fs::write(git.0.join(".gitattributes"), "payload filter=wait552\n").unwrap();
        git.git(&["add", "payload", ".gitattributes"]);
        git.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-m",
            "payload",
        ]);
        let tree = git.0.join("linked");
        git.git(&["worktree", "add", "-b", "linked", tree.to_str().unwrap()]);
        let gate = git.0.join("filter-gate");
        std::fs::create_dir(&gate).unwrap();
        let script = gate.join("clean.sh");
        std::fs::write(&script, "#!/bin/sh\n: > \"$1/started\"\ni=0\nwhile [ ! -e \"$1/release\" ]; do\n i=$((i + 1))\n [ \"$i\" -lt 1000 ] || exit 1\n sleep 0.02\ndone\ncat >/dev/null\nprintf 'original\\n'\n").unwrap();
        let quote = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
        git.git(&[
            "config",
            "filter.wait552.clean",
            &format!("sh {} {}", quote(&script), quote(&gate)),
        ]);
        std::fs::write(tree.join("payload"), "modified\n").unwrap();
        (git, tree, FilterGate(gate))
    }

    #[test]
    fn removal_preflight_keeps_requests_live_and_revalidates_a_session_started_during_git() {
        let (git, tree, gate) = blocked_removal_fixture();
        std::fs::write(gate.0.join("release"), "").unwrap();
        let mut prototype = host();
        prototype.directory = tree.clone();
        let mut host = SharedWorkspaceHost::new(prototype);
        create_shared(&mut host, 1);
        let review = review_removal(&mut host, &git.0, &tree);
        assert!(!review.changes);
        std::fs::write(tree.join("payload"), "changed!\n").unwrap();
        let clean = std::process::Command::new("git")
            .arg("-C")
            .arg(&tree)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(
            clean.status.success() && clean.stdout.is_empty(),
            "fixture is not clean: {:?}",
            clean.stdout
        );
        std::fs::write(tree.join("payload"), "second!!\n").unwrap();
        std::fs::remove_file(gate.0.join("release")).unwrap();
        std::fs::remove_file(gate.0.join("started")).unwrap();
        let operation = Operation::Remove {
            root: git.0.clone(),
            path: tree.clone(),
            boot: review.boot,
            confirmation: review.confirmation,
        };
        let started = Instant::now();
        let reply = host.request(
            [1; 32],
            [1; 32],
            Request::Mutate {
                sequence: 2,
                mutation: Mutation::Worktree(operation.clone()),
            },
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "write preflight held the terminal request lock"
        );
        assert!(
            matches!(reply, Reply::MutationPending),
            "unexpected preflight reply: {reply:?}"
        );
        // The filter holds Git for at least 20 seconds, so this waits only for Git to start.
        let until = Instant::now() + Duration::from_secs(12);
        while !gate.0.join("started").exists() {
            assert!(
                matches!(
                    host.request([1; 32], [1; 32], Request::Resolve { sequence: 2 }),
                    Reply::MutationPending
                ),
                "preflight finished before the filter"
            );
            assert!(
                Instant::now() < until,
                "preflight did not reach the blocking filter"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let pane = host.state.inventory.workspaces[0].tabs[0].root_pane;
        let Reply::Attached { lease } = host.request(
            [1; 32],
            [2; 32],
            Request::SelectPane {
                pane,
                explicit: true,
                size: size(),
                style: None,
            },
        ) else {
            panic!("start reviewed Session during preflight");
        };
        let other = Mutation::CreateWorkspace {
            name: "Other client".into(),
            directory: None,
            control: spaceterm_server::hierarchy::CreateControl::Creator,
        };
        assert!(matches!(
            host.request(
                [3; 32],
                [3; 32],
                Request::Mutate {
                    sequence: 1,
                    mutation: other.clone()
                }
            ),
            Reply::MutationPending
        ));
        drop(gate);
        assert_eq!(
            finish_write(&mut host, 2, operation),
            Outcome::WorktreeFailed(Error::Changed)
        );
        assert!(tree.exists());
        assert!(screen(&mut host, [1; 32], lease).is_ok());
        assert!(
            matches!(host.request([3; 32], [3; 32], Request::Mutate { sequence: 1, mutation: other }), Reply::Outcome(record) if matches!(record.outcome, Outcome::Accepted { .. }))
        );
        host.shutdown().unwrap();
    }

    #[test]
    fn a_blocked_worktree_review_does_not_block_other_terminal_work() {
        use crate::runtime::server::tests::{await_text, command};
        let (git, tree, gate) = blocked_removal_fixture();
        let mut prototype = host();
        prototype.directory = git.0.parent().unwrap().to_owned();
        let mut host = SharedWorkspaceHost::new(prototype);
        create_shared(&mut host, 1);
        let pane = host.state.inventory.workspaces[0].tabs[0].root_pane;
        let Reply::Attached { lease } = host.request(
            [1; 32],
            [2; 32],
            Request::SelectPane {
                pane,
                explicit: true,
                size: size(),
                style: None,
            },
        ) else {
            panic!("start unrelated Terminal Session");
        };
        let started = Instant::now();
        let id = RequestId(1);
        let reply = host.read(
            [1; 32],
            [1; 32],
            id,
            crate::runtime::server::HostRead::Worktree(Read::CheckRemoval {
                root: git.0.clone(),
                path: tree,
            }),
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a Git read held the terminal request lock"
        );
        assert!(reply.is_none(), "read must be asynchronous");
        // Another connection's cancellation never reaches this read.
        host.cancel_read([3; 32], id);
        // The filter holds Git for at least 20 seconds, so this waits only for Git to start.
        let until = Instant::now() + Duration::from_secs(12);
        while !gate.0.join("started").exists() {
            assert!(
                Instant::now() < until,
                "Git did not reach the blocking filter"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(
            host.request([1; 32], [2; 32], Request::Inventory),
            Reply::Inventory { .. }
        ));
        command(
            &mut host,
            2,
            lease,
            "printf '\\nother-terminal-responsive\\n'\n",
        );
        await_text(&mut host, lease, "other-terminal-responsive");
        drop(gate);
        assert!(matches!(
            crate::runtime::server::tests::await_read(&mut host, [1; 32], id),
            ReplyBody::Worktree(Ok(ReadReply::Removal(_)))
        ));
        assert!(host.settled_reads().is_empty());
        host.shutdown().unwrap();
    }

    #[test]
    fn a_worktree_write_never_publishes_main_membership_for_a_nested_worktree() {
        use spaceterm_server::worktree::{Branch, BranchName, Operation};
        let git = GitFixture::new();
        let nested = git.0.join("nested");
        git.git(&["worktree", "add", "-b", "nested", nested.to_str().unwrap()]);
        let mut prototype = host();
        prototype.directory = nested.clone();
        let mut host = SharedWorkspaceHost::new(prototype);
        let workspace = create_shared(&mut host, 1);
        let initial = git_facts(&mut host, &nested, |facts| facts.worktrees.is_some());
        let root_tab = initial.workspaces[0].root_tab;
        assert!(matches!(
            finish_write(
                &mut host,
                2,
                Operation::Create {
                    control: spaceterm_server::hierarchy::CreateControl::Creator,
                    workspace,
                    root: git.0.clone(),
                    path: git.0.join("other"),
                    branch: Branch::New {
                        name: "other".into(),
                        base: BranchName::Local("main".into())
                    },
                }
            ),
            spaceterm_server::hierarchy::Outcome::Accepted { .. }
        ));
        let tab = host.state.inventory.workspaces[0]
            .tabs
            .iter()
            .find(|t| t.id == root_tab)
            .unwrap();
        assert_eq!(tab.worktree.as_ref(), Some(&nested));
        host.shutdown().unwrap();
    }

    #[test]
    fn interrupted_worktree_recovery_keeps_the_server_usable_when_the_repository_is_gone() {
        let git = GitFixture::new();
        let mut prototype = host();
        prototype.directory = git.0.clone();
        let mut original = SharedWorkspaceHost::new(prototype);
        let workspace = create_shared(&mut original, 1);
        let operation = Operation::Create {
            control: spaceterm_server::hierarchy::CreateControl::Creator,
            workspace,
            root: git.0.clone(),
            path: git.0.join("interrupted"),
            branch: Branch::New {
                name: "interrupted".into(),
                base: BranchName::Local("main".into()),
            },
        };
        let mut pending = original.state.clone();
        pending
            .apply(
                [1; 32],
                2,
                Mutation::Worktree(operation.clone()),
                &git.0,
                |_| false,
            )
            .unwrap();
        let bytes = serde_json::to_vec(&pending.envelope()).unwrap();
        original.shutdown().unwrap();
        std::fs::remove_dir_all(&git.0).unwrap();

        let mut prototype = host();
        prototype.directory = git.0.clone();
        let mut restored = SharedWorkspaceHost::new(prototype);
        restored.state = *serde_json::from_slice::<spaceterm_server::organization::Organization>(&bytes)
        .unwrap()
        .shared;
        restored.state.begin_boot(restored.prototype.boot);
        assert!(matches!(
            restored.request([1; 32], [1; 32], Request::Inventory),
            Reply::Inventory { .. }
        ));
        assert!(
            matches!(restored.request([1; 32], [1; 32], Request::Mutate {
            sequence: 2, mutation: Mutation::Worktree(operation),
        }), Reply::Outcome(record) if record.outcome == Outcome::WorktreeFailed(Error::Interrupted))
        );
        assert_eq!(shared_inventory(&mut restored).workspaces.len(), 1);
        restored.shutdown().unwrap();
        assert!(!git.0.exists());
    }

    #[test]
    fn removal_ignores_a_missing_pinned_directory_in_an_unrelated_workspace() {
        let git = GitFixture::new();
        let tree = git.0.join("linked");
        git.git(&["worktree", "add", "-b", "linked", tree.to_str().unwrap()]);
        let unrelated = git.0.join("unrelated");
        std::fs::create_dir(&unrelated).unwrap();
        let mut prototype = host();
        prototype.directory = git.0.clone();
        let mut host = SharedWorkspaceHost::new(prototype);
        create_shared(&mut host, 1);
        let workspace = create_shared(&mut host, 2);
        assert!(matches!(
            host.request(
                [1; 32],
                [1; 32],
                Request::Mutate {
                    sequence: 3,
                    mutation: Mutation::PinWorkspace {
                        workspace,
                        version: 0,
                        directory: Some(unrelated.clone())
                    },
                }
            ),
            Reply::Outcome(_)
        ));
        std::fs::remove_dir(&unrelated).unwrap();

        let review = review_removal(&mut host, &git.0, &tree);
        assert!(matches!(
            finish_write(
                &mut host,
                4,
                Operation::Remove {
                    root: git.0.clone(),
                    path: tree.clone(),
                    boot: review.boot,
                    confirmation: review.confirmation,
                }
            ),
            Outcome::Accepted { .. }
        ));
        assert!(!tree.exists());
        host.shutdown().unwrap();
    }

    #[test]
    fn removal_captures_symlinked_pane_directories_in_other_unattached_workspaces() {
        let git = GitFixture::new();
        let tree = git.0.join("linked");
        git.git(&["worktree", "add", "-b", "linked", tree.to_str().unwrap()]);
        let alias = git.0.join("alias");
        std::os::unix::fs::symlink(&tree, &alias).unwrap();
        let mut prototype = host();
        prototype.directory = git.0.clone();
        let mut host = SharedWorkspaceHost::new(prototype);
        create_shared(&mut host, 1);
        create_shared(&mut host, 2);
        let pane = host.state.inventory.workspaces[1].tabs[0].root_pane;
        // Shell integration retains logical $PWD while the Tab keeps its original membership.
        host.state.inventory.workspaces[1].tabs[0].worktree = Some(git.0.clone());
        host.state.inventory.workspaces[1].tabs[0].panes[0].confirmed_directory = Some(alias);
        let review = review_removal(&mut host, &git.0, &tree);
        assert_eq!(
            review.tabs.len(),
            1,
            "review includes the physically contained Pane"
        );
        assert_eq!(
            review.tabs[0].tab,
            host.state.inventory.workspaces[1].tabs[0].id
        );
        assert!(review.matches_inventory(&host.inventory()));
        let Reply::Attached { .. } = host.request(
            [1; 32],
            [3; 32],
            Request::SelectPane {
                pane,
                explicit: true,
                size: size(),
                style: None,
            },
        ) else {
            panic!("start affected session")
        };
        assert!(!review.matches_inventory(&host.inventory()));
        assert_eq!(
            finish_write(
                &mut host,
                3,
                Operation::Remove {
                    root: git.0.clone(),
                    path: tree.clone(),
                    boot: review.boot,
                    confirmation: review.confirmation
                }
            ),
            Outcome::WorktreeFailed(Error::Changed)
        );
        assert!(tree.exists());
        let review = review_removal(&mut host, &git.0, &tree);
        assert!(review.tabs[0].panes[0].session.is_some());
        assert!(matches!(
            finish_write(
                &mut host,
                4,
                Operation::Remove {
                    root: git.0.clone(),
                    path: tree.clone(),
                    boot: review.boot,
                    confirmation: review.confirmation
                }
            ),
            Outcome::Accepted { .. }
        ));
        assert!(!tree.exists());
        assert!(
            shared_inventory(&mut host)
                .close
                .unwrap()
                .sessions
                .iter()
                .all(|p| p.session.is_none())
        );
        host.shutdown().unwrap();
    }
}
