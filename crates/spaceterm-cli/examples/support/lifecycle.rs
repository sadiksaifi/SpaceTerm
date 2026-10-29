//! Shared real-host fixture. The child guard only terminates the server it started.
use spaceterm_server::{
    ServerConnection, ServerError, connection::link::ServerLink, pane::PaneSize,
};
use std::{
    error::Error,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub fn require_idle(client: &ServerConnection) -> Result<(), Box<dyn Error>> {
    // Checking the listener first also protects running work before client setup.
    // SAFETY: geteuid has no preconditions.
    let directories = spaceterm_directories::ServerDirectories::for_account(
        std::path::Path::new("/"),
        unsafe { libc::geteuid() },
        "spaceterm-development",
    )?;
    if std::os::unix::net::UnixStream::connect(directories.runtime.join("s")).is_ok() {
        return Err("Stop the Development server before this exercise.".into());
    }
    match client.refresh() {
        Err(ServerError::NotRunning | ServerError::NotSetUp) => Ok(()),
        Ok(_) => Err("Stop the Development server before this exercise.".into()),
        Err(error) => Err(error.into()),
    }
}
/// Explicit runtime fixture; never discovers, installs or replaces an account service.
pub struct Server(pub Child);
impl Server {
    pub fn start() -> Result<Self, Box<dyn Error>> {
        let executable = std::env::current_exe()?
            .parent()
            .ok_or("example directory")?
            .parent()
            .ok_or("build directory")?
            .join("spaceterm");
        Ok(Self(
            Command::new(executable)
                .args(["serve", "spaceterm-development"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        ))
    }
    pub fn kill(&mut self) -> Result<(), Box<dyn Error>> {
        self.0.kill()?;
        self.0.wait()?;
        Ok(())
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
/// Opens a connection of its own once the started server accepts it.
pub fn connect(server: &ServerConnection) -> Result<ServerLink, Box<dyn Error>> {
    let until = Instant::now() + Duration::from_secs(8);
    loop {
        match super::support::connect(server) {
            Ok(link) => return Ok(link),
            Err(ServerError::NotRunning) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(error) => return Err(error.into()),
        }
    }
}
pub fn size() -> PaneSize {
    PaneSize {
        columns: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 18,
    }
}
