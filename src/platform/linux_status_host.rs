//! Whether a status-area host shows the client's StatusNotifierItem.
//!
//! A StatusNotifierWatcher accepts items whether or not any host draws them, and it can outlive
//! every host. The watcher's own IsStatusNotifierHostRegistered answer therefore decides. Each
//! watcher that appears receives the item's registration. Any failure counts as no host, so a
//! windowless client never keeps running where nothing can reach it.
use std::sync::mpsc;
use zbus::{MatchRule, blocking::Connection, message::Type, zvariant::OwnedValue};

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";

/// Registers the item with the session's watcher and reports, on a background thread, whether a
/// host shows it: once at first and again whenever that changes.
pub(super) fn observe(connection: &Connection, report: impl Fn(bool) + Send + 'static) {
    let (wake, woken) = mpsc::channel();
    // Subscribing before the first read means no change between the two goes unnoticed.
    for rule in rules() {
        subscribe(connection, rule, wake.clone());
    }
    let _ = wake.send(());
    let connection = connection.clone();
    std::thread::spawn(move || {
        let mut registered_with = None;
        let mut reported = None;
        // One thread reads the current answer after each change, so a slow read never reports
        // after a newer one.
        while woken.recv().is_ok() {
            while woken.try_recv().is_ok() {}
            let hosted = hosted(&connection, &mut registered_with);
            if reported != Some(hosted) {
                reported = Some(hosted);
                report(hosted);
            }
        }
    });
}

/// The changes after which the answer can differ: a watcher starting, exiting, or being
/// replaced, and the watcher announcing a host or a property change.
fn rules() -> Vec<MatchRule<'static>> {
    let owner = MatchRule::builder()
        .msg_type(Type::Signal)
        .sender("org.freedesktop.DBus")
        .and_then(|rule| rule.interface("org.freedesktop.DBus"))
        .and_then(|rule| rule.member("NameOwnerChanged"))
        .and_then(|rule| rule.arg(0, WATCHER))
        .map(|rule| rule.build());
    let hosts = MatchRule::builder()
        .msg_type(Type::Signal)
        .interface(WATCHER)
        .and_then(|rule| rule.path(WATCHER_PATH))
        .map(|rule| rule.build());
    let properties = MatchRule::builder()
        .msg_type(Type::Signal)
        .interface("org.freedesktop.DBus.Properties")
        .and_then(|rule| rule.member("PropertiesChanged"))
        .and_then(|rule| rule.path(WATCHER_PATH))
        .map(|rule| rule.build());
    [owner, hosts, properties]
        .into_iter()
        .filter_map(Result::ok)
        .map(|rule| rule.to_owned())
        .collect()
}

fn subscribe(connection: &Connection, rule: MatchRule<'static>, wake: mpsc::Sender<()>) {
    let Ok(messages) = zbus::blocking::MessageIterator::for_match_rule(rule, connection, None)
    else {
        return;
    };
    std::thread::spawn(move || {
        for _ in messages {
            if wake.send(()).is_err() {
                return;
            }
        }
    });
}

/// Registers the item with the current watcher if it has not seen it, then asks that watcher
/// whether a host is registered.
fn hosted(connection: &Connection, registered_with: &mut Option<String>) -> bool {
    let owner = zbus::blocking::fdo::DBusProxy::new(connection)
        .and_then(|bus| bus.get_name_owner(WATCHER.try_into()?).map_err(Into::into));
    let Ok(owner) = owner else {
        *registered_with = None;
        return false;
    };
    if registered_with.as_deref() != Some(owner.as_str()) {
        let Some(unique) = connection.unique_name().map(|name| name.to_string()) else {
            return false;
        };
        // The item registers by this connection's unique name. A previous client registered
        // its own, so a watcher that processes that client's exit removes only its item.
        let registered = connection.call_method(
            Some(WATCHER),
            WATCHER_PATH,
            Some(WATCHER),
            "RegisterStatusNotifierItem",
            &(unique.as_str(),),
        );
        if registered.is_err() {
            return false;
        }
        *registered_with = Some(owner.to_string());
    }
    connection
        .call_method(
            Some(WATCHER),
            WATCHER_PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(WATCHER, "IsStatusNotifierHostRegistered"),
        )
        .ok()
        .and_then(|reply| reply.body().deserialize::<OwnedValue>().ok())
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false)
}

#[cfg(all(test, feature = "native-tests"))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;
    use zbus::object_server::SignalEmitter;

    /// A session bus of the test's own, so nothing reaches the person's desktop session.
    struct PrivateBus {
        child: Child,
        address: String,
    }

    impl PrivateBus {
        fn new() -> Self {
            let mut child = Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("private session bus");
            let mut address = String::new();
            BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut address)
                .unwrap();
            Self {
                child,
                address: address.trim().to_owned(),
            }
        }

        fn connection(&self) -> zbus::blocking::connection::Builder<'_> {
            zbus::blocking::connection::Builder::address(self.address.as_str()).unwrap()
        }
    }

    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// A watcher that accepts items and whose host can come and go.
    struct Watcher {
        registered: mpsc::Sender<String>,
        hosted: Arc<AtomicBool>,
    }

    #[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
    impl Watcher {
        fn register_status_notifier_item(&self, service: &str) {
            let _ = self.registered.send(service.to_owned());
        }

        #[zbus(property)]
        fn is_status_notifier_host_registered(&self) -> bool {
            self.hosted.load(Ordering::SeqCst)
        }

        #[zbus(signal)]
        async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

        #[zbus(signal)]
        async fn status_notifier_host_unregistered(emitter: &SignalEmitter<'_>)
        -> zbus::Result<()>;
    }

    struct RunningWatcher {
        connection: Connection,
        hosted: Arc<AtomicBool>,
        registered: mpsc::Receiver<String>,
    }

    impl RunningWatcher {
        fn start(bus: &PrivateBus, hosted: bool) -> Self {
            let (registered, received) = mpsc::channel();
            let hosted = Arc::new(AtomicBool::new(hosted));
            let connection = bus
                .connection()
                .name(WATCHER)
                .unwrap()
                .serve_at(
                    WATCHER_PATH,
                    Watcher {
                        registered,
                        hosted: Arc::clone(&hosted),
                    },
                )
                .unwrap()
                .build()
                .unwrap();
            Self {
                connection,
                hosted,
                registered: received,
            }
        }

        fn set_host(&self, present: bool) {
            self.hosted.store(present, Ordering::SeqCst);
            let watcher = self
                .connection
                .object_server()
                .interface::<_, Watcher>(WATCHER_PATH)
                .unwrap();
            let emitter = watcher.signal_emitter();
            if present {
                zbus::block_on(Watcher::status_notifier_host_registered(emitter)).unwrap();
            } else {
                zbus::block_on(Watcher::status_notifier_host_unregistered(emitter)).unwrap();
            }
        }
    }

    const WAIT: Duration = Duration::from_secs(5);

    #[test]
    fn availability_follows_the_watchers_hosts_and_each_new_watcher() {
        let bus = PrivateBus::new();
        // A watcher with no host accepts the item, which nothing draws.
        let watcher = RunningWatcher::start(&bus, false);
        let item = bus.connection().build().unwrap();
        let (report, reports) = mpsc::channel();
        observe(&item, move |hosted| {
            let _ = report.send(hosted);
        });
        let unique = item.unique_name().unwrap().to_string();
        assert_eq!(watcher.registered.recv_timeout(WAIT).unwrap(), unique);
        assert_eq!(reports.recv_timeout(WAIT).unwrap(), false);

        watcher.set_host(true);
        assert_eq!(reports.recv_timeout(WAIT).unwrap(), true);

        // The last host leaves while its watcher stays.
        watcher.set_host(false);
        assert_eq!(reports.recv_timeout(WAIT).unwrap(), false);

        // A replacement watcher with a host receives the item again.
        drop(watcher);
        let replacement = RunningWatcher::start(&bus, true);
        assert_eq!(replacement.registered.recv_timeout(WAIT).unwrap(), unique);
        let mut hosted = false;
        while let Ok(report) = reports.recv_timeout(WAIT) {
            hosted = report;
            if hosted {
                break;
            }
        }
        assert!(hosted, "the replacement watcher's host shows the item");
    }
}
