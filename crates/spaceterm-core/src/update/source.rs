//! Where a running SpaceTerm Core reads the update facts it reports to every connected client.
use super::facts::UpdateFacts;
use std::time::Duration;

/// The one producer of a Core's [`UpdateFacts`]. The work protocol sends `facts` after
/// admission and after every change; the service lane answers `Facts` and `WaitFacts` from it.
pub trait FactsSource: Send + Sync {
    /// The facts as they are now.
    fn facts(&self) -> UpdateFacts;

    /// The facts once their revision differs from `revision`, or the unchanged facts after
    /// `timeout`.
    fn wait(&self, revision: u64, timeout: Duration) -> UpdateFacts;
}
