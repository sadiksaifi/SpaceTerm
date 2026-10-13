//! Real Git and SQLite evidence at the external-write boundary.
//! Process-kill tests do not establish power-loss durability. The Git actor bridge
//! is not on this base; native writes enter through the existing host serializer.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};
use spaceterm_core::{
    authority::*,
    identity::*,
    operation_ledger::{canonical::CanonicalCodec, model::*},
    operation_ledger_contracts::*,
    operations::*,
    organization::*,
    reducer::*,
    storage::*,
    worktrees::*,
};
use spaceterm_host::{
    git::{self as native, RepositoryProgramRunner, UnixRepositoryProgramRunner},
    organization_store::SqliteOrganizationStore,
    storage::testing::{FaultPoint, TestControl},
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{ServerPin, SigningKey},
};
use spaceterm_server::git::host::{GitReadCancellation, NativeServerGitHost, ServerGitHost};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{BufReader, Read, Write},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command as Program, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

#[expect(
    dead_code,
    reason = "Other composition suites use the remaining enrollment helpers."
)]
#[path = "store_pairing_composition/support.rs"]
mod physical;

const WAIT: Duration = Duration::from_secs(300);
const MARKER: &[u8] = b"EXTERNAL-WRITE-BARRIER\n";
const CHILD_ROOT: &str = "SPACETERM_EXTERNAL_WRITE_TEST_ROOT";
const CHILD_STAGE: &str = "SPACETERM_EXTERNAL_WRITE_TEST_STAGE";

struct Key(Ed25519KeyPair);
impl Key {
    fn new(seed: u8) -> Self {
        Self(Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap())
    }
    fn pin(&self) -> ServerPin {
        ServerPin {
            identity: physical::identity().server.bytes(),
            public_key: self.public_key(),
        }
    }
}
impl SigningKey for Key {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}
fn directory(path: &Path) -> ServerDirectory {
    ServerDirectory::new(
        physical::identity().server,
        path.as_os_str().as_encoded_bytes().to_vec(),
    )
    .unwrap()
}
fn private_root() -> tempfile::TempDir {
    #[cfg(target_os = "macos")]
    let parent = PathBuf::from("/private/tmp");
    #[cfg(target_os = "linux")]
    let parent = {
        let path =
            PathBuf::from(std::env::var_os("HOME").unwrap()).join("spaceterm-build/extwrite");
        fs::create_dir_all(&path).unwrap();
        path
    };
    let root = tempfile::tempdir_in(parent).unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}
fn git_command(root: &Path, arguments: &[&str]) -> Program {
    let mut command = Program::new("/usr/bin/git");
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
        ])
        .args(arguments)
        .current_dir(root.join("repository"))
        .env_clear()
        .env("HOME", root)
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ALLOW_PROTOCOL", "")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}
fn run_git(root: &Path, arguments: &[&str]) {
    assert!(
        spaceterm_host::process::spawn(&mut git_command(root, arguments))
            .unwrap()
            .wait()
            .unwrap()
            .success(),
        "fixture Git command failed"
    );
}
fn init_repository(root: &Path) {
    fs::create_dir(root.join("repository")).unwrap();
    run_git(root, &["init", "-b", "main"]);
    fs::write(root.join("repository/payload"), b"fixture payload\n").unwrap();
    fs::write(
        root.join("repository/.gitattributes"),
        b"payload filter=gate\n",
    )
    .unwrap();
    run_git(root, &["add", "."]);
    run_git(
        root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "initial",
        ],
    );
}

struct Fixture {
    storage: physical::Fixture,
    stream: StreamBinding,
    request: CanonicalOperationRequest,
    intent: GitWriteIntent,
}
impl Fixture {
    fn new(root: &Path) -> Self {
        let database = root.join("database");
        fs::create_dir(&database).unwrap();
        fs::set_permissions(&database, fs::Permissions::from_mode(0o700)).unwrap();
        let mut storage = physical::Fixture::new(&database);
        storage.enroll(42);
        let recovered = storage.recovered();
        let stream = StreamBinding::from_stream(
            recovered.pairing.as_ref().unwrap().parts().authorizations[0]
                .parts()
                .clients[0]
                .parts()
                .ordinary,
        );
        let scope = RepositoryScope {
            server: physical::identity().server,
            repository: RepositoryId::new(physical::identity().server, 1).unwrap(),
        };
        let request = CanonicalOperationRequest::new(
            OperationId::new(stream.stream(), OperationSequence::new(1).unwrap()),
            Command::CreateWorktree {
                scope,
                branch: BranchName::new("topic".into()).unwrap(),
            },
            &CanonicalCodec,
        )
        .unwrap();
        let context = storage.context();
        let AdmissionPreparation::Prepared(admission) = storage
            .ledger
            .prepare_admission(
                &stream,
                request.binding().id,
                request.command(),
                PendingKind::External {
                    effect: ExternalEffectKind::GitWrite,
                },
                context,
            )
            .unwrap()
        else {
            panic!("new Git admission")
        };
        storage.commit(admission, None);
        let recovered = storage.recovered();
        let reducer = OrganizationReducer::from_recovered(
            RetainedOrganization::new(
                recovered.organization.clone(),
                recovered.high_water,
                directory(root),
            ),
            recovered.store_revision,
            recovered.last_epoch,
            OrganizationLimits::default(),
        )
        .unwrap();
        let reduction = reducer
            .prepare_git_allocation(
                &GitAllocationRequest {
                    repository: true,
                    worktree: true,
                    fallbacks: vec![],
                },
                &ReductionContext {
                    principal: MutationPrincipal {
                        pairing: stream.stream().pairing(),
                        client: stream.stream().client(),
                    },
                    operation: request.binding().id,
                    request_digest: request.binding().digest,
                    now: ReducerTick::new(1000),
                    facts: ReductionFacts::default(),
                },
            )
            .unwrap();
        let mut parts = storage.parts();
        parts.resulting_high_water = reduction.writes().resulting_high_water();
        reduction.writes().append_to(&mut parts).unwrap();
        let allocation = parts
            .operation_writes
            .iter()
            .find_map(|write| match write {
                OperationWrite::GitAllocation(GitAllocationMutation::Reserve(value)) => {
                    Some(value.as_ref().clone())
                }
                _ => None,
            })
            .unwrap();
        let transaction =
            DurableTransaction::new_with_validation(parts, &physical::services()).unwrap();
        let CommitDisposition::Durable(receipt) = storage.store.commit(&transaction) else {
            panic!("durable allocation")
        };
        storage.ledger.observe_store_commit(receipt).unwrap();
        let intent = GitWriteIntent::new(GitWriteIntentParts {
            binding: request.binding(),
            repository: RepositoryLocator::new(RepositoryLocatorParts {
                scope,
                main_root: directory(&root.join("repository")),
                common_directory: directory(&root.join("repository/.git")),
            })
            .unwrap(),
            action: RetainedGitAction::Create {
                worktree: allocation.parts().worktree.unwrap(),
                directory: directory(&root.join("destination/worktree")),
                branch: WorktreeBranchChoice::New {
                    name: BranchName::new("topic".into()).unwrap(),
                    base: BranchReference::Local(BranchName::new("main".into()).unwrap()),
                },
                target: CreationTargetObservation::Absent,
                template_version: None,
            },
            phase: GitIntentPhase::Prepared,
            assessment_digest: GitAssessmentDigest::from_bytes([8; 32]),
            affected: RetainedRemovalScope::empty(scope.server),
            allocation: request.binding(),
            external_result: None,
        })
        .unwrap();
        init_repository(root);
        fs::create_dir(root.join("destination")).unwrap();
        Self {
            storage,
            stream,
            request,
            intent,
        }
    }
    fn control(&self) -> TestControl {
        self.storage.store.authority_test_control()
    }
    fn progress(
        &mut self,
        phase: GitIntentPhase,
        external: Option<RetainedExternalResult>,
    ) -> (PreparedLedgerTransition, DurableTransaction) {
        let before = self.storage.recovered();
        let reference = ExternalIntentReference::new(ExternalIntentReferenceParts {
            binding: self.request.binding(),
            scope: ExternalIntentScope::Git(self.intent.parts().repository.scope()),
            phase: match phase {
                GitIntentPhase::Prepared => ExternalIntentPhase::Prepared,
                GitIntentPhase::Started => ExternalIntentPhase::Started,
                GitIntentPhase::ExternalFinished => ExternalIntentPhase::ExternalFinished,
            },
            recorded_at: before.store_revision.checked_successor().unwrap(),
        })
        .unwrap();
        let progress = PendingExternalProgress::new(PendingExternalProgressParts {
            binding: self.request.binding(),
            effect: ExternalEffectKind::GitWrite,
            state: ExternalProgressState::RecordedIntent {
                reference,
                retirement: None,
            },
        })
        .unwrap();
        let context = self.storage.context();
        let prepared = self
            .storage
            .ledger
            .prepare_external_progress(
                &self.stream,
                self.request.binding().id,
                self.request.binding().digest,
                progress,
                context,
            )
            .unwrap();
        let mut parts = self.storage.parts();
        prepared.projection().append_to(&mut parts).unwrap();
        if phase == GitIntentPhase::Prepared {
            parts.operation_writes.extend([
                OperationWrite::GitRepository(GitRepositoryMutation::Put(
                    self.intent.parts().repository.clone(),
                )),
                OperationWrite::GitIntent(GitIntentMutation::Insert(Box::new(self.intent.clone()))),
            ]);
        } else {
            parts
                .operation_writes
                .push(OperationWrite::GitIntent(GitIntentMutation::Advance {
                    binding: self.request.binding(),
                    expected_phase: if phase == GitIntentPhase::Started {
                        GitIntentPhase::Prepared
                    } else {
                        GitIntentPhase::Started
                    },
                    next: phase,
                    result: external,
                }));
        }
        let transaction =
            DurableTransaction::new_with_validation(parts, &physical::services()).unwrap();
        (prepared, transaction)
    }
    fn persist(&mut self, phase: GitIntentPhase, external: Option<RetainedExternalResult>) {
        let (prepared, transaction) = self.progress(phase, external);
        let disposition = self.storage.store.commit(&transaction);
        assert!(
            matches!(disposition, CommitDisposition::Durable(_)),
            "{disposition:?}"
        );
        self.storage
            .ledger
            .resolve_commit(prepared, disposition)
            .unwrap();
        self.intent = self.storage.recovered().git_intents[0].clone();
    }
    fn completion(
        &mut self,
        observation: RecoveryObservation,
    ) -> (PreparedLedgerTransition, DurableTransaction) {
        let before = self.storage.recovered();
        let basis = OperationReductionBasis::new(OperationReductionBasisParts {
            binding: self.request.binding(),
            base: before.organization.revision,
            resulting: before.organization.revision,
        })
        .unwrap();
        let outcome = Outcome::Git(
            GitMutationOutcome::new(GitMutationOutcomeParts::Recovered {
                server: physical::identity().server,
                observation,
                revision: None,
            })
            .unwrap(),
        );
        let context = TransactionContext::new(
            before.store_revision,
            before.organization.revision,
            before.organization.revision,
            Some(basis),
        )
        .unwrap();
        let prepared = self
            .storage
            .ledger
            .prepare_completion(
                &self.stream,
                self.request.binding().id,
                self.request.binding().digest,
                CompactOutcome::new(&outcome, &CanonicalCodec).unwrap(),
                before.organization.revision,
                context,
            )
            .unwrap();
        let mut parts = self.storage.parts();
        prepared.projection().append_to(&mut parts).unwrap();
        parts.operation_writes.extend([
            OperationWrite::ReductionBasis(basis),
            OperationWrite::GitIntent(GitIntentMutation::Complete {
                binding: self.request.binding(),
            }),
            OperationWrite::GitAllocation(GitAllocationMutation::Abandon {
                binding: self.request.binding(),
            }),
        ]);
        (
            prepared,
            DurableTransaction::new_with_validation(parts, &physical::services()).unwrap(),
        )
    }
}

#[derive(Default)]
struct ReadAudit(Mutex<Vec<Vec<OsString>>>);
impl RepositoryProgramRunner for ReadAudit {
    fn run(
        &self,
        request: &native::ProgramRequest,
        stdout: &mut dyn FnMut(&[u8]),
        cancellation: &native::CancellationToken,
    ) -> Result<native::ProgramExit, native::ProgramError> {
        self.0.lock().unwrap().push(request.arguments.clone());
        UnixRepositoryProgramRunner::new().run(request, stdout, cancellation)
    }
}
fn host(root: &Path, runner: Arc<dyn RepositoryProgramRunner>) -> NativeServerGitHost {
    NativeServerGitHost::new(
        runner,
        "/usr/bin/git".into(),
        root.to_owned(),
        native::MINIMUM_GIT_VERSION,
        Arc::new(
            spaceterm_host::server_directories::native::NativeDirectoryProvider::new(
                physical::identity().server,
                root.to_owned(),
            )
            .unwrap(),
        ),
    )
}
fn tree(path: &Path) -> BTreeMap<PathBuf, [u8; 32]> {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, [u8; 32]>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let mut hash = Sha256::new();
        hash.update(metadata.permissions().mode().to_be_bytes());
        if metadata.is_symlink() {
            hash.update(fs::read_link(path).unwrap().as_os_str().as_encoded_bytes());
        } else if metadata.is_file() {
            hash.update(fs::read(path).unwrap());
        } else if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), entries);
            }
        } else {
            panic!("unexpected repository entry")
        }
        entries.insert(
            path.strip_prefix(root).unwrap().to_owned(),
            hash.finalize().into(),
        );
    }
    let mut entries = BTreeMap::new();
    visit(path, path, &mut entries);
    entries
}
fn observe_without_writes(root: &Path, intent: &GitWriteIntent) -> RecoveryObservation {
    let before = tree(&root.join("repository"));
    let worktree = root.join("destination/worktree");
    let before_worktree = worktree.exists().then(|| tree(&worktree));
    let audit = Arc::new(ReadAudit::default());
    let host = host(root, audit.clone());
    let observed = host
        .reconcile(intent, &GitReadCancellation::default())
        .unwrap();
    host.shutdown().unwrap();
    assert_eq!(
        tree(&root.join("repository")),
        before,
        "recovery changed repository bytes"
    );
    assert_eq!(
        worktree.exists().then(|| tree(&worktree)),
        before_worktree,
        "recovery changed checkout bytes"
    );
    let requests = audit.0.lock().unwrap();
    assert!(!requests.is_empty());
    for args in requests.iter() {
        assert!(!args.windows(2).any(|pair| pair[0] == "worktree"
            && matches!(pair[1].to_str(), Some("add" | "remove" | "prune"))));
        assert!(
            !args
                .iter()
                .any(|arg| matches!(arg.to_str(), Some("-d" | "-D" | "--delete")))
        );
    }
    observed.observation
}
fn reopen(root: &Path) -> (SqliteOrganizationStore, OperationLedger, RecoveredState) {
    let mut store = physical::reopen(&root.join("database")).unwrap();
    let recovered = store.recover().unwrap();
    let ledger = OperationLedger::recover(
        recovered.organization.server,
        recovered.ledger.clone().unwrap(),
    )
    .unwrap();
    (store, ledger, recovered)
}

struct OwnedChild {
    process: Child,
    reaped: bool,
}
impl OwnedChild {
    fn kill_and_wait(&mut self) {
        if !self.reaped {
            // Signal before reaping, even if a fixture assertion ended the leader.
            // Its unreaped PID still binds this private group and cannot be reused.
            let mut kill = Program::new("/bin/kill");
            kill.args(["-KILL", "--", &format!("-{}", self.process.id())])
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let _ = spaceterm_host::process::spawn(&mut kill)
                .unwrap()
                .wait()
                .unwrap();
            let deadline = Instant::now() + WAIT;
            while self.process.try_wait().unwrap().is_none() {
                assert!(
                    Instant::now() < deadline,
                    "owned crash fixture did not exit"
                );
                thread::sleep(Duration::from_millis(10));
            }
            self.reaped = true;
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.kill_and_wait();
    }
}
fn barrier() -> ! {
    std::io::stdout().write_all(MARKER).unwrap();
    std::io::stdout().flush().unwrap();
    let mut byte = [0];
    let _ = std::io::stdin().read_exact(&mut byte);
    panic!("crash barrier was released instead of killed")
}
fn crash_at(test: &str, stage: &str) -> tempfile::TempDir {
    let root = private_root();
    let mut program = Program::new(std::env::current_exe().unwrap());
    program
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD_ROOT, root.path())
        .env(CHILD_STAGE, stage)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .process_group(0);
    let mut child = OwnedChild {
        process: spaceterm_host::process::spawn(&mut program).unwrap(),
        reaped: false,
    };
    let stdout = child.process.stdout.take().unwrap();
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let mut seen = Vec::new();
        for byte in BufReader::new(stdout).bytes().take(65536) {
            let Ok(byte) = byte else { break };
            seen.push(byte);
            if seen.ends_with(MARKER) {
                let _ = sender.send(());
                return;
            }
        }
    });
    let reached = receiver.recv_timeout(WAIT);
    child.kill_and_wait();
    reader.join().unwrap();
    assert!(
        reached.is_ok(),
        "child did not reach the requested crash barrier: {reached:?}"
    );
    root
}
fn child_stage() -> Option<(PathBuf, String)> {
    std::env::var_os(CHILD_ROOT)
        .map(|root| (PathBuf::from(root), std::env::var(CHILD_STAGE).unwrap()))
}
fn crash_child(root: &Path, stage: &str) -> ! {
    let mut fixture = Fixture::new(root);
    fixture.persist(GitIntentPhase::Prepared, None);
    if stage == "prepared" {
        barrier();
    }
    fixture.persist(GitIntentPhase::Started, None);
    if stage == "started" {
        barrier();
    }
    if stage == "branch-only" {
        // The fixture-only committed-ref hook pauses Git before checkout publication.
        // Production Git still disables hooks. The parent kills this entire owned group.
        let gate = NativeGate::branch_publication(root);
        let mut git = spaceterm_host::process::spawn(&mut git_command(
            root,
            &[
                "-c",
                &format!("core.hooksPath={}", root.join("hooks").display()),
                "worktree",
                "add",
                "-b",
                "topic",
                root.join("destination/worktree").to_str().unwrap(),
                "main",
            ],
        ))
        .unwrap();
        gate.wait();
        assert!(root.join("repository/.git/refs/heads/topic").is_file());
        assert!(!root.join("destination/worktree").exists());
        assert!(!root.join("repository/.git/worktrees").exists());
        assert!(
            git.try_wait().unwrap().is_none(),
            "branch-only crash must interrupt Git before it exits"
        );
        barrier();
    }
    run_git(
        root,
        &[
            "worktree",
            "add",
            "-b",
            "topic",
            root.join("destination/worktree").to_str().unwrap(),
            "main",
        ],
    );
    let observed = host(root, Arc::new(UnixRepositoryProgramRunner::new()))
        .reconcile(&fixture.intent, &GitReadCancellation::default())
        .unwrap();
    fixture.persist(GitIntentPhase::ExternalFinished, Some(observed.external));
    let (_, transaction) = fixture.completion(observed.observation);
    fixture
        .control()
        .barrier(FaultPoint::BeforeCommit, || barrier());
    fixture.storage.store.commit(&transaction);
    panic!("completion-publication barrier was not reached")
}

#[test]
fn crash_after_prepared_retains_original_pending_identity_without_external_write() {
    if let Some((root, stage)) = child_stage() {
        crash_child(&root, &stage);
    }
    let root = crash_at(
        "crash_after_prepared_retains_original_pending_identity_without_external_write",
        "prepared",
    );
    let (_store, ledger, recovered) = reopen(root.path());
    let intent = &recovered.git_intents[0];
    assert_eq!(intent.phase(), GitIntentPhase::Prepared);
    assert_eq!(
        ledger
            .lookup(
                &StreamBinding::from_stream(intent.binding().id.stream()),
                intent.binding().id,
                Some(intent.binding().digest)
            )
            .unwrap(),
        OperationLookup::Pending
    );
    assert_eq!(recovered.git_allocations[0].binding(), intent.binding());
    assert!(!root.path().join("destination/worktree").exists());
    assert_eq!(
        observe_without_writes(root.path(), intent),
        RecoveryObservation::PreStateObserved
    );
}
#[test]
fn crash_after_started_before_spawn_observes_prestate_without_repeating_add() {
    if let Some((root, stage)) = child_stage() {
        crash_child(&root, &stage);
    }
    let root = crash_at(
        "crash_after_started_before_spawn_observes_prestate_without_repeating_add",
        "started",
    );
    let (_store, ledger, recovered) = reopen(root.path());
    let intent = &recovered.git_intents[0];
    assert_eq!(intent.phase(), GitIntentPhase::Started);
    assert_eq!(
        ledger
            .lookup(
                &StreamBinding::from_stream(intent.binding().id.stream()),
                intent.binding().id,
                Some(intent.binding().digest)
            )
            .unwrap(),
        OperationLookup::Pending
    );
    assert_eq!(
        observe_without_writes(root.path(), intent),
        RecoveryObservation::PreStateObserved
    );
}
#[test]
fn forced_termination_while_git_add_has_only_created_branch() {
    if let Some((root, stage)) = child_stage() {
        crash_child(&root, &stage);
    }
    let root = crash_at(
        "forced_termination_while_git_add_has_only_created_branch",
        "branch-only",
    );
    let (_store, ledger, recovered) = reopen(root.path());
    let intent = &recovered.git_intents[0];
    assert_eq!(intent.phase(), GitIntentPhase::Started);
    assert!(
        root.path()
            .join("repository/.git/refs/heads/topic")
            .is_file()
    );
    assert!(!root.path().join("destination/worktree").exists());
    assert_eq!(
        observe_without_writes(root.path(), intent),
        RecoveryObservation::PartialOrAmbiguous
    );
    assert_eq!(
        ledger
            .lookup(
                &StreamBinding::from_stream(intent.binding().id.stream()),
                intent.binding().id,
                Some(intent.binding().digest)
            )
            .unwrap(),
        OperationLookup::Pending
    );
}
#[test]
fn crash_after_external_completion_before_publication_retains_original_identity() {
    if let Some((root, stage)) = child_stage() {
        crash_child(&root, &stage);
    }
    let root = crash_at(
        "crash_after_external_completion_before_publication_retains_original_identity",
        "external-complete",
    );
    let (_store, ledger, recovered) = reopen(root.path());
    let intent = &recovered.git_intents[0];
    assert_eq!(intent.phase(), GitIntentPhase::ExternalFinished);
    assert_eq!(
        observe_without_writes(root.path(), intent),
        RecoveryObservation::DesiredStateObserved
    );
    assert_eq!(
        ledger
            .lookup(
                &StreamBinding::from_stream(intent.binding().id.stream()),
                intent.binding().id,
                Some(intent.binding().digest)
            )
            .unwrap(),
        OperationLookup::Pending
    );
}

#[test]
fn replacement_checkout_is_ambiguous_and_recovery_leaves_it_untouched() {
    let root = private_root();
    let mut fixture = Fixture::new(root.path());
    fixture.persist(GitIntentPhase::Prepared, None);
    fixture.persist(GitIntentPhase::Started, None);
    run_git(
        root.path(),
        &[
            "worktree",
            "add",
            "-b",
            "topic",
            root.path().join("destination/worktree").to_str().unwrap(),
            "main",
        ],
    );
    fs::rename(
        root.path().join("destination/worktree"),
        root.path().join("original-worktree"),
    )
    .unwrap();
    fs::create_dir(root.path().join("destination/worktree")).unwrap();
    fs::write(
        root.path().join("destination/worktree/sentinel"),
        b"replacement belongs to another owner",
    )
    .unwrap();
    let binding = fixture.request.binding();
    drop(fixture);
    let (_store, ledger, recovered) = reopen(root.path());
    assert_eq!(
        observe_without_writes(root.path(), &recovered.git_intents[0]),
        RecoveryObservation::PartialOrAmbiguous
    );
    assert_eq!(
        ledger
            .lookup(
                &StreamBinding::from_stream(binding.id.stream()),
                binding.id,
                Some(binding.digest)
            )
            .unwrap(),
        OperationLookup::Pending
    );
    assert!(root.path().join("original-worktree/.git").is_file());
    assert_eq!(
        fs::read(root.path().join("destination/worktree/sentinel")).unwrap(),
        b"replacement belongs to another owner"
    );
}

#[test]
fn intent_io_and_sync_uncertainty_poison_admission_until_validated_reopen() {
    for phase in [GitIntentPhase::Prepared, GitIntentPhase::Started] {
        for fault in [
            FaultPoint::DatabaseWrite,
            FaultPoint::DatabaseSync,
            FaultPoint::DirectorySync,
            #[cfg(target_os = "macos")]
            FaultPoint::DeleteFullSync,
            FaultPoint::AfterCommit,
        ] {
            let root = private_root();
            let mut fixture = Fixture::new(root.path());
            if phase == GitIntentPhase::Started {
                fixture.persist(GitIntentPhase::Prepared, None);
            }
            let binding = fixture.request.binding();
            let (prepared, transaction) = fixture.progress(phase, None);
            let control = fixture.control();
            control.fail_once(fault, 0);
            assert_eq!(
                fixture.storage.store.commit(&transaction),
                CommitDisposition::Indeterminate,
                "{phase:?} {fault:?}"
            );
            assert!(
                !control.fault_pending(),
                "the selected physical fault did not fire"
            );
            fixture
                .storage
                .ledger
                .resolve_commit(prepared, CommitDisposition::Indeterminate)
                .unwrap();
            let calls = control.sql_calls();
            control.repeat_sync().unwrap();
            assert_eq!(
                fixture.storage.store.recover(),
                Err(StoreFailure::RecoveryRequired)
            );
            assert_eq!(
                fixture.storage.store.commit(&transaction),
                CommitDisposition::NotApplied(StoreFailure::RecoveryRequired)
            );
            assert_eq!(control.sql_calls(), calls);
            assert!(!root.path().join("destination/worktree").exists());
            drop(fixture);
            let (_store, ledger, recovered) = reopen(root.path());
            assert_eq!(
                ledger
                    .lookup(
                        &StreamBinding::from_stream(binding.id.stream()),
                        binding.id,
                        Some(binding.digest)
                    )
                    .unwrap(),
                OperationLookup::Pending
            );
            assert_eq!(recovered.git_allocations[0].binding(), binding);
            if let Some(intent) = recovered.git_intents.first() {
                assert_eq!(intent.binding(), binding);
                assert_eq!(
                    observe_without_writes(root.path(), intent),
                    RecoveryObservation::PreStateObserved
                );
            }
        }
    }
}

#[test]
fn completion_io_and_sync_uncertainty_resolves_original_result_without_second_write() {
    for fault in [
        FaultPoint::DatabaseWrite,
        FaultPoint::DatabaseSync,
        FaultPoint::DirectorySync,
        #[cfg(target_os = "macos")]
        FaultPoint::DeleteFullSync,
        FaultPoint::AfterCommit,
    ] {
        let root = private_root();
        let mut fixture = Fixture::new(root.path());
        fixture.persist(GitIntentPhase::Prepared, None);
        fixture.persist(GitIntentPhase::Started, None);
        run_git(
            root.path(),
            &[
                "worktree",
                "add",
                "-b",
                "topic",
                root.path().join("destination/worktree").to_str().unwrap(),
                "main",
            ],
        );
        let observed = host(root.path(), Arc::new(UnixRepositoryProgramRunner::new()))
            .reconcile(&fixture.intent, &GitReadCancellation::default())
            .unwrap();
        fixture.persist(GitIntentPhase::ExternalFinished, Some(observed.external));
        let binding = fixture.request.binding();
        let (prepared, transaction) = fixture.completion(observed.observation);
        let control = fixture.control();
        control.fail_once(fault, 0);
        assert_eq!(
            fixture.storage.store.commit(&transaction),
            CommitDisposition::Indeterminate,
            "{fault:?}"
        );
        assert!(!control.fault_pending());
        fixture
            .storage
            .ledger
            .resolve_commit(prepared, CommitDisposition::Indeterminate)
            .unwrap();
        let calls = control.sql_calls();
        control.repeat_sync().unwrap();
        assert_eq!(
            fixture.storage.store.commit(&transaction),
            CommitDisposition::NotApplied(StoreFailure::RecoveryRequired)
        );
        assert_eq!(control.sql_calls(), calls);
        drop(fixture);
        let (_store, ledger, recovered) = reopen(root.path());
        let lookup = ledger
            .lookup(
                &StreamBinding::from_stream(binding.id.stream()),
                binding.id,
                Some(binding.digest),
            )
            .unwrap();
        match lookup {
            OperationLookup::Pending => {
                assert_eq!(
                    recovered.git_intents[0].phase(),
                    GitIntentPhase::ExternalFinished
                );
                assert_eq!(recovered.git_allocations[0].binding(), binding);
                assert_eq!(
                    observe_without_writes(root.path(), &recovered.git_intents[0]),
                    RecoveryObservation::DesiredStateObserved
                );
            }
            OperationLookup::Completed {
                result: Outcome::Git(outcome),
                ..
            } => {
                assert!(matches!(
                    outcome.parts(),
                    GitMutationOutcomeParts::Recovered {
                        observation: RecoveryObservation::DesiredStateObserved,
                        ..
                    }
                ));
                assert!(recovered.git_intents.is_empty() && recovered.git_allocations.is_empty());
            }
            other => panic!("original operation lost: {other:?}"),
        }
        assert!(root.path().join("destination/worktree/.git").is_file());
    }
}

struct ValidFixture;
impl native::server::FinalWriteValidation for ValidFixture {
    fn validate(&self, _: &native::CancellationToken, _: Instant) -> Result<(), GitFailure> {
        // This uncontrolled native-check port does not supply a durable start decision.
        Ok(())
    }
}
fn coordinator(root: &Path) -> native::GitCoordinator {
    let runner: Arc<dyn RepositoryProgramRunner> = Arc::new(UnixRepositoryProgramRunner::new());
    native::GitCoordinator::new(
        Arc::new(native::LocalWorktreeGit::new(
            runner.clone(),
            "/usr/bin/git".into(),
            root.to_owned(),
            native::MINIMUM_GIT_VERSION,
        )),
        Arc::new(native::LocalRepositoryReader::new(
            runner,
            Arc::new(native::UnixRepositoryMarkerReader::new()),
            "/usr/bin/git".into(),
            root.to_owned(),
            native::MINIMUM_GIT_VERSION,
        )),
        Arc::new(native::filesystem::NativeWorktreeFilesystem),
    )
}
fn prepare_native(
    root: &Path,
    coordinator: &native::GitCoordinator,
    binding: OperationBinding,
) -> native::server::PreparedRepositoryWrite {
    let repository = coordinator
        .repository(
            &root.join("repository"),
            &native::CancellationToken::default(),
        )
        .unwrap();
    coordinator
        .prepare_server_write(
            &repository,
            native::GitWrite::Create {
                path: root.join("destination/worktree"),
                branch: native::WorktreeBranch::New {
                    name: "topic".into(),
                    base: native::BranchName::Local("main".into()),
                },
            },
            binding,
            &native::CancellationToken::default(),
            Arc::new(ValidFixture),
        )
        .unwrap()
        .1
}

#[test]
fn uncertain_started_receipt_drops_native_preparation_without_a_repository_write() {
    let root = private_root();
    let mut fixture = Fixture::new(root.path());
    fixture.persist(GitIntentPhase::Prepared, None);
    let coordinator = coordinator(root.path());
    let prepared_native = prepare_native(root.path(), &coordinator, fixture.request.binding());
    let before = tree(&root.path().join("repository"));
    let (prepared, transaction) = fixture.progress(GitIntentPhase::Started, None);
    let control = fixture.control();
    // Even a physically committed Started row is not an execution permit when its receipt is uncertain.
    control.fail_once(FaultPoint::AfterCommit, 0);
    assert_eq!(
        fixture.storage.store.commit(&transaction),
        CommitDisposition::Indeterminate
    );
    fixture
        .storage
        .ledger
        .resolve_commit(prepared, CommitDisposition::Indeterminate)
        .unwrap();
    drop(prepared_native);
    coordinator.shutdown();
    assert_eq!(tree(&root.path().join("repository")), before);
    assert!(!root.path().join("destination/worktree").exists());
    drop(fixture);
    let (_store, _, recovered) = reopen(root.path());
    assert_eq!(recovered.git_intents[0].phase(), GitIntentPhase::Started);
    assert_eq!(
        observe_without_writes(root.path(), &recovered.git_intents[0]),
        RecoveryObservation::PreStateObserved
    );
}

struct NativeGate {
    ready: File,
    release: File,
}
impl NativeGate {
    fn pipes(root: &Path) -> Self {
        let ready = root.join("git-ready");
        let release = root.join("git-release");
        let mut command = Program::new("/usr/bin/mkfifo");
        command
            .args(["-m", "600"])
            .arg(&ready)
            .arg(&release)
            .stderr(Stdio::null());
        assert!(
            spaceterm_host::process::spawn(&mut command)
                .unwrap()
                .wait()
                .unwrap()
                .success()
        );
        let open = |path| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap()
        };
        Self {
            ready: open(&ready),
            release: open(&release),
        }
    }
    fn branch_publication(root: &Path) -> Self {
        let gate = Self::pipes(root);
        let hooks = root.join("hooks");
        fs::create_dir(&hooks).unwrap();
        let hook = hooks.join("reference-transaction");
        fs::write(
            &hook,
            format!(
                "#!/bin/sh\n[ \"$1\" = committed ] || exit 0\nprintf 'GIT-WRITE-BARRIER\\n' > '{}'\nread release < '{}'\n",
                root.join("git-ready").display(),
                root.join("git-release").display(),
            ),
        )
        .unwrap();
        fs::set_permissions(hook, fs::Permissions::from_mode(0o700)).unwrap();
        gate
    }
    fn new(root: &Path) -> Self {
        let gate = Self::pipes(root);
        run_git(
            root,
            &[
                "config",
                "filter.gate.smudge",
                &format!(
                    "printf 'GIT-WRITE-BARRIER\\n' > '{}'; read release < '{}'; /bin/cat",
                    root.join("git-ready").display(),
                    root.join("git-release").display(),
                ),
            ],
        );
        gate
    }
    fn wait(&self) {
        const NATIVE_MARKER: &[u8] = b"GIT-WRITE-BARRIER\n";
        let mut ready = self.ready.try_clone().unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        let reader = thread::spawn(move || {
            let mut bytes = [0; NATIVE_MARKER.len()];
            let _ = sender.send(ready.read_exact(&mut bytes).map(|()| bytes));
        });
        let received = receiver.recv_timeout(WAIT);
        if received.is_err() {
            // Unblock the owned reader before reporting a fixture failure.
            self.ready
                .try_clone()
                .unwrap()
                .write_all(&[0; NATIVE_MARKER.len()])
                .unwrap();
        }
        reader.join().unwrap();
        assert_eq!(received.unwrap().unwrap(), NATIVE_MARKER);
    }
    fn release(&mut self) {
        self.release.write_all(b"continue\n").unwrap();
    }
}
impl Drop for NativeGate {
    fn drop(&mut self) {
        let _ = self.release.write_all(b"continue\n");
    }
}

#[test]
fn observer_loss_keeps_native_ownership_and_reserved_completion_under_admission_pressure() {
    let root = private_root();
    let mut fixture = Fixture::new(root.path());
    fixture.persist(GitIntentPhase::Prepared, None);
    let gate = &mut NativeGate::new(root.path());
    let coordinator = coordinator(root.path());
    let prepared_native = prepare_native(root.path(), &coordinator, fixture.request.binding());
    fixture.persist(GitIntentPhase::Started, None);
    let binding = fixture.request.binding();
    let (observer, observation) = mpsc::sync_channel(1);
    drop(observation);
    let (finished, completion) = mpsc::sync_channel(1);
    let (settle, settlement) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let executed = prepared_native.execute(binding).unwrap();
        assert_eq!(executed.outcome(), native::GitWriteOutcome::Created);
        assert!(observer.send(executed.outcome()).is_err());
        finished.send(()).unwrap();
        executed
            .finish(settlement.recv_timeout(WAIT).unwrap())
            .unwrap();
    });
    gate.wait();
    assert!(matches!(
        completion.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let draining = coordinator.clone();
    let (entered, entering) = mpsc::sync_channel(1);
    let (drained, drain) = mpsc::sync_channel(1);
    let shutdown = thread::spawn(move || {
        entered.send(()).unwrap();
        draining.shutdown();
        drained.send(()).unwrap();
    });
    entering.recv_timeout(WAIT).unwrap();
    let command = Command::AssessEndAllSessions {
        server: physical::identity().server,
    };
    let ceiling = LedgerLimits::production_v1().ordinary_stream_slots();
    // The original result reservation remains occupied even when no observer exists.
    for sequence in 2..=u64::from(ceiling) {
        let context = fixture.storage.context();
        let AdmissionPreparation::Prepared(prepared) = fixture
            .storage
            .ledger
            .prepare_admission(
                &fixture.stream,
                OperationId::new(
                    fixture.stream.stream(),
                    OperationSequence::new(sequence).unwrap(),
                ),
                &command,
                PendingKind::AtomicMutation,
                context,
            )
            .unwrap()
        else {
            panic!("new competing operation")
        };
        fixture.storage.commit(prepared, None);
    }
    let context = fixture.storage.context();
    let failure = fixture
        .storage
        .ledger
        .prepare_admission(
            &fixture.stream,
            OperationId::new(
                fixture.stream.stream(),
                OperationSequence::new(u64::from(ceiling) + 1).unwrap(),
            ),
            &command,
            PendingKind::AtomicMutation,
            context,
        )
        .err()
        .unwrap();
    assert_eq!(failure.class, LedgerFailureClass::QuotaExceeded);
    gate.release();
    completion.recv_timeout(WAIT).unwrap();
    // Native completion does not release the repository lane before durable settlement.
    assert!(matches!(drain.try_recv(), Err(mpsc::TryRecvError::Empty)));
    let observed = host(root.path(), Arc::new(UnixRepositoryProgramRunner::new()))
        .reconcile(&fixture.intent, &GitReadCancellation::default())
        .unwrap();
    fixture.persist(GitIntentPhase::ExternalFinished, Some(observed.external));
    let (prepared, transaction) = fixture.completion(observed.observation);
    let disposition = fixture.storage.store.commit(&transaction);
    assert!(matches!(disposition, CommitDisposition::Durable(_)));
    fixture
        .storage
        .ledger
        .resolve_commit(prepared, disposition)
        .unwrap();
    settle.send(Ok(())).unwrap();
    worker.join().unwrap();
    drain.recv_timeout(WAIT).unwrap();
    shutdown.join().unwrap();
    drop(fixture);
    let (_store, ledger, recovered) = reopen(root.path());
    assert!(matches!(
        ledger
            .lookup(
                &StreamBinding::from_stream(binding.id.stream()),
                binding.id,
                Some(binding.digest)
            )
            .unwrap(),
        OperationLookup::Completed {
            result: Outcome::Git(_),
            ..
        }
    ));
    assert!(recovered.git_intents.is_empty() && recovered.git_allocations.is_empty());
    assert!(recovered.organization.workspaces.is_empty());
}

#[path = "support/job_a.rs"]
mod injected;

impl spaceterm_server::contracts::ServerCredentialPort for Key {}
struct Startup;
impl spaceterm_server::api::StartupAuthorityPort for Startup {
    fn validate_startup(&self) -> Result<ServerId, spaceterm_server::contracts::ServerFailure> {
        Ok(physical::identity().server)
    }
    fn validate_credentials(
        &self,
        credentials: &dyn spaceterm_server::contracts::ServerCredentialPort,
        _: ServerId,
    ) -> Result<(), spaceterm_server::contracts::ServerFailure> {
        assert_eq!(
            credentials.public_key(),
            physical::identity().server_public_key
        );
        Ok(())
    }
}
struct PairingSnapshot(spaceterm_core::pairing::policy::PairingSnapshot);
impl spaceterm_server::pairing::protocol_lookup::PairingSnapshotPort for &PairingSnapshot {
    fn snapshot(&self) -> Result<spaceterm_core::pairing::policy::PairingSnapshot, ProtocolError> {
        Ok(self.0.clone())
    }
}
impl spaceterm_protocol::authentication::PairingLookup for PairingSnapshot {
    fn lookup(
        &self,
        server: [u8; 16],
        pairing: [u8; 16],
        client: [u8; 16],
    ) -> Result<spaceterm_protocol::authentication::PairingRecord, ProtocolError> {
        spaceterm_server::pairing::protocol_lookup::PairingProtocolLookup::new(self)
            .lookup(server, pairing, client)
    }
}
impl spaceterm_server::contracts::PairingAuthorizationPort for PairingSnapshot {}

#[tokio::test]
async fn actor_lookup_after_reopen_returns_original_compact_result_without_resurrection() {
    use spaceterm_protocol::authentication::{self, PairingLookup};
    use spaceterm_server::{
        adapters::core::CorePorts, api::*, service::RecoveredServer,
        shutdown::TrustedShutdownRequest,
    };
    let root = private_root();
    let mut fixture = Fixture::new(root.path());
    fixture.persist(GitIntentPhase::Prepared, None);
    fixture.persist(GitIntentPhase::Started, None);
    run_git(
        root.path(),
        &[
            "worktree",
            "add",
            "-b",
            "topic",
            root.path().join("destination/worktree").to_str().unwrap(),
            "main",
        ],
    );
    let observed = host(root.path(), Arc::new(UnixRepositoryProgramRunner::new()))
        .reconcile(&fixture.intent, &GitReadCancellation::default())
        .unwrap();
    fixture.persist(GitIntentPhase::ExternalFinished, Some(observed.external));
    let binding = fixture.request.binding();
    let (prepared, transaction) = fixture.completion(observed.observation);
    let disposition = fixture.storage.store.commit(&transaction);
    assert!(matches!(disposition, CommitDisposition::Durable(_)));
    fixture
        .storage
        .ledger
        .resolve_commit(prepared, disposition)
        .unwrap();
    drop(fixture); // Lose the reply and every process-local outcome owner.
    let (store, _, recovered) = reopen(root.path());
    let lookup = Arc::new(PairingSnapshot(
        spaceterm_core::pairing::policy::PairingSnapshot::new(
            recovered.organization.server,
            recovered.last_epoch,
            recovered.store_revision,
            recovered.high_water,
            recovered.pairing.clone().unwrap(),
        )
        .unwrap(),
    ));
    let authorization = recovered.pairing.as_ref().unwrap().parts().authorizations[0].parts();
    let signer = Arc::new(Key::new(71));
    let principal = authentication::Principal {
        server: physical::identity().server.bytes(),
        device: authorization.record.device.bytes(),
        pairing: injected::local(binding.id.stream().pairing().local()),
        client: injected::local(binding.id.stream().client().local()),
    };
    lookup
        .lookup(principal.server, principal.pairing, principal.client)
        .unwrap();
    let (client, hello) = authentication::ClientHandshake::begin(
        signer.pin(),
        principal,
        authentication::Purpose::Ordinary,
        authentication::HandshakeConfig::default(),
        Arc::new(Key::new(43)),
    )
    .unwrap();
    let (server, reply) = authentication::ServerHandshake::accept(
        principal.server,
        recovered.last_epoch.get() + 1,
        authentication::HandshakeConfig::default(),
        signer.clone(),
        lookup.as_ref(),
        &hello,
    )
    .unwrap();
    let (client, proof) = client.receive_server(&reply).unwrap();
    let (established, ready) = server.receive_device(lookup.as_ref(), &proof).unwrap();
    client.receive_ready(&ready).unwrap();
    let authentication::Established::Ordinary(codec) = established else {
        panic!("ordinary principal")
    };
    let principal =
        VerifiedPrincipal::from_authenticated(&codec, ConnectionGeneration::new(1)).unwrap();
    let native = injected::Fixture::new();
    let runtime = native.factory.clone();
    let mut config = native.config;
    config.host.authority = Arc::new(Startup);
    config.core = CorePorts::production(
        physical::identity(),
        directory(root.path()),
        Arc::new(injected::Clock),
    )
    .unwrap();
    let mut dependencies = native.dependencies;
    dependencies.store = Box::new(store);
    dependencies.credentials = signer;
    dependencies.pairing = lookup;
    let server = RecoveredServer::recover(config, dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    injected::connect(&handle, principal).await;
    assert!(
        matches!(handle.lookup_state(principal, binding).await.unwrap(),
        OperationLookup::Completed { result: Outcome::Git(outcome), .. }
        if matches!(outcome.parts(), GitMutationOutcomeParts::Recovered {
            observation: RecoveryObservation::DesiredStateObserved, ..
        }))
    );
    let changed = OperationBinding {
        digest: RequestDigest::from_bytes([99; 32]),
        ..binding
    };
    assert_eq!(
        handle.lookup_state(principal, changed).await.unwrap(),
        OperationLookup::IdentityReusedWithDifferentContents
    );
    assert!(handle.snapshot().state().workspaces.is_empty());
    assert!(runtime.starts.lock().unwrap().is_empty());
    handle
        .shutdown(TrustedShutdownRequest {
            authority: native.host,
        })
        .await
        .unwrap();
    server.wait().await.unwrap();
    let (_store, ledger, recovered) = reopen(root.path());
    assert!(matches!(
        ledger
            .lookup(
                &StreamBinding::from_stream(binding.id.stream()),
                binding.id,
                Some(binding.digest)
            )
            .unwrap(),
        OperationLookup::Completed {
            result: Outcome::Git(_),
            ..
        }
    ));
    assert!(recovered.organization.workspaces.is_empty());
}

impl Fixture {
    fn prepare_removal(&mut self, root: &Path) {
        use spaceterm_core::organization_retained::*;
        let observed = host(root, Arc::new(UnixRepositoryProgramRunner::new()))
            .reconcile(&self.intent, &GitReadCancellation::default())
            .unwrap();
        self.persist(GitIntentPhase::ExternalFinished, Some(observed.external));
        let before = self.storage.recovered();
        let revision = before.organization.revision.checked_successor().unwrap();
        let basis = OperationReductionBasis::new(OperationReductionBasisParts {
            binding: self.request.binding(),
            base: before.organization.revision,
            resulting: revision,
        })
        .unwrap();
        let context = TransactionContext::new(
            before.store_revision,
            before.organization.revision,
            revision,
            Some(basis),
        )
        .unwrap();
        let worktree = self.intent.parts().action.worktree();
        let scope = self.intent.parts().repository.scope();
        let outcome = Outcome::Git(
            GitMutationOutcome::new(GitMutationOutcomeParts::Created { worktree, revision })
                .unwrap(),
        );
        let prepared = self
            .storage
            .ledger
            .prepare_completion(
                &self.stream,
                self.request.binding().id,
                self.request.binding().digest,
                CompactOutcome::new(&outcome, &CanonicalCodec).unwrap(),
                revision,
                context,
            )
            .unwrap();
        let mut parts = self.storage.parts();
        parts.resulting_organization_revision = revision;
        prepared.projection().append_to(&mut parts).unwrap();
        parts.operation_writes.extend([
            OperationWrite::ReductionBasis(basis),
            OperationWrite::GitIntent(GitIntentMutation::Complete {
                binding: self.request.binding(),
            }),
            OperationWrite::GitAllocation(GitAllocationMutation::Consume {
                binding: self.request.binding(),
            }),
        ]);
        let RetainedGitAction::Create {
            directory: target, ..
        } = &self.intent.parts().action
        else {
            panic!("creation fixture")
        };
        parts.organization_writes.extend([
            OrganizationWrite::PutRepository(RepositoryRecord {
                id: scope.repository,
                directory: self.intent.parts().repository.parts().main_root.clone(),
            }),
            OrganizationWrite::PutWorktree(WorktreeRecord {
                id: worktree,
                repository: scope.repository,
                directory: target.clone(),
                main: false,
                missing: false,
            }),
            OrganizationWrite::Retained(RetainedOrganizationWrite::SetRepositoryInventoryVersion(
                RepositoryInventoryVersion::new(scope.repository, FieldVersion::new(1)),
            )),
            OrganizationWrite::Retained(RetainedOrganizationWrite::SetWorktreeInventoryFacts(
                WorktreeInventoryFacts::new(worktree, true, FieldVersion::new(1)),
            )),
        ]);
        let transaction =
            DurableTransaction::new_with_validation(parts, &physical::services()).unwrap();
        let disposition = self.storage.store.commit(&transaction);
        assert!(
            matches!(disposition, CommitDisposition::Durable(_)),
            "{disposition:?}"
        );
        self.storage
            .ledger
            .resolve_commit(prepared, disposition)
            .unwrap();
        let old = self.intent.clone();
        self.request = CanonicalOperationRequest::new(
            OperationId::new(self.stream.stream(), OperationSequence::new(2).unwrap()),
            Command::RemoveWorktreeConfirmed {
                scope,
                worktree,
                assessment: AssessmentRef {
                    operation: old.binding().id,
                    digest: old.binding().digest,
                },
                force: false,
            },
            &CanonicalCodec,
        )
        .unwrap();
        let context = self.storage.context();
        let AdmissionPreparation::Prepared(prepared) = self
            .storage
            .ledger
            .prepare_admission(
                &self.stream,
                self.request.binding().id,
                self.request.command(),
                PendingKind::External {
                    effect: ExternalEffectKind::GitWrite,
                },
                context,
            )
            .unwrap()
        else {
            panic!("new removal")
        };
        self.storage.commit(prepared, None);
        let before = self.storage.recovered();
        let reducer = OrganizationReducer::from_recovered(
            RetainedOrganization::new(
                before.organization.clone(),
                before.high_water,
                directory(root),
            ),
            before.store_revision,
            before.last_epoch,
            OrganizationLimits::default(),
        )
        .unwrap();
        let prepared = reducer
            .prepare_git_allocation(
                &GitAllocationRequest {
                    repository: false,
                    worktree: false,
                    fallbacks: vec![],
                },
                &ReductionContext {
                    principal: MutationPrincipal {
                        pairing: self.stream.stream().pairing(),
                        client: self.stream.stream().client(),
                    },
                    operation: self.request.binding().id,
                    request_digest: self.request.binding().digest,
                    now: ReducerTick::new(1000),
                    facts: ReductionFacts::default(),
                },
            )
            .unwrap();
        let mut parts = self.storage.parts();
        prepared.writes().append_to(&mut parts).unwrap();
        let transaction =
            DurableTransaction::new_with_validation(parts, &physical::services()).unwrap();
        let CommitDisposition::Durable(receipt) = self.storage.store.commit(&transaction) else {
            panic!("removal allocation")
        };
        self.storage.ledger.observe_store_commit(receipt).unwrap();
        self.intent = GitWriteIntent::new(GitWriteIntentParts {
            binding: self.request.binding(),
            repository: old.parts().repository.clone(),
            action: RetainedGitAction::Remove {
                worktree,
                directory: before.organization.worktrees[0].directory.clone(),
                head: host(root, Arc::new(UnixRepositoryProgramRunner::new()))
                    .removal_facts(
                        &old.parts().repository,
                        &before.organization.worktrees[0],
                        &GitReadCancellation::default(),
                    )
                    .unwrap()
                    .head,
                approval: ApprovedRemoval::Clean,
            },
            phase: GitIntentPhase::Prepared,
            assessment_digest: GitAssessmentDigest::from_bytes([9; 32]),
            affected: RetainedRemovalScope::empty(scope.server),
            allocation: self.request.binding(),
            external_result: None,
        })
        .unwrap();
        self.persist(GitIntentPhase::Prepared, None);
        self.persist(GitIntentPhase::Started, None);
    }
}
#[test]
fn pending_removal_recovery_never_retires_the_checkout_or_deletes_its_branch() {
    let root = private_root();
    let mut fixture = Fixture::new(root.path());
    fixture.persist(GitIntentPhase::Prepared, None);
    fixture.persist(GitIntentPhase::Started, None);
    run_git(
        root.path(),
        &[
            "worktree",
            "add",
            "-b",
            "topic",
            root.path().join("destination/worktree").to_str().unwrap(),
            "main",
        ],
    );
    fixture.prepare_removal(root.path());
    let binding = fixture.request.binding();
    drop(fixture);
    let (_store, ledger, recovered) = reopen(root.path());
    assert_eq!(recovered.git_intents[0].binding(), binding);
    assert_eq!(
        observe_without_writes(root.path(), &recovered.git_intents[0]),
        RecoveryObservation::PartialOrAmbiguous
    );
    assert!(root.path().join("destination/worktree/.git").is_file());
    assert!(
        root.path()
            .join("repository/.git/refs/heads/topic")
            .is_file()
    );
    assert_eq!(
        ledger
            .lookup(
                &StreamBinding::from_stream(binding.id.stream()),
                binding.id,
                Some(binding.digest)
            )
            .unwrap(),
        OperationLookup::Pending
    );
}

#[test]
fn completed_external_removal_recovery_keeps_original_identity_and_branch() {
    let root = private_root();
    let mut fixture = Fixture::new(root.path());
    fixture.persist(GitIntentPhase::Prepared, None);
    fixture.persist(GitIntentPhase::Started, None);
    run_git(
        root.path(),
        &[
            "worktree",
            "add",
            "-b",
            "topic",
            root.path().join("destination/worktree").to_str().unwrap(),
            "main",
        ],
    );
    fixture.prepare_removal(root.path());
    run_git(
        root.path(),
        &[
            "worktree",
            "remove",
            root.path().join("destination/worktree").to_str().unwrap(),
        ],
    );
    let binding = fixture.request.binding();
    drop(fixture);
    let (_store, ledger, recovered) = reopen(root.path());
    assert_eq!(
        observe_without_writes(root.path(), &recovered.git_intents[0]),
        RecoveryObservation::DesiredStateObserved
    );
    assert!(
        root.path()
            .join("repository/.git/refs/heads/topic")
            .is_file()
    );
    assert_eq!(
        ledger
            .lookup(
                &StreamBinding::from_stream(binding.id.stream()),
                binding.id,
                Some(binding.digest)
            )
            .unwrap(),
        OperationLookup::Pending
    );
}
