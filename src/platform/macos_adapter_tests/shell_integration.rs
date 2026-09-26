//! Native Adapter integration evidence.
use crate::platform::launch_host::resource_root;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn zsh_reports_reserved_and_unicode_metadata_without_changing_protocol_structure() {
    use crate::local_path::LocalPathSemantics;
    use crate::terminal::metadata::{LocalMachine, MetadataTracker};
    use std::time::Instant;

    let integration = resource_root().join("shell-integration/zsh/spaceterm-integration");
    let directory = "/tmp/space #?;%20/हैलो";
    let command = "printf '%s' 'हैलो'; echo %20\n\u{7}\u{1b}]0;forged";
    let output = Command::new("/bin/zsh")
        .args([
            "-dfi", "-c",
            r#"builtin source -- "$1"; PWD=$2; _spaceterm_report_directory; _spaceterm_before_command "$3""#,
            "spaceterm",
        ])
        .arg(integration)
        .arg(directory)
        .arg(command)
        .env("SPACETERM_SHELL_INTEGRATION_VERSION", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let reports = osc_reports(&output.stdout);
    assert_eq!(
        reports.len(),
        2,
        "metadata must not inject additional OSC reports"
    );
    let now = Instant::now();
    let mut tracker = MetadataTracker::new(
        LocalPathSemantics::Posix,
        "/initial",
        "zsh",
        LocalMachine::default(),
        now,
    );
    assert!(tracker.set_reported_directory(reports[0].strip_prefix("7;").unwrap()));
    assert_eq!(&*tracker.snapshot().directory.path, directory);
    assert!(tracker.apply_semantic_prompt(reports[1].strip_prefix("133;").unwrap(), now));
    assert_eq!(
        &*tracker.snapshot().command.as_ref().unwrap().line,
        command
            .chars()
            .filter(|character| !character.is_control())
            .collect::<String>(),
    );
}

#[test]
fn zsh_metadata_encoding_ignores_a_user_defined_printf_function() {
    let integration = resource_root().join("shell-integration/zsh/spaceterm-integration");
    let output = Command::new("/bin/zsh")
        .args([
            "-dfi",
            "-c",
            r#"printf() { builtin print -rn -- compromised; }; builtin source -- "$1"; _spaceterm_encode '/tmp/a b'"#,
            "spaceterm",
        ])
        .arg(integration)
        .env("SPACETERM_SHELL_INTEGRATION_VERSION", "1")
        .output()
        .unwrap();

    assert_eq!(
        (output.status.success(), output.stderr, output.stdout),
        (true, Vec::new(), b"/tmp/a%20b".to_vec()),
    );
}

#[test]
fn bash_reports_reserved_and_unicode_directory_without_losing_exit_status() {
    use crate::local_path::LocalPathSemantics;
    use crate::terminal::metadata::{LocalMachine, MetadataTracker};
    use std::time::Instant;

    let integration = resource_root().join("shell-integration/bash/spaceterm.bash");
    let directory = "/tmp/space #?;%20/हैलो";
    let output = Command::new("/bin/bash")
        .args([
            "--noprofile",
            "--norc",
            "-ic",
            r#"source "$1"; PWD=$2; _spaceterm_command_active=1; (exit 7); _spaceterm_prompt"#,
            "spaceterm",
        ])
        .arg(integration)
        .arg(directory)
        .env("SPACETERM_SHELL_INTEGRATION_VERSION", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    let reports = osc_reports(&output.stdout);
    assert_eq!(reports.len(), 3);
    assert_eq!(reports[0], "133;D;7");
    assert_eq!(reports[2], "133;A;redraw=last");
    let mut tracker = MetadataTracker::new(
        LocalPathSemantics::Posix,
        "/initial",
        "bash",
        LocalMachine::default(),
        Instant::now(),
    );
    assert!(tracker.set_reported_directory(reports[1].strip_prefix("7;").unwrap()));
    assert_eq!(&*tracker.snapshot().directory.path, directory);
}

#[test]
fn bash_encodes_a_long_directory_without_blocking_prompt_rendering() {
    let integration = resource_root().join("shell-integration/bash/spaceterm.bash");
    let directory = format!("/tmp/{}", "a".repeat(4000));
    let mut child = Command::new("/bin/bash")
        .args([
            "--noprofile",
            "--norc",
            "-ic",
            r#"source "$1"; PWD=$2; _spaceterm_prompt"#,
            "spaceterm",
        ])
        .arg(integration)
        .arg(&directory)
        .env("SPACETERM_SHELL_INTEGRATION_VERSION", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "long directory encoding exceeded five seconds: {}",
                String::from_utf8_lossy(&output.stderr),
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();

    assert!(output.status.success(), "{output:?}");
    let reports = osc_reports(&output.stdout);
    assert_eq!(reports[0], format!("7;file://localhost{directory}"));
}

fn osc_reports(bytes: &[u8]) -> Vec<&str> {
    std::str::from_utf8(bytes)
        .unwrap()
        .split("\u{1b}]")
        .skip(1)
        .map(|report| report.split_once('\u{7}').unwrap().0)
        .collect()
}

#[test]
fn sh_reports_reserved_and_unicode_directory_through_its_tty() {
    let integration = resource_root().join("shell-integration/sh/spaceterm.sh");
    let mut command = Command::new("/usr/bin/script");
    command.args([
        "-q",
        "/dev/null",
        "/bin/sh",
        "-ic",
        r#". "$1"; _spaceterm_report_directory"#,
        "spaceterm",
    ]);
    command.arg(integration);
    assert_directory_report(command);
}

#[test]
fn fish_reports_reserved_and_unicode_metadata() {
    let Some(mut command) = installed_shell("fish") else {
        return;
    };
    let integration = resource_root()
        .join("shell-integration/fish/vendor_conf.d/spaceterm-shell-integration.fish");
    command.args([
        "--no-config",
        "-ic",
        r#"source "$argv[1]"; _spaceterm_prompt; _spaceterm_preexec "printf '%s' हैलो; echo %20""#,
    ]);
    command.arg(integration);
    let reports = assert_directory_report(command);
    use crate::local_path::LocalPathSemantics;
    use crate::terminal::metadata::{LocalMachine, MetadataTracker};
    let report = reports
        .iter()
        .find_map(|report| report.strip_prefix("133;C;"))
        .unwrap();
    let now = std::time::Instant::now();
    let mut tracker = MetadataTracker::new(
        LocalPathSemantics::Posix,
        "/initial",
        "fish",
        LocalMachine::default(),
        now,
    );
    assert!(tracker.apply_semantic_prompt(&format!("C;{report}"), now));
    assert_eq!(
        &*tracker.snapshot().command.as_ref().unwrap().line,
        "printf '%s' हैलो; echo %20"
    );
}

#[test]
fn nushell_reports_reserved_and_unicode_directory() {
    let Some(mut command) = installed_shell("nu") else {
        return;
    };
    let integration =
        resource_root().join("shell-integration/nushell/vendor/autoload/spaceterm.nu");
    command.args(["--no-config-file", "-c"]);
    command.arg(format!(
        "source '{}'; use spaceterm install; install; do ($env.config.hooks.pre_prompt | last)",
        integration.display()
    ));
    assert_directory_report(command);
}

#[test]
fn elvish_reports_reserved_and_unicode_directory() {
    let Some(shell) = installed_shell("elvish") else {
        return;
    };
    let integration =
        resource_root().join("shell-integration/elvish/lib/spaceterm-integration.elv");
    let fixture = DirectoryFixture::new();
    let rc = fixture.path().join("rc.elv");
    std::fs::write(
        &rc,
        format!(
            "eval (slurp < '{}'); $edit:before-readline[-1]; exit",
            integration.display()
        ),
    )
    .unwrap();
    let mut command = Command::new("/usr/bin/script");
    command.args(["-q", "/dev/null"]);
    command
        .arg(shell.get_program())
        .arg("-rc")
        .arg(rc)
        .arg("-sock")
        .arg(fixture.path().join("sock"))
        .arg("-db")
        .arg(fixture.path().join("db"));
    assert_directory_report(command);
}

fn installed_shell(name: &str) -> Option<Command> {
    match Command::new(name).arg("--version").output() {
        Ok(_) => Some(Command::new(name)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("{name} is not installed; skipping its runtime protocol check");
            None
        }
        Err(error) => panic!("failed to start {name}: {error}"),
    }
}

fn assert_directory_report(mut command: Command) -> Vec<String> {
    use crate::local_path::LocalPathSemantics;
    use crate::terminal::metadata::{LocalMachine, MetadataTracker};
    use std::time::Instant;

    let fixture = DirectoryFixture::new();
    let directory = fixture.path().join("space #?;%20").join("हैलो");
    std::fs::create_dir_all(&directory).unwrap();
    let directory = directory.canonicalize().unwrap();
    let output = command
        .current_dir(&directory)
        .env("SPACETERM_SHELL_INTEGRATION_VERSION", "1")
        .env_remove("_SPACETERM_INTEGRATION_LOADED")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let reports = osc_reports(&output.stdout);
    let directory_report = reports
        .iter()
        .find_map(|report| report.strip_prefix("7;"))
        .expect("shell must report its directory");
    let mut tracker = MetadataTracker::new(
        LocalPathSemantics::Posix,
        "/initial",
        "shell",
        LocalMachine::default(),
        Instant::now(),
    );
    assert!(
        tracker.set_reported_directory(directory_report),
        "{directory_report}"
    );
    assert_eq!(
        &*tracker.snapshot().directory.path,
        directory.to_str().unwrap()
    );
    reports.into_iter().map(str::to_owned).collect()
}

struct DirectoryFixture(std::path::PathBuf);

impl DirectoryFixture {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "spaceterm-shell-protocol-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for DirectoryFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn every_supported_resource_uses_the_same_version_and_protocol_marks() {
    let root = resource_root().join("shell-integration");
    assert_eq!(
        std::fs::read_to_string(root.join("VERSION"))
            .unwrap()
            .trim(),
        "1"
    );
    for relative in [
        "bash/spaceterm.bash",
        "elvish/lib/spaceterm-integration.elv",
        "fish/vendor_conf.d/spaceterm-shell-integration.fish",
        "nushell/vendor/autoload/spaceterm.nu",
        "zsh/spaceterm-integration",
    ] {
        let script = std::fs::read_to_string(root.join(relative)).unwrap();
        assert!(
            script.contains("133;"),
            "{relative} must emit OSC 133 marks"
        );
        assert!(
            script.contains("SPACETERM_SHELL_INTEGRATION_VERSION"),
            "{relative} must verify the resource handshake"
        );
        let redraw = if relative.starts_with("bash/") {
            "133;A;redraw=last"
        } else {
            "133;A;redraw=1"
        };
        assert!(script.contains(redraw), "{relative} must state its prompt redraw policy");
    }
}

#[test]
fn zsh_prompt_hook_renders_protocol_marker_once_without_changing_printable_prompt() {
    let integration = resource_root().join("shell-integration/zsh/spaceterm-integration");
    let output = Command::new("/bin/zsh")
        .args([
            "-dfi",
            "-c",
            r#"PS1='SPACE> '; builtin source -- "$1"; _spaceterm_command_active=1; (builtin exit 7); "$precmd_functions[-1]"; "$precmd_functions[-1]"; builtin print -nrP -- "$PS1"; builtin print -nrP -- "$PS1""#,
            "spaceterm",
        ])
        .arg(integration)
        .env("SPACETERM_SHELL_INTEGRATION_VERSION", "1")
        .output()
        .unwrap();
    let completion = b"\x1b]133;D;7\x07";
    let prompt_start = b"\x1b]133;A;redraw=1\x07";
    let prompt_marker = b"\x1b]133;B\x07";
    let literal_prompt_marker = br"\e]133;B\a";
    let reported_prior_status = output
        .stdout
        .windows(completion.len())
        .any(|window| window == completion);
    let rendered_prompt_markers = output
        .stdout
        .windows(prompt_marker.len())
        .filter(|window| *window == prompt_marker)
        .count();
    let redrawable_prompt_starts = output
        .stdout
        .windows(prompt_start.len())
        .filter(|window| *window == prompt_start)
        .count();
    let rendered_literal_marker = output
        .stdout
        .windows(literal_prompt_marker.len())
        .any(|window| window == literal_prompt_marker);
    let rendered_prompt_is_preserved = output.stdout.ends_with(
        b"\x1b]133;A;redraw=1\x07SPACE> \x1b]133;B\x07\x1b]133;A;redraw=1\x07SPACE> \x1b]133;B\x07",
    );

    assert_eq!(
        (
            output.status.success(),
            output.stderr.as_slice(),
            reported_prior_status,
            redrawable_prompt_starts,
            rendered_prompt_markers,
            rendered_literal_marker,
            rendered_prompt_is_preserved,
        ),
        (true, &[][..], true, 2, 2, false, true),
        "stdout: {:?}",
        String::from_utf8_lossy(&output.stdout),
    );
}
