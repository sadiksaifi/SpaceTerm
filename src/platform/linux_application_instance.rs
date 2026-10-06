//! Freedesktop single-instance activation. Tokens cross the bus without diagnostics content.
use super::linux_desktop_events::DesktopEventSender;
use super::linux_session_bus::{SessionBus, SessionBusError};
use crate::application_identity::ApplicationIdentity;
use std::collections::HashMap;
use zbus::zvariant::OwnedValue;

struct Application {
    events: DesktopEventSender,
}
#[zbus::interface(name = "org.freedesktop.Application")]
impl Application {
    fn activate(&self, platform_data: HashMap<String, OwnedValue>) {
        let token = platform_data
            .get("activation-token")
            .or_else(|| platform_data.get("desktop-startup-id"))
            .and_then(|value| <&str>::try_from(value).ok())
            .map(str::to_owned);
        self.events.activate(token);
    }
}

/// How a launch treats an instance that already owns the application name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InstanceLaunch {
    /// Desktop and command-line launches activate the running instance and exit.
    Activate,
    /// The development launcher runs a fresh build beside a running instance, like macOS
    /// `open -n`, and takes over activation when the earlier instance exits.
    New,
}

impl InstanceLaunch {
    pub(super) const NEW_INSTANCE_ARGUMENT: &str = "--new-instance";

    /// `arguments` excludes the program name.
    pub(super) fn from_arguments(mut arguments: impl Iterator<Item = std::ffi::OsString>) -> Self {
        match (arguments.next(), arguments.next()) {
            (Some(argument), None) if argument == Self::NEW_INSTANCE_ARGUMENT => Self::New,
            _ => Self::Activate,
        }
    }
}

/// A bus outage leaves source builds usable. Only a confirmed forwarded activation exits.
pub(super) fn forward_if_secondary(
    bus: &SessionBus,
    identity: ApplicationIdentity,
    launch: InstanceLaunch,
    events: DesktopEventSender,
    token: Option<String>,
) -> Result<bool, SessionBusError> {
    bus.query(move |connection| {
        let name = identity.application_id();
        let path = format!("/{}", name.replace('.', "/").replace('-', "_"));
        // Register before claiming, so another launch cannot race an uninstalled interface.
        connection
            .object_server()
            .at(path.as_str(), Application { events })
            .map_err(SessionBusError::from)?;
        let flags = match launch {
            InstanceLaunch::Activate => zbus::fdo::RequestNameFlags::DoNotQueue.into(),
            InstanceLaunch::New => Default::default(),
        };
        let reply = match connection.request_name_with_flags(name, flags) {
            Ok(reply) => reply,
            Err(zbus::Error::NameTaken) => zbus::fdo::RequestNameReply::Exists,
            Err(error) => return Err(error.into()),
        };
        // A queued new instance owns activation once every earlier instance exits.
        if matches!(
            reply,
            zbus::fdo::RequestNameReply::PrimaryOwner
                | zbus::fdo::RequestNameReply::AlreadyOwner
                | zbus::fdo::RequestNameReply::InQueue
        ) {
            return Ok(false);
        }
        let mut data = HashMap::new();
        if let Some(token) = token.filter(|token| token.len() <= 4096) {
            data.insert("activation-token", zbus::zvariant::Value::from(token));
        }
        let proxy = zbus::blocking::Proxy::new(
            connection,
            name,
            path.as_str(),
            "org.freedesktop.Application",
        )
        .map_err(SessionBusError::from)?;
        proxy
            .call::<_, _, ()>("Activate", &(data,))
            .map_err(SessionBusError::from)?;
        Ok(true)
    })
}
