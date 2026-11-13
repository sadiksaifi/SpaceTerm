//! Where a running SpaceTerm Core reads the update facts it reports to every connected client.
use super::facts::{DetailedUpdateFacts, UpdateFacts};
use std::time::Duration;

/// The one producer of a Core's [`UpdateFacts`]. The work protocol sends `facts` after
/// admission and after every change; the service lane answers `Facts` and `WaitFacts` from it.
pub trait FactsSource: Send + Sync {
    /// The facts as they are now.
    fn facts(&self) -> UpdateFacts;

    /// The facts once their revision differs from `revision`, or the unchanged facts after
    /// `timeout`. None once the server is shutting down, at once and without waiting.
    fn wait(&self, revision: u64, timeout: Duration) -> Option<UpdateFacts>;

    /// The facts with what only the update engine knows. A source without an engine derives
    /// the continuation from the facts and counts every live Terminal Session as working.
    fn detailed(&self) -> DetailedUpdateFacts {
        derived(self.facts())
    }

    /// As [`Self::wait`], for the detailed facts.
    fn wait_detailed(&self, revision: u64, timeout: Duration) -> Option<DetailedUpdateFacts> {
        self.wait(revision, timeout).map(derived)
    }
}

/// `facts` with only what they show.
fn derived(facts: UpdateFacts) -> DetailedUpdateFacts {
    DetailedUpdateFacts {
        continuation: super::continuation::continuation(&facts),
        working: facts.sessions,
        restart: None,
        facts,
    }
}
