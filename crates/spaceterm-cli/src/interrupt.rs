//! `SIGINT` for every command, as section 5.2 of the output contract requires.
//!
//! Before a command dispatches its first effect, `SIGINT` ends it at once: the handler writes the
//! `interrupted` envelope and exits 130, and nothing happened. From [`dispatch`] on, and while a
//! [`Cooperation`] lives, `SIGINT` is only recorded. The command then resolves what it dispatched
//! for at most [`GRACE`], through [`limit`], and its result, rendered after [`finish`], reports
//! the confirmed outcome with exit 130, or 7 when it could not be confirmed.
//!
//! A command that never called [`install`], such as a unit test, is never interrupted.
use crate::contract::{self, Code, Empty, Envelope, Failure, Mode};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How long a command keeps resolving what it dispatched after `SIGINT`.
pub const GRACE: Duration = Duration::from_secs(5);

/// What decides how `SIGINT` ends the command.
struct Phase {
    command: String,
    mode: Mode,
    /// An effect was dispatched, so the command must resolve it before it exits.
    dispatched: bool,
    /// Live [`Cooperation`]s: code that withdraws what it opened when interrupted.
    cooperating: u32,
    /// The final result is being written, so the handler leaves output to it.
    finishing: bool,
    interrupted: Option<Instant>,
}

static PHASE: Mutex<Option<Phase>> = Mutex::new(None);
/// Set by the signal itself, so a waiting loop can read it without the lock.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

fn phase() -> std::sync::MutexGuard<'static, Option<Phase>> {
    PHASE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Takes over `SIGINT` for the command at `command`, before any effect.
pub fn install(command: &str, mode: Mode) -> Result<(), Failure> {
    let storage = || Failure::new(Code::StorageUnavailable);
    let mut signals = signal_hook::iterator::Signals::new([signal_hook::consts::SIGINT])
        .map_err(|_| storage())?;
    *phase() = Some(Phase {
        command: command.to_owned(),
        mode,
        dispatched: false,
        cooperating: 0,
        finishing: false,
        interrupted: None,
    });
    std::thread::Builder::new()
        .name("spaceterm-interrupt".into())
        .spawn(move || {
            for _ in signals.forever() {
                interrupted_now();
            }
        })
        .map_err(|_| storage())?;
    Ok(())
}

fn interrupted_now() {
    INTERRUPTED.store(true, Ordering::SeqCst);
    let mut guard = phase();
    let Some(phase) = guard.as_mut() else {
        return;
    };
    phase.interrupted.get_or_insert_with(Instant::now);
    if phase.dispatched || phase.cooperating > 0 || phase.finishing {
        return;
    }
    // Nothing was dispatched, so nothing needs resolving. The lock stays held, so no effect
    // starts and no other result is written before the process exits. The main thread may hold
    // the stdout lock, so the envelope is written to the descriptors directly.
    let envelope = Envelope::error(
        phase.command.as_str(),
        None,
        Failure::new(Code::Interrupted),
        Empty {},
    );
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut exit = contract::render(
        &envelope,
        "",
        "",
        phase.mode,
        false,
        &mut stdout,
        &mut stderr,
    );
    if let Err(error) = write_all(libc::STDOUT_FILENO, &stdout) {
        if error.kind() == std::io::ErrorKind::BrokenPipe {
            exit = contract::EXIT_BROKEN_PIPE;
        }
    } else {
        let _ = write_all(libc::STDERR_FILENO, &stderr);
    }
    std::process::exit(i32::from(exit));
}

fn write_all(fd: libc::c_int, mut bytes: &[u8]) -> std::io::Result<()> {
    while !bytes.is_empty() {
        // SAFETY: writes at most `bytes.len()` bytes from a live buffer.
        let count = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if count < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        bytes = &bytes[count as usize..];
    }
    Ok(())
}

/// Marks the command's first effect, before it is sent. Fails with `interrupted` if `SIGINT`
/// already arrived, so the effect is never sent.
pub fn dispatch() -> Result<(), Failure> {
    let mut guard = phase();
    if let Some(phase) = guard.as_mut() {
        if phase.interrupted.is_some() {
            return Err(Failure::new(Code::Interrupted));
        }
        phase.dispatched = true;
    }
    Ok(())
}

/// While this lives, `SIGINT` is only recorded, because the holder withdraws what it opened
/// when [`interrupted`] reports it.
pub struct Cooperation(());

impl Drop for Cooperation {
    fn drop(&mut self) {
        if let Some(phase) = phase().as_mut() {
            phase.cooperating -= 1;
        }
    }
}

/// Starts a [`Cooperation`]. Fails with `interrupted` if `SIGINT` already arrived.
pub fn cooperate() -> Result<Cooperation, Failure> {
    let mut guard = phase();
    if let Some(phase) = guard.as_mut() {
        if phase.interrupted.is_some() {
            return Err(Failure::new(Code::Interrupted));
        }
        phase.cooperating += 1;
    }
    Ok(Cooperation(()))
}

/// Whether `SIGINT` arrived.
pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

/// When an interrupted command must stop resolving: [`GRACE`] after `SIGINT`.
pub fn settle_by() -> Option<Instant> {
    if !interrupted() {
        return None;
    }
    phase()
        .as_ref()
        .and_then(|phase| phase.interrupted)
        .map(|at| at + GRACE)
}

/// `deadline`, or earlier once the command was interrupted.
pub fn limit(deadline: Instant) -> Instant {
    settle_by().map_or(deadline, |until| until.min(deadline))
}

/// How long to wait at most before checking for `SIGINT` again.
pub const TICK: Duration = Duration::from_millis(100);

/// Marks the final result as being written, and reports whether `SIGINT` arrived, which
/// [`crate::contract::render`] then reports. Every command calls it once, as it renders.
pub fn finish() -> bool {
    let mut guard = phase();
    if let Some(phase) = guard.as_mut() {
        phase.finishing = true;
    }
    interrupted()
}
