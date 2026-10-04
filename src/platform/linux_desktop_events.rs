//! Coalesces desktop callbacks into one application-owned foreground task.
use gpui::{App, Task};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Pending {
    windows: HashMap<gpui::AnyWindowHandle, WindowEffects>,
    activation: Option<Option<String>>,
}

#[derive(Default)]
struct WindowEffects {
    bell: bool,
    attention: Option<bool>,
}

/// GPUI's native window operations are the only effects of this foreground event pump.
trait DesktopEffects {
    fn bell(&mut self, window: gpui::AnyWindowHandle);
    fn attention(&mut self, window: gpui::AnyWindowHandle, requested: bool);
    fn activate(&mut self, token: Option<String>);
}
struct GpuiEffects<'a>(&'a mut App);
impl DesktopEffects for GpuiEffects<'_> {
    fn bell(&mut self, handle: gpui::AnyWindowHandle) {
        let _ = handle.update(self.0, |_, window, _| window.play_system_bell());
    }
    fn attention(&mut self, handle: gpui::AnyWindowHandle, requested: bool) {
        let _ = handle.update(self.0, |_, window, _| {
            if requested {
                window.request_attention();
            } else {
                window.cancel_attention();
            }
        });
    }
    fn activate(&mut self, token: Option<String>) {
        crate::app::activate_default_window(self.0, token.as_deref());
    }
}
impl Pending {
    fn apply(self, effects: &mut impl DesktopEffects) {
        for (window, pending) in self.windows {
            if pending.bell {
                effects.bell(window);
            }
            if let Some(requested) = pending.attention {
                effects.attention(window, requested);
            }
        }
        if let Some(token) = self.activation {
            effects.activate(token);
        }
    }
}

#[derive(Clone)]
pub(super) struct DesktopEventSender {
    pending: Arc<Mutex<Pending>>,
    wake: async_channel::Sender<()>,
}
impl DesktopEventSender {
    fn send(&self, change: impl FnOnce(&mut Pending)) {
        change(
            &mut self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let _ = self.wake.try_send(());
    }
    pub(super) fn bell(&self, window: gpui::AnyWindowHandle) {
        self.send(|pending| pending.windows.entry(window).or_default().bell = true);
    }
    pub(super) fn attention(&self, window: gpui::AnyWindowHandle, requested: bool) {
        self.send(|pending| pending.windows.entry(window).or_default().attention = Some(requested));
    }
    pub(super) fn activate(&self, token: Option<String>) {
        let token = token.filter(|value| !value.is_empty() && value.len() <= 4096);
        self.send(|pending| pending.activation = Some(token));
    }
}

pub(super) struct LinuxDesktopEvents {
    identity: crate::application_identity::ApplicationIdentity,
    pending: Arc<Mutex<Pending>>,
    receiver: RefCell<Option<async_channel::Receiver<()>>>,
    task: RefCell<Option<Task<()>>>,
}
impl LinuxDesktopEvents {
    pub(super) fn new(
        identity: crate::application_identity::ApplicationIdentity,
    ) -> (DesktopEventSender, Self) {
        let (wake, receiver) = async_channel::bounded(1);
        let pending = Arc::new(Mutex::new(Pending::default()));
        (
            DesktopEventSender {
                pending: pending.clone(),
                wake,
            },
            Self {
                identity,
                pending,
                receiver: RefCell::new(Some(receiver)),
                task: RefCell::new(None),
            },
        )
    }
    fn start(&self, cx: &mut App) {
        let Some(receiver) = self.receiver.borrow_mut().take() else {
            return;
        };
        let pending = self.pending.clone();
        *self.task.borrow_mut() = Some(cx.spawn(async move |cx| {
            while receiver.recv().await.is_ok() {
                let events = std::mem::take(
                    &mut *pending
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                );
                cx.update(|cx| events.apply(&mut GpuiEffects(cx)));
            }
        }));
    }
}

impl super::desktop_events::DesktopEventAdapter for LinuxDesktopEvents {
    fn install(&self, cx: &mut App) {
        cx.set_app_identity(self.identity.application_id(), self.identity.display_name());
        self.start(cx);
    }
}

#[cfg(all(test, feature = "native-tests"))]
mod tests {
    use crate::platform::linux_session_bus::SessionBus;
    use crate::terminal::attention_notification::NotificationAdapter;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;
    use zbus::zvariant::OwnedValue;

    #[gpui::test]
    fn linux_desktop_events_coalesce_and_deliver_to_each_origin(cx: &mut gpui::TestAppContext) {
        #[derive(Default)]
        struct Recorded {
            bells: Vec<gpui::AnyWindowHandle>,
            attention: HashMap<gpui::AnyWindowHandle, bool>,
        }
        impl super::DesktopEffects for Recorded {
            fn bell(&mut self, window: gpui::AnyWindowHandle) {
                self.bells.push(window);
            }
            fn attention(&mut self, window: gpui::AnyWindowHandle, requested: bool) {
                self.attention.insert(window, requested);
            }
            fn activate(&mut self, _token: Option<String>) {}
        }
        use crate::terminal::attention_runtime::{AudioBell, DockAttentionDriver};
        let first = cx.add_window(|_, _| gpui::Empty).into();
        let second = cx.add_window(|_, _| gpui::Empty).into();
        let (sender, events) = super::LinuxDesktopEvents::new(
            crate::application_identity::ApplicationIdentity::current(),
        );
        let mut audio = crate::platform::linux_attention::LinuxAudioBell(sender.clone());
        let mut attention = crate::platform::linux_attention::LinuxWindowAttention::new(sender);
        audio.play(Some(first));
        audio.play(Some(first));
        audio.play(Some(second));
        attention.set_windows(&[first]);
        attention.request().unwrap();
        attention.set_windows(&[first, second]);
        let mut recorded = Recorded::default();
        std::mem::take(&mut *events.pending.lock().unwrap()).apply(&mut recorded);
        assert_eq!(recorded.bells.len(), 2);
        assert!(recorded.bells.contains(&first) && recorded.bells.contains(&second));
        assert_eq!(
            recorded.attention,
            HashMap::from([(first, true), (second, true)])
        );
        attention.set_windows(&[second]);
        attention.cancel();
        let mut cancellations = Recorded::default();
        std::mem::take(&mut *events.pending.lock().unwrap()).apply(&mut cancellations);
        assert_eq!(
            cancellations.attention,
            HashMap::from([(first, false), (second, false)])
        );
    }

    #[gpui::test]
    fn linux_desktop_events_install_identity_and_start_pump(cx: &mut gpui::TestAppContext) {
        use crate::platform::desktop_events::DesktopEventAdapter;
        let identity = crate::application_identity::ApplicationIdentity::current();
        let (sender, events) = super::LinuxDesktopEvents::new(identity);
        let window = cx.add_window(|_, _| gpui::Empty).into();
        cx.update(|cx| events.install(cx));
        assert_eq!(
            cx.app_identity(),
            Some((
                identity.application_id().into(),
                identity.display_name().into()
            ))
        );
        sender.bell(window);
        cx.run_until_parked();
        assert!(events.pending.lock().unwrap().windows.is_empty());
        assert!(events.task.borrow().is_some());
    }

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
        fn client(&self) -> SessionBus {
            SessionBus::connect_to(Some(self.address.clone())).unwrap()
        }
        fn server(&self) -> zbus::blocking::connection::Builder<'_> {
            zbus::blocking::connection::Builder::address(self.address.as_str()).unwrap()
        }
    }
    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[derive(Debug)]
    struct Notification {
        replaces: u32,
        summary: String,
        body: String,
        actions: Vec<String>,
        desktop: String,
        urgency: u8,
    }
    struct Notifications {
        submitted: mpsc::Sender<Notification>,
        closed: mpsc::Sender<u32>,
        reply_delay: Duration,
    }
    #[zbus::interface(name = "org.freedesktop.Notifications")]
    impl Notifications {
        fn get_server_information(&self) -> (String, String, String, String) {
            ("fixture".into(), "fixture".into(), "1".into(), "1.2".into())
        }
        #[allow(clippy::too_many_arguments)]
        fn notify(
            &self,
            _app_name: &str,
            replaces_id: u32,
            _icon: &str,
            summary: &str,
            body: &str,
            actions: Vec<String>,
            hints: HashMap<String, OwnedValue>,
            _expire: i32,
        ) -> u32 {
            self.submitted
                .send(Notification {
                    replaces: replaces_id,
                    summary: summary.into(),
                    body: body.into(),
                    actions,
                    desktop: <&str>::try_from(hints.get("desktop-entry").unwrap())
                        .unwrap()
                        .into(),
                    urgency: u8::try_from(hints.get("urgency").unwrap()).unwrap(),
                })
                .unwrap();
            assert_eq!(hints.len(), 2);
            std::thread::sleep(self.reply_delay);
            42
        }
        fn close_notification(&self, id: u32) {
            self.closed.send(id).unwrap();
        }
    }
    struct RestartNotifications {
        submitted: mpsc::Sender<u32>,
        gate: std::sync::Mutex<Option<mpsc::Receiver<()>>>,
        reject: std::sync::Arc<std::sync::atomic::AtomicBool>,
        id: u32,
        foreign_id: bool,
    }
    #[zbus::interface(name = "org.freedesktop.Notifications")]
    impl RestartNotifications {
        #[allow(clippy::too_many_arguments)]
        fn notify(
            &self,
            _app: &str,
            replaces: u32,
            _icon: &str,
            _summary: &str,
            _body: &str,
            _actions: Vec<String>,
            _hints: HashMap<String, OwnedValue>,
            _expire: i32,
        ) -> zbus::fdo::Result<u32> {
            let rejected = (self.foreign_id && replaces == 42)
                || self.reject.swap(false, std::sync::atomic::Ordering::SeqCst);
            self.submitted.send(replaces).unwrap();
            if let Some(gate) = self.gate.lock().unwrap().take() {
                gate.recv_timeout(Duration::from_secs(3)).unwrap();
            }
            if rejected {
                return Err(zbus::fdo::Error::AccessDenied("fixture rejection".into()));
            }
            Ok(self.id)
        }
        fn close_notification(&self, _id: u32) {}
    }

    fn notification_restart(in_flight: bool) {
        let private = PrivateBus::new();
        let (submitted, notifications) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let old = private
            .server()
            .allow_name_replacements(true)
            .serve_at(
                "/org/freedesktop/Notifications",
                RestartNotifications {
                    submitted: submitted.clone(),
                    gate: std::sync::Mutex::new(in_flight.then_some(gate)),
                    reject: Default::default(),
                    id: 42,
                    foreign_id: false,
                },
            )
            .unwrap()
            .name("org.freedesktop.Notifications")
            .unwrap()
            .build()
            .unwrap();
        let (sender, _events) = super::LinuxDesktopEvents::new(
            crate::application_identity::ApplicationIdentity::current(),
        );
        let adapter = crate::platform::linux_notification::LinuxNotificationAdapter::new(
            Some(private.client()),
            crate::application_identity::ApplicationIdentity::current(),
            sender,
        );
        adapter.submit(1).unwrap();
        assert_eq!(
            notifications.recv_timeout(Duration::from_secs(2)).unwrap(),
            0
        );
        if !in_flight {
            // Receipt of Clear at the old service would retire the ID, so instead wait for the
            // notification reply through a second request and keep its returned ID retained.
            adapter.submit(2).unwrap();
            assert_eq!(
                notifications.recv_timeout(Duration::from_secs(2)).unwrap(),
                42
            );
        }
        if !in_flight {
            old.release_name("org.freedesktop.Notifications").unwrap();
        }
        let replacement = private
            .server()
            .replace_existing_names(true)
            .serve_at(
                "/org/freedesktop/Notifications",
                RestartNotifications {
                    submitted,
                    gate: std::sync::Mutex::new(None),
                    reject: Default::default(),
                    id: 43,
                    foreign_id: true,
                },
            )
            .unwrap()
            .name("org.freedesktop.Notifications")
            .unwrap()
            .build()
            .unwrap();
        // The old connection remains alive and can reply after losing the well-known name.
        if in_flight {
            release.send(()).unwrap();
        }
        adapter.submit(3).unwrap();
        assert_eq!(
            notifications.recv_timeout(Duration::from_secs(3)).unwrap(),
            0,
            "the replacement service owns ID 42 for another client"
        );
        adapter.submit(4).unwrap();
        assert_eq!(
            notifications.recv_timeout(Duration::from_secs(2)).unwrap(),
            43
        );
        drop(replacement);
    }
    #[test]
    fn linux_desktop_notifications_restart_invalidates_retained_id() {
        notification_restart(false);
    }
    #[test]
    fn linux_desktop_notifications_restart_discards_in_flight_id() {
        notification_restart(true);
    }

    #[test]
    fn linux_desktop_notifications_rejected_replacement_is_not_retained() {
        let private = PrivateBus::new();
        let (submitted, notifications) = mpsc::channel();
        let reject = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _server = private
            .server()
            .serve_at(
                "/org/freedesktop/Notifications",
                RestartNotifications {
                    submitted,
                    gate: std::sync::Mutex::new(None),
                    reject: reject.clone(),
                    id: 43,
                    foreign_id: false,
                },
            )
            .unwrap()
            .name("org.freedesktop.Notifications")
            .unwrap()
            .build()
            .unwrap();
        let (sender, _events) = super::LinuxDesktopEvents::new(
            crate::application_identity::ApplicationIdentity::current(),
        );
        let adapter = crate::platform::linux_notification::LinuxNotificationAdapter::new(
            Some(private.client()),
            crate::application_identity::ApplicationIdentity::current(),
            sender,
        );
        adapter.submit(1).unwrap();
        assert_eq!(
            notifications.recv_timeout(Duration::from_secs(2)).unwrap(),
            0
        );
        adapter.submit(2).unwrap();
        assert_eq!(
            notifications.recv_timeout(Duration::from_secs(2)).unwrap(),
            43
        );
        reject.store(true, std::sync::atomic::Ordering::SeqCst);
        adapter.submit(3).unwrap();
        assert_eq!(
            notifications.recv_timeout(Duration::from_secs(2)).unwrap(),
            43
        );
        adapter.submit(4).unwrap();
        assert_eq!(
            notifications.recv_timeout(Duration::from_secs(2)).unwrap(),
            0
        );
    }

    fn receive<T>(receiver: &async_channel::Receiver<T>) -> Result<T, async_channel::TryRecvError> {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match receiver.try_recv() {
                Err(async_channel::TryRecvError::Empty) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                result => return result,
            }
        }
    }
    fn wait_activation(events: &super::LinuxDesktopEvents) -> Option<String> {
        receive(events.receiver.borrow().as_ref().unwrap()).unwrap();
        events.pending.lock().unwrap().activation.take().unwrap()
    }
    #[test]
    fn linux_desktop_notifications_replace_close_and_forward_activation_token() {
        let private = PrivateBus::new();
        let (submitted, notifications) = mpsc::channel();
        let (closed, closes) = mpsc::channel();
        let server = private
            .server()
            .serve_at(
                "/org/freedesktop/Notifications",
                Notifications {
                    submitted,
                    closed,
                    reply_delay: Duration::ZERO,
                },
            )
            .unwrap()
            .name("org.freedesktop.Notifications")
            .unwrap()
            .build()
            .unwrap();
        let bus = private.client();
        let (sender, events) = super::LinuxDesktopEvents::new(
            crate::application_identity::ApplicationIdentity::current(),
        );
        let identity = crate::application_identity::ApplicationIdentity::current();
        let adapter = crate::platform::linux_notification::LinuxNotificationAdapter::new(
            Some(bus.clone()),
            identity,
            sender,
        );
        let (authorized, authorization) = mpsc::channel();
        adapter.settings(Box::new(move |result| {
            authorized.send(result).unwrap();
        }));
        assert!(
            authorization
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap()
                .alert_enabled
        );
        adapter.submit(3).unwrap();
        let first = notifications.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(first.replaces, 0);
        assert_eq!(first.summary, identity.display_name());
        assert_eq!(first.body, "Terminal requested attention (3)");
        assert_eq!(first.actions, ["default", "Open"]);
        assert_eq!(first.desktop, identity.application_id());
        assert_eq!(first.urgency, 1);
        bus.query(|_| Ok(())).unwrap();
        adapter.submit(4).unwrap();
        assert_eq!(
            notifications
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .replaces,
            42
        );
        bus.query(|_| Ok(())).unwrap();
        server
            .emit_signal(
                None::<&str>,
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
                "ActivationToken",
                &(42u32, "private-activation-token"),
            )
            .unwrap();
        server
            .emit_signal(
                None::<&str>,
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
                "ActionInvoked",
                &(42u32, "default"),
            )
            .unwrap();
        assert_eq!(
            wait_activation(&events).as_deref(),
            Some("private-activation-token")
        );
        adapter.clear().unwrap();
        assert_eq!(closes.recv_timeout(Duration::from_secs(2)).unwrap(), 42);
        adapter.submit(1).unwrap();
        assert_eq!(
            notifications
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .replaces,
            0
        );
    }

    #[test]
    fn linux_desktop_notifications_own_an_id_that_arrives_after_the_method_timeout() {
        let private = PrivateBus::new();
        let (submitted, notifications) = mpsc::channel();
        let (closed, closes) = mpsc::channel();
        let _server = private
            .server()
            .serve_at(
                "/org/freedesktop/Notifications",
                Notifications {
                    submitted,
                    closed,
                    reply_delay: crate::platform::linux_session_bus::METHOD_TIMEOUT
                        + Duration::from_millis(250),
                },
            )
            .unwrap()
            .name("org.freedesktop.Notifications")
            .unwrap()
            .build()
            .unwrap();
        let (sender, _events) = super::LinuxDesktopEvents::new(
            crate::application_identity::ApplicationIdentity::current(),
        );
        let adapter = crate::platform::linux_notification::LinuxNotificationAdapter::new(
            Some(private.client()),
            crate::application_identity::ApplicationIdentity::current(),
            sender,
        );

        let wait = Duration::from_secs(3);
        adapter.submit(1).unwrap();
        assert_eq!(notifications.recv_timeout(wait).unwrap().replaces, 0);
        adapter.clear().unwrap();
        assert_eq!(
            closes.recv_timeout(wait).unwrap(),
            42,
            "clearing closes the notification whose id arrives later"
        );

        adapter.submit(2).unwrap();
        assert_eq!(notifications.recv_timeout(wait).unwrap().replaces, 0);
        adapter.submit(3).unwrap();
        let replacement = notifications.recv_timeout(wait).unwrap();
        assert_eq!(
            replacement.replaces, 42,
            "replacement uses the id the server returned late"
        );
        assert_eq!(replacement.body, "Terminal requested attention (3)");
        assert!(closes.try_recv().is_err());
    }

    #[test]
    fn linux_desktop_second_instance_forwards_token_without_claiming_the_name() {
        let private = PrivateBus::new();
        let primary = private.client();
        let secondary = private.client();
        let (sender, events) = super::LinuxDesktopEvents::new(
            crate::application_identity::ApplicationIdentity::current(),
        );
        let identity = crate::application_identity::ApplicationIdentity::current();
        assert!(
            !crate::platform::linux_application_instance::forward_if_secondary(
                &primary,
                identity,
                crate::platform::linux_application_instance::InstanceLaunch::Activate,
                sender.clone(),
                None
            )
            .unwrap()
        );
        assert!(
            crate::platform::linux_application_instance::forward_if_secondary(
                &secondary,
                identity,
                crate::platform::linux_application_instance::InstanceLaunch::Activate,
                sender,
                Some("launch-token".into())
            )
            .unwrap()
        );
        assert_eq!(wait_activation(&events).as_deref(), Some("launch-token"));
        assert!(events.pending.lock().unwrap().activation.is_none());
    }

    #[test]
    fn linux_desktop_new_instance_runs_beside_the_owner_and_inherits_activation() {
        use crate::platform::linux_application_instance::{InstanceLaunch, forward_if_secondary};
        let private = PrivateBus::new();
        let identity = crate::application_identity::ApplicationIdentity::current();
        let name = identity.application_id();
        let earlier = private.server().name(name).unwrap().build().unwrap();
        let fresh = private.client();
        let (sender, events) = super::LinuxDesktopEvents::new(
            crate::application_identity::ApplicationIdentity::current(),
        );

        assert!(
            !forward_if_secondary(&fresh, identity, InstanceLaunch::New, sender, None).unwrap(),
            "a new instance runs even while another owns activation"
        );

        earlier.release_name(name).unwrap();
        let (later, _) = super::LinuxDesktopEvents::new(
            crate::application_identity::ApplicationIdentity::current(),
        );
        assert!(
            forward_if_secondary(
                &private.client(),
                identity,
                InstanceLaunch::Activate,
                later,
                Some("launch-token".into())
            )
            .unwrap()
        );
        assert_eq!(wait_activation(&events).as_deref(), Some("launch-token"));
    }

    #[test]
    fn linux_desktop_only_the_new_instance_argument_selects_a_new_instance() {
        use crate::platform::linux_application_instance::InstanceLaunch;
        let launch = |arguments: &[&str]| {
            InstanceLaunch::from_arguments(arguments.iter().map(std::ffi::OsString::from))
        };
        assert_eq!(launch(&["--new-instance"]), InstanceLaunch::New);
        assert_eq!(launch(&[]), InstanceLaunch::Activate);
        assert_eq!(launch(&["--new-instance", "x"]), InstanceLaunch::Activate);
        assert_eq!(launch(&["--new"]), InstanceLaunch::Activate);
    }

    /// Dark, increased contrast, reduced motion, and status shapes differ from the defaults.
    fn non_default_settings() -> HashMap<String, HashMap<String, OwnedValue>> {
        HashMap::from([
            (
                "org.freedesktop.appearance".into(),
                HashMap::from([
                    ("color-scheme".into(), 1u32.into()),
                    ("contrast".into(), 1u32.into()),
                ]),
            ),
            (
                "org.gnome.desktop.interface".into(),
                HashMap::from([("enable-animations".into(), false.into())]),
            ),
            (
                "org.gnome.desktop.a11y.interface".into(),
                HashMap::from([("show-status-shapes".into(), true.into())]),
            ),
        ])
    }
    struct Settings(HashMap<String, HashMap<String, OwnedValue>>);
    #[zbus::interface(name = "org.freedesktop.portal.Settings")]
    impl Settings {
        fn read_all(
            &self,
            namespaces: Vec<String>,
        ) -> HashMap<String, HashMap<String, OwnedValue>> {
            self.0
                .iter()
                .filter(|(namespace, _)| namespaces.contains(namespace))
                .map(|(namespace, values)| {
                    (
                        namespace.clone(),
                        values
                            .iter()
                            .map(|(key, value)| (key.clone(), value.try_clone().unwrap()))
                            .collect(),
                    )
                })
                .collect()
        }
    }
    #[test]
    fn linux_desktop_portal_window_style_decodes_theme_and_icons() {
        use crate::platform::app_directories::{
            AppDirectoryEnvironment, DesktopResourceDirectories,
        };
        let private = PrivateBus::new();
        let temporary =
            std::env::temp_dir().join(format!("spaceterm-portal-icons-{}", std::process::id()));
        let icon = temporary.as_path().join("icons/ReviewIcons/16/ui");
        std::fs::create_dir_all(&icon).unwrap();
        std::fs::write(
            icon.parent().unwrap().parent().unwrap().join("index.theme"),
            "[Icon Theme]\nDirectories=16/ui\n[16/ui]\nSize=16\nType=Fixed\n",
        )
        .unwrap();
        std::fs::write(
            icon.join("window-close-symbolic.svg"),
            "<svg>review-icon</svg>",
        )
        .unwrap();
        let resources = DesktopResourceDirectories::resolve(
            &AppDirectoryEnvironment {
                xdg_data_home: Some(temporary.as_path().into()),
                xdg_config_home: Some(temporary.as_path().into()),
                ..Default::default()
            },
            Some(temporary.as_path().as_os_str()),
        );
        let _server = private
            .server()
            .serve_at(
                "/org/freedesktop/portal/desktop",
                Settings(HashMap::from([(
                    "org.gnome.desktop.interface".into(),
                    HashMap::from([
                        (
                            "gtk-theme".into(),
                            zbus::zvariant::Value::from("Breeze").try_into().unwrap(),
                        ),
                        (
                            "icon-theme".into(),
                            zbus::zvariant::Value::from("ReviewIcons")
                                .try_into()
                                .unwrap(),
                        ),
                    ]),
                )])),
            )
            .unwrap()
            .name("org.freedesktop.portal.Desktop")
            .unwrap()
            .build()
            .unwrap();
        let controls = crate::platform::linux_window_style::capture_for_desktop(
            Some(&private.client()),
            &resources,
            "",
        );
        assert_eq!(controls.style, spaceterm_ui::DesktopWindowStyle::Breeze);
        assert_eq!(
            controls.icons[0].as_deref(),
            Some(b"<svg>review-icon</svg>".as_slice())
        );
        std::fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    fn linux_desktop_portal_appearance_reads_watches_and_stops_on_drop() {
        use crate::platform::appearance::AppearancePlatform;
        let private = PrivateBus::new();
        let server = private
            .server()
            .serve_at(
                "/org/freedesktop/portal/desktop",
                Settings(non_default_settings()),
            )
            .unwrap()
            .name("org.freedesktop.portal.Desktop")
            .unwrap()
            .build()
            .unwrap();
        let appearance =
            crate::platform::linux_appearance::LinuxAppearancePlatform::new(Some(private.client()));
        assert_eq!(
            appearance.system_appearance(),
            Some(crate::appearance::Appearance::Dark)
        );
        assert!(appearance.prefers_reduced_motion());
        assert!(appearance.accessibility_display_options().increase_contrast);
        assert!(
            appearance
                .accessibility_display_options()
                .differentiate_without_color
        );
        let observation = appearance.observe().unwrap();
        for (value, expected) in [
            (OwnedValue::from(false), false),
            (OwnedValue::from(true), true),
            (OwnedValue::from(1u32), false),
        ] {
            server
                .emit_signal(
                    None::<&str>,
                    "/org/freedesktop/portal/desktop",
                    "org.freedesktop.portal.Settings",
                    "SettingChanged",
                    &(
                        "org.gnome.desktop.a11y.interface",
                        "show-status-shapes",
                        value,
                    ),
                )
                .unwrap();
            receive(&observation.changed).unwrap();
            assert_eq!(
                appearance
                    .accessibility_display_options()
                    .differentiate_without_color,
                expected
            );
        }
        server
            .emit_signal(
                None::<&str>,
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.Settings",
                "SettingChanged",
                &(
                    "org.freedesktop.appearance",
                    "color-scheme",
                    zbus::zvariant::Value::from(2u32),
                ),
            )
            .unwrap();
        receive(&observation.changed).unwrap();
        assert_eq!(
            appearance.system_appearance(),
            Some(crate::appearance::Appearance::Light)
        );
        let changed = observation.changed.clone();
        drop(observation);
        assert_eq!(
            receive(&changed),
            Err(async_channel::TryRecvError::Closed),
            "dropping observation stops and closes the watcher"
        );
    }

    fn assert_status_shapes_disabled(value: Option<OwnedValue>) {
        use crate::platform::appearance::AppearancePlatform;
        let private = PrivateBus::new();
        let values = value
            .map(|value| HashMap::from([("show-status-shapes".into(), value)]))
            .unwrap_or_default();
        let _server = private
            .server()
            .serve_at(
                "/org/freedesktop/portal/desktop",
                Settings(HashMap::from([(
                    "org.gnome.desktop.a11y.interface".into(),
                    values,
                )])),
            )
            .unwrap()
            .name("org.freedesktop.portal.Desktop")
            .unwrap()
            .build()
            .unwrap();
        let appearance =
            crate::platform::linux_appearance::LinuxAppearancePlatform::new(Some(private.client()));
        assert!(
            !appearance
                .accessibility_display_options()
                .differentiate_without_color
        );
    }

    #[test]
    fn linux_desktop_portal_absent_status_shapes_keeps_color_only_cues() {
        assert_status_shapes_disabled(None);
    }

    #[test]
    fn linux_desktop_portal_malformed_status_shapes_keeps_color_only_cues() {
        assert_status_shapes_disabled(Some(1u32.into()));
    }

    #[test]
    fn linux_desktop_session_bus_rejects_backlog_without_blocking_the_caller() {
        use crate::platform::linux_session_bus::SessionBusError;
        let private = PrivateBus::new();
        let bus = private.client();
        let (entered, running) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        bus.dispatch(move |_| {
            entered.send(()).unwrap();
            proceed.recv_timeout(Duration::from_secs(2)).unwrap();
        })
        .unwrap();
        running.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..32 {
            bus.dispatch(|_| {}).unwrap();
        }
        assert_eq!(
            bus.dispatch(|_| panic!("rejected operation must not run")),
            Err(SessionBusError::Rejected)
        );
        release.send(()).unwrap();
    }

    struct UnresponsiveSettings;
    #[zbus::interface(name = "org.freedesktop.portal.Settings")]
    impl UnresponsiveSettings {
        fn read_all(
            &self,
            _namespaces: Vec<String>,
        ) -> HashMap<String, HashMap<String, OwnedValue>> {
            // Later than the method timeout but within the caller's wait, so only the
            // method timeout keeps these late facts out.
            std::thread::sleep(Duration::from_millis(750));
            non_default_settings()
        }
    }
    #[test]
    fn linux_desktop_portal_timeout_keeps_default_appearance() {
        use crate::platform::appearance::AppearancePlatform;
        let private = PrivateBus::new();
        let _server = private
            .server()
            .serve_at("/org/freedesktop/portal/desktop", UnresponsiveSettings)
            .unwrap()
            .name("org.freedesktop.portal.Desktop")
            .unwrap()
            .build()
            .unwrap();
        let bus = private.client();
        let started = std::time::Instant::now();
        let appearance = crate::platform::linux_appearance::LinuxAppearancePlatform::new(Some(bus));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            appearance.system_appearance(),
            Some(crate::appearance::Appearance::Light)
        );
        assert!(!appearance.prefers_reduced_motion());
        assert!(!appearance.accessibility_display_options().increase_contrast);
    }
}
