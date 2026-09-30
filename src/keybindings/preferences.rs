use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use super::{Command, Shortcut, TerminalConvention};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct KeybindingPreferences(BTreeMap<Command, Option<Shortcut>>);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum KeybindingPreferencesError {
    #[error("multiple commands override the same shortcut")]
    DuplicateShortcut,
    #[error("an override is reserved for terminal input on every desktop")]
    TerminalReserved(TerminalConvention),
}

/// A settings document never assigns terminal input that every desktop reserves. The Keymap
/// Profile blocks the remaining desktop-specific reservations when it resolves the overrides.
impl<'de> Deserialize<'de> for KeybindingPreferences {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let overrides = BTreeMap::<Command, Option<Shortcut>>::deserialize(deserializer)?;
        if let Some(convention) = overrides
            .values()
            .flatten()
            .find_map(super::terminal_conventions::universal_reservation)
        {
            return Err(serde::de::Error::custom(
                KeybindingPreferencesError::TerminalReserved(convention),
            ));
        }
        Ok(Self(overrides))
    }
}

impl KeybindingPreferences {
    pub fn validate(&self) -> Result<(), KeybindingPreferencesError> {
        let mut seen = HashSet::new();
        for shortcut in self.0.values().flatten() {
            if !seen.insert(shortcut) {
                return Err(KeybindingPreferencesError::DuplicateShortcut);
            }
        }
        Ok(())
    }

    pub fn get(&self, command: Command) -> Option<&Option<Shortcut>> {
        self.0.get(&command)
    }

    pub fn is_overridden(&self, command: Command) -> bool {
        self.0.contains_key(&command)
    }

    pub fn iter(&self) -> impl Iterator<Item = (Command, Option<&Shortcut>)> {
        self.0
            .iter()
            .map(|(&command, shortcut)| (command, shortcut.as_ref()))
    }

    pub(super) fn set(&mut self, command: Command, shortcut: Option<Shortcut>) {
        self.0.insert(command, shortcut);
    }

    pub(super) fn remove(&mut self, command: Command) {
        self.0.remove(&command);
    }
}
