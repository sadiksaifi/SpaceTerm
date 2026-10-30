//! Shows a file selected in the desktop's file manager through the FileManager1 interface,
//! which Files, Dolphin, Nemo, Caja and Thunar provide.
use std::path::Path;

use super::linux_session_bus::SessionBus;
use crate::command_line_tool::{FileViewer, Reveal, ToolFailure};

const SERVICE: &str = "org.freedesktop.FileManager1";
const PATH: &str = "/org/freedesktop/FileManager1";

pub(crate) struct FileManagerViewer {
    bus: Option<SessionBus>,
}

impl FileManagerViewer {
    pub(crate) fn new(bus: Option<SessionBus>) -> Self {
        Self { bus }
    }
}

impl FileViewer for FileManagerViewer {
    /// Settles with the file manager's reply. A missing file manager, a refusal, and no reply
    /// within the bus's method timeout are all unavailable.
    fn reveal(&self, path: &Path) -> Reveal {
        let unavailable =
            || -> Reveal { Box::pin(std::future::ready(Err(ToolFailure::RevealUnavailable))) };
        let Some(bus) = &self.bus else {
            return unavailable();
        };
        let uri = super::linux_file_preview::file_uri(path);
        let (answer, settled) = async_channel::bounded(1);
        let dispatched = bus.dispatch(move |connection| {
            // GPUI exposes no activation token to pass on, and file managers accept an empty one.
            let shown = connection
                .call_method(
                    Some(SERVICE),
                    PATH,
                    Some(SERVICE),
                    "ShowItems",
                    &(vec![uri.as_str()], ""),
                )
                .map(|_| ())
                .map_err(|_| ToolFailure::RevealUnavailable);
            let _ = answer.try_send(shown);
        });
        if dispatched.is_err() {
            return unavailable();
        }
        Box::pin(async move {
            settled
                .recv()
                .await
                .unwrap_or(Err(ToolFailure::RevealUnavailable))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_desktop_without_a_session_bus_reports_the_reveal_unavailable() {
        let viewer = FileManagerViewer::new(None);
        assert_eq!(
            futures_lite::future::block_on(viewer.reveal(Path::new("/home/someone/.local/bin/x"))),
            Err(ToolFailure::RevealUnavailable)
        );
    }
}

#[cfg(all(test, feature = "native-tests"))]
mod linux_adapter_tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;

    struct FileManager {
        refuse: bool,
        shown: mpsc::Sender<Vec<String>>,
    }

    #[zbus::interface(name = "org.freedesktop.FileManager1")]
    impl FileManager {
        fn show_items(&self, uris: Vec<String>, _startup_id: &str) -> zbus::fdo::Result<()> {
            self.shown.send(uris).unwrap();
            if self.refuse {
                Err(zbus::fdo::Error::Failed("fixture".into()))
            } else {
                Ok(())
            }
        }
    }

    /// A private session bus, killed when the test ends.
    struct Bus(std::process::Child, String);

    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn bus() -> Bus {
        let mut process = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(process.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        Bus(process, address.trim().to_owned())
    }

    fn serve(bus: &Bus, refuse: bool) -> (zbus::blocking::Connection, mpsc::Receiver<Vec<String>>) {
        let (shown, shows) = mpsc::channel();
        let connection = zbus::blocking::connection::Builder::address(bus.1.as_str())
            .unwrap()
            .serve_at(PATH, FileManager { refuse, shown })
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .build()
            .unwrap();
        (connection, shows)
    }

    fn reveal(bus: &Bus) -> Result<(), ToolFailure> {
        let viewer =
            FileManagerViewer::new(Some(SessionBus::connect_to(Some(bus.1.clone())).unwrap()));
        futures_lite::future::block_on(viewer.reveal(Path::new("/home/some one/.local/bin/x")))
    }

    #[test]
    fn linux_reveal_reports_the_file_managers_answer() {
        let absent = bus();
        assert_eq!(reveal(&absent), Err(ToolFailure::RevealUnavailable));

        let refusing = bus();
        let (_server, shows) = serve(&refusing, true);
        assert_eq!(reveal(&refusing), Err(ToolFailure::RevealUnavailable));
        assert_eq!(shows.try_recv().unwrap().len(), 1);

        let showing = bus();
        let (_server, shows) = serve(&showing, false);
        assert_eq!(reveal(&showing), Ok(()));
        assert_eq!(
            shows.try_recv().unwrap(),
            ["file:///home/some%20one/.local/bin/x"]
        );
    }
}
