//! Native evidence that the remote repository scripts read real repositories through `/bin/sh`.
use super::*;
use crate::platform::unix_adapter_tests::short_temporary_root;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Where the fixtures look for git, in the order a person's PATH usually lists them.
const GIT_DIRECTORIES: [&str; 4] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];

/// One temporary home and the git that builds its repositories.
struct GitFixture {
    root: PathBuf,
    home: PathBuf,
    search_path: String,
}

impl GitFixture {
    /// Returns `None` when the host has no runnable git.
    fn new(name: &str) -> Option<Self> {
        let git_directory = GIT_DIRECTORIES.into_iter().find(|directory| {
            let git = Path::new(directory).join("git");
            git.is_file()
                && Command::new(&git)
                    .arg("--version")
                    .env_clear()
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success())
        })?;
        let root = short_temporary_root().join(format!(
            "spaceterm-repository-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        Some(Self {
            root,
            home,
            search_path: format!("{git_directory}:/usr/bin:/bin"),
        })
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// Runs setup git with a private identity and no system or global configuration.
    fn git(&self, directory: &Path, arguments: &[&str]) -> Output {
        Command::new("git")
            .args(arguments)
            .current_dir(directory)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.search_path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "SpaceTerm")
            .env("GIT_AUTHOR_EMAIL", "spaceterm@example.invalid")
            .env("GIT_COMMITTER_NAME", "SpaceTerm")
            .env("GIT_COMMITTER_EMAIL", "spaceterm@example.invalid")
            .env("GIT_EDITOR", "true")
            // Setup never fetches a partial clone's missing objects behind a test's back.
            .env("GIT_NO_LAZY_FETCH", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn checked_git(&self, directory: &Path, arguments: &[&str]) {
        let output = self.git(directory, arguments);
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Creates a repository on `main` with one committed file.
    fn repository(&self, relative: &str) -> PathBuf {
        let repository = self.path(relative);
        fs::create_dir_all(&repository).unwrap();
        self.checked_git(&repository, &["init", "-q", "-b", "main"]);
        self.checked_git(&repository, &["config", "commit.gpgsign", "false"]);
        fs::write(repository.join("tracked"), b"base\n").unwrap();
        self.checked_git(&repository, &["add", "tracked"]);
        self.checked_git(&repository, &["commit", "-q", "-m", "base"]);
        repository
    }

    fn run(&self, script: &[u8]) -> Output {
        self.run_with_search_path(script, &self.search_path)
    }

    fn run_with_search_path(&self, script: &[u8], search_path: &str) -> Output {
        let mut child = Command::new("/bin/sh")
            .current_dir(&self.root)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", search_path)
            .env("TMPDIR", &self.root)
            // Inherited repository selection must not survive the script's own reset.
            .env("GIT_DIR", self.path("elsewhere"))
            .env("GIT_CONFIG_PARAMETERS", "'core.fsmonitor'='true'")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(script).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "repository script failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn probe(&self, directory: &Path) -> RemoteRepositoryProbe {
        let script = build_repository_script(
            REPOSITORY_PROBE_KIND,
            directory.to_str().unwrap(),
            FsmonitorPolicy::Disabled,
        )
        .unwrap();
        parse_repository_probe(&self.run(&script).stdout).unwrap()
    }

    fn count(&self, root: &Path, fsmonitor: FsmonitorPolicy) -> RemoteRepositoryCount {
        let script =
            build_repository_script(REPOSITORY_COUNT_KIND, root.to_str().unwrap(), fsmonitor)
                .unwrap();
        parse_repository_count(&self.run(&script).stdout).unwrap()
    }

    /// Every script removes its private staging directory.
    fn assert_no_staging_left(&self) {
        let staged = fs::read_dir(&self.root)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("spaceterm-repository.")
            });
        assert!(!staged, "the script left its staging directory behind");
    }
}

impl Drop for GitFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.split(|byte| *byte == b'\n').collect()
}

fn records(bytes: &[u8]) -> Vec<&[u8]> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .collect()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn generated_repository_scripts_should_be_valid_posix_shell_syntax() {
    for script in [
        build_repository_script(
            REPOSITORY_PROBE_KIND,
            "~/space ' $(touch x)",
            FsmonitorPolicy::Disabled,
        )
        .unwrap(),
        build_repository_script(REPOSITORY_COUNT_KIND, "/srv/repo", FsmonitorPolicy::Builtin)
            .unwrap(),
        build_repository_script(
            REPOSITORY_COUNT_KIND,
            "/srv/repo",
            FsmonitorPolicy::Disabled,
        )
        .unwrap(),
    ] {
        let mut child = Command::new("/bin/sh")
            .arg("-n")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&script).unwrap();
        let output = child.wait_with_output().unwrap();

        assert!(
            output.status.success(),
            "generated script failed syntax validation: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn repository_probe_should_read_a_work_tree_without_walking_it() {
    let Some(fixture) = GitFixture::new("plain") else {
        return;
    };
    let repository = fixture.repository("work tree's $(touch injected)");
    let subdirectory = repository.join("sub");
    fs::create_dir_all(&subdirectory).unwrap();
    fixture.checked_git(
        &repository,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/owner/repo.git",
        ],
    );
    fixture.checked_git(
        &repository,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    fixture.checked_git(&repository, &["config", "branch.main.remote", "origin"]);
    fixture.checked_git(
        &repository,
        &["config", "branch.main.merge", "refs/heads/main"],
    );
    fs::write(repository.join("tracked"), b"ahead\n").unwrap();
    fixture.checked_git(&repository, &["commit", "-q", "-am", "ahead"]);
    fs::write(repository.join("tracked"), b"modified\n").unwrap();
    fs::write(repository.join("untracked"), b"new\n").unwrap();

    let probe = fixture.probe(&subdirectory);

    assert_eq!(probe.outcome, RemoteProbeOutcome::Repository);
    assert!(probe.discovery_succeeded);
    assert!(probe.git_version.starts_with(b"git version "));
    let repository_spelling = repository.to_str().unwrap().as_bytes();
    let git_directory = repository.join(".git");
    assert_eq!(
        lines(&probe.discovery),
        [
            b"false".as_slice(),
            b"false",
            git_directory.to_str().unwrap().as_bytes(),
            b"../.git",
            repository_spelling,
            b"sub/",
            b"",
        ]
    );
    assert_eq!(
        probe.physical_home,
        fs::canonicalize(&fixture.home)
            .unwrap()
            .to_str()
            .unwrap()
            .as_bytes()
    );
    let headers = records(&probe.status_headers);
    assert!(headers.iter().all(|record| record.starts_with(b"# ")));
    assert!(
        headers
            .iter()
            .any(|record| record.starts_with(b"# branch.oid "))
    );
    assert!(headers.contains(&b"# branch.head main".as_slice()));
    assert!(headers.contains(&b"# branch.upstream origin/main".as_slice()));
    assert!(headers.contains(&b"# branch.ab +1 -0".as_slice()));
    assert_eq!(
        records(&probe.config),
        [
            b"branch.main.remote\norigin".as_slice(),
            b"branch.main.merge\nrefs/heads/main",
            b"remote.origin.url\nhttps://example.invalid/owner/repo.git",
        ]
    );
    assert_eq!(probe.markers, OperationMarkers::default());
    assert!(!fixture.path("injected").exists());
    fixture.assert_no_staging_left();
}

#[test]
fn repository_probe_should_report_git_directories_and_bare_repositories_for_hiding() {
    let Some(fixture) = GitFixture::new("hidden") else {
        return;
    };
    let repository = fixture.repository("work");
    let bare = fixture.path("bare.git");
    fs::create_dir_all(&bare).unwrap();
    fixture.checked_git(&bare, &["init", "-q", "--bare"]);

    let inside_git_directory = fixture.probe(&repository.join(".git"));
    let bare_repository = fixture.probe(&bare);

    assert_eq!(inside_git_directory.outcome, RemoteProbeOutcome::Repository);
    assert!(!inside_git_directory.discovery_succeeded);
    assert!(inside_git_directory.discovery.starts_with(b"true\nfalse\n"));
    assert!(inside_git_directory.status_headers.is_empty());
    assert_eq!(bare_repository.outcome, RemoteProbeOutcome::Repository);
    assert!(!bare_repository.discovery_succeeded);
    assert!(bare_repository.discovery.starts_with(b"true\ntrue\n"));
}

#[test]
fn repository_probe_should_classify_missing_directories_repositories_and_git() {
    let Some(fixture) = GitFixture::new("outcomes") else {
        return;
    };
    let plain = fixture.path("plain");
    fs::create_dir_all(&plain).unwrap();
    let empty_bin = fixture.path("empty-bin");
    fs::create_dir_all(&empty_bin).unwrap();
    let repository = fixture.repository("work");

    let not_repository = fixture.probe(&plain);
    let missing = fixture.probe(&fixture.path("missing"));
    let script = build_repository_script(
        REPOSITORY_PROBE_KIND,
        repository.to_str().unwrap(),
        FsmonitorPolicy::Disabled,
    )
    .unwrap();
    let git_missing = parse_repository_probe(
        &fixture
            .run_with_search_path(&script, empty_bin.to_str().unwrap())
            .stdout,
    )
    .unwrap();

    assert_eq!(not_repository.outcome, RemoteProbeOutcome::NotRepository);
    assert!(not_repository.discovery.is_empty());
    assert_eq!(missing.outcome, RemoteProbeOutcome::DirectoryUnavailable);
    assert_eq!(git_missing.outcome, RemoteProbeOutcome::GitMissing);
    fixture.assert_no_staging_left();
}

#[test]
fn repository_probe_should_read_rebase_markers() {
    let Some(fixture) = GitFixture::new("rebase") else {
        return;
    };
    let repository = fixture.repository("work");
    fixture.checked_git(&repository, &["checkout", "-q", "-b", "topic"]);
    fs::write(repository.join("tracked"), b"topic\n").unwrap();
    fixture.checked_git(&repository, &["commit", "-q", "-am", "topic"]);
    fixture.checked_git(&repository, &["checkout", "-q", "main"]);
    fs::write(repository.join("tracked"), b"main\n").unwrap();
    fixture.checked_git(&repository, &["commit", "-q", "-am", "main"]);
    fixture.checked_git(&repository, &["checkout", "-q", "topic"]);
    let rebase = fixture.git(&repository, &["rebase", "--merge", "main"]);
    assert!(
        !rebase.status.success(),
        "the rebase must stop on a conflict"
    );

    let probe = fixture.probe(&repository);

    assert_eq!(
        probe.markers,
        OperationMarkers {
            rebase_merge: Some(StepMarkers {
                current: Some(b"1\n".to_vec()),
                total: Some(b"1\n".to_vec()),
            }),
            ..OperationMarkers::default()
        }
    );
}

#[test]
fn repository_probe_should_read_only_regular_markers_within_the_limit() {
    let Some(fixture) = GitFixture::new("marker-links") else {
        return;
    };
    let repository = fixture.repository("work");
    let git_directory = repository.join(".git");
    let outside = fixture.path("outside-rebase");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("msgnum"), b"2\n").unwrap();
    fs::write(outside.join("end"), b"5\n").unwrap();
    symlink(&outside, git_directory.join("rebase-merge")).unwrap();
    symlink(git_directory.join("HEAD"), git_directory.join("MERGE_HEAD")).unwrap();
    let apply = git_directory.join("rebase-apply");
    fs::create_dir_all(&apply).unwrap();
    symlink(outside.join("msgnum"), apply.join("next")).unwrap();
    fs::write(apply.join("last"), [b'9'; MAXIMUM_MARKER_BYTES]).unwrap();
    symlink(outside.join("end"), apply.join("rebasing")).unwrap();
    fs::write(apply.join("applying"), b"").unwrap();
    fs::write(git_directory.join("BISECT_LOG"), b"").unwrap();

    let linked = fixture.probe(&repository);
    fs::remove_file(apply.join("next")).unwrap();
    let fifo = Command::new("mkfifo")
        .arg(apply.join("next"))
        .status()
        .unwrap();
    assert!(fifo.success());
    fs::write(apply.join("last"), [b'9'; MAXIMUM_MARKER_BYTES + 1]).unwrap();
    let irregular = fixture.probe(&repository);

    assert_eq!(
        linked.markers,
        OperationMarkers {
            rebase_apply: Some(ApplyMarkers {
                step: StepMarkers {
                    current: None,
                    total: Some(vec![b'9'; MAXIMUM_MARKER_BYTES]),
                },
                rebasing: false,
                applying: true,
            }),
            bisect_log: true,
            ..OperationMarkers::default()
        }
    );
    assert_eq!(
        irregular.markers.rebase_apply.unwrap().step,
        StepMarkers::default()
    );
}

#[test]
fn repository_reads_should_never_run_a_hook_program_fsmonitor() {
    let Some(fixture) = GitFixture::new("fsmonitor-hook") else {
        return;
    };
    let repository = fixture.repository("work");
    let ran = fixture.path("fsmonitor-ran");
    let hook = fixture.path("fsmonitor-hook");
    fs::write(
        &hook,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", ran.display()),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).unwrap();
    fixture.checked_git(
        &repository,
        &["config", "core.fsmonitor", hook.to_str().unwrap()],
    );
    fs::write(repository.join("tracked"), b"modified\n").unwrap();

    let probe = fixture.probe(&repository);
    let count = fixture.count(&repository, FsmonitorPolicy::Disabled);

    assert!(!ran.exists(), "a hook-program fsmonitor ran");
    let hook_record = format!("core.fsmonitor\n{}", hook.display());
    assert_eq!(records(&probe.config), [hook_record.as_bytes()]);
    assert!(
        records(&count.status)
            .iter()
            .any(|record| record.starts_with(b"1 .M "))
    );
}

#[test]
fn repository_count_should_not_fetch_through_a_transport_the_repository_allows() {
    let Some(fixture) = GitFixture::new("transport") else {
        return;
    };
    let source = fixture.path("source");
    fs::create_dir_all(&source).unwrap();
    fixture.checked_git(&source, &["init", "-q", "-b", "main"]);
    fixture.checked_git(&source, &["config", "uploadpack.allowFilter", "true"]);
    let lines: String = (1..=200).map(|line| format!("{line}\n")).collect();
    fs::write(source.join("renamed"), &lines).unwrap();
    fixture.checked_git(&source, &["add", "renamed"]);
    fixture.checked_git(&source, &["commit", "-q", "-m", "base"]);
    let url = format!("file://{}", source.display());
    fixture.checked_git(
        &fixture.root,
        &[
            "clone",
            "-q",
            "--filter=blob:none",
            "--no-checkout",
            &url,
            "clone",
        ],
    );
    let clone = fixture.path("clone");
    fixture.checked_git(&clone, &["config", "protocol.file.allow", "always"]);
    fixture.checked_git(&clone, &["read-tree", "HEAD"]);
    // A staged near-identical rename makes status compare against the missing blob.
    fs::write(clone.join("moved"), format!("{lines}extra\n")).unwrap();
    fixture.checked_git(&clone, &["update-index", "--add", "moved"]);
    fixture.checked_git(&clone, &["update-index", "--force-remove", "renamed"]);
    let promised = fixture.git(&clone, &["rev-parse", "HEAD:renamed"]).stdout;
    let promised = String::from_utf8(promised).unwrap();
    let missing = || {
        !fixture
            .git(&clone, &["cat-file", "-e", promised.trim()])
            .status
            .success()
    };
    assert!(missing(), "the fixture's blob should start out missing");
    // Git before 2.44 ignores `GIT_NO_LAZY_FETCH`.
    let script = String::from_utf8(
        build_repository_script(
            REPOSITORY_COUNT_KIND,
            clone.to_str().unwrap(),
            FsmonitorPolicy::Disabled,
        )
        .unwrap(),
    )
    .unwrap()
    .replace("GIT_NO_LAZY_FETCH=1", "GIT_NO_LAZY_FETCH=");

    fixture.run(script.as_bytes());

    assert!(
        missing(),
        "the count fetched through the repository's transport"
    );
    fixture.assert_no_staging_left();
}

/// Runs `script` as sshd would, in a shell that leads its own process group, and returns how long
/// it took.
fn run_as_session_leader(fixture: &GitFixture, script: &[u8]) -> Duration {
    use std::os::unix::process::CommandExt;

    let started = Instant::now();
    let mut child = Command::new("/bin/sh")
        .current_dir(&fixture.root)
        .env_clear()
        .env("HOME", &fixture.home)
        .env("PATH", &fixture.search_path)
        .env("TMPDIR", &fixture.root)
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(script).unwrap();
    child.wait().unwrap();
    started.elapsed()
}

fn process_exists(process: libc::pid_t) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(process, 0) == 0 }
}

#[test]
fn repository_count_should_end_every_program_git_started_at_its_remote_deadline() {
    let Some(fixture) = GitFixture::new("deadline") else {
        return;
    };
    let repository = fixture.repository("work");
    let filter = fixture.path("blocking-filter");
    let filter_process = fixture.path("filter-process");
    fs::write(
        &filter,
        format!(
            "#!/bin/sh\necho $$ > '{}'\nexec sleep 30\n",
            filter_process.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&filter, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        repository.join(".gitattributes"),
        b"tracked filter=blocking\n",
    )
    .unwrap();
    fixture.checked_git(
        &repository,
        &["config", "filter.blocking.clean", filter.to_str().unwrap()],
    );
    // A newer file of the same size makes status read it through the clean filter.
    std::thread::sleep(Duration::from_millis(1100));
    fs::write(repository.join("tracked"), b"next\n").unwrap();
    let script = String::from_utf8(
        build_repository_script(
            REPOSITORY_COUNT_KIND,
            repository.to_str().unwrap(),
            FsmonitorPolicy::Disabled,
        )
        .unwrap(),
    )
    .unwrap()
    .replace("sleep 50 &", "sleep 1 &");

    let elapsed = run_as_session_leader(&fixture, script.as_bytes());

    assert!(
        elapsed < Duration::from_secs(20),
        "the script outlived its deadline"
    );
    let process: libc::pid_t = fs::read_to_string(&filter_process)
        .expect("the clean filter should have started")
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_exists(process) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !process_exists(process),
        "the clean filter outlived the read"
    );
    fixture.assert_no_staging_left();
}

#[test]
fn repository_count_should_respect_untracked_configuration_and_keep_raw_names() {
    let Some(fixture) = GitFixture::new("count") else {
        return;
    };
    let repository = fixture.repository("work");
    fs::write(repository.join("line\nbreak"), b"new\n").unwrap();

    let shown = fixture.count(&repository, FsmonitorPolicy::Disabled);
    fixture.checked_git(&repository, &["config", "status.showUntrackedFiles", "no"]);
    let hidden = fixture.count(&repository, FsmonitorPolicy::Disabled);

    assert!(!shown.truncated);
    assert!(contains(&shown.status, b"\0? line\nbreak\0"));
    assert!(records(&shown.status)[0].starts_with(b"# branch.oid "));
    assert!(!hidden.truncated);
    assert!(
        !records(&hidden.status)
            .iter()
            .any(|record| record.starts_with(b"? "))
    );
    fixture.assert_no_staging_left();
}

#[test]
fn repository_count_should_cut_large_status_output_at_its_limit() {
    let Some(fixture) = GitFixture::new("count-limit") else {
        return;
    };
    let repository = fixture.repository("work");
    let padding = "x".repeat(96);
    let files = MAXIMUM_REMOTE_REPOSITORY_STATUS_BYTES / padding.len() + 64;
    for index in 0..files {
        fs::write(repository.join(format!("{padding}-{index:05}")), b"").unwrap();
    }

    let count = fixture.count(&repository, FsmonitorPolicy::Disabled);

    assert!(count.truncated);
    assert_eq!(count.status.len(), MAXIMUM_REMOTE_REPOSITORY_STATUS_BYTES);
    fixture.assert_no_staging_left();
}

#[test]
fn repository_count_should_report_a_vanished_root() {
    let Some(fixture) = GitFixture::new("count-missing") else {
        return;
    };
    let script = build_repository_script(
        REPOSITORY_COUNT_KIND,
        fixture.path("missing").to_str().unwrap(),
        FsmonitorPolicy::Disabled,
    )
    .unwrap();

    assert_eq!(
        parse_repository_count(&fixture.run(&script).stdout).unwrap_err(),
        RemoteUtilityError::Missing
    );
}
