//! GNOME Sushi preview with a retained exported parent and shared presentation ownership.
use super::linux_session_bus::{SessionBus, SessionBusError};
use crate::terminal::native_services::FilePreviewTarget;
use crate::terminal::native_services::file_preview::{
    FilePreviewError, FilePreviewFactory, FilePreviewPanel,
};
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
const NAME: &str = "org.gnome.NautilusPreviewer";
const PATH: &str = "/org/gnome/NautilusPreviewer";
const INTERFACE: &str = "org.gnome.NautilusPreviewer2";
#[derive(Default)]
struct Preview {
    owner: Option<u64>,
    target: Option<FilePreviewTarget>,
    release_parent: Option<async_channel::Sender<()>>,
    close_queued: bool,
}
pub(super) struct LinuxFilePreviewFactory {
    bus: Option<SessionBus>,
    state: Arc<Mutex<Preview>>,
    next: AtomicU64,
}
impl LinuxFilePreviewFactory {
    pub(super) fn new(bus: Option<SessionBus>) -> Self {
        Self {
            bus: bus.filter(|bus| bus.available(NAME)),
            state: Arc::default(),
            next: AtomicU64::new(1),
        }
    }
}
impl FilePreviewFactory for LinuxFilePreviewFactory {
    fn is_available(&self) -> bool {
        self.bus.is_some()
    }
    fn create(&self) -> Box<dyn FilePreviewPanel> {
        Box::new(Panel {
            bus: self.bus.clone(),
            state: self.state.clone(),
            owner: self.next.fetch_add(1, Ordering::Relaxed),
            exporting: Arc::new(AtomicBool::new(false)),
        })
    }
}
struct Panel {
    bus: Option<SessionBus>,
    state: Arc<Mutex<Preview>>,
    owner: u64,
    exporting: Arc<AtomicBool>,
}
/// Serial bus jobs present one snapshot, then reschedule if a newer file arrived while
/// Sushi was answering. No UI operation waits for a desktop reply while holding this lock.
fn present_latest(
    bus: SessionBus,
    state: Arc<Mutex<Preview>>,
    owner: u64,
    parent: String,
    release_parent: async_channel::Sender<()>,
    exporting: Arc<AtomicBool>,
) {
    let next_bus = bus.clone();
    let completed = exporting.clone();
    let result = bus.dispatch(move |connection| {
        let target = {
            let state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.owner != Some(owner) {
                completed.store(false, Ordering::Release);
                return;
            }
            state
                .target
                .clone()
                .expect("preview owner retains its authorized target")
        };
        let proxy = zbus::blocking::Proxy::new(connection, NAME, PATH, INTERFACE);
        // Export and queue waits grant no new file authority. Revalidate immediately
        // before constructing the URI and handing it to the external service.
        let result = target.revalidated_path().map(|path| {
            let uri = file_uri(&path);
            proxy.and_then(|proxy| {
                proxy.call::<_, _, ()>("ShowFile", &(uri.as_str(), parent.as_str(), false))
            })
        });
        let (repeat, retired, previous_parent) = {
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if result.is_none()
                && state.owner == Some(owner)
                && state.target.as_ref() == Some(&target)
            {
                state.owner = None;
                state.target = None;
            }
            // A timeout does not cancel a delivered method call. Sushi may still show this
            // parent after its startup finishes, so retirement owns it even without a reply.
            let retired = state.owner.is_none();
            let previous_parent = if retired {
                state.target = None;
                state.release_parent.take()
            } else {
                None
            };
            if !retired && result.is_some() {
                state.release_parent = Some(release_parent.clone());
            }
            let repeat = state.owner == Some(owner) && state.target.as_ref() != Some(&target);
            if !repeat {
                completed.store(false, Ordering::Release);
            }
            (repeat, retired, previous_parent)
        };
        if let Some(Err(error)) = &result {
            eprintln!(
                "desktop preview failed: {}",
                SessionBusError::from(error.clone())
            );
        }
        if retired && (result.is_some() || previous_parent.is_some()) {
            // Dismiss may have raced ShowFile or been rejected by the bounded queue.
            // This worker still owns both parent leases until Close completes.
            close_preview(connection);
            drop(previous_parent);
        }
        if repeat {
            present_latest(next_bus, state, owner, parent, release_parent, completed);
        }
    });
    if let Err(error) = result {
        exporting.store(false, Ordering::Release);
        eprintln!("desktop preview failed: {error}");
    }
}
fn close_preview(connection: &zbus::blocking::Connection) {
    let result = zbus::blocking::Proxy::new(connection, NAME, PATH, INTERFACE)
        .and_then(|proxy| proxy.call::<_, _, ()>("Close", &()));
    if let Err(error) = result {
        eprintln!("desktop preview failed: {}", SessionBusError::from(error));
    }
}
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~') {
            uri.push(char::from(byte));
        } else {
            uri.push('%');
            uri.push(char::from(HEX[usize::from(byte >> 4)]));
            uri.push(char::from(HEX[usize::from(byte & 15)]));
        }
    }
    uri
}

impl Panel {
    fn preview_with_parent(
        &mut self,
        target: FilePreviewTarget,
        export: impl FnOnce() -> gpui::Task<Option<gpui::ExternalWindowParent>>,
        cx: &mut gpui::App,
    ) -> Result<(), FilePreviewError> {
        let bus = self
            .bus
            .clone()
            .ok_or(FilePreviewError::PlatformUnavailable)?;
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.owner = Some(self.owner);
            state.target = Some(target);
        }
        if self.exporting.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let export = export();
        let state = self.state.clone();
        let owner = self.owner;
        let exporting = self.exporting.clone();
        cx.spawn(async move |_| {
            if let Some(parent) = export.await {
                let (release, closed) = async_channel::bounded::<()>(1);
                present_latest(
                    bus,
                    state,
                    owner,
                    parent.identifier().to_owned(),
                    release,
                    exporting,
                );
                // Bus jobs only own a string and a release sender. The actual native lease stays
                // here on the foreground thread through replacement or Close acknowledgement.
                let _ = closed.recv().await;
                drop(parent);
            } else {
                exporting.store(false, Ordering::Release);
            }
        })
        .detach();
        Ok(())
    }
}

impl FilePreviewPanel for Panel {
    fn preview_file(&mut self, _: &Path) -> Result<(), FilePreviewError> {
        Err(FilePreviewError::PlatformUnavailable)
    }
    fn preview_file_in_window(
        &mut self,
        target: FilePreviewTarget,
        window: &gpui::Window,
        cx: &mut gpui::App,
    ) -> Result<(), FilePreviewError> {
        self.preview_with_parent(target, || window.export_external_parent(), cx)
    }
    fn dismiss(&mut self) {
        let Some(bus) = &self.bus else {
            return;
        };
        let state = self.state.clone();
        let owner = self.owner;
        // Revoke before queuing Close so an in-flight export cannot show a retired Pane.
        {
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.owner != Some(owner) {
                return;
            }
            state.owner = None;
            if state.close_queued {
                return;
            }
            state.close_queued = true;
        }
        let pending = state.clone();
        let result = bus.dispatch_cleanup(move |connection| {
            let parent = {
                let mut state = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.close_queued = false;
                if state.owner.is_some() {
                    return;
                }
                state.target = None;
                state.release_parent.take()
            };
            if parent.is_some() {
                close_preview(connection);
            }
            drop(parent);
        });
        if let Err(error) = result {
            let mut state = pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.close_queued = false;
            if state.owner.is_none() {
                state.target = None;
                state.release_parent.take();
            }
            eprintln!("desktop preview failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sushi_file_uri_encodes_reserved_and_non_utf8_path_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(b"/a b#?%\xff".to_vec()));
        assert_eq!(file_uri(&path), "file:///a%20b%23%3F%25%FF");
        assert_eq!(file_uri(Path::new("/café")), "file:///caf%C3%A9");
    }
}

#[cfg(all(test, target_os = "linux", feature = "native-tests"))]
mod linux_adapter_tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;
    struct Sushi {
        shown: mpsc::Sender<(String, String, bool)>,
        closed: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    #[zbus::interface(name = "org.gnome.NautilusPreviewer2")]
    impl Sushi {
        fn show_file(&self, uri: &str, parent: &str, close_if_already_shown: bool) {
            self.shown
                .send((uri.into(), parent.into(), close_if_already_shown))
                .unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .unwrap();
        }
        fn close(&self) {
            self.closed.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .unwrap();
        }
    }
    #[test]
    fn linux_desktop_sushi_keeps_latest_request_and_parent_until_close_finishes() {
        struct BusProcess(std::process::Child);
        impl Drop for BusProcess {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut process = BusProcess(
            Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut address = String::new();
        BufReader::new(process.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let (shown, shows) = mpsc::channel();
        let (closed, closes) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        let _server = zbus::blocking::connection::Builder::address(address.trim())
            .unwrap()
            .serve_at(
                PATH,
                Sushi {
                    shown,
                    closed,
                    release: Mutex::new(proceed),
                },
            )
            .unwrap()
            .name(NAME)
            .unwrap()
            .build()
            .unwrap();
        let bus = SessionBus::connect_to(Some(address.trim().into())).unwrap();
        assert!(LinuxFilePreviewFactory::new(Some(bus.clone())).is_available());
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let fixture = Fixture(
            std::env::temp_dir().join(format!("spaceterm-preview-lifetime-{}", std::process::id())),
        );
        std::fs::create_dir_all(&fixture.0).unwrap();
        for name in [
            "first",
            "second",
            "third",
            "queue-saturated",
            "late-startup",
        ] {
            std::fs::write(fixture.0.join(name), b"fixture").unwrap();
        }
        let target = |name: &str| {
            use crate::terminal::{HyperlinkTarget, TerminalLocalFileCapabilities};
            let link = HyperlinkTarget::osc8(
                &format!("file:{name}"),
                &fixture.0,
                None,
                TerminalLocalFileCapabilities::Enabled,
            )
            .unwrap();
            FilePreviewTarget::from_link(&link, TerminalLocalFileCapabilities::Enabled).unwrap()
        };
        let state = Arc::new(Mutex::new(Preview {
            owner: Some(1),
            target: Some(target("first")),
            release_parent: None,
            close_queued: false,
        }));
        let exporting = Arc::new(AtomicBool::new(true));
        let parent = "x11:123".to_owned();
        let (release_parent, retained) = async_channel::bounded::<()>(1);
        present_latest(
            bus.clone(),
            state.clone(),
            1,
            parent.clone(),
            release_parent.clone(),
            exporting.clone(),
        );
        assert_eq!(
            shows.recv_timeout(Duration::from_secs(2)).unwrap(),
            (file_uri(&fixture.0.join("first")), "x11:123".into(), false)
        );
        state.lock().unwrap().target = Some(target("second"));
        release.send(()).unwrap();
        assert_eq!(
            shows.recv_timeout(Duration::from_secs(2)).unwrap(),
            (file_uri(&fixture.0.join("second")), "x11:123".into(), false)
        );
        release.send(()).unwrap();
        bus.query(|_| Ok(())).unwrap();
        assert!(!exporting.load(Ordering::Acquire));
        state.lock().unwrap().target = Some(target("third"));
        exporting.store(true, Ordering::Release);
        present_latest(
            bus.clone(),
            state.clone(),
            1,
            parent,
            release_parent,
            exporting.clone(),
        );
        shows.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut panel = Panel {
            bus: Some(bus.clone()),
            state,
            owner: 1,
            exporting,
        };
        panel.dismiss();
        release.send(()).unwrap();
        closes.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            retained.try_recv(),
            Err(async_channel::TryRecvError::Empty),
            "parent is exported until Sushi closes"
        );
        release.send(()).unwrap();
        bus.query(|_| Ok(())).unwrap();
        assert_eq!(
            retained.try_recv(),
            Err(async_channel::TryRecvError::Closed)
        );
        assert!(shows.try_recv().is_err());

        // Retirement must close an already-visible preview even when regular work is full.
        let (release_parent, retired_parent) = async_channel::bounded::<()>(1);
        {
            let mut state = panel.state.lock().unwrap();
            state.owner = Some(1);
            state.target = Some(target("queue-saturated"));
        }
        panel.exporting.store(true, Ordering::Release);
        present_latest(
            bus.clone(),
            panel.state.clone(),
            1,
            "wayland:queue-saturated".into(),
            release_parent,
            panel.exporting.clone(),
        );
        shows.recv_timeout(Duration::from_secs(2)).unwrap();
        release.send(()).unwrap();
        bus.query(|_| Ok(())).unwrap();
        let (entered, running) = mpsc::channel();
        let (release_bus, resume_bus) = mpsc::channel();
        bus.dispatch(move |_| {
            entered.send(()).unwrap();
            resume_bus.recv_timeout(Duration::from_secs(2)).unwrap();
        })
        .unwrap();
        running.recv_timeout(Duration::from_secs(2)).unwrap();
        let (drained, queue_drained) = mpsc::channel();
        for index in 0..32 {
            let drained = drained.clone();
            bus.dispatch(move |_| {
                if index == 31 {
                    drained.send(()).unwrap();
                }
            })
            .unwrap();
        }
        panel.dismiss();
        let retired = retired_parent.try_recv();
        release_bus.send(()).unwrap();
        assert_eq!(retired, Err(async_channel::TryRecvError::Empty));
        closes.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            retired_parent.try_recv(),
            Err(async_channel::TryRecvError::Empty)
        );
        release.send(()).unwrap();
        queue_drained.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            retired_parent.try_recv(),
            Err(async_channel::TryRecvError::Closed)
        );

        // First service activation can time out even though Sushi later opens the file.
        // Keep the exported parent until explicit retirement, including this ambiguous reply.
        bus.query(|_| Ok(())).unwrap();
        {
            let mut state = panel.state.lock().unwrap();
            state.owner = Some(1);
            state.target = Some(target("late-startup"));
        }
        let (release_parent, late_parent) = async_channel::bounded::<()>(1);
        panel.exporting.store(true, Ordering::Release);
        present_latest(
            bus.clone(),
            panel.state.clone(),
            1,
            "wayland:retained-startup".into(),
            release_parent,
            panel.exporting.clone(),
        );
        shows.recv_timeout(Duration::from_secs(2)).unwrap();
        std::thread::sleep(
            super::super::linux_session_bus::METHOD_TIMEOUT + Duration::from_millis(100),
        );
        let retained_after_timeout = late_parent.try_recv();
        release.send(()).unwrap();
        assert_eq!(
            retained_after_timeout,
            Err(async_channel::TryRecvError::Empty)
        );
        panel.dismiss();
        closes.recv_timeout(Duration::from_secs(2)).unwrap();
        release.send(()).unwrap();
        bus.query(|_| Ok(())).unwrap();
        assert_eq!(
            late_parent.try_recv(),
            Err(async_channel::TryRecvError::Closed)
        );
    }
    #[gpui::test]
    fn linux_desktop_sushi_revalidates_authority_after_export_and_bus_waits(
        cx: &mut gpui::TestAppContext,
    ) {
        // The real desktop worker wakes foreground lease tasks across OS threads.
        cx.background_executor.allow_parking();
        use crate::terminal::native_services::FilePreviewTarget;
        use crate::terminal::native_services::file_preview::FilePreviewPresenter;
        use crate::terminal::{HyperlinkTarget, TerminalLocalFileCapabilities};
        struct BusProcess(std::process::Child);
        impl Drop for BusProcess {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        struct Directory(std::path::PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        struct Lease(mpsc::Sender<()>);
        impl Drop for Lease {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        // The parent export is the only substituted native boundary; the presenter,
        // retained file identity, Linux ownership and private D-Bus service are real.
        struct ExportPanel {
            inner: Panel,
            export: std::rc::Rc<
                std::cell::RefCell<Option<gpui::Task<Option<gpui::ExternalWindowParent>>>>,
            >,
        }
        impl FilePreviewPanel for ExportPanel {
            fn preview_file(&mut self, _: &Path) -> Result<(), FilePreviewError> {
                unreachable!()
            }
            fn preview_file_in_window(
                &mut self,
                target: FilePreviewTarget,
                _: &gpui::Window,
                cx: &mut gpui::App,
            ) -> Result<(), FilePreviewError> {
                let export = self.export.borrow_mut().take().unwrap();
                self.inner.preview_with_parent(target, || export, cx)
            }
            fn dismiss(&mut self) {
                self.inner.dismiss();
            }
        }
        struct PreviewService {
            shown: mpsc::Sender<String>,
            closed: mpsc::Sender<()>,
            release: Mutex<mpsc::Receiver<()>>,
        }
        #[zbus::interface(name = "org.gnome.NautilusPreviewer2")]
        impl PreviewService {
            fn show_file(&self, uri: &str, parent: &str, close_if_already_shown: bool) {
                assert!(!parent.is_empty());
                assert!(!close_if_already_shown);
                self.shown.send(uri.into()).unwrap();
            }
            fn close(&self) {
                self.closed.send(()).unwrap();
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
            }
        }
        let mut process = BusProcess(
            Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut address = String::new();
        BufReader::new(process.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let (shown, shows) = mpsc::channel();
        let (closed, closes) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        let _server = zbus::blocking::connection::Builder::address(address.trim())
            .unwrap()
            .serve_at(
                PATH,
                PreviewService {
                    shown,
                    closed,
                    release: Mutex::new(proceed),
                },
            )
            .unwrap()
            .name(NAME)
            .unwrap()
            .build()
            .unwrap();
        let bus = SessionBus::connect_to(Some(address.trim().into())).unwrap();
        let directory = Directory(std::env::temp_dir().join(format!(
            "spaceterm-preview-authority-{}",
            std::process::id()
        )));
        std::fs::create_dir_all(&directory.0).unwrap();
        let authority = crate::platform::local_filesystem::LocalFilesystemAuthority::new(
            crate::local_path::LocalPathSemantics::Posix,
            Arc::new(crate::platform::unix_local_identity::UnixLocalIdentity),
        );
        let target = |name: &str| {
            let link = HyperlinkTarget::resolve_osc8(
                &format!("file:{name}"),
                &directory.0,
                None,
                TerminalLocalFileCapabilities::Enabled,
                &authority,
            )
            .unwrap();
            FilePreviewTarget::from_link(&link, TerminalLocalFileCapabilities::Enabled).unwrap()
        };
        let cx = cx.add_empty_window();
        for delayed_export in [true, false] {
            let original = directory.0.join("original");
            let requested = directory.0.join("requested");
            std::fs::write(&original, b"original").unwrap();
            std::fs::write(&requested, b"authorized").unwrap();
            let (old_released, old_lease) = mpsc::channel();
            let state = Arc::default();
            let exporting = Arc::new(AtomicBool::new(false));
            let export = std::rc::Rc::new(std::cell::RefCell::new(Some(gpui::Task::ready(Some(
                gpui::ExternalWindowParent::new("x11:original", Lease(old_released)),
            )))));
            let mut presenter = FilePreviewPresenter::new(ExportPanel {
                inner: Panel {
                    bus: Some(bus.clone()),
                    state: Arc::clone(&state),
                    owner: 1,
                    exporting: Arc::clone(&exporting),
                },
                export: export.clone(),
            });
            cx.update(|window, app| presenter.preview_in_window(&target("original"), window, app))
                .unwrap();
            cx.run_until_parked();
            assert_eq!(
                shows.recv_timeout(Duration::from_secs(2)).unwrap(),
                file_uri(&original)
            );
            bus.query(|_| Ok(())).unwrap();

            let requested_target = target("requested");
            let (new_released, new_lease) = mpsc::channel();
            let parent = gpui::ExternalWindowParent::new("wayland:requested", Lease(new_released));
            let (finish_export, exported) = async_channel::bounded(1);
            let (release_bus, resume_bus) = mpsc::channel();
            if delayed_export {
                *export.borrow_mut() =
                    Some(cx.update(|_, app| app.spawn(async move |_| exported.recv().await.ok())));
            } else {
                *export.borrow_mut() = Some(gpui::Task::ready(Some(parent.clone())));
                let (entered, running) = mpsc::channel();
                bus.dispatch(move |_| {
                    entered.send(()).unwrap();
                    resume_bus.recv_timeout(Duration::from_secs(2)).unwrap();
                })
                .unwrap();
                running.recv_timeout(Duration::from_secs(2)).unwrap();
            }
            cx.update(|window, app| presenter.preview_in_window(&requested_target, window, app))
                .unwrap();
            cx.run_until_parked();
            std::fs::rename(&requested, directory.0.join("retired")).unwrap();
            std::fs::write(&requested, b"unauthorized replacement").unwrap();
            assert!(requested_target.revalidated_path().is_none());
            if delayed_export {
                finish_export.try_send(parent).unwrap();
                cx.run_until_parked();
            } else {
                drop(parent);
                release_bus.send(()).unwrap();
            }
            // A stale pending request retires the previous presentation, with its
            // parent still owned until the real service acknowledges Close.
            let retired = closes.recv_timeout(Duration::from_secs(2));
            assert!(
                shows.try_recv().is_err(),
                "replacement must never reach ShowFile"
            );
            retired.expect("stale request must retire the visible preview");
            assert_eq!(
                old_lease.try_recv(),
                Err(mpsc::TryRecvError::Empty),
                "old parent released before Close acknowledgement"
            );
            assert_eq!(
                new_lease.try_recv(),
                Err(mpsc::TryRecvError::Empty),
                "pending parent released before Close acknowledgement"
            );
            release.send(()).unwrap();
            bus.query(|_| Ok(())).unwrap();
            cx.run_until_parked();
            old_lease.recv_timeout(Duration::from_secs(2)).unwrap();
            new_lease.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(!exporting.load(Ordering::Acquire));
            assert!(state.lock().unwrap().owner.is_none());
        }
    }
}
