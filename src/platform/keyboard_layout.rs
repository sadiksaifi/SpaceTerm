//! Host facts used to resolve retained shortcut spellings without changing Settings.

use std::collections::BTreeMap;

use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
#[error("keyboard layout is unavailable")]
pub(crate) struct KeyboardLayoutUnavailable;

pub(crate) trait KeyboardLayoutAdapter: std::fmt::Debug {
    fn snapshot(&self) -> Result<KeyboardLayout, KeyboardLayoutUnavailable>;
}

/// Shift translations in the host's dispatch alphabet, with and without Command.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct KeyboardLayout {
    shifted: [BTreeMap<String, String>; 2],
}

impl KeyboardLayout {
    pub(crate) fn insert(&mut self, command: bool, base: &str, shifted: &str) {
        let printable = |s: &str| {
            s.chars().count() == 1 && s.chars().all(|c| !c.is_control() && !c.is_whitespace())
        };
        // GPUI retains Shift for ASCII letters and named keys only.
        if printable(base) && printable(shifted) && !base.chars().all(|c| c.is_ascii_alphabetic()) {
            self.shifted[usize::from(command)]
                .entry(base.into())
                .or_insert_with(|| shifted.into());
        }
    }

    pub(crate) fn shifted(&self, command: bool, key: &str) -> Option<&str> {
        let shifted = &self.shifted[usize::from(command)];
        shifted.get(key).map(String::as_str).or_else(|| {
            shifted
                .values()
                .find(|value| *value == key)
                .map(String::as_str)
        })
    }
}

impl KeyboardLayoutAdapter for KeyboardLayout {
    fn snapshot(&self) -> Result<KeyboardLayout, KeyboardLayoutUnavailable> {
        Ok(self.clone())
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::rc::Rc;

    pub(crate) fn us() -> Rc<dyn KeyboardLayoutAdapter> {
        let mut layout = KeyboardLayout::default();
        for (base, shifted) in "`1234567890-=[]\\;',./"
            .chars()
            .zip("~!@#$%^&*()_+{}|:\"<>?".chars())
        {
            for command in [false, true] {
                layout.insert(command, &base.to_string(), &shifted.to_string());
            }
        }
        Rc::new(layout)
    }
}
