use super::super::repository_tests::GitFixture;
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// How long a test waits between looks at a reader it drives.
const PAUSE: Duration = Duration::from_millis(10);
/// How long a test waits before it calls a read stuck, as the shared-host tests do.
const STUCK: Duration = Duration::from_secs(12);
/// How long a held process waits for its release. It is far past `STUCK`, so a read that waits
/// for a held one fails a test instead of finishing when the hold gives up.
const HELD: Duration = Duration::from_secs(60);

/// Delay only a full scan at the process boundary; discovery, porcelain parsing, Worktree
/// listing, physical directory validation, and publication all use the production readers.
struct DelayedCount {
    root: PathBuf,
    also_delayed: Sources,
    released: AtomicBool,
    cancelled: AtomicBool,
    calls: Mutex<BTreeMap<PathBuf, usize>>,
    started: Mutex<Option<Instant>>,
}
impl RepositoryProgramRunner for DelayedCount {
    fn run(
        &self,
        request: &ProgramRequest,
        stdout: &mut dyn FnMut(&[u8]),
        cancellation: &CancellationToken,
    ) -> Result<ProgramExit, ProgramError> {
        let arguments = &request.arguments;
        let count_arguments: Vec<std::ffi::OsString> = policy::local_read::COUNT_ARGUMENTS
            .iter()
            .map(Into::into)
            .collect();
        if arguments.ends_with(&count_arguments) {
            *self
                .calls
                .lock()
                .unwrap()
                .entry(request.directory.clone())
                .or_default() += 1;
            if request.directory == self.root || self.also_delayed.contains(&request.directory) {
                *self.started.lock().unwrap() = Some(Instant::now());
                let until = Instant::now() + HELD;
                while !self.released.load(Ordering::Acquire) {
                    if cancellation.is_cancelled() {
                        self.cancelled.store(true, Ordering::Release);
                        return Err(ProgramError::Cancelled);
                    }
                    if Instant::now() >= until {
                        return Err(ProgramError::TimedOut);
                    }
                    std::thread::sleep(PAUSE);
                }
            }
        }
        DeadlineRunner.run(request, stdout, cancellation)
    }
}
struct Reading {
    repositories: ServerRepositories,
    state: State,
}
/// A watch set that cannot start, so only explicit rereads read a directory again.
struct Unwatchable;
impl WatchSets for Unwatchable {
    fn start(
        &self,
        _: policy::ChangedPaths,
    ) -> Result<Box<dyn WatchSet>, policy::RepositoryWatchError> {
        Err(policy::RepositoryWatchError)
    }
}
impl Reading {
    fn start(sources: Sources, runner: Arc<dyn RepositoryProgramRunner>, home: PathBuf) -> Self {
        let watcher = crate::platform::server_watch_sets();
        Self::watched(sources, runner, home, watcher)
    }
    fn watched(
        sources: Sources,
        runner: Arc<dyn RepositoryProgramRunner>,
        home: PathBuf,
        watcher: Arc<dyn WatchSets>,
    ) -> Self {
        let git = std::process::Command::new("/usr/bin/which")
            .arg("git")
            .output()
            .unwrap();
        assert!(git.status.success());
        let git = PathBuf::from(String::from_utf8(git.stdout).unwrap().trim());
        let version = ToolVersion {
            major: 2,
            minor: 40,
            patch: 0,
        };
        let readers = (
            policy::local_read::LocalRepositoryReader::new(
                runner.clone(),
                Arc::new(
                    crate::platform::repository_marker_reader::UnixRepositoryMarkerReader::new(),
                ),
                git.clone(),
                home.clone(),
                version,
            ),
            crate::worktrees::git::LocalWorktreeGit::new(runner, git, home, version),
        );
        let mut state = State::empty();
        for directory in &sources {
            let mut single = State::empty();
            single.ensure_first_workspace([1; 32], directory).unwrap();
            state
                .inventory
                .workspaces
                .extend(single.inventory.workspaces);
        }
        let mut repositories = ServerRepositories::new([1; 32]);
        repositories.sources = sources.clone();
        let cancellation = CancellationToken::default();
        let sent_sources = sources.clone();
        let (inputs, receiver) = mpsc::channel();
        let (sender, results) = mpsc::sync_channel(RESULTS);
        let reader = Reader {
            inputs: (inputs.clone(), receiver),
            cancellation: cancellation.clone(),
            sender,
            ring: repositories.ring.clone(),
            watcher,
        };
        let worker = std::thread::spawn(move || {
            read_sources(
                sources,
                Some(readers),
                GitToolStatus::Ready(version),
                reader,
            );
        });
        repositories.worker = Some(Worker {
            cancellation,
            handle: worker,
            results,
            inputs,
            sent_sources,
        });
        Self {
            repositories,
            state,
        }
    }
    fn poll(&mut self) -> Snapshot {
        self.repositories.update(&mut self.state);
        // Exercise the same wire representation the desktop receives during a partial read.
        serde_json::from_slice(&serde_json::to_vec(&self.repositories.snapshot()).unwrap()).unwrap()
    }
    fn finish(&mut self) -> Snapshot {
        let until = Instant::now() + STUCK;
        loop {
            let snapshot = self.poll();
            if snapshot.directories.len() == self.repositories.sources.len()
                && snapshot.directories.iter().all(|facts| {
                    facts.failure.is_none()
                        && facts.repository.as_ref().is_none_or(|repository| {
                            repository.counting.is_none() && facts.worktrees.is_some()
                        })
                })
            {
                return snapshot;
            }
            assert!(
                Instant::now() < until,
                "current reads must finish without stopping recurring discovery"
            );
            std::thread::sleep(PAUSE);
        }
    }
}

#[test]
fn a_directory_deleted_during_discovery_keeps_saved_worktree_membership() {
    struct DeleteDuringDiscovery {
        directory: PathBuf,
        deleted: AtomicBool,
    }
    impl RepositoryProgramRunner for DeleteDuringDiscovery {
        fn run(
            &self,
            request: &ProgramRequest,
            stdout: &mut dyn FnMut(&[u8]),
            cancellation: &CancellationToken,
        ) -> Result<ProgramExit, ProgramError> {
            let discovery: Vec<std::ffi::OsString> = policy::local_read::DISCOVERY_ARGUMENTS
                .iter()
                .map(Into::into)
                .collect();
            if request.directory == self.directory
                && request.arguments.ends_with(&discovery)
                && !self.deleted.swap(true, Ordering::AcqRel)
            {
                // The native runner validates and enters the disposable directory first.
                // Delete it in that child before executing the real Git discovery command.
                let mut request = request.clone();
                let mut arguments = vec![
                    "-c".into(),
                    "/bin/rm -rf -- \"$1\"; shift; exec \"$@\"".into(),
                    "delete-during-discovery".into(),
                    self.directory.as_os_str().to_owned(),
                    request.executable.clone().into_os_string(),
                ];
                arguments.extend(request.arguments);
                request.arguments = arguments;
                request.executable = "/bin/sh".into();
                return DeadlineRunner.run(&request, stdout, cancellation);
            }
            DeadlineRunner.run(request, stdout, cancellation)
        }
    }

    let git = GitFixture::new();
    let tree = git.0.join("linked");
    git.git(&["worktree", "add", "-b", "feature", tree.to_str().unwrap()]);
    let runner = Arc::new(DeleteDuringDiscovery {
        directory: tree.clone(),
        deleted: AtomicBool::new(false),
    });
    let mut reading = Reading::start([tree.clone()].into(), runner, git.0.join("unused-home"));
    let tab = &mut reading.state.inventory.workspaces[0].tabs[0];
    tab.worktree = Some(tree.clone());
    tab.worktree_repository = Some(git.0.clone());
    let until = Instant::now() + STUCK;
    let facts = loop {
        let snapshot = reading.poll();
        if let Some(facts) = snapshot
            .directories
            .iter()
            .find(|facts| facts.directory == tree)
        {
            break facts.clone();
        }
        assert!(
            Instant::now() < until,
            "deleted-directory read did not finish"
        );
        std::thread::sleep(PAUSE);
    };
    assert!(
        !tree.exists(),
        "the real child must delete the fixture directory"
    );
    assert_eq!(
        reading.state.inventory.workspaces[0].tabs[0]
            .worktree
            .as_ref(),
        Some(&tree),
        "failed discovery cannot erase saved membership"
    );
    assert_eq!(facts.failure, Some(RepositoryReadError::Unavailable));
    let restored = State::from_organization(&reading.state.envelope(&git.0).unwrap());
    assert_eq!(
        restored.inventory.workspaces[0].tabs[0].worktree.as_ref(),
        Some(&tree)
    );
}

/// Polls until `predicate` holds, and fails with `what` once the read looks stuck.
fn wait(reading: &mut Reading, what: &str, predicate: impl Fn(&Snapshot) -> bool) {
    let until = Instant::now() + STUCK;
    while !predicate(&reading.poll()) {
        assert!(Instant::now() < until, "{what}");
        std::thread::sleep(PAUSE);
    }
}

/// A discovery run, as the readers issue it.
fn is_discovery(request: &ProgramRequest) -> bool {
    let discovery: Vec<std::ffi::OsString> = policy::local_read::DISCOVERY_ARGUMENTS
        .iter()
        .map(Into::into)
        .collect();
    request.arguments.ends_with(&discovery)
}

#[test]
fn a_worktree_emptied_during_discovery_keeps_membership_and_its_repository_lists_it_missing() {
    /// Removes the Worktree's `.git` file before discovery run `at` of an armed read, as
    /// `rm -rf` does before it removes the directory. Git then finds the containing repository.
    struct EmptyDuringDiscovery {
        tree: PathBuf,
        at: usize,
        armed: AtomicBool,
        runs: AtomicUsize,
    }
    impl RepositoryProgramRunner for EmptyDuringDiscovery {
        fn run(
            &self,
            request: &ProgramRequest,
            stdout: &mut dyn FnMut(&[u8]),
            cancellation: &CancellationToken,
        ) -> Result<ProgramExit, ProgramError> {
            if request.directory == self.tree
                && is_discovery(request)
                && self.armed.load(Ordering::Acquire)
                && self.runs.fetch_add(1, Ordering::AcqRel) == self.at
            {
                std::fs::remove_file(self.tree.join(".git")).unwrap();
            }
            DeadlineRunner.run(request, stdout, cancellation)
        }
    }
    // Before the probe, so every run finds the containing repository, or between the probe
    // and the Worktree list.
    for at in [0, 1] {
        let git = GitFixture::new();
        let tree = git.0.join("linked");
        git.git(&["worktree", "add", "-b", "feature", tree.to_str().unwrap()]);
        let runner = Arc::new(EmptyDuringDiscovery {
            tree: tree.clone(),
            at,
            armed: AtomicBool::new(false),
            runs: AtomicUsize::new(0),
        });
        // Without watches, only the rereads below read a directory again.
        let mut reading = Reading::watched(
            [git.0.clone(), tree.clone()].into(),
            runner.clone(),
            git.0.join("unused-home"),
            Arc::new(Unwatchable),
        );
        wait(&mut reading, "the first reads did not finish", |snapshot| {
            snapshot.directories.len() == 2
                && snapshot
                    .directories
                    .iter()
                    .all(|facts| facts.failure.is_none() && facts.worktrees.is_some())
        });
        let membership = |reading: &Reading| {
            reading
                .state
                .inventory
                .workspaces
                .iter()
                .find(|workspace| workspace_directory(workspace) == Some(tree.as_path()))
                .unwrap()
                .tabs[0]
                .worktree
                .clone()
        };
        assert_eq!(membership(&reading), Some(tree.clone()));

        runner.armed.store(true, Ordering::Release);
        reading.repositories.reread([tree.clone()].into());
        wait(
            &mut reading,
            "the emptied Worktree's read did not fail",
            |snapshot| {
                snapshot
                    .directories
                    .iter()
                    .any(|facts| facts.directory == tree && facts.failure.is_some())
            },
        );
        assert_eq!(
            membership(&reading),
            Some(tree.clone()),
            "a read that found the containing repository moved the Tab"
        );
        assert_own_facts(&reading.poll(), &tree);

        // A focus on the removed Worktree reads its repository again, after a failed read too.
        std::fs::remove_dir_all(&tree).unwrap();
        reading.repositories.reread([tree.clone()].into());
        wait(
            &mut reading,
            "the repository did not list the removed Worktree as missing",
            |snapshot| {
                snapshot.directories.iter().any(|facts| {
                    facts.directory == git.0
                        && facts.worktrees.as_ref().is_some_and(|list| {
                            list.worktrees
                                .iter()
                                .any(|record| record.root == tree && record.missing)
                        })
                })
            },
        );
        assert_eq!(membership(&reading), Some(tree.clone()));
    }
}

/// The last facts published for `tree` are its own Worktree's, failed or not.
fn assert_own_facts(snapshot: &Snapshot, tree: &Path) {
    let facts = snapshot
        .directories
        .iter()
        .find(|facts| facts.directory == tree)
        .unwrap();
    assert_eq!(
        facts
            .repository
            .as_ref()
            .map(|repository| repository.root.as_path()),
        Some(tree),
        "the published repository is not the Worktree's own"
    );
    let list = facts.worktrees.as_ref().unwrap();
    assert_eq!(
        list.current
            .map(|current| list.worktrees[current].root.as_path()),
        Some(tree),
        "the published current Worktree is not the Worktree's own"
    );
}

#[test]
fn a_read_while_a_worktree_is_being_removed_keeps_membership_and_its_own_facts() {
    // A Worktree inside its repository, and one outside any repository.
    for nested in [true, false] {
        let git = GitFixture::new();
        let parent = GitFixture::new();
        std::fs::remove_dir_all(parent.0.join(".git")).unwrap();
        let tree = if nested { &git.0 } else { &parent.0 }.join("linked");
        git.git(&["worktree", "add", "-b", "feature", tree.to_str().unwrap()]);
        let mut reading = Reading::watched(
            [tree.clone()].into(),
            Arc::new(DeadlineRunner),
            git.0.join("unused-home"),
            Arc::new(Unwatchable),
        );
        wait(&mut reading, "the first read did not finish", |snapshot| {
            snapshot
                .directories
                .iter()
                .any(|facts| facts.failure.is_none() && facts.worktrees.is_some())
        });
        assert_eq!(
            reading.state.inventory.workspaces[0].tabs[0]
                .worktree
                .as_ref(),
            Some(&tree)
        );
        // `rm -rf` and `git worktree remove` can delete `.git` long before the directory.
        std::fs::remove_file(tree.join(".git")).unwrap();
        reading.repositories.reread([tree.clone()].into());
        wait(
            &mut reading,
            "the read during removal did not fail",
            |snapshot| {
                snapshot
                    .directories
                    .iter()
                    .any(|facts| facts.failure == Some(RepositoryReadError::Unavailable))
            },
        );
        assert_eq!(
            reading.state.inventory.workspaces[0].tabs[0]
                .worktree
                .as_ref(),
            Some(&tree),
            "nested: {nested}"
        );
        assert_own_facts(&reading.poll(), &tree);
    }
}

#[test]
fn a_failed_read_keeps_following_its_repositorys_git_directories() {
    /// Times out the first discovery run once armed, as a loaded host can.
    struct FailOnce {
        armed: AtomicBool,
    }
    impl RepositoryProgramRunner for FailOnce {
        fn run(
            &self,
            request: &ProgramRequest,
            stdout: &mut dyn FnMut(&[u8]),
            cancellation: &CancellationToken,
        ) -> Result<ProgramExit, ProgramError> {
            if is_discovery(request) && self.armed.swap(false, Ordering::AcqRel) {
                return Err(ProgramError::TimedOut);
            }
            DeadlineRunner.run(request, stdout, cancellation)
        }
    }
    let git = GitFixture::new();
    let runner = Arc::new(FailOnce {
        armed: AtomicBool::new(false),
    });
    let mut reading = Reading::start(
        [git.0.clone()].into(),
        runner.clone(),
        git.0.join("unused-home"),
    );
    wait(&mut reading, "the first read did not finish", |snapshot| {
        snapshot
            .directories
            .iter()
            .any(|facts| facts.failure.is_none() && facts.worktrees.is_some())
    });
    runner.armed.store(true, Ordering::Release);
    reading.repositories.reread([git.0.clone()].into());
    wait(&mut reading, "the armed read did not fail", |snapshot| {
        snapshot
            .directories
            .iter()
            .any(|facts| facts.failure == Some(RepositoryReadError::TimedOut))
    });
    // Only the watch on the git directories can see this change.
    git.git(&["branch", "-m", "renamed"]);
    wait(
        &mut reading,
        "a failed read stopped following its repository",
        |snapshot| {
            snapshot.directories.iter().any(|facts| {
                facts.failure.is_none()
                    && facts.repository.as_ref().is_some_and(|repository| {
                        repository.head == RepositoryHead::Branch("renamed".into())
                    })
            })
        },
    );
}

#[test]
fn a_failed_read_keeps_its_repositorys_count_in_flight() {
    /// Holds each count until released, and times out the first discovery run once armed.
    #[derive(Default)]
    struct HeldCount {
        armed: AtomicBool,
        counting: AtomicBool,
        released: AtomicBool,
        cancelled: AtomicBool,
        ended: AtomicBool,
    }
    impl RepositoryProgramRunner for HeldCount {
        fn run(
            &self,
            request: &ProgramRequest,
            stdout: &mut dyn FnMut(&[u8]),
            cancellation: &CancellationToken,
        ) -> Result<ProgramExit, ProgramError> {
            if is_discovery(request) && self.armed.swap(false, Ordering::AcqRel) {
                return Err(ProgramError::TimedOut);
            }
            let count: Vec<std::ffi::OsString> = policy::local_read::COUNT_ARGUMENTS
                .iter()
                .map(Into::into)
                .collect();
            if request.arguments.ends_with(&count) && !self.released.load(Ordering::Acquire) {
                self.counting.store(true, Ordering::Release);
                let until = Instant::now() + HELD;
                // Cancellation is checked first, so a count cancelled before its release says so.
                let result = loop {
                    if cancellation.is_cancelled() {
                        self.cancelled.store(true, Ordering::Release);
                        break Err(ProgramError::Cancelled);
                    }
                    if self.released.load(Ordering::Acquire) || Instant::now() >= until {
                        break DeadlineRunner.run(request, stdout, cancellation);
                    }
                    std::thread::sleep(PAUSE);
                };
                self.ended.store(true, Ordering::Release);
                return result;
            }
            DeadlineRunner.run(request, stdout, cancellation)
        }
    }
    let git = GitFixture::new();
    let runner = Arc::new(HeldCount::default());
    let mut reading = Reading::watched(
        [git.0.clone()].into(),
        runner.clone(),
        git.0.join("unused-home"),
        Arc::new(Unwatchable),
    );
    wait(&mut reading, "the first read did not finish", |snapshot| {
        runner.counting.load(Ordering::Acquire)
            && snapshot
                .directories
                .iter()
                .any(|facts| facts.worktrees.is_some())
    });
    runner.armed.store(true, Ordering::Release);
    reading.repositories.reread([git.0.clone()].into());
    wait(&mut reading, "the armed read did not fail", |snapshot| {
        snapshot
            .directories
            .iter()
            .any(|facts| facts.failure == Some(RepositoryReadError::TimedOut))
    });
    runner.released.store(true, Ordering::Release);
    wait(&mut reading, "the held count did not end", |_| {
        runner.ended.load(Ordering::Acquire)
    });
    assert!(
        !runner.cancelled.load(Ordering::Acquire),
        "a failed read cancelled its repository's count"
    );
}

#[test]
fn a_fixed_root_tab_leaves_its_worktree_only_after_reading_outside_the_repository() {
    let git = GitFixture::new();
    let outside = GitFixture::new();
    std::fs::remove_dir_all(outside.0.join(".git")).unwrap();
    let missing = outside.0.join("missing");
    let mut reading = Reading::start(
        [outside.0.clone(), missing.clone()].into(),
        Arc::new(DeadlineRunner),
        git.0.join("unused-home"),
    );
    // Each Workspace's only Tab is its unpinned Root Tab, as after directory repair.
    for workspace in &mut reading.state.inventory.workspaces {
        let tab = &mut workspace.tabs[0];
        tab.worktree = Some(git.0.clone());
        tab.worktree_repository = Some(git.0.clone());
        tab.worktree_membership = spaceterm_server::hierarchy::WorktreeMembership::Repaired;
    }
    // The Tabs' saved Worktree is a source too, so wait for the two directories under test.
    wait(&mut reading, "directory reads did not finish", |snapshot| {
        [&outside.0, &missing].into_iter().all(|directory| {
            snapshot
                .directories
                .iter()
                .any(|facts| facts.directory == *directory)
        })
    });
    let snapshot = reading.poll();
    assert!(
        snapshot
            .directories
            .iter()
            .any(|facts| facts.directory == missing && facts.failure.is_some())
    );
    for workspace in &reading.state.inventory.workspaces {
        let tab = &workspace.tabs[0];
        if tab.panes[0].repository_directory() == outside.0 {
            assert_eq!(tab.worktree, None, "{:?}", snapshot.directories);
            assert_eq!(tab.worktree_repository, None);
            assert_eq!(
                tab.worktree_membership,
                spaceterm_server::hierarchy::WorktreeMembership::Following
            );
        } else {
            assert_eq!(tab.worktree.as_ref(), Some(&git.0));
            assert_eq!(tab.worktree_repository.as_ref(), Some(&git.0));
            assert_eq!(
                tab.worktree_membership,
                spaceterm_server::hierarchy::WorktreeMembership::Repaired
            );
        }
    }
}

#[test]
fn slow_count_publishes_branch_worktrees_other_sources_and_progress_before_completion() {
    let git = GitFixture::new();
    let other = GitFixture::new();
    let tree = git.0.join("linked");
    git.git(&["worktree", "add", "-b", "feature", tree.to_str().unwrap()]);
    git.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/owner/example.git",
    ]);
    let subdirectory = git.0.join("src");
    std::fs::create_dir(&subdirectory).unwrap();
    let alias = git.0.join("alias");
    std::os::unix::fs::symlink(&subdirectory, &alias).unwrap();
    std::fs::write(git.0.join("changed"), "untracked").unwrap();
    let runner = Arc::new(DelayedCount {
        root: git.0.clone(),
        also_delayed: Sources::new(),
        released: AtomicBool::new(false),
        cancelled: AtomicBool::new(false),
        calls: Mutex::default(),
        started: Mutex::default(),
    });
    // All three spellings must share the same expensive scan. The other repository and linked
    // Worktree remain independently readable even if this count occupies a worker for its full deadline.
    let sources = [
        git.0.clone(),
        subdirectory.clone(),
        alias.clone(),
        other.0.clone(),
        tree.clone(),
    ]
    .into();
    let requested_at = Instant::now();
    // A new watch reads its repository once more, which would count this root twice.
    let mut reading = Reading::watched(
        sources,
        runner.clone(),
        other.0.join("unused-home"),
        Arc::new(Unwatchable),
    );
    let until = Instant::now() + STUCK;
    let mut published = BTreeMap::new();
    let mut progress_at = None;
    let mut initial_pending = false;
    let mut responsive = false;
    while Instant::now() < until {
        let snapshot = reading.poll();
        for facts in snapshot.directories {
            // Each spelling shares the root's count, and whichever is discovered first shows
            // the count pending. Discovery of the root itself can finish after counting starts.
            if facts
                .repository
                .as_ref()
                .is_some_and(|repository| repository.root == git.0)
            {
                let repository = facts.repository.as_ref().unwrap();
                assert_eq!(repository.head, RepositoryHead::Branch("main".into()));
                assert_eq!(
                    repository
                        .pull_request
                        .as_ref()
                        .unwrap()
                        .head_branch
                        .as_ref(),
                    "main"
                );
                assert!(
                    repository.changes.is_err(),
                    "the delayed count has not completed"
                );
                initial_pending |= repository.counting == Some(ChangeCounting::Pending);
                if repository.counting == Some(ChangeCounting::Counting) {
                    progress_at.get_or_insert(Instant::now());
                }
            }
            published.insert(facts.directory.clone(), facts);
        }
        let discovered = published
            .get(&git.0)
            .and_then(|facts| facts.worktrees.as_ref())
            .is_some_and(|list| list.worktrees.iter().any(|record| record.root == tree));
        let other_finished = published.get(&other.0).is_some_and(|facts| {
            facts.worktrees.is_some()
                && facts.repository.as_ref().is_some_and(|repository| {
                    repository.counting.is_none() && repository.changes.is_ok()
                })
        });
        responsive = discovered
            && other_finished
            && progress_at.is_some()
            && published.contains_key(&subdirectory)
            && published.contains_key(&alias);
        if responsive {
            break;
        }
        std::thread::sleep(PAUSE);
    }
    assert!(
        responsive,
        "partial facts must reach the snapshot before the delayed count finishes"
    );
    assert!(initial_pending);
    let progress_at = progress_at.expect("useful counting progress before completion");
    // Delivery can lag behind the server queuing Pending. Measure the lower bound from
    // the read request so that publication backpressure cannot shorten the observed delay.
    let since_request = progress_at.duration_since(requested_at);
    assert!(
        since_request >= Duration::from_millis(900),
        "{since_request:?}"
    );
    assert!(
        published[&git.0]
            .worktrees
            .as_ref()
            .unwrap()
            .worktrees
            .iter()
            .any(|record| record.root == tree)
    );
    assert!(
        published[&other.0]
            .repository
            .as_ref()
            .unwrap()
            .changes
            .is_ok()
    );
    assert!(!runner.released.load(Ordering::Acquire));
    runner.released.store(true, Ordering::Release);
    let completed: BTreeSet<_> = reading
        .finish()
        .directories
        .into_iter()
        .filter_map(|facts| {
            let repository = facts.repository?;
            if repository.root != git.0 || repository.counting.is_some() {
                return None;
            }
            assert!(matches!(repository.changes.unwrap().total, ChangeTotal::Exact(n) if n >= 1));
            Some(facts.directory)
        })
        .collect();
    assert_eq!(completed, [git.0.clone(), subdirectory, alias].into());
    assert_eq!(*runner.calls.lock().unwrap().get(&git.0).unwrap(), 1);
    assert!(*runner.calls.lock().unwrap().get(&other.0).unwrap() >= 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 3);
}

#[test]
fn refreshing_branch_retains_known_changes_until_the_new_count_finishes() {
    let git = GitFixture::new();
    std::fs::write(git.0.join("first"), "untracked").unwrap();
    let previous = Reading::start(
        [git.0.clone()].into(),
        Arc::new(DeadlineRunner),
        git.0.join("unused-home"),
    )
    .finish();
    assert_eq!(
        previous.directories[0]
            .repository
            .as_ref()
            .unwrap()
            .changes
            .as_ref()
            .unwrap()
            .total,
        ChangeTotal::Exact(1)
    );
    git.git(&["branch", "-m", "feature"]);
    std::fs::write(git.0.join("second"), "also untracked").unwrap();
    let runner = Arc::new(DelayedCount {
        root: git.0.clone(),
        also_delayed: Sources::new(),
        released: AtomicBool::new(false),
        cancelled: AtomicBool::new(false),
        calls: Mutex::default(),
        started: Mutex::default(),
    });
    let mut reading = Reading::start(
        [git.0.clone()].into(),
        runner.clone(),
        git.0.join("unused-home"),
    );
    reading.repositories.snapshot = previous;
    let until = Instant::now() + STUCK;
    let mut refreshed = false;
    while Instant::now() < until {
        let snapshot = reading.poll();
        let facts = &snapshot.directories[0];
        let repository = facts.repository.as_ref().unwrap();
        assert_eq!(
            repository.changes.as_ref().unwrap().total,
            ChangeTotal::Exact(1)
        );
        assert!(
            facts.worktrees.is_some(),
            "refresh retains the last inventory"
        );
        if repository.head == RepositoryHead::Branch("feature".into())
            && repository.counting == Some(ChangeCounting::Counting)
        {
            refreshed = true;
            break;
        }
        std::thread::sleep(PAUSE);
    }
    assert!(
        refreshed,
        "a new branch must not wait for its delayed count"
    );
    runner.released.store(true, Ordering::Release);
    let snapshot = reading.finish();
    let repository = snapshot.directories[0].repository.as_ref().unwrap();
    assert_eq!(repository.counting, None);
    assert_eq!(
        repository.changes.as_ref().unwrap().total,
        ChangeTotal::Exact(2)
    );
}

#[test]
fn discovery_refreshes_external_branch_and_worktree_changes_while_counts_remain_in_flight() {
    let git = GitFixture::new();
    let second = GitFixture::new();
    let third = GitFixture::new();
    let roots: Sources = [git.0.clone(), second.0.clone(), third.0.clone()].into();
    let runner = Arc::new(DelayedCount {
        root: git.0.clone(),
        also_delayed: [second.0.clone(), third.0.clone()].into(),
        released: AtomicBool::new(false),
        cancelled: AtomicBool::new(false),
        calls: Mutex::default(),
        started: Mutex::default(),
    });
    let mut reading = Reading::start(roots.clone(), runner.clone(), git.0.join("unused-home"));
    let until = Instant::now() + STUCK;
    loop {
        let snapshot = reading.poll();
        if snapshot.directories.len() == roots.len()
            && snapshot
                .directories
                .iter()
                .all(|facts| facts.worktrees.is_some())
            && runner.calls.lock().unwrap().len() == COUNT_WORKERS
        {
            assert_eq!(
                snapshot
                    .directories
                    .iter()
                    .find(|facts| facts.directory == git.0)
                    .unwrap()
                    .worktrees
                    .as_ref()
                    .unwrap()
                    .worktrees
                    .len(),
                1
            );
            break;
        }
        assert!(
            Instant::now() < until,
            "publish the first listing and start the slow scans"
        );
        std::thread::sleep(PAUSE);
    }
    // Both count workers are blocked, with a third physical root waiting. Only now change
    // independent Git facts, so initial publication cannot satisfy this regression.
    let initially_started: Sources = runner.calls.lock().unwrap().keys().cloned().collect();
    git.git(&["branch", "-m", "refreshed"]);
    let tree = git.0.join("external-worktree");
    git.git(&["worktree", "add", "-b", "external", tree.to_str().unwrap()]);
    let changed_at = Instant::now();
    let until = changed_at + STUCK;
    loop {
        let snapshot = reading.poll();
        let updated = snapshot
            .directories
            .iter()
            .find(|facts| facts.directory == git.0)
            .is_some_and(|facts| {
                facts.repository.as_ref().is_some_and(|repository| {
                    repository.head == RepositoryHead::Branch("refreshed".into())
                        && repository.counting.is_some()
                }) && facts
                    .worktrees
                    .as_ref()
                    .is_some_and(|list| list.worktrees.iter().any(|record| record.root == tree))
            });
        // The new row is a source edit as well: it must enter discovery without restarting
        // either existing scan, even though there is no free count worker to service it.
        let new_source = snapshot.directories.iter().any(|facts| {
            facts.directory == tree
                && facts.repository.as_ref().is_some_and(|repository| {
                    repository.head == RepositoryHead::Branch("external".into())
                })
        });
        if updated && new_source {
            break;
        }
        assert!(
            Instant::now() < until,
            "recurring discovery must not wait for the count queue"
        );
        std::thread::sleep(PAUSE);
    }
    assert!(!runner.released.load(Ordering::Acquire));
    assert!(
        !runner.cancelled.load(Ordering::Acquire),
        "source expansion must retain scans still in use"
    );
    let calls = runner.calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        COUNT_WORKERS,
        "no slow scan has completed to free a worker"
    );
    for root in initially_started {
        assert_eq!(
            calls[&root], 1,
            "recurring discovery must share the in-flight scan"
        );
    }
    assert!(
        reading
            .repositories
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.cancellation.is_cancelled())
    );
}

#[test]
fn withdrawing_the_last_source_cancels_its_in_flight_count_and_discards_its_facts() {
    let git = GitFixture::new();
    let runner = Arc::new(DelayedCount {
        root: git.0.clone(),
        also_delayed: Sources::new(),
        released: AtomicBool::new(false),
        cancelled: AtomicBool::new(false),
        calls: Mutex::default(),
        started: Mutex::default(),
    });
    let mut reading = Reading::start(
        [git.0.clone()].into(),
        runner.clone(),
        git.0.join("unused-home"),
    );
    let until = Instant::now() + STUCK;
    while runner.started.lock().unwrap().is_none() {
        reading.poll();
        assert!(
            Instant::now() < until,
            "start the count before withdrawing its source"
        );
        std::thread::sleep(PAUSE);
    }
    reading.state.inventory.workspaces.clear();
    let until = Instant::now() + STUCK;
    loop {
        assert!(reading.poll().directories.is_empty());
        if runner.cancelled.load(Ordering::Acquire) {
            break;
        }
        assert!(
            Instant::now() < until,
            "a count without any interest must be cancelled"
        );
        std::thread::sleep(PAUSE);
    }
    runner.released.store(true, Ordering::Release);
    assert!(reading.poll().directories.is_empty());
}

#[test]
fn cancellation_releases_count_workers_with_a_full_publication_queue() {
    let git = GitFixture::new();
    let runner = Arc::new(DelayedCount {
        root: git.0.clone(),
        also_delayed: Sources::new(),
        released: AtomicBool::new(false),
        cancelled: AtomicBool::new(false),
        calls: Mutex::default(),
        started: Mutex::default(),
    });
    let reading = Reading::start(
        [git.0.clone()].into(),
        runner.clone(),
        git.0.join("unused-home"),
    );
    let until = Instant::now() + STUCK;
    while runner.started.lock().unwrap().is_none() && Instant::now() < until {
        std::thread::sleep(PAUSE);
    }
    assert!(runner.started.lock().unwrap().is_some());
    // Stop draining: the final listing or progress now encounters publication backpressure.
    let stopping = Instant::now();
    drop(reading);
    assert!(stopping.elapsed() < STUCK, "{:?}", stopping.elapsed());
}

#[derive(Clone, Copy)]
enum SaturatedRead {
    Count,
    Discovery,
}
impl SaturatedRead {
    fn workers(self) -> usize {
        match self {
            Self::Count => COUNT_WORKERS,
            Self::Discovery => DISCOVERY_WORKERS,
        }
    }
    fn matches(self, request: &ProgramRequest) -> bool {
        let arguments: &[&str] = match self {
            Self::Count => &policy::local_read::COUNT_ARGUMENTS,
            // Headers run once per discovery, unlike rev-parse which also belongs to listing.
            Self::Discovery => &policy::local_read::HEADER_ARGUMENTS,
        };
        let suffix: Vec<std::ffi::OsString> = arguments.iter().map(Into::into).collect();
        request.arguments.ends_with(&suffix)
    }
}

/// Hold slow processes at the existing runner boundary until the test releases one read.
/// Repeated reads block again, so recurring work keeps the pool saturated throughout the test.
struct SaturatedReads {
    stage: SaturatedRead,
    slow: Sources,
    calls: Mutex<BTreeMap<PathBuf, usize>>,
    permits: Mutex<BTreeMap<PathBuf, usize>>,
}
impl SaturatedReads {
    fn started(&self) -> Sources {
        self.calls.lock().unwrap().keys().cloned().collect()
    }
    fn release(&self, roots: &Sources) {
        let mut permits = self.permits.lock().unwrap();
        for root in roots {
            *permits.entry(root.clone()).or_default() += 1;
        }
    }
    fn recurring(&self) -> bool {
        self.calls.lock().unwrap().values().any(|count| *count > 1)
    }
}
impl RepositoryProgramRunner for SaturatedReads {
    fn run(
        &self,
        request: &ProgramRequest,
        stdout: &mut dyn FnMut(&[u8]),
        cancellation: &CancellationToken,
    ) -> Result<ProgramExit, ProgramError> {
        if self.stage.matches(request) && self.slow.contains(&request.directory) {
            *self
                .calls
                .lock()
                .unwrap()
                .entry(request.directory.clone())
                .or_default() += 1;
            let until = Instant::now() + HELD;
            loop {
                if cancellation.is_cancelled() {
                    return Err(ProgramError::Cancelled);
                }
                if Instant::now() >= until {
                    return Err(ProgramError::TimedOut);
                }
                let permitted = self
                    .permits
                    .lock()
                    .unwrap()
                    .get_mut(&request.directory)
                    .is_some_and(|count| {
                        if *count == 0 {
                            return false;
                        }
                        *count -= 1;
                        true
                    });
                if permitted {
                    break;
                }
                std::thread::sleep(PAUSE);
            }
        }
        DeadlineRunner.run(request, stdout, cancellation)
    }
}

fn saturation_snapshot(reading: &mut Reading, predicate: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let until = Instant::now() + STUCK;
    loop {
        let snapshot = reading.poll();
        if predicate(&snapshot) {
            return snapshot;
        }
        assert!(
            Instant::now() < until,
            "waiting sources must progress through a saturated reader"
        );
        std::thread::sleep(PAUSE);
    }
}

fn assert_late_source_progresses_while_earlier_sources_refresh(stage: SaturatedRead) {
    let workers = stage.workers();
    // Three cohorts fill the workers, queue, and waiting set. All slow roots sort before
    // the late fast root, so lexical dispatch can repeatedly refresh them ahead of it.
    let mut fixtures: Vec<_> = (0..3 * workers + 1).map(|_| GitFixture::new()).collect();
    fixtures.sort_by_key(|fixture| fixture.0.clone());
    let fast = fixtures.last().unwrap();
    std::fs::write(fast.0.join("fast-change"), "untracked").unwrap();
    let slow: Sources = fixtures[..3 * workers]
        .iter()
        .map(|fixture| fixture.0.clone())
        .collect();
    let runner = Arc::new(SaturatedReads {
        stage,
        slow: slow.clone(),
        calls: Mutex::default(),
        permits: Mutex::default(),
    });
    // A watch may report the fixtures' creation late. Only the explicit reread below asks again.
    let mut reading = Reading::watched(
        slow.clone(),
        runner.clone(),
        fast.0.join("unused-home"),
        Arc::new(Unwatchable),
    );
    saturation_snapshot(&mut reading, |snapshot| {
        runner.started().len() == workers
            && (matches!(stage, SaturatedRead::Discovery)
                || slow.iter().all(|root| {
                    snapshot
                        .directories
                        .iter()
                        .any(|facts| facts.directory == *root && facts.worktrees.is_some())
                }))
    });
    let first = runner.started();
    let mut late = State::empty();
    late.ensure_first_workspace([1; 32], &fast.0).unwrap();
    reading
        .state
        .inventory
        .workspaces
        .extend(late.inventory.workspaces);
    reading.poll();
    runner.release(&first);
    saturation_snapshot(&mut reading, |snapshot| {
        runner.started().len() == 2 * workers
            // The late root waits for a free worker of the saturated stage.
            && (matches!(stage, SaturatedRead::Discovery)
                || snapshot.directories.iter().any(|facts| {
                    facts.directory == fast.0 && facts.repository.is_some()
                }))
            && first.iter().all(|root| {
                snapshot.directories.iter().any(|facts| {
                    facts.directory == *root
                        && facts.worktrees.is_some()
                        && facts
                            .repository
                            .as_ref()
                            .is_some_and(|repository| repository.counting.is_none())
                })
            })
    });
    let second: Sources = runner.started().difference(&first).cloned().collect();
    // Ask for the completed first cohort again while the second cohort occupies the workers
    // and the third occupies the queue. Its new read must not overtake the late root.
    reading.repositories.reread(first.clone());
    reading.poll();
    runner.release(&second);
    saturation_snapshot(&mut reading, |_| runner.started().len() == 3 * workers);
    let third: Sources = slow
        .difference(&first)
        .filter(|root| !second.contains(*root))
        .cloned()
        .collect();
    runner.release(&third);
    let snapshot = saturation_snapshot(&mut reading, |snapshot| {
        runner.recurring()
            && snapshot.directories.iter().any(|facts| {
                facts.directory == fast.0
                    && facts.failure.is_none()
                    && facts.worktrees.is_some()
                    && facts.repository.as_ref().is_some_and(|repository| {
                        repository.counting.is_none()
                            && repository
                                .changes
                                .as_ref()
                                .is_ok_and(|changes| changes.total == ChangeTotal::Exact(1))
                    })
            })
    });
    assert_eq!(
        snapshot
            .directories
            .iter()
            .find(|facts| facts.directory == fast.0)
            .unwrap()
            .repository
            .as_ref()
            .unwrap()
            .head,
        RepositoryHead::Branch("main".into())
    );
}

#[test]
fn a_late_fast_source_finishes_counting_while_earlier_slow_roots_refresh() {
    assert_late_source_progresses_while_earlier_sources_refresh(SaturatedRead::Count);
}

#[test]
fn a_late_fast_source_is_discovered_while_earlier_slow_sources_refresh() {
    assert_late_source_progresses_while_earlier_sources_refresh(SaturatedRead::Discovery);
}

/// Git's built-in fsmonitor daemon writes cookies below the git directory during every read.
/// A read must not cause the next one, so the reader goes idle.
#[cfg(target_os = "macos")]
#[test]
fn a_linked_worktree_using_the_builtin_fsmonitor_goes_idle() {
    #[derive(Default)]
    struct Counting(Mutex<usize>);
    impl RepositoryProgramRunner for Counting {
        fn run(
            &self,
            request: &ProgramRequest,
            stdout: &mut dyn FnMut(&[u8]),
            cancellation: &CancellationToken,
        ) -> Result<ProgramExit, ProgramError> {
            *self.0.lock().unwrap() += 1;
            DeadlineRunner.run(request, stdout, cancellation)
        }
    }
    let git = GitFixture::new();
    let tree = git.0.join("linked");
    git.git(&["worktree", "add", "-b", "feature", tree.to_str().unwrap()]);
    git.git(&["config", "core.fsmonitor", "true"]);
    let runner = Arc::new(Counting::default());
    let mut reading = Reading::start(
        [git.0.clone(), tree.clone()].into(),
        runner.clone(),
        git.0.join("unused-home"),
    );
    reading.finish();
    let runs = || *runner.0.lock().unwrap();
    // The reads that follow each new watch end within a few settling periods. Then nothing
    // runs for longer than two of them.
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let before = runs();
        let quiet = Instant::now() + SETTLE * 5 / 2;
        while Instant::now() < quiet {
            reading.poll();
            std::thread::sleep(PAUSE);
        }
        if runs() == before {
            break;
        }
        assert!(Instant::now() < until, "each read started another");
    }
    // The daemon keeps its cookies here. Its socket may live elsewhere for a long path.
    let used = git
        .0
        .join(".git/worktrees/linked/fsmonitor--daemon")
        .is_dir()
        && git.0.join(".git/fsmonitor--daemon").is_dir();
    for directory in [&git.0, &tree] {
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(["fsmonitor--daemon", "stop"])
            .output();
    }
    assert!(used, "the reads used the fsmonitor daemon");
}
