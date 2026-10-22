use super::super::repository_tests::GitFixture;
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

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
        cancellation: &SshCancellationToken,
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
                let until = Instant::now() + Duration::from_secs(8);
                while !self.released.load(Ordering::Acquire) {
                    if cancellation.is_cancelled() {
                        self.cancelled.store(true, Ordering::Release);
                        return Err(ProgramError::Cancelled);
                    }
                    if Instant::now() >= until {
                        return Err(ProgramError::TimedOut);
                    }
                    std::thread::sleep(POLL);
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
impl Reading {
    fn start(sources: Sources, runner: Arc<dyn RepositoryProgramRunner>, home: PathBuf) -> Self {
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
        let cancellation = SshCancellationToken::default();
        let token = cancellation.clone();
        let sent_sources = sources.clone();
        let (interests, updates) = mpsc::sync_channel(1);
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            read_sources(
                sources,
                updates,
                Some(readers),
                GitToolStatus::Ready(version),
                token,
                sender,
            );
        });
        repositories.worker = Some(Worker {
            cancellation,
            handle: worker,
            results: receiver,
            interests,
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
        let until = Instant::now() + Duration::from_secs(4);
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
            std::thread::sleep(POLL);
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
    let mut reading = Reading::start(sources, runner.clone(), other.0.join("unused-home"));
    let until = Instant::now() + Duration::from_secs(6);
    let mut published = BTreeMap::new();
    let mut progress_at = None;
    let mut initial_pending = false;
    let mut pending_at = None;
    let mut responsive = false;
    while Instant::now() < until {
        let snapshot = reading.poll();
        for facts in snapshot.directories {
            if facts.directory == git.0 {
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
                if repository.counting == Some(ChangeCounting::Pending) {
                    pending_at.get_or_insert(Instant::now());
                }
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
        std::thread::sleep(POLL);
    }
    assert!(
        responsive,
        "partial facts must reach the snapshot before the delayed count finishes"
    );
    assert!(initial_pending);
    let elapsed = progress_at
        .expect("useful counting progress before completion")
        .duration_since(pending_at.expect("an initial pending count"));
    assert!(
        elapsed >= Duration::from_millis(900) && elapsed < Duration::from_secs(3),
        "{elapsed:?}"
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
    let until = Instant::now() + Duration::from_secs(4);
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
        std::thread::sleep(POLL);
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
    let until = Instant::now() + Duration::from_secs(4);
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
        std::thread::sleep(POLL);
    }
    // Both count workers are blocked, with a third physical root waiting. Only now change
    // independent Git facts, so initial publication cannot satisfy this regression.
    let initially_started: Sources = runner.calls.lock().unwrap().keys().cloned().collect();
    git.git(&["branch", "-m", "refreshed"]);
    let tree = git.0.join("external-worktree");
    git.git(&["worktree", "add", "-b", "external", tree.to_str().unwrap()]);
    let changed_at = Instant::now();
    let until = changed_at + Duration::from_secs(4);
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
        std::thread::sleep(POLL);
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
    let until = Instant::now() + Duration::from_secs(4);
    while runner.started.lock().unwrap().is_none() {
        reading.poll();
        assert!(
            Instant::now() < until,
            "start the count before withdrawing its source"
        );
        std::thread::sleep(POLL);
    }
    reading.state.inventory.workspaces.clear();
    let until = Instant::now() + Duration::from_secs(1);
    loop {
        assert!(reading.poll().directories.is_empty());
        if runner.cancelled.load(Ordering::Acquire) {
            break;
        }
        assert!(
            Instant::now() < until,
            "a count without any interest must be cancelled"
        );
        std::thread::sleep(POLL);
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
    let until = Instant::now() + Duration::from_secs(4);
    while runner.started.lock().unwrap().is_none() && Instant::now() < until {
        std::thread::sleep(POLL);
    }
    assert!(runner.started.lock().unwrap().is_some());
    // Stop draining: the final listing or progress now encounters publication backpressure.
    let stopping = Instant::now();
    drop(reading);
    assert!(stopping.elapsed() < Duration::from_secs(1));
}
