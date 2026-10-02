//! Portal Settings facts with bounded startup discovery and cancellable observation.
use super::appearance::{
    AccessibilityDisplayOptions, AppearancePlatform, SystemAppearanceObservation,
    SystemAppearanceSubscription,
};
use super::linux_session_bus::{BusSubscription, SessionBus, SessionBusError};
use crate::appearance::Appearance;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zbus::zvariant::OwnedValue;
const NAME: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.Settings";
type Settings = HashMap<String, HashMap<String, OwnedValue>>;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Facts {
    appearance: Appearance,
    contrast: bool,
    reduced_motion: Option<bool>,
    animations: bool,
    primary: bool,
}
impl Default for Facts {
    fn default() -> Self {
        Self {
            appearance: Appearance::Light,
            contrast: false,
            reduced_motion: None,
            animations: true,
            primary: true,
        }
    }
}
impl Facts {
    fn update(&mut self, namespace: &str, key: &str, value: &OwnedValue) {
        match (namespace, key) {
            ("org.freedesktop.appearance", "color-scheme") => {
                if let Ok(value) = u32::try_from(value) {
                    self.appearance = if value == 1 {
                        Appearance::Dark
                    } else {
                        Appearance::Light
                    };
                }
            }
            ("org.freedesktop.appearance", "contrast") => {
                if let Ok(value) = u32::try_from(value) {
                    self.contrast = value == 1;
                }
            }
            ("org.freedesktop.appearance", "reduced-motion") => {
                self.reduced_motion = u32::try_from(value)
                    .ok()
                    .map(|value| value == 1)
                    .or_else(|| bool::try_from(value).ok());
            }
            ("org.gnome.desktop.interface", "enable-animations") => {
                if let Ok(value) = bool::try_from(value) {
                    self.animations = value;
                }
            }
            ("org.gnome.desktop.interface", "gtk-enable-primary-paste") => {
                if let Ok(value) = bool::try_from(value) {
                    self.primary = value;
                }
            }
            _ => {}
        }
    }
    fn read(connection: &zbus::blocking::Connection) -> Result<Self, SessionBusError> {
        let proxy = zbus::blocking::Proxy::new(connection, NAME, PATH, INTERFACE)
            .map_err(SessionBusError::from)?;
        let settings: Settings = proxy
            .call(
                "ReadAll",
                &(vec![
                    "org.freedesktop.appearance",
                    "org.gnome.desktop.interface",
                ],),
            )
            .map_err(SessionBusError::from)?;
        let mut facts = Self::default();
        for (namespace, values) in settings {
            for (key, value) in values {
                facts.update(&namespace, &key, &value);
            }
        }
        Ok(facts)
    }
}

pub(super) struct LinuxAppearancePlatform {
    bus: Option<SessionBus>,
    facts: Arc<Mutex<Facts>>,
}
impl LinuxAppearancePlatform {
    pub(super) fn new(bus: Option<SessionBus>) -> Self {
        let facts = bus
            .as_ref()
            .and_then(|bus| bus.query(Facts::read).ok())
            .unwrap_or_default();
        Self {
            bus,
            facts: Arc::new(Mutex::new(facts)),
        }
    }
    fn facts(&self) -> Facts {
        *self
            .facts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(super) fn primary_enabled(&self) -> bool {
        self.facts().primary
    }
}
struct Observation {
    _subscription: BusSubscription,
}
impl SystemAppearanceSubscription for Observation {}
impl AppearancePlatform for LinuxAppearancePlatform {
    fn system_appearance(&self) -> Option<Appearance> {
        Some(self.facts().appearance)
    }
    fn prefers_reduced_motion(&self) -> bool {
        let facts = self.facts();
        facts.reduced_motion.unwrap_or(!facts.animations)
    }
    fn accessibility_display_options(&self) -> AccessibilityDisplayOptions {
        AccessibilityDisplayOptions {
            increase_contrast: self.facts().contrast,
            ..Default::default()
        }
    }
    fn observe(&self) -> Option<SystemAppearanceObservation> {
        let bus = self.bus.as_ref()?;
        let (sender, changed) = async_channel::bounded(1);
        let facts = self.facts.clone();
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(NAME)
            .ok()?
            .interface(INTERFACE)
            .ok()?
            .path(PATH)
            .ok()?
            .member("SettingChanged")
            .ok()?
            .build()
            .to_owned();
        let subscription = bus
            .subscribe(rule.into(), move |message| {
                if let Ok((namespace, key, value)) =
                    message.body().deserialize::<(String, String, OwnedValue)>()
                {
                    let mut facts = facts
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let old = *facts;
                    facts.update(&namespace, &key, &value);
                    if old != *facts {
                        let _ = sender.try_send(());
                    }
                }
            })
            .ok()?;
        Some(SystemAppearanceObservation {
            changed,
            subscription: Box::new(Observation {
                _subscription: subscription,
            }),
        })
    }
    fn apply_native_appearance(&self, _: Appearance) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn appearance_keys_apply_only_their_documented_values() {
        let mut facts = Facts::default();
        facts.update("org.freedesktop.appearance", "color-scheme", &1u32.into());
        facts.update("org.freedesktop.appearance", "contrast", &1u32.into());
        facts.update(
            "org.gnome.desktop.interface",
            "enable-animations",
            &false.into(),
        );
        assert_eq!(facts.appearance, Appearance::Dark);
        assert!(facts.contrast);
        assert!(!facts.animations);
        facts.update("org.freedesktop.appearance", "reduced-motion", &2u32.into());
        assert_eq!(facts.reduced_motion, Some(false));
        facts.update("org.freedesktop.appearance", "color-scheme", &2u32.into());
        assert_eq!(facts.appearance, Appearance::Light);
        facts.update("org.freedesktop.appearance", "color-scheme", &0u32.into());
        assert_eq!(facts.appearance, Appearance::Light);
    }
}
