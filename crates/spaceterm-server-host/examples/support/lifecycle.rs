//! Shared real-host fixture. The child guard only terminates the server it started.
use spaceterm_server::{LocalServer, ServerError, pane::*};
use std::{
    error::Error,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
pub struct Server(pub Child);
impl Server {
    pub fn start() -> Result<Self, Box<dyn Error>> {
        let executable = std::env::current_exe()?
            .parent()
            .ok_or("example directory")?
            .parent()
            .ok_or("build directory")?
            .join("spaceterm-server");
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
pub fn connect(server: &LocalServer) -> Result<PaneConnection, Box<dyn Error>> {
    let until = Instant::now() + Duration::from_secs(8);
    loop {
        match server.connect_pane() {
            Ok(connection) => return Ok(connection),
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
