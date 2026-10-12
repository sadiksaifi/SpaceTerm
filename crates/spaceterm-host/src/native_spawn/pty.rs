use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, BorrowedFd},
        unix::fs::OpenOptionsExt,
    },
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use super::{
    client::{self, CredentialFreeSpawner, RemoteProcess, RequestBudget},
    protocol::{Failure, Retirement},
    service::native_host,
};
use crate::{
    pty::{
        NativePtyAdapter, NativePtyAdapterConstructionFailure as Construction,
        NativePtyAdapterFactory, NativePtyAdapterParts, NativePtyExit, NativePtyOperationFailure,
        NativePtySize, NativePtyTermination, NativePtyWaitFailure,
        unix_pty::{MasterReadError, UnixPtyHost},
    },
    shell_launch::PreparedShellLaunch,
};
use portable_pty::{MasterPty, PtySize};

/// Server-parented PTYs. Only PTY creation and I/O occur in the secret server; the reusable
/// credential-free peer owns shell creation, session signals and exact-child reaping.
#[derive(Clone)]
pub struct CredentialFreePtyAdapterFactory {
    spawner: CredentialFreeSpawner,
}
impl CredentialFreePtyAdapterFactory {
    /// Reuse the same pre-secret connection selected for repository programs.
    pub fn new(spawner: CredentialFreeSpawner) -> Self {
        Self { spawner }
    }
}

impl NativePtyAdapterFactory for CredentialFreePtyAdapterFactory {
    fn create(
        &self,
        launch: PreparedShellLaunch,
        size: NativePtySize,
    ) -> Result<NativePtyAdapterParts, Construction> {
        if size.rows == 0 || size.columns == 0 {
            return Err(Construction::ResourceConfigurationFailed);
        }
        let pair = portable_pty::native_pty_system()
            .openpty(portable_size(size))
            .map_err(|_| Construction::ResourceCreationFailed)?;
        let fd = pair
            .master
            .as_raw_fd()
            .ok_or(Construction::ResourceConfigurationFailed)?;
        let mut attributes = termios(fd).map_err(|_| Construction::ResourceConfigurationFailed)?;
        attributes.c_iflag |= libc::IUTF8;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &attributes) } != 0 {
            return Err(Construction::ResourceConfigurationFailed);
        }
        // SAFETY: master owns fd throughout these independent I/O clones.
        let readiness = unsafe { BorrowedFd::borrow_raw(fd) }
            .try_clone_to_owned()
            .map(File::from)
            .map_err(|_| Construction::InputOutputUnavailable)?;
        client::set_nonblocking(&readiness)
            .map_err(|_| Construction::ResourceConfigurationFailed)?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|_| Construction::InputOutputUnavailable)?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|_| Construction::InputOutputUnavailable)?;
        let path = pair
            .master
            .tty_name()
            .ok_or(Construction::ResourceConfigurationFailed)?;
        let slave = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| Construction::ResourceCreationFailed)?;
        let directory = client::directory(launch.working_directory())
            .map_err(|_| Construction::ProcessStartFailed)?;
        let process = self
            .spawner
            .spawn(
                self.spawner.shell_program(&launch),
                directory,
                slave.into(),
                RequestBudget::default(),
            )
            .map_err(|_| Construction::ProcessStartFailed)?;
        drop(pair.slave);
        let termination = Arc::new(Termination {
            process,
            requested: AtomicBool::new(false),
        });
        Ok(NativePtyAdapterParts {
            adapter: Box::new(Adapter {
                master: pair.master,
                reader: Some(Box::new(Reader { reader, readiness })),
                writer,
                termination: Arc::clone(&termination),
            }),
            termination,
        })
    }
}

fn portable_size(size: NativePtySize) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.columns,
        pixel_width: size.pixel_width,
        pixel_height: size.pixel_height,
    }
}

struct Termination {
    process: RemoteProcess,
    requested: AtomicBool,
}
impl NativePtyTermination for Termination {
    fn request_termination(&self) -> io::Result<()> {
        self.requested.store(true, Ordering::Release);
        self.process
            .retire(Retirement::Hangup, RequestBudget::default())
            .map(|_| ())
            .map_err(Failure::io)
    }
}

struct Adapter {
    master: Box<dyn MasterPty + Send>,
    reader: Option<Box<dyn Read + Send>>,
    writer: Box<dyn Write + Send>,
    termination: Arc<Termination>,
}
impl Write for Adapter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writer.write(bytes).map_err(classify_io)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush().map_err(classify_io)
    }
}
impl NativePtyAdapter for Adapter {
    fn take_reader(&mut self) -> io::Result<Box<dyn Read + Send>> {
        self.reader
            .take()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))
    }
    fn resize(&self, size: NativePtySize) -> Result<(), NativePtyOperationFailure> {
        if size.rows == 0 || size.columns == 0 {
            return Err(NativePtyOperationFailure::new(io::ErrorKind::InvalidInput));
        }
        self.master
            .resize(portable_size(size))
            .map_err(|_| NativePtyOperationFailure::new(io::ErrorKind::Other))
    }
    fn hidden_input(&self) -> Result<bool, NativePtyOperationFailure> {
        let fd = self
            .master
            .as_raw_fd()
            .ok_or_else(|| NativePtyOperationFailure::new(io::ErrorKind::Other))?;
        let attributes =
            termios(fd).map_err(|error| NativePtyOperationFailure::new(error.kind()))?;
        Ok(attributes.c_lflag & libc::ICANON != 0 && attributes.c_lflag & libc::ECHO == 0)
    }
    fn wait_for_exit(&mut self, timeout: Duration) -> Result<NativePtyExit, NativePtyWaitFailure> {
        let result = self
            .termination
            .process
            .wait(timeout)
            .map_err(|error| NativePtyWaitFailure::new(error.kind()))?;
        if self.termination.requested.load(Ordering::Acquire) {
            return Ok(if result.forced {
                NativePtyExit::ForcedShutdown
            } else {
                NativePtyExit::GracefulShutdown
            });
        }
        use std::os::unix::process::ExitStatusExt;
        match (result.status.code(), result.status.signal()) {
            (Some(0), _) => Ok(NativePtyExit::Success),
            (Some(code), _) => Ok(NativePtyExit::ExitCode(code as u32)),
            (_, Some(signal)) => Ok(NativePtyExit::Signal(format!("SIG{signal}"))),
            _ => Err(NativePtyWaitFailure::new(io::ErrorKind::Other)),
        }
    }
}
impl Drop for Adapter {
    fn drop(&mut self) {
        let _ = self.termination.request_termination();
    }
}

struct Reader {
    reader: Box<dyn Read + Send>,
    readiness: File,
}
impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            match self.reader.read(bytes) {
                Ok(count) => return Ok(count),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let mut poll = libc::pollfd {
                        fd: self.readiness.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    if unsafe { libc::poll(&mut poll, 1, -1) } < 0
                        && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
                    {
                        return Err(io::Error::from(io::ErrorKind::Other));
                    }
                }
                Err(error)
                    if native_host().master_read_error(&error) == MasterReadError::Hangup =>
                {
                    return Ok(0);
                }
                Err(error) => return Err(classify_io(error)),
            }
        }
    }
}

fn termios(fd: i32) -> io::Result<libc::termios> {
    let mut attributes = MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(fd, attributes.as_mut_ptr()) } != 0 {
        return Err(io::Error::from(io::ErrorKind::Other));
    }
    Ok(unsafe { attributes.assume_init() })
}
fn classify_io(error: io::Error) -> io::Error {
    io::Error::from(error.kind())
}
