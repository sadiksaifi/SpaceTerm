//! GNOME Sushi preview with a retained exported parent and shared presentation ownership.
//!
//! Sushi shows one window for every client. This process owns that window only from its own
//! ShowFile reply until it closes the window, a newer request replaces it, or Sushi reports
//! that the window closed or moved to another client's parent. A request without a reply proves
//! no ownership, so it keeps its parent exported but never closes the shared window.
use super::linux_session_bus::{
    BusSubscription, RETAINED_REPLY_TIMEOUT, SessionBus, SessionBusError,
};
use crate::terminal::native_services::FilePreviewTarget;
use crate::terminal::native_services::file_preview::{
    FilePreviewError, FilePreviewFactory, FilePreviewPanel, FilePreviewSubmission,
};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use zbus::names::{BusName, OwnedUniqueName};
use zbus::zvariant::OwnedValue;
const NAME: &str = "org.gnome.NautilusPreviewer";
const PATH: &str = "/org/gnome/NautilusPreviewer";
const INTERFACE: &str = "org.gnome.NautilusPreviewer2";
/// Property changes kept while a ShowFile reply is outstanding. Sushi emits a few per request.
const UNORDERED_CHANGE_LIMIT: usize = 8;

/// The two published ShowFile input contracts. Cache discovery only for one service owner.
#[derive(Clone, Copy)]
enum ShowSignature {
    Parent,
    Activation,
}
impl ShowSignature {
    fn from_xml(xml: &str) -> Result<Self, SessionBusError> {
        let document = roxmltree::Document::parse_with_options(
            xml,
            roxmltree::ParsingOptions {
                // D-Bus introspection includes an external DOCTYPE. No entity resolver is
                // installed, so parsing cannot fetch that declaration or other resources.
                allow_dtd: true,
                nodes_limit: 1024,
                ..Default::default()
            },
        )
        .map_err(|_| SessionBusError::Rejected)?;
        let method = document
            .root_element()
            .children()
            .find(|node| {
                node.has_tag_name("interface") && node.attribute("name") == Some(INTERFACE)
            })
            .and_then(|interface| {
                interface.children().find(|node| {
                    node.has_tag_name("method") && node.attribute("name") == Some("ShowFile")
                })
            })
            .ok_or(SessionBusError::Rejected)?;
        let inputs: Vec<_> = method
            .children()
            .filter(|node| node.has_tag_name("arg") && node.attribute("direction") != Some("out"))
            .map(|node| node.attribute("type"))
            .collect();
        match inputs.as_slice() {
            [Some("s"), Some("s"), Some("b")] => Ok(Self::Parent),
            [Some("s"), Some("s"), Some("b"), Some("s")] => Ok(Self::Activation),
            _ => Err(SessionBusError::Rejected),
        }
    }
}
#[derive(Debug, thiserror::Error)]
enum ShowError {
    #[error("{0}")]
    Discovery(SessionBusError),
    #[error("{0}")]
    Request(SessionBusError),
    #[error("the preview target changed")]
    StaleTarget,
}
struct Endpoint {
    owner: OwnedUniqueName,
    signature: Result<ShowSignature, SessionBusError>,
}
#[derive(Default)]
struct Protocol {
    endpoint: Option<Endpoint>,
}
impl Protocol {
    fn endpoint(
        &mut self,
        connection: &zbus::blocking::Connection,
    ) -> Result<(&OwnedUniqueName, ShowSignature), SessionBusError> {
        let proxy =
            zbus::blocking::fdo::DBusProxy::new(connection).map_err(SessionBusError::from)?;
        let name = BusName::try_from(NAME).map_err(|_| SessionBusError::Rejected)?;
        let owner = match proxy.get_name_owner(name.clone()) {
            Ok(owner) => owner,
            Err(zbus::fdo::Error::NameHasNoOwner(_)) => {
                proxy
                    .start_service_by_name(
                        NAME.try_into().map_err(|_| SessionBusError::Rejected)?,
                        0,
                    )
                    .map_err(|error| SessionBusError::from(zbus::Error::from(error)))?;
                proxy
                    .get_name_owner(name)
                    .map_err(|error| SessionBusError::from(zbus::Error::from(error)))?
            }
            Err(error) => return Err(SessionBusError::from(zbus::Error::from(error))),
        };
        if !self
            .endpoint
            .as_ref()
            .is_some_and(|endpoint| endpoint.owner == owner)
        {
            // The connection's retained method deadline bounds activation and introspection.
            // Bound the reply before decoding XML, then bound the parser's node allocation.
            let signature = connection
                .call_method(
                    Some(owner.as_str()),
                    PATH,
                    Some("org.freedesktop.DBus.Introspectable"),
                    "Introspect",
                    &(),
                )
                .map_err(SessionBusError::from)
                .and_then(|reply| {
                    if reply.body().len() > 64 * 1024 {
                        return Err(SessionBusError::Rejected);
                    }
                    let body = reply.body();
                    let xml: &str = body.deserialize().map_err(SessionBusError::from)?;
                    ShowSignature::from_xml(xml)
                });
            self.endpoint = Some(Endpoint { owner, signature });
        }
        let endpoint = self.endpoint.as_ref().expect("discovered endpoint");
        Ok((&endpoint.owner, endpoint.signature?))
    }
}

/// An exported parent window. The native lease stays on the foreground thread until every
/// clone is dropped.
#[derive(Clone)]
struct Parent {
    handle: Arc<str>,
    _lease: async_channel::Sender<()>,
}

/// The latest preview request from any Pane. A newer request supersedes it quietly.
struct Request {
    owner: u64,
    target: FilePreviewTarget,
    /// `None` while the Pane exports its window.
    parent: Option<Parent>,
    failure: async_channel::Sender<FilePreviewError>,
}

/// The Sushi connection that answered ShowFile and the reply's place in its message order.
struct Reply {
    service: OwnedUniqueName,
    serial: u32,
}

/// What this process can claim about Sushi's shared window after handing it a file.
enum Ownership {
    /// ShowFile answered. Only a later departure from that connection ends ownership, and Close
    /// reaches only that connection.
    Proven(Reply),
    /// ShowFile timed out or its reply named no sender. Sushi may still show the file after a
    /// slow start, so the parent stays exported, but property changes cannot be ordered against
    /// the request and the window may belong to another client. Any observed departure ends the
    /// claim, and retirement never closes the window.
    Uncertain,
}

/// What this process last handed to Sushi's shared window.
struct Presented {
    owner: u64,
    parent: Parent,
    ownership: Ownership,
    /// Its Pane dismissed it. Retire it unless a ready request replaces it first.
    retiring: bool,
}

/// A Sushi property change, reduced to the facts that can end this process's ownership.
struct Change {
    service: OwnedUniqueName,
    serial: u32,
    visible: Option<bool>,
    parent: Option<String>,
}

struct Showing {
    owner: u64,
    retire: bool,
}

#[derive(Default)]
struct Preview {
    request: Option<Request>,
    presented: Option<Presented>,
    /// ShowFile is outstanding for this owner.
    showing: Option<Showing>,
    /// Changes that arrived while ShowFile was outstanding, ordered once its reply is recorded.
    unordered: VecDeque<Change>,
    /// A reconcile job is queued or running and observes every later state change.
    reconciling: bool,
}

impl Preview {
    fn observe(&mut self, change: Change) {
        if self.showing.is_some() {
            if self.unordered.len() == UNORDERED_CHANGE_LIMIT {
                self.unordered.pop_front();
            }
            self.unordered.push_back(change);
        } else {
            self.relinquish_after(&change);
        }
    }

    fn order_unordered(&mut self) {
        while let Some(change) = self.unordered.pop_front() {
            self.relinquish_after(&change);
        }
    }

    /// Sushi closed the window or shows it for another client after this process's request.
    fn relinquish_after(&mut self, change: &Change) {
        let Some(presented) = &self.presented else {
            return;
        };
        let departed = change.visible == Some(false)
            || change
                .parent
                .as_deref()
                .is_some_and(|parent| parent != &*presented.parent.handle);
        let relinquished = departed
            && match &presented.ownership {
                Ownership::Proven(reply) => {
                    change.service == reply.service && change.serial > reply.serial
                }
                Ownership::Uncertain => true,
            };
        if relinquished {
            self.presented = None;
        }
    }
}

fn lock(state: &Mutex<Preview>) -> MutexGuard<'_, Preview> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Sushi calls run in order on a dedicated connection that waits for a slow first activation,
/// so a reply decides ownership instead of the shared connection's short method timeout.
#[derive(Clone)]
struct Service {
    bus: SessionBus,
    state: Arc<Mutex<Preview>>,
    protocol: Arc<Mutex<Protocol>>,
}

impl Service {
    /// At most one reconcile job is queued, so preview traffic cannot fill the queue.
    fn schedule(&self) -> Result<(), SessionBusError> {
        {
            let mut state = lock(&self.state);
            if state.reconciling {
                return Ok(());
            }
            state.reconciling = true;
        }
        let state = self.state.clone();
        let protocol = self.protocol.clone();
        self.bus
            .dispatch(move |connection| {
                let mut protocol = protocol
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                reconcile(connection, &state, &mut protocol);
            })
            .inspect_err(|_| lock(&self.state).reconciling = false)
    }
}

enum Step {
    Show(Request),
    /// Ends a dismissed presentation, closing the window only where ownership is proven.
    Retire(Option<OwnedUniqueName>),
}

fn reconcile(
    connection: &zbus::blocking::Connection,
    state: &Mutex<Preview>,
    protocol: &mut Protocol,
) {
    // A stale request's parent stays exported until the retirement it caused completes.
    let mut retained = Vec::new();
    loop {
        let step = {
            let mut state = lock(state);
            if let Some(request) = state.request.take_if(|request| request.parent.is_some()) {
                state.showing = Some(Showing {
                    owner: request.owner,
                    retire: false,
                });
                Step::Show(request)
            } else if let Some(presented) = state
                .presented
                .as_ref()
                .filter(|presented| presented.retiring)
            {
                Step::Retire(match &presented.ownership {
                    Ownership::Proven(reply) => Some(reply.service.clone()),
                    Ownership::Uncertain => None,
                })
            } else {
                state.reconciling = false;
                return;
            }
        };
        match step {
            Step::Show(request) => {
                retained.extend(show(connection, state, request, protocol));
            }
            Step::Retire(service) => {
                if let Some(service) = service {
                    close(connection, service);
                }
                let mut state = lock(state);
                if state
                    .presented
                    .as_ref()
                    .is_some_and(|presented| presented.retiring)
                {
                    state.presented = None;
                }
                retained.clear();
            }
        }
    }
}

/// Returns the parent of a stale request, which must outlive the Close it caused.
fn show(
    connection: &zbus::blocking::Connection,
    state: &Mutex<Preview>,
    request: Request,
    protocol: &mut Protocol,
) -> Option<Parent> {
    let Request {
        owner,
        target,
        parent,
        failure,
    } = request;
    let parent = parent.expect("only a request with an exported parent is shown");
    let result = (|| {
        let (service, signature) = protocol
            .endpoint(connection)
            .map_err(ShowError::Discovery)?;
        // Export, queue and protocol discovery waits grant no new file authority. Revalidate
        // after all of them, immediately before handing the URI to the external service.
        let path = target.revalidated_path().ok_or(ShowError::StaleTarget)?;
        let uri = file_uri(&path);
        let destination = Some(service.as_str());
        let reply = match signature {
            ShowSignature::Parent => connection.call_method(
                destination,
                PATH,
                Some(INTERFACE),
                "ShowFile",
                &(uri.as_str(), &*parent.handle, false),
            ),
            // GPUI can consume activation tokens, but exposes no operation to request one
            // for an external preview. Sushi accepts an empty token in that case.
            ShowSignature::Activation => connection.call_method(
                destination,
                PATH,
                Some(INTERFACE),
                "ShowFile",
                &(uri.as_str(), &*parent.handle, false, ""),
            ),
        };
        reply.map_err(|error| ShowError::Request(error.into()))
    })();
    let mut state = lock(state);
    let retire = state.showing.take().is_some_and(|showing| showing.retire);
    match result {
        Err(ShowError::StaleTarget) => {
            let _ = failure.try_send(FilePreviewError::StaleTarget);
            state.order_unordered();
            if let Some(presented) = state
                .presented
                .as_mut()
                .filter(|presented| presented.owner == owner)
            {
                presented.retiring = true;
            }
            return Some(parent);
        }
        Ok(reply) => {
            let header = reply.header();
            let ownership = header.sender().map_or(Ownership::Uncertain, |service| {
                Ownership::Proven(Reply {
                    service: service.to_owned().into(),
                    serial: header.primary().serial_num().get(),
                })
            });
            state.presented = Some(Presented {
                owner,
                parent,
                ownership,
                retiring: retire,
            });
            state.order_unordered();
        }
        Err(ShowError::Request(SessionBusError::TimedOut)) => {
            state.presented = Some(Presented {
                owner,
                parent,
                ownership: Ownership::Uncertain,
                retiring: retire,
            });
            // Changes from before the timeout cannot be ordered against a request whose reply
            // never arrived, so only later departures end the claim.
            state.unordered.clear();
        }
        Err(error) => {
            // Sushi showed nothing for this request, so its parent is released now and an
            // earlier presentation keeps its own ownership.
            state.order_unordered();
            drop(state);
            let _ = failure.try_send(FilePreviewError::PlatformUnavailable);
            eprintln!("desktop preview failed: {error}");
        }
    }
    None
}

/// Close targets the Sushi connection that answered, so a restarted service keeps another
/// client's window.
fn close(connection: &zbus::blocking::Connection, service: OwnedUniqueName) {
    let destination = BusName::Unique(service.into_inner());
    let result = connection.call_method(Some(destination), PATH, Some(INTERFACE), "Close", &());
    if let Err(error) = result {
        eprintln!("desktop preview failed: {}", SessionBusError::from(error));
    }
}

fn change(message: &zbus::Message) -> Option<Change> {
    let header = message.header();
    let (interface, changed, _): (String, HashMap<String, OwnedValue>, Vec<String>) =
        message.body().deserialize().ok()?;
    if interface != INTERFACE {
        return None;
    }
    Some(Change {
        service: header.sender()?.to_owned().into(),
        serial: header.primary().serial_num().get(),
        visible: changed
            .get("Visible")
            .and_then(|value| bool::try_from(value).ok()),
        parent: changed
            .get("ParentHandle")
            .and_then(|value| <&str>::try_from(value).ok())
            .map(str::to_owned),
    })
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

pub(super) struct LinuxFilePreviewFactory {
    service: Option<Service>,
    next: AtomicU64,
    _changes: Option<BusSubscription>,
}
impl LinuxFilePreviewFactory {
    pub(super) fn new(bus: Option<SessionBus>) -> Self {
        Self::with_service_bus(
            bus.filter(|bus| bus.available(NAME))
                .and_then(|bus| bus.dedicated(RETAINED_REPLY_TIMEOUT).ok()),
        )
    }
    fn with_service_bus(bus: Option<SessionBus>) -> Self {
        let service = bus.map(|bus| Service {
            bus,
            state: Arc::default(),
            protocol: Arc::default(),
        });
        let changes = service.as_ref().and_then(|service| {
            let rule = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(NAME)
                .ok()?
                .path(PATH)
                .ok()?
                .interface("org.freedesktop.DBus.Properties")
                .ok()?
                .member("PropertiesChanged")
                .ok()?
                .arg(0, INTERFACE)
                .ok()?
                .build()
                .to_owned();
            let state = service.state.clone();
            service
                .bus
                .subscribe(rule.into(), move |message| {
                    if let Some(change) = change(&message) {
                        lock(&state).observe(change);
                    }
                })
                .ok()
        });
        Self {
            service,
            next: AtomicU64::new(1),
            _changes: changes,
        }
    }
    fn panel(&self) -> Panel {
        Panel {
            service: self.service.clone(),
            owner: self.next.fetch_add(1, Ordering::Relaxed),
            exporting: Arc::new(AtomicBool::new(false)),
        }
    }
}
impl FilePreviewFactory for LinuxFilePreviewFactory {
    fn is_available(&self) -> bool {
        self.service.is_some()
    }
    fn create(&self) -> Box<dyn FilePreviewPanel> {
        Box::new(self.panel())
    }
}

struct Panel {
    service: Option<Service>,
    owner: u64,
    exporting: Arc<AtomicBool>,
}

impl Panel {
    fn preview_with_parent(
        &mut self,
        target: FilePreviewTarget,
        export: impl FnOnce() -> gpui::Task<Option<gpui::ExternalWindowParent>>,
        cx: &mut gpui::App,
    ) -> Result<FilePreviewSubmission, FilePreviewError> {
        let service = self
            .service
            .clone()
            .ok_or(FilePreviewError::PlatformUnavailable)?;
        let owner = self.owner;
        let (failure, pending) = async_channel::bounded(1);
        let ready = {
            let mut state = lock(&service.state);
            // A newer request from this Pane keeps the parent it already exported.
            let parent = state
                .request
                .take()
                .filter(|request| request.owner == owner)
                .and_then(|request| request.parent);
            let ready = parent.is_some();
            state.request = Some(Request {
                owner,
                target,
                parent,
                failure,
            });
            ready
        };
        if ready {
            schedule_or_fail(&service, owner);
        } else if !self.exporting.swap(true, Ordering::AcqRel) {
            let export = export();
            let exporting = self.exporting.clone();
            cx.spawn(async move |_| {
                let exported = export.await;
                exporting.store(false, Ordering::Release);
                let Some(native) = exported else {
                    fail_request(&service, owner, FilePreviewError::PlatformUnavailable);
                    return;
                };
                let (lease, released) = async_channel::bounded::<()>(1);
                let attached = {
                    let mut state = lock(&service.state);
                    match &mut state.request {
                        Some(request) if request.owner == owner && request.parent.is_none() => {
                            request.parent = Some(Parent {
                                handle: native.identifier().into(),
                                _lease: lease,
                            });
                            true
                        }
                        _ => false,
                    }
                };
                if attached {
                    schedule_or_fail(&service, owner);
                }
                // Bus jobs only own a string and a lease sender. The native lease stays here on
                // the foreground thread through replacement or Close acknowledgement.
                let _ = released.recv().await;
                drop(native);
            })
            .detach();
        }
        Ok(FilePreviewSubmission::Pending(pending))
    }
}

fn schedule_or_fail(service: &Service, owner: u64) {
    if let Err(error) = service.schedule() {
        fail_request(service, owner, FilePreviewError::PlatformUnavailable);
        eprintln!("desktop preview failed: {error}");
    }
}

fn fail_request(service: &Service, owner: u64, error: FilePreviewError) {
    let request = lock(&service.state)
        .request
        .take_if(|request| request.owner == owner);
    if let Some(request) = request {
        let _ = request.failure.try_send(error);
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
    ) -> Result<FilePreviewSubmission, FilePreviewError> {
        self.preview_with_parent(target, || window.export_external_parent(), cx)
    }
    fn dismiss(&mut self) {
        let Some(service) = &self.service else {
            return;
        };
        let owner = self.owner;
        // Revoke before queuing Close so an in-flight export cannot show a retired Pane.
        let retire = {
            let mut state = lock(&service.state);
            // Dropping the request ends its completion without a failure.
            state.request.take_if(|request| request.owner == owner);
            if let Some(showing) = state
                .showing
                .as_mut()
                .filter(|showing| showing.owner == owner)
            {
                showing.retire = true;
            }
            match state.presented.as_mut() {
                Some(presented) if presented.owner == owner && !presented.retiring => {
                    presented.retiring = true;
                    true
                }
                _ => false,
            }
        };
        if retire && let Err(error) = service.schedule() {
            let mut state = lock(&service.state);
            if state
                .presented
                .as_ref()
                .is_some_and(|presented| presented.retiring)
            {
                state.presented = None;
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
    use zbus::zvariant::Value;

    const WAIT: Duration = Duration::from_secs(2);

    #[derive(Clone, Copy)]
    enum Answer {
        /// ShowFile returns once the test releases it.
        Gated,
        Immediate,
        Reject,
    }
    struct Sushi {
        answer: Answer,
        close_gated: bool,
        shown: mpsc::Sender<(String, String, bool)>,
        closed: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    #[zbus::interface(name = "org.gnome.NautilusPreviewer2")]
    impl Sushi {
        fn show_file(
            &self,
            uri: &str,
            parent: &str,
            close_if_already_shown: bool,
        ) -> zbus::fdo::Result<()> {
            self.shown
                .send((uri.into(), parent.into(), close_if_already_shown))
                .unwrap();
            match self.answer {
                Answer::Gated => {
                    let _ = self.release.lock().unwrap().recv_timeout(WAIT);
                    Ok(())
                }
                Answer::Immediate => Ok(()),
                Answer::Reject => Err(zbus::fdo::Error::Failed("fixture".into())),
            }
        }
        fn close(&self) {
            self.closed.send(()).unwrap();
            if self.close_gated {
                let _ = self.release.lock().unwrap().recv_timeout(WAIT);
            }
        }
    }

    struct SushiWithActivation {
        inner: Sushi,
        tokens: mpsc::Sender<String>,
    }
    #[zbus::interface(name = "org.gnome.NautilusPreviewer2")]
    impl SushiWithActivation {
        fn show_file(
            &self,
            uri: &str,
            parent: &str,
            close_if_already_shown: bool,
            activation_token: &str,
        ) -> zbus::fdo::Result<()> {
            self.tokens.send(activation_token.into()).unwrap();
            self.inner.show_file(uri, parent, close_if_already_shown)
        }
        fn close(&self) {
            self.inner.close();
        }
    }

    /// A private bus with a Sushi fixture and a directory of preview targets.
    struct Fixture {
        address: String,
        server: zbus::blocking::Connection,
        shows: mpsc::Receiver<(String, String, bool)>,
        closes: mpsc::Receiver<()>,
        tokens: mpsc::Receiver<String>,
        release: mpsc::Sender<()>,
        directory: std::path::PathBuf,
        process: std::process::Child,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.process.kill();
            let _ = self.process.wait();
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
    impl Fixture {
        fn new(name: &str, answer: Answer, close_gated: bool) -> Self {
            Self::with_activation(name, answer, close_gated, false)
        }
        fn with_activation(
            name: &str,
            answer: Answer,
            close_gated: bool,
            activation: bool,
        ) -> Self {
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
            let address = address.trim().to_owned();
            let (shown, shows) = mpsc::channel();
            let (closed, closes) = mpsc::channel();
            let (release, proceed) = mpsc::channel();
            let (tokens, token_receiver) = mpsc::channel();
            let sushi = Sushi {
                answer,
                close_gated,
                shown,
                closed,
                release: Mutex::new(proceed),
            };
            let builder = zbus::blocking::connection::Builder::address(address.as_str()).unwrap();
            let builder = if activation {
                builder
                    .serve_at(
                        PATH,
                        SushiWithActivation {
                            inner: sushi,
                            tokens,
                        },
                    )
                    .unwrap()
            } else {
                builder.serve_at(PATH, sushi).unwrap()
            };
            let server = builder.name(NAME).unwrap().build().unwrap();
            let directory = std::env::temp_dir()
                .join(format!("spaceterm-preview-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&directory).unwrap();
            Self {
                address,
                server,
                shows,
                closes,
                tokens: token_receiver,
                release,
                directory,
                process,
            }
        }
        fn replace_service(&mut self, activation: bool) {
            let (shown, shows) = mpsc::channel();
            let (closed, closes) = mpsc::channel();
            let (release, proceed) = mpsc::channel();
            let (tokens, token_receiver) = mpsc::channel();
            let sushi = Sushi {
                answer: Answer::Immediate,
                close_gated: false,
                shown,
                closed,
                release: Mutex::new(proceed),
            };
            let builder =
                zbus::blocking::connection::Builder::address(self.address.as_str()).unwrap();
            let builder = if activation {
                builder
                    .serve_at(
                        PATH,
                        SushiWithActivation {
                            inner: sushi,
                            tokens,
                        },
                    )
                    .unwrap()
            } else {
                builder.serve_at(PATH, sushi).unwrap()
            };
            self.server.release_name(NAME).unwrap();
            self.server = builder.name(NAME).unwrap().build().unwrap();
            self.shows = shows;
            self.closes = closes;
            self.release = release;
            self.tokens = token_receiver;
        }
        fn client(&self) -> SessionBus {
            SessionBus::connect_to(Some(self.address.clone())).unwrap()
        }
        fn target(&self, name: &str) -> FilePreviewTarget {
            use crate::terminal::{HyperlinkTarget, TerminalLocalFileCapabilities};
            std::fs::write(self.directory.join(name), b"fixture").unwrap();
            let link = HyperlinkTarget::osc8(
                &format!("file:{name}"),
                &self.directory,
                None,
                TerminalLocalFileCapabilities::Enabled,
            )
            .unwrap();
            FilePreviewTarget::from_link(&link, TerminalLocalFileCapabilities::Enabled).unwrap()
        }
        fn uri(&self, name: &str) -> String {
            file_uri(&self.directory.join(name))
        }
        /// Sushi reports its shared window's properties after each change.
        fn emit(&self, changed: &[(&str, Value<'_>)]) {
            let changed: HashMap<&str, &Value<'_>> =
                changed.iter().map(|(name, value)| (*name, value)).collect();
            self.server
                .emit_signal(
                    None::<&str>,
                    PATH,
                    "org.freedesktop.DBus.Properties",
                    "PropertiesChanged",
                    &(INTERFACE, changed, Vec::<&str>::new()),
                )
                .unwrap();
        }
    }

    fn parent(handle: &str) -> (Parent, async_channel::Receiver<()>) {
        let (lease, released) = async_channel::bounded(1);
        (
            Parent {
                handle: handle.into(),
                _lease: lease,
            },
            released,
        )
    }
    fn request(
        service: &Service,
        owner: u64,
        target: FilePreviewTarget,
        parent: Parent,
    ) -> async_channel::Receiver<FilePreviewError> {
        let (failure, pending) = async_channel::bounded(1);
        lock(&service.state).request = Some(Request {
            owner,
            target,
            parent: Some(parent),
            failure,
        });
        service.schedule().unwrap();
        pending
    }
    fn settle(service: &Service) {
        service.bus.query(|_| Ok(())).unwrap();
    }
    fn wait_released(
        lease: &async_channel::Receiver<()>,
    ) -> Result<(), async_channel::TryRecvError> {
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            match lease.try_recv() {
                Err(async_channel::TryRecvError::Empty) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                result => return result,
            }
        }
    }

    #[test]
    fn linux_desktop_sushi_discovers_three_and_four_argument_interfaces() {
        let mut fixture = Fixture::new("signatures-and-restarts", Answer::Immediate, false);
        let factory = LinuxFilePreviewFactory::new(Some(fixture.client()));
        let panel = factory.panel();
        let service = panel.service.clone().unwrap();
        for activation in [false, true, false] {
            fixture.replace_service(activation);
            for name in ["first", "second"] {
                let (parent, lease) = parent("wayland:signature");
                let failure = request(&service, panel.owner, fixture.target(name), parent);
                assert_eq!(
                    fixture
                        .shows
                        .recv_timeout(WAIT)
                        .expect("ShowFile must use the service's signature"),
                    (fixture.uri(name), "wayland:signature".into(), false)
                );
                settle(&service);
                assert_eq!(failure.try_recv(), Err(async_channel::TryRecvError::Closed));
                assert_eq!(lease.try_recv(), Err(async_channel::TryRecvError::Empty));
                if activation {
                    assert_eq!(fixture.tokens.recv_timeout(WAIT).unwrap(), "");
                }
            }
        }
    }

    #[test]
    fn linux_desktop_sushi_keeps_latest_request_and_parent_until_close_finishes() {
        let fixture = Fixture::new("lifetime", Answer::Gated, true);
        let shared = fixture.client();
        let factory = LinuxFilePreviewFactory::new(Some(shared.clone()));
        assert!(factory.is_available());
        let mut panel = factory.panel();
        let service = panel.service.clone().unwrap();
        let owner = panel.owner;

        let (shared_parent, retained) = parent("x11:123");
        let _first = request(
            &service,
            owner,
            fixture.target("first"),
            shared_parent.clone(),
        );
        assert_eq!(
            fixture.shows.recv_timeout(WAIT).unwrap(),
            (fixture.uri("first"), "x11:123".into(), false)
        );
        let _second = request(
            &service,
            owner,
            fixture.target("second"),
            shared_parent.clone(),
        );
        fixture.release.send(()).unwrap();
        assert_eq!(
            fixture.shows.recv_timeout(WAIT).unwrap(),
            (fixture.uri("second"), "x11:123".into(), false)
        );
        fixture.release.send(()).unwrap();
        settle(&service);
        assert!(!lock(&service.state).reconciling);
        let _third = request(&service, owner, fixture.target("third"), shared_parent);
        fixture.shows.recv_timeout(WAIT).unwrap();
        panel.dismiss();
        fixture.release.send(()).unwrap();
        fixture.closes.recv_timeout(WAIT).unwrap();
        assert_eq!(
            retained.try_recv(),
            Err(async_channel::TryRecvError::Empty),
            "parent is exported until Sushi closes"
        );
        fixture.release.send(()).unwrap();
        settle(&service);
        assert_eq!(
            retained.try_recv(),
            Err(async_channel::TryRecvError::Closed)
        );
        assert!(fixture.shows.try_recv().is_err());

        // Retirement must close an already-visible preview even when shared work is full.
        let (queued_parent, retired_parent) = parent("wayland:queue-saturated");
        let _queued = request(
            &service,
            owner,
            fixture.target("queue-saturated"),
            queued_parent,
        );
        fixture.shows.recv_timeout(WAIT).unwrap();
        fixture.release.send(()).unwrap();
        settle(&service);
        let (entered, running) = mpsc::channel();
        let (release_bus, resume_bus) = mpsc::channel();
        shared
            .dispatch(move |_| {
                entered.send(()).unwrap();
                resume_bus.recv_timeout(WAIT).unwrap();
            })
            .unwrap();
        running.recv_timeout(WAIT).unwrap();
        for _ in 0..32 {
            shared.dispatch(|_| {}).unwrap();
        }
        panel.dismiss();
        fixture.closes.recv_timeout(WAIT).unwrap();
        assert_eq!(
            retired_parent.try_recv(),
            Err(async_channel::TryRecvError::Empty)
        );
        fixture.release.send(()).unwrap();
        settle(&service);
        assert_eq!(
            retired_parent.try_recv(),
            Err(async_channel::TryRecvError::Closed)
        );
        release_bus.send(()).unwrap();

        // First service activation can answer after the shared method timeout. The dedicated
        // connection still receives that answer, so the request is presented, not failed.
        let (late_parent, late_lease) = parent("wayland:retained-startup");
        let late = request(&service, owner, fixture.target("late-startup"), late_parent);
        fixture.shows.recv_timeout(WAIT).unwrap();
        std::thread::sleep(
            super::super::linux_session_bus::METHOD_TIMEOUT + Duration::from_millis(100),
        );
        let retained_after_timeout = late_lease.try_recv();
        fixture.release.send(()).unwrap();
        settle(&service);
        assert_eq!(
            retained_after_timeout,
            Err(async_channel::TryRecvError::Empty)
        );
        assert_eq!(late.try_recv(), Err(async_channel::TryRecvError::Closed));
        panel.dismiss();
        fixture.closes.recv_timeout(WAIT).unwrap();
        fixture.release.send(()).unwrap();
        settle(&service);
        assert_eq!(
            late_lease.try_recv(),
            Err(async_channel::TryRecvError::Closed)
        );
    }

    #[test]
    fn linux_desktop_sushi_keeps_an_unanswered_request_until_retirement_without_closing() {
        let fixture = Fixture::new("unanswered", Answer::Gated, false);
        // A short reply wait stands in for the retained wait expiring.
        let factory = LinuxFilePreviewFactory::with_service_bus(Some(fixture.client()));
        let mut panel = factory.panel();
        let service = panel.service.clone().unwrap();
        let (unanswered, lease) = parent("wayland:unanswered");
        let failure = request(&service, panel.owner, fixture.target("slow"), unanswered);
        fixture.shows.recv_timeout(WAIT).unwrap();
        settle(&service);
        fixture.release.send(()).unwrap();
        assert_eq!(lease.try_recv(), Err(async_channel::TryRecvError::Empty));
        assert_eq!(
            failure.try_recv(),
            Err(async_channel::TryRecvError::Closed),
            "an unanswered request is not a failure"
        );
        panel.dismiss();
        settle(&service);
        assert_eq!(lease.try_recv(), Err(async_channel::TryRecvError::Closed));
        assert!(
            fixture.closes.try_recv().is_err(),
            "an unanswered request never proved it owns Sushi's window"
        );
    }

    #[test]
    fn linux_desktop_sushi_unanswered_request_yields_to_another_clients_preview() {
        let fixture = Fixture::new("unanswered-foreign", Answer::Gated, false);
        // A short reply wait stands in for the retained wait expiring.
        let factory = LinuxFilePreviewFactory::with_service_bus(Some(fixture.client()));
        let mut panel = factory.panel();
        let service = panel.service.clone().unwrap();
        let (unanswered, lease) = parent("wayland:unanswered");
        let _request = request(&service, panel.owner, fixture.target("slow"), unanswered);
        fixture.shows.recv_timeout(WAIT).unwrap();
        settle(&service);
        // Sushi recovers, shows this request, then another client's preview takes the window.
        fixture.release.send(()).unwrap();
        fixture.emit(&[
            ("ParentHandle", Value::from("wayland:unanswered")),
            ("Visible", Value::from(true)),
        ]);
        fixture.emit(&[("ParentHandle", Value::from("x11:another-client"))]);
        assert_eq!(
            wait_released(&lease),
            Err(async_channel::TryRecvError::Closed),
            "another client's preview ends an unproven claim"
        );
        panel.dismiss();
        settle(&service);
        assert!(
            fixture.closes.try_recv().is_err(),
            "Close would end another client's preview"
        );
    }

    #[test]
    fn linux_desktop_sushi_rejection_fails_the_request_and_owns_nothing() {
        let fixture = Fixture::new("rejection", Answer::Reject, false);
        let factory = LinuxFilePreviewFactory::new(Some(fixture.client()));
        let mut panel = factory.panel();
        let service = panel.service.clone().unwrap();
        let (rejected, lease) = parent("x11:rejected");
        let failure = request(&service, panel.owner, fixture.target("rejected"), rejected);
        fixture.shows.recv_timeout(WAIT).unwrap();
        settle(&service);
        assert_eq!(
            failure.try_recv(),
            Ok(FilePreviewError::PlatformUnavailable)
        );
        assert_eq!(
            lease.try_recv(),
            Err(async_channel::TryRecvError::Closed),
            "a rejected request releases its parent"
        );
        panel.dismiss();
        settle(&service);
        assert!(
            fixture.closes.try_recv().is_err(),
            "a rejected request never owned Sushi's window"
        );
    }

    #[test]
    fn linux_desktop_sushi_relinquishes_a_window_that_closed_or_moved_to_another_client() {
        let fixture = Fixture::new("relinquish", Answer::Immediate, false);
        let factory = LinuxFilePreviewFactory::new(Some(fixture.client()));
        let mut panel = factory.panel();
        let service = panel.service.clone().unwrap();
        for departure in [
            ("Visible", Value::from(false)),
            ("ParentHandle", Value::from("x11:another-client")),
        ] {
            let (ours, lease) = parent("x11:ours");
            let _request = request(&service, panel.owner, fixture.target("shown"), ours);
            fixture.shows.recv_timeout(WAIT).unwrap();
            settle(&service);
            fixture.emit(&[
                ("ParentHandle", Value::from("x11:ours")),
                ("Visible", Value::from(true)),
            ]);
            std::thread::sleep(Duration::from_millis(200));
            assert_eq!(
                lease.try_recv(),
                Err(async_channel::TryRecvError::Empty),
                "showing this parent keeps ownership"
            );
            fixture.emit(&[departure]);
            assert_eq!(
                wait_released(&lease),
                Err(async_channel::TryRecvError::Closed)
            );
            panel.dismiss();
            settle(&service);
            assert!(
                fixture.closes.try_recv().is_err(),
                "Close would end another client's preview"
            );
        }
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
            ) -> Result<FilePreviewSubmission, FilePreviewError> {
                let export = self.export.borrow_mut().take().unwrap();
                self.inner.preview_with_parent(target, || export, cx)
            }
            fn dismiss(&mut self) {
                self.inner.dismiss();
            }
        }
        let fixture = Fixture::new("authority", Answer::Immediate, true);
        let factory = LinuxFilePreviewFactory::new(Some(fixture.client()));
        let authority = crate::platform::local_filesystem::LocalFilesystemAuthority::new(
            crate::local_path::LocalPathSemantics::Posix,
            Arc::new(crate::platform::unix_local_identity::UnixLocalIdentity),
        );
        let directory = fixture.directory.clone();
        let target = |name: &str| {
            let link = HyperlinkTarget::resolve_osc8(
                &format!("file:{name}"),
                &directory,
                None,
                TerminalLocalFileCapabilities::Enabled,
                &authority,
            )
            .unwrap();
            FilePreviewTarget::from_link(&link, TerminalLocalFileCapabilities::Enabled).unwrap()
        };
        let cx = cx.add_empty_window();
        for delayed_export in [true, false] {
            let original = directory.join("original");
            let requested = directory.join("requested");
            std::fs::write(&original, b"original").unwrap();
            std::fs::write(&requested, b"authorized").unwrap();
            let (old_released, old_lease) = mpsc::channel();
            let inner = factory.panel();
            let service = inner.service.clone().unwrap();
            let exporting = Arc::clone(&inner.exporting);
            let export = std::rc::Rc::new(std::cell::RefCell::new(Some(gpui::Task::ready(Some(
                gpui::ExternalWindowParent::new("x11:original", Lease(old_released)),
            )))));
            let mut presenter = FilePreviewPresenter::new(ExportPanel {
                inner,
                export: export.clone(),
            });
            let shown = cx
                .update(|window, app| presenter.preview_in_window(&target("original"), window, app))
                .unwrap()
                .expect("Linux previews are deferred");
            cx.run_until_parked();
            assert_eq!(
                fixture.shows.recv_timeout(WAIT).unwrap().0,
                file_uri(&original)
            );
            settle(&service);
            assert!(
                pollster::block_on(shown.failure()).is_none(),
                "a presented request completes without a failure"
            );

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
                service
                    .bus
                    .dispatch(move |_| {
                        entered.send(()).unwrap();
                        resume_bus.recv_timeout(WAIT).unwrap();
                    })
                    .unwrap();
                running.recv_timeout(WAIT).unwrap();
            }
            let stale = cx
                .update(|window, app| presenter.preview_in_window(&requested_target, window, app))
                .unwrap()
                .expect("Linux previews are deferred");
            cx.run_until_parked();
            std::fs::rename(&requested, directory.join("retired")).unwrap();
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
            let retired = fixture.closes.recv_timeout(WAIT);
            assert!(
                fixture.shows.try_recv().is_err(),
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
            fixture.release.send(()).unwrap();
            settle(&service);
            cx.run_until_parked();
            old_lease.recv_timeout(WAIT).unwrap();
            new_lease.recv_timeout(WAIT).unwrap();
            assert!(!exporting.load(Ordering::Acquire));
            let failure = pollster::block_on(stale.failure()).expect("a stale request fails");
            assert_eq!(
                presenter.settle(failure),
                Some(FilePreviewError::StaleTarget)
            );
            let state = lock(&service.state);
            assert!(state.presented.is_none() && state.request.is_none());
        }

        // A window that cannot be exported fails the request instead of staying silent.
        let export = std::rc::Rc::new(std::cell::RefCell::new(Some(gpui::Task::ready(None))));
        let mut presenter = FilePreviewPresenter::new(ExportPanel {
            inner: factory.panel(),
            export,
        });
        std::fs::write(directory.join("unexported"), b"fixture").unwrap();
        let unexported = cx
            .update(|window, app| presenter.preview_in_window(&target("unexported"), window, app))
            .unwrap()
            .expect("Linux previews are deferred");
        cx.run_until_parked();
        let failure =
            pollster::block_on(unexported.failure()).expect("a failed export fails the request");
        assert_eq!(
            presenter.settle(failure),
            Some(FilePreviewError::PlatformUnavailable)
        );
        assert!(fixture.shows.try_recv().is_err());
    }
}
