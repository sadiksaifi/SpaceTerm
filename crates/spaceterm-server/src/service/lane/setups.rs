//! The Permission Setups whose preparations run on the lane, and the cancellations they read.
//!
//! A cancellation of a running preparation stays until that preparation finishes, so no later
//! traffic can revive it. Admission is bounded instead: at most [`MAX_PREPARATIONS`] run at once,
//! and a further one is refused as busy. A cancellation can arrive before its preparation. Such
//! an early cancellation waits at most [`EARLY_CANCELLATION_LIFE`], and at most
//! [`MAX_EARLY_CANCELLATIONS`] wait at once, so names no preparation ever claims cannot grow
//! without bound.
use crate::ServerError;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How many preparations run at once. A person runs one Permission Setup at a time.
pub(super) const MAX_PREPARATIONS: usize = 4;
/// How many cancellations may wait for a preparation that has not arrived.
pub(super) const MAX_EARLY_CANCELLATIONS: usize = 64;
/// How long a cancellation waits for its preparation. A client sends a preparation before it
/// can cancel it, so an early cancellation only covers a preparation still in flight.
pub(super) const EARLY_CANCELLATION_LIFE: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(super) struct Setups {
    /// Each running preparation's setup name, with whether its setup was cancelled.
    running: HashMap<String, bool>,
    /// Cancellations whose preparation has not arrived, with when each arrived.
    early: Vec<(String, Instant)>,
}

impl Setups {
    /// Admits a preparation of `name`. It starts cancelled when its cancellation arrived first.
    pub(super) fn begin(&mut self, name: &str, now: Instant) -> Result<(), ServerError> {
        self.expire(now);
        if self.running.contains_key(name) {
            return Err(ServerError::Protocol);
        }
        if self.running.len() >= MAX_PREPARATIONS {
            return Err(ServerError::ServiceBusy);
        }
        let before = self.early.len();
        self.early.retain(|(early, _)| early != name);
        self.running
            .insert(name.to_owned(), self.early.len() != before);
        Ok(())
    }

    /// Whether the running preparation of `name` was cancelled. A name that is not running takes
    /// no effect either.
    pub(super) fn cancelled(&self, name: &str) -> bool {
        self.running.get(name).copied().unwrap_or(true)
    }

    /// Ends the preparation of `name` and forgets its cancellation.
    pub(super) fn finish(&mut self, name: &str) {
        self.running.remove(name);
    }

    /// Cancels the setup `name`, now when its preparation runs, or when it arrives.
    pub(super) fn cancel(&mut self, name: &str, now: Instant) -> Result<(), ServerError> {
        if let Some(cancelled) = self.running.get_mut(name) {
            *cancelled = true;
            return Ok(());
        }
        self.expire(now);
        if self.early.iter().any(|(early, _)| early == name) {
            return Ok(());
        }
        if self.early.len() >= MAX_EARLY_CANCELLATIONS {
            return Err(ServerError::ServiceBusy);
        }
        self.early.push((name.to_owned(), now));
        Ok(())
    }

    fn expire(&mut self, now: Instant) {
        self.early.retain(|(_, arrived)| {
            now.saturating_duration_since(*arrived) < EARLY_CANCELLATION_LIFE
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(index: usize) -> String {
        format!("{index:032x}")
    }

    /// The sequence that once revived a cancelled setup: it is cancelled while it waits, then
    /// more cancellations than any eviction bound arrive. It stays cancelled until it finishes.
    #[test]
    fn later_cancellations_never_revive_a_running_one() {
        let now = Instant::now();
        let mut setups = Setups::default();
        setups.begin("a", now).unwrap();
        setups.cancel("a", now).unwrap();
        for index in 0..MAX_EARLY_CANCELLATIONS {
            setups.cancel(&name(index), now).unwrap();
        }
        assert_eq!(
            setups.cancel(&name(MAX_EARLY_CANCELLATIONS), now),
            Err(ServerError::ServiceBusy),
            "a full set of early cancellations refuses another rather than evicting one"
        );
        assert!(setups.cancelled("a"));
        setups.finish("a");
        assert!(setups.cancelled("a"), "a finished setup takes no effect");
    }

    #[test]
    fn a_cancellation_before_its_preparation_starts_it_cancelled() {
        let now = Instant::now();
        let mut setups = Setups::default();
        setups.cancel("a", now).unwrap();
        setups.begin("a", now).unwrap();
        assert!(setups.cancelled("a"));
        setups.begin("b", now).unwrap();
        assert!(!setups.cancelled("b"));
    }

    #[test]
    fn an_early_cancellation_that_no_preparation_claims_expires() {
        let now = Instant::now();
        let mut setups = Setups::default();
        for index in 0..MAX_EARLY_CANCELLATIONS {
            setups.cancel(&name(index), now).unwrap();
        }
        let later = now + EARLY_CANCELLATION_LIFE;
        setups.cancel("b", later).unwrap();
        assert_eq!(setups.early.len(), 1, "the expired names were dropped");
        setups.begin(&name(0), later).unwrap();
        assert!(!setups.cancelled(&name(0)));
    }

    #[test]
    fn admission_is_bounded_and_a_duplicate_is_refused() {
        let now = Instant::now();
        let mut setups = Setups::default();
        for index in 0..MAX_PREPARATIONS {
            setups.begin(&name(index), now).unwrap();
        }
        assert_eq!(
            setups.begin(&name(MAX_PREPARATIONS), now),
            Err(ServerError::ServiceBusy)
        );
        assert_eq!(setups.begin(&name(0), now), Err(ServerError::Protocol));
        setups.finish(&name(0));
        setups.begin(&name(MAX_PREPARATIONS), now).unwrap();
    }
}
