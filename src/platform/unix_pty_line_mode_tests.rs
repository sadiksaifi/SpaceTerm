//! Real PTY evidence for the line mode that predictive echo relies on. Each test runs a real
//! program in a PTY, reads the mode from the master as the server does, and checks whether the
//! predictor would show typed keys under it.
use super::*;
use crate::terminal::server_session::shows_keys_under;

const DEADLINE: Duration = Duration::from_secs(20);

/// A program running in a real PTY, with its output collected on a thread.
struct Program {
    pty: SpawnedPty,
    output: Arc<Mutex<Vec<u8>>>,
}

impl Program {
    fn start(program: &str, args: &[&str]) -> Self {
        let mut command = CommandBuilder::new(program);
        command.args(args);
        command.env("TERM", "xterm-256color");
        command.env("BASH_SILENCE_DEPRECATION_WARNING", "1");
        command.env("PS1", "$ ");
        let size = PtySize {
            rows: 24,
            cols: 80,
            ..PtySize::default()
        };
        let (mut pty, _terminator) = spawn_command_in_pty(test_host(), size, command).unwrap();
        let mut reader = pty.take_reader().unwrap();
        let output = Arc::new(Mutex::new(Vec::new()));
        let collected = Arc::clone(&output);
        thread::spawn(move || {
            let mut buffer = [0_u8; 4096];
            while let Ok(read @ 1..) = reader.read(&mut buffer) {
                collected.lock().unwrap().extend_from_slice(&buffer[..read]);
            }
        });
        Self { pty, output }
    }

    /// An interactive zsh without startup files, waiting at its line editor.
    fn zsh() -> Self {
        let zsh = Self::start("/bin/zsh", &["-f", "-i"]);
        zsh.wait_for_mode("the zsh prompt", LineMode::line_editor);
        zsh
    }

    fn write(&mut self, text: &str) {
        self.pty.write_all(text.as_bytes()).unwrap();
        self.pty.flush().unwrap();
    }

    fn output(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }

    fn wait_for_output(&self, marker: &str) {
        let deadline = Instant::now() + DEADLINE;
        while !self.output().contains(marker) {
            assert!(
                Instant::now() < deadline,
                "{marker:?} never appeared in {:?}",
                self.output()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Reads the mode as the server does until `condition` holds.
    fn wait_for_mode(&self, what: &str, condition: impl Fn(LineMode) -> bool) -> LineMode {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let mode = self.pty.line_mode().unwrap();
            if condition(mode) {
                return mode;
            }
            assert!(
                Instant::now() < deadline,
                "{what}: the mode stayed {mode:?}; output {:?}",
                self.output()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn isolated(name: &str) -> Option<std::sync::MutexGuard<'static, ()>> {
    (!isolate_real_pty_test(name)).then(lock_real_pty_test)
}

#[test]
fn zsh_and_bash_prompts_report_the_line_editor_and_predict() {
    const NAME: &str = "platform::unix_pty::line_mode_tests::zsh_and_bash_prompts_report_the_line_editor_and_predict";
    let Some(_lock) = isolated(NAME) else {
        return;
    };
    let mut zsh = Program::zsh();
    let mode = zsh.pty.line_mode().unwrap();
    assert!(mode.line_editor() && !mode.hidden_input(), "{mode:?}");
    assert!(shows_keys_under(mode));
    // A command runs and the shell's line editor reads again.
    zsh.write("echo DONE-$((40 + 2))\r");
    zsh.wait_for_output("DONE-42");
    zsh.wait_for_mode("zsh after a command", LineMode::line_editor);

    let mut bash = Program::start("/bin/bash", &["--norc", "--noprofile", "-i"]);
    let mode = bash.wait_for_mode("the bash prompt", LineMode::line_editor);
    assert!(shows_keys_under(mode));
    bash.write("echo DONE-$((40 + 2))\r");
    bash.wait_for_output("DONE-42");
    bash.wait_for_mode("bash after a command", LineMode::line_editor);
}

#[test]
fn a_dash_prompt_reads_in_cooked_mode_and_never_predicts() {
    const NAME: &str = "platform::unix_pty::line_mode_tests::a_dash_prompt_reads_in_cooked_mode_and_never_predicts";
    let Some(_lock) = isolated(NAME) else {
        return;
    };
    let Some(dash) = ["/bin/dash", "/usr/bin/dash"]
        .into_iter()
        .find(|path| Path::new(path).exists())
    else {
        panic!("dash is missing; macOS and Debian ship it");
    };
    let mut dash = Program::start(dash, &["-i"]);
    dash.write("echo READY-$((1 + 1))\n");
    dash.wait_for_output("READY-2");
    let mode = dash.pty.line_mode().unwrap();
    // dash has no line editor: the kernel collects and echoes the line.
    assert!(
        mode.canonical && mode.echo && mode.shell_foreground,
        "{mode:?}"
    );
    assert!(!mode.line_editor() && !mode.hidden_input());
    assert!(!shows_keys_under(mode));
}

#[test]
fn read_s_hides_input_and_never_predicts_in_zsh_and_bash() {
    const NAME: &str = "platform::unix_pty::line_mode_tests::read_s_hides_input_and_never_predicts_in_zsh_and_bash";
    let Some(_lock) = isolated(NAME) else {
        return;
    };
    for mut shell in [
        Program::zsh(),
        Program::start("/bin/bash", &["--norc", "--noprofile", "-i"]),
    ] {
        shell.wait_for_mode("the prompt", LineMode::line_editor);
        shell.write("read -s secret; echo LENGTH-${#secret}\r");
        let mode = shell.wait_for_mode("read -s", LineMode::hidden_input);
        // The shell itself reads, so it still owns the foreground.
        assert!(mode.shell_foreground && !mode.line_editor(), "{mode:?}");
        assert!(!shows_keys_under(mode));
        shell.write("hunter2\r");
        shell.wait_for_output("LENGTH-7");
        assert!(!shell.output().contains("hunter2"));
    }
}

#[test]
fn password_prompts_of_other_programs_hide_input_and_never_predict() {
    const NAME: &str = "platform::unix_pty::line_mode_tests::password_prompts_of_other_programs_hide_input_and_never_predict";
    let Some(_lock) = isolated(NAME) else {
        return;
    };
    let fixture = crate::terminal::testing::ShellResourcesFixture::new();
    let key = fixture.path().join("key");
    let generated = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "passphrase", "-f"])
        .arg(&key)
        .status()
        .unwrap();
    assert!(generated.success());

    // Python's getpass sets the termios sudo's prompt sets: line mode on, echo off.
    // ssh-keygen asks for a key passphrase the same way.
    let prompts = [
        "python3 -c 'import getpass; getpass.getpass(\"Password:\")'".to_owned(),
        format!("ssh-keygen -y -f '{}'", key.display()),
    ];
    for prompt in prompts {
        let mut zsh = Program::zsh();
        zsh.write(&format!("{prompt}\r"));
        let mode = zsh.wait_for_mode(&prompt, LineMode::hidden_input);
        assert!(!mode.shell_foreground && !mode.line_editor(), "{mode:?}");
        assert!(!shows_keys_under(mode), "{prompt}");
    }
}

#[test]
fn raw_mode_programs_and_foreground_jobs_never_predict() {
    const NAME: &str =
        "platform::unix_pty::line_mode_tests::raw_mode_programs_and_foreground_jobs_never_predict";
    let Some(_lock) = isolated(NAME) else {
        return;
    };
    // vim turns signals off, as ssh does for a remote session.
    let mut zsh = Program::zsh();
    zsh.write("vim -u NONE -N -n\r");
    let mode = zsh.wait_for_mode("vim", |mode| !mode.signals);
    assert!(!mode.line_editor() && !mode.hidden_input(), "{mode:?}");
    assert!(!shows_keys_under(mode));
    zsh.write("\x1b:q!\r");
    zsh.wait_for_mode("zsh after vim", LineMode::line_editor);

    let raw = Program::start(
        "/bin/sh",
        &["-c", "stty raw -echo; printf READY; exec sleep 30"],
    );
    raw.wait_for_output("READY");
    let mode = raw.pty.line_mode().unwrap();
    assert!(!mode.signals && !mode.line_editor(), "{mode:?}");
    assert!(!shows_keys_under(mode));

    // The python REPL reads like a line editor; only the foreground tells it from the shell.
    let mut zsh = Program::zsh();
    zsh.write("python3 -q\r");
    let mode = zsh.wait_for_mode("the python REPL", |mode| !mode.shell_foreground);
    assert!(!mode.line_editor(), "{mode:?}");
    assert!(!shows_keys_under(mode));
}
