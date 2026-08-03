use std::env;
use std::io::{Read, Write};

use anyhow::Error as AnyError;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use thiserror::Error;

pub(crate) struct SpawnedPty {
    pub(crate) master: Box<dyn MasterPty + Send>,
    pub(crate) reader: Option<Box<dyn Read + Send>>,
    pub(crate) writer: Box<dyn Write + Send>,
    pub(crate) child: Box<dyn Child + Send + Sync>,
}

impl Drop for SpawnedPty {
    fn drop(&mut self) {
        match self.child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(error) => eprintln!("failed to inspect shell process during cleanup: {error}"),
        }

        if let Err(error) = self.child.kill() {
            eprintln!("failed to terminate shell process during cleanup: {error}");
            return;
        }
        if let Err(error) = self.child.wait() {
            eprintln!("failed to reap shell process during cleanup: {error}");
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum PtyError {
    #[error("failed to open the macOS pseudo-terminal: {0}")]
    Open(#[source] AnyError),
    #[error("failed to start shell {shell}: {source}")]
    SpawnShell {
        shell: String,
        #[source]
        source: AnyError,
    },
    #[error("failed to clone the pseudo-terminal reader: {0}")]
    CloneReader(#[source] AnyError),
    #[error("failed to acquire the pseudo-terminal writer: {0}")]
    TakeWriter(#[source] AnyError),
}

pub(crate) fn spawn_user_shell(size: PtySize) -> Result<SpawnedPty, PtyError> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(size).map_err(PtyError::Open)?;

    let shell = env::var("SHELL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "/bin/zsh".to_owned());

    let mut command = CommandBuilder::new(&shell);
    command.arg("-l");
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("TERM_PROGRAM", "Termspace");
    command.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));

    if let Ok(cwd) = env::current_dir() {
        command.cwd(cwd);
    }

    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|source| PtyError::SpawnShell {
            shell: shell.clone(),
            source,
        })?;

    // The application never uses the slave side after spawning the shell.
    drop(pair.slave);

    let reader = match pair.master.try_clone_reader() {
        Ok(reader) => reader,
        Err(source) => {
            terminate_after_startup_failure(child.as_mut());
            return Err(PtyError::CloneReader(source));
        }
    };
    let writer = match pair.master.take_writer() {
        Ok(writer) => writer,
        Err(source) => {
            terminate_after_startup_failure(child.as_mut());
            return Err(PtyError::TakeWriter(source));
        }
    };

    Ok(SpawnedPty {
        master: pair.master,
        reader: Some(reader),
        writer,
        child,
    })
}

fn terminate_after_startup_failure(child: &mut dyn Child) {
    if let Err(error) = child.kill() {
        eprintln!("failed to terminate shell after PTY setup failed: {error}");
        return;
    }
    if let Err(error) = child.wait() {
        eprintln!("failed to reap shell after PTY setup failed: {error}");
    }
}
