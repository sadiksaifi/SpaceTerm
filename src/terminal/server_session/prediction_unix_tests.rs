//! Predictive echo end to end against real programs in a real PTY. A small server loop feeds the
//! program's output through the Terminal Emulator, reads the line mode from the PTY master and
//! hands both to the predictor in the server's order. Each test types keys one at a time and
//! records what the view would draw before the key reaches the program.
use super::tests::{key, keys, shown};
use super::*;
use crate::platform::unix_pty::test_program::TestProgram;
use crate::platform::unix_pty::{isolate_real_pty_test, lock_real_pty_test};
use crate::terminal::emulator::TerminalEmulator;
use crate::terminal::geometry::{BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry};
use spaceterm_server::pane::LineMode;

/// The round trip the virtual clock gives every frame, above the prediction threshold.
const ROUND_TRIP: Duration = Duration::from_millis(80);
/// How long a program stays silent before its output counts as complete.
const QUIET: Duration = Duration::from_millis(120);

struct Server {
    program: TestProgram,
    emulator: TerminalEmulator,
    /// Output bytes the emulator has received.
    fed: usize,
    line: LineState,
    /// The highest Input serial written to the PTY.
    written: u32,
    screen: Arc<ScreenSnapshot>,
    predictor: Predictor,
    now: Instant,
    /// What the view drew right after each key, before the key reached the program.
    views: Vec<(String, String)>,
}

impl Server {
    fn start(program: &str, args: &[&str]) -> Self {
        let geometry = TerminalGeometry::from_grid(
            CellGridSize::new(TestProgram::COLUMNS, TestProgram::ROWS),
            LogicalCellSize::new(8.0, 16.0),
            BackingScale::ONE,
        );
        let mut emulator = TerminalEmulator::new(geometry).unwrap();
        let screen = emulator.snapshot().unwrap().unwrap();
        Self {
            program: TestProgram::start(program, args),
            emulator,
            fed: 0,
            line: LineState::default(),
            written: 0,
            screen,
            predictor: Predictor::default(),
            now: Instant::now(),
            views: Vec::new(),
        }
    }

    /// An interactive shell waiting at its prompt.
    fn shell(program: &str, args: &[&str]) -> Self {
        let mut server = Self::start(program, args);
        server
            .program
            .wait_for_mode("the prompt", LineMode::line_editor);
        server.settle();
        server
    }

    fn zsh() -> Self {
        Self::shell("/bin/zsh", &["-f", "-i"])
    }

    fn bash() -> Self {
        Self::shell("/bin/bash", &["--norc", "--noprofile", "-i"])
    }

    /// Publishes the output up to byte `end` as the server does: the line mode first, then the
    /// frame captured after it, one round trip after the last key.
    fn publish_through(&mut self, end: usize) {
        let output = self.program.output();
        self.emulator.feed(&output[self.fed..end]);
        self.fed = end;
        let responses = self.emulator.take_pty_responses();
        if !responses.is_empty() {
            self.program.write(&responses);
        }
        let mode = self.program.line_mode();
        if mode != self.line.mode {
            self.line = LineState {
                epoch: self.line.epoch + 1,
                mode,
            };
            self.predictor.line(self.line);
        }
        self.now += ROUND_TRIP;
        // The server sends a frame only when something on screen changed, not when only the
        // Input serial moved.
        if let Some(screen) = self.emulator.snapshot().unwrap()
            && (screen.rows != self.screen.rows || screen.cursor != self.screen.cursor)
        {
            self.screen = screen;
            self.predictor.frame(&self.screen, self.written, self.now);
        }
        // The worker expires overdue predictions on each turn.
        self.predictor.expire(self.now);
    }

    /// The person pauses for `pause`. The worker wakes at the predictor's deadline, so the view
    /// is recorded after any overdue prediction expired.
    fn pause(&mut self, pause: Duration) {
        self.now += pause;
        self.predictor.expire(self.now);
        self.views
            .push((String::new(), shown(&self.predictor, &self.screen)));
    }

    /// Publishes everything the program writes until it goes quiet.
    fn settle(&mut self) {
        let end = self.program.settle(QUIET).len();
        self.publish_through(end);
    }

    /// Sends one key: the predictor sees it sent, the view is recorded, then the PTY gets it.
    fn send(&mut self, text: &str) {
        self.written += 1;
        let request = keys(vec![key(text, 0)]);
        self.predictor.sent(&request, Some(self.written), self.now);
        self.views
            .push((text.into(), shown(&self.predictor, &self.screen)));
        self.program.write(text.as_bytes());
    }

    /// Types `text` one key at a time, publishing the program's echo after each.
    fn type_text(&mut self, text: &str) {
        for character in text.chars() {
            self.send(&character.to_string());
            self.settle();
        }
    }

    fn enter(&mut self) {
        self.type_text("\r");
    }

    /// Types a command at the prompt. The prompt predicts its later keys, which also confirms
    /// that prediction is active before the keys that matter.
    fn command(&mut self, command: &str) {
        let first = self.views.len();
        self.type_text(command);
        let predicted = self.views[first..]
            .iter()
            .filter(|(key, view)| view.ends_with(key.as_str()))
            .count();
        assert!(
            predicted * 2 > command.len(),
            "the prompt predicted only {predicted} of {command:?}: {:?}",
            &self.views[first..]
        );
        self.enter();
    }

    /// Types `secret` and asserts that the view never draws any of it.
    fn type_unseen(&mut self, secret: &str) {
        let first = self.views.len();
        self.type_text(secret);
        let drawn: Vec<_> = self.views[first..]
            .iter()
            .filter(|(_, view)| !view.is_empty())
            .collect();
        assert!(drawn.is_empty(), "the view drew {drawn:?}");
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.program.output()).into_owned()
    }
}

fn isolated(name: &str) -> Option<std::sync::MutexGuard<'static, ()>> {
    (!isolate_real_pty_test(name)).then(lock_real_pty_test)
}

const PREFIX: &str = "terminal::server_session::prediction::unix_tests::";

#[test]
fn zsh_and_bash_prompts_show_typed_keys_before_their_echo() {
    let Some(_lock) = isolated(&format!(
        "{PREFIX}zsh_and_bash_prompts_show_typed_keys_before_their_echo"
    )) else {
        return;
    };
    for mut shell in [Server::zsh(), Server::bash()] {
        shell.command("echo hello-world");
        shell.program.wait_for_output("hello-world\r\n");
        shell.settle();
        assert_eq!(shown(&shell.predictor, &shell.screen), "");
    }
}

#[test]
fn a_dash_prompt_never_predicts() {
    let Some(_lock) = isolated(&format!("{PREFIX}a_dash_prompt_never_predicts")) else {
        return;
    };
    let dash = ["/bin/dash", "/usr/bin/dash"]
        .into_iter()
        .find(|path| std::path::Path::new(path).exists())
        .expect("macOS and Debian ship dash");
    let mut dash = Server::start(dash, &["-i"]);
    dash.program.wait_for_output("$ ");
    dash.settle();
    // dash has no line editor: the kernel collects and echoes the line.
    dash.type_unseen("echo hi");
}

#[test]
fn read_s_never_shows_the_typed_secret_in_zsh_and_bash() {
    let Some(_lock) = isolated(&format!(
        "{PREFIX}read_s_never_shows_the_typed_secret_in_zsh_and_bash"
    )) else {
        return;
    };
    for mut shell in [Server::zsh(), Server::bash()] {
        shell.command("read -s secret; echo LENGTH-${#secret}");
        shell
            .program
            .wait_for_mode("read -s", LineMode::hidden_input);
        shell.settle();
        shell.type_unseen("hunter2");
        shell.enter();
        shell.program.wait_for_output("LENGTH-7");
        assert!(!shell.text().contains("hunter2"));
    }
}

/// bash's `read -rsn4` and zsh's `read -sk4` read in the line editor's own mode, with the shell
/// in the foreground. The first key arrives before the prompt, at the cell where the prompt
/// starts with the same character, which must not confirm the epoch.
#[test]
fn a_silent_read_in_the_line_editors_mode_never_shows_its_keys() {
    let Some(_lock) = isolated(&format!(
        "{PREFIX}a_silent_read_in_the_line_editors_mode_never_shows_its_keys"
    )) else {
        return;
    };
    for (mut shell, command) in [
        (Server::bash(), "read -rsn4 -p 'PIN: ' pin; echo GOT-$pin"),
        (Server::zsh(), "read -sk4 'pin?PIN: '; echo GOT-$pin"),
    ] {
        shell.command_without_enter(command);
        shell.send("\r");
        shell.wait_for_new_output("PIN: ");
        shell
            .program
            .wait_for_mode("the silent read", LineMode::line_editor);
        // The frame that reflects Enter shows the new, empty line.
        let output = shell.program.output();
        let newline = output[shell.fed..]
            .iter()
            .position(|byte| *byte == b'\n')
            .expect("Enter starts a new line");
        shell.publish_through(shell.fed + newline + 1);
        assert_eq!(
            shell.line.mode,
            LineMode {
                canonical: false,
                echo: false,
                signals: true,
                shell_foreground: true,
            },
            "the read looks like the line editor"
        );

        let first = shell.views.len();
        shell.send("P");
        shell.settle();
        shell.type_text("123");
        let drawn: Vec<_> = shell.views[first..]
            .iter()
            .filter(|(_, view)| !view.is_empty())
            .collect();
        assert!(drawn.is_empty(), "the view drew {drawn:?}");
        shell.program.wait_for_output("GOT-P123");
    }
}

/// A function echoes one key it read silently, then reads a PIN silently, both in the line
/// editor's mode. The echo confirms the epoch, and the mode never changes where the server can
/// see it. A PIN key that shows is withdrawn when its echo is overdue, and later keys stay hidden.
#[test]
fn a_pin_read_silently_after_an_echoed_key_is_withdrawn_and_stays_hidden() {
    let Some(_lock) = isolated(&format!(
        "{PREFIX}a_pin_read_silently_after_an_echoed_key_is_withdrawn_and_stays_hidden"
    )) else {
        return;
    };
    for (mut shell, function) in [
        (
            Server::zsh(),
            "f() { read -sk1 'a?Continue? '; print -n -- $a; read -sk4 pin; echo; echo GOT-$pin }",
        ),
        (
            Server::bash(),
            "f() { read -rsn1 -p 'Continue? ' a; printf %s \"$a\"; read -rsn4 pin; echo; echo GOT-$pin; }",
        ),
    ] {
        shell.command(function);
        shell.settle();
        shell.command_without_enter("f");
        shell.send("\r");
        shell.wait_for_new_output("Continue? ");
        shell
            .program
            .wait_for_mode("the first read", LineMode::line_editor);
        shell.settle();
        shell.send("y");
        shell.settle();

        let first = shell.views.len();
        for key in ["1", "2", "3", "4"] {
            shell.send(key);
            shell.settle();
            shell.pause(Duration::from_millis(250));
        }
        // Only the first PIN key may show, until its echo is overdue.
        let drawn: Vec<_> = shell.views[first + 1..]
            .iter()
            .filter(|(_, view)| !view.is_empty())
            .collect();
        assert!(drawn.is_empty(), "the view drew {drawn:?}");
        assert!(
            shell.views[first].1.is_empty() || shell.views[first].1 == "1",
            "{:?}",
            shell.views[first]
        );
        shell.program.wait_for_output("GOT-1234");
    }
}

impl Server {
    /// Types a command at the prompt without running it.
    fn command_without_enter(&mut self, command: &str) {
        self.type_text(command);
    }

    /// Waits until output the emulator has not received yet contains `marker`.
    fn wait_for_new_output(&self, marker: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !String::from_utf8_lossy(&self.program.output()[self.fed..]).contains(marker) {
            assert!(Instant::now() < deadline, "{marker:?} never appeared");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn password_prompts_of_other_programs_never_show_the_typed_secret() {
    let Some(_lock) = isolated(&format!(
        "{PREFIX}password_prompts_of_other_programs_never_show_the_typed_secret"
    )) else {
        return;
    };
    let fixture = crate::terminal::testing::ShellResourcesFixture::new();
    let key_path = fixture.path().join("key");
    let generated = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "passphrase", "-f"])
        .arg(&key_path)
        .status()
        .unwrap();
    assert!(generated.success());

    // Python's getpass sets the termios sudo's prompt sets: line mode on, echo off.
    // ssh-keygen asks for a key passphrase the same way.
    for prompt in [
        "python3 -c 'import getpass; getpass.getpass()'".to_owned(),
        format!("ssh-keygen -y -f {}", key_path.display()),
    ] {
        let mut zsh = Server::zsh();
        zsh.command(&prompt);
        zsh.program.wait_for_mode(&prompt, |mode| {
            mode.hidden_input() && !mode.shell_foreground
        });
        zsh.settle();
        zsh.type_unseen("secret");
    }
}

#[test]
fn raw_mode_programs_and_foreground_jobs_never_predict() {
    let Some(_lock) = isolated(&format!(
        "{PREFIX}raw_mode_programs_and_foreground_jobs_never_predict"
    )) else {
        return;
    };
    // vim turns signals off, as ssh does for a remote session.
    let mut zsh = Server::zsh();
    zsh.command("vim -u NONE -N -n");
    zsh.program.wait_for_mode("vim", |mode| !mode.signals);
    zsh.settle();
    zsh.type_unseen("ihello");

    let mut zsh = Server::zsh();
    zsh.command("stty raw -echo; sleep 30");
    zsh.program.wait_for_mode("stty raw", |mode| !mode.signals);
    zsh.settle();
    zsh.type_unseen("typed");

    // The python REPL reads like a line editor; only the foreground tells it from the shell.
    let mut zsh = Server::zsh();
    zsh.command("python3 -q");
    zsh.program
        .wait_for_mode("the python REPL", |mode| !mode.shell_foreground);
    zsh.program.wait_for_output(">>> ");
    zsh.settle();
    zsh.type_unseen("1+1");
}
