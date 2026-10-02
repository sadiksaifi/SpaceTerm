use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{Command, Shortcut};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeybindingPreferences(BTreeMap<Command, Option<Shortcut>>);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum KeybindingPreferencesError {
    #[error("multiple commands override the same shortcut")]
    DuplicateShortcut,
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
