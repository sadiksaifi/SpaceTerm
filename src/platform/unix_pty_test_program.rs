//! A real program in a real PTY, for tests that need what a Terminal Session's PTY reports.
use super::*;

/// How long a test waits for a real program before it fails.
pub(crate) const DEADLINE: Duration = Duration::from_secs(20);

/// A program running in a real 80x24 PTY, with its output collected on a thread.
pub(crate) struct TestProgram {
    pty: SpawnedPty,
    output: Arc<Mutex<Vec<u8>>>,
}

impl TestProgram {
    pub(crate) const COLUMNS: u16 = 80;
    pub(crate) const ROWS: u16 = 24;

    pub(crate) fn start(program: &str, args: &[&str]) -> Self {
        let mut command = CommandBuilder::new(program);
        command.args(args);
        command.env("TERM", "xterm-256color");
        command.env("BASH_SILENCE_DEPRECATION_WARNING", "1");
        command.env("PS1", "$ ");
        let size = PtySize {
            rows: Self::ROWS,
            cols: Self::COLUMNS,
            ..PtySize::default()
        };
        let (mut pty, _terminator) =
            spawn_command_in_pty(test_host(), size, command, Arc::default()).unwrap();
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

    pub(crate) fn write(&mut self, bytes: &[u8]) {
        self.pty.write_all(bytes).unwrap();
        self.pty.flush().unwrap();
    }

    /// Everything the program wrote so far.
    pub(crate) fn output(&self) -> Vec<u8> {
        self.output.lock().unwrap().clone()
    }

    /// Waits until the program writes nothing for `quiet`, and returns everything it wrote.
    pub(crate) fn settle(&self, quiet: Duration) -> Vec<u8> {
        let deadline = Instant::now() + DEADLINE;
        let mut seen = self.output.lock().unwrap().len();
        let mut since = Instant::now();
        while since.elapsed() < quiet {
            assert!(Instant::now() < deadline, "the program never went quiet");
            thread::sleep(Duration::from_millis(5));
            let now = self.output.lock().unwrap().len();
            if now != seen {
                seen = now;
                since = Instant::now();
            }
        }
        self.output()
    }

    /// Waits until the program's output contains `marker`.
    pub(crate) fn wait_for_output(&self, marker: &str) {
        let deadline = Instant::now() + DEADLINE;
        while !String::from_utf8_lossy(&self.output()).contains(marker) {
            assert!(
                Instant::now() < deadline,
                "{marker:?} never appeared in {:?}",
                String::from_utf8_lossy(&self.output())
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// The PTY's line mode, read from the master as the server reads it.
    pub(crate) fn line_mode(&self) -> LineMode {
        self.pty.line_mode().unwrap()
    }

    /// Reads the line mode until `condition` holds.
    pub(crate) fn wait_for_mode(&self, what: &str, condition: impl Fn(LineMode) -> bool) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let mode = self.line_mode();
            if condition(mode) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{what}: the mode stayed {mode:?}; output {:?}",
                String::from_utf8_lossy(&self.output())
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}
