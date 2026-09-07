use crate::terminal::RemoteChannelUnavailable;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// A bounded phase in one Remote Project Workspace's Control Connection lifecycle.
///
/// A phase is meaningful only together with its owning connection generation.
pub(crate) enum RemoteConnectionPhase {
    Connected,
    Reconnecting,
    Disconnected,
    Failed,
    Closing,
}

/// One bounded connection phase coupled to the operation generation that produced it.
///
/// Generations are monotonic within one Remote Project Workspace. Closing is terminal, and an
/// observation from a predecessor generation cannot mutate a successor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RemoteConnectionState {
    generation: u64,
    phase: RemoteConnectionPhase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// The result of reducing one proposed Remote connection transition.
///
/// Rejected transitions leave the existing state unchanged. `Stale` identifies a predecessor
/// generation; `Illegal` identifies a transition outside the explicit lifecycle table.
pub(crate) enum RemoteConnectionReduction {
    Applied,
    Stale,
    Illegal,
}

impl RemoteConnectionState {
    pub(crate) fn begin_reconnect(&mut self) -> Option<RemoteConnectionReduction> {
        if !matches!(
            self.phase,
            RemoteConnectionPhase::Disconnected | RemoteConnectionPhase::Failed
        ) {
            return Some(RemoteConnectionReduction::Illegal);
        }
        Some(self.reduce(Self::reconnecting(self.generation.checked_add(1)?)))
    }

    pub(crate) fn begin_close(&mut self) -> RemoteConnectionReduction {
        self.reduce(Self::closing(self.generation))
    }
    pub(crate) const fn connected(generation: u64) -> Self {
        Self::new(generation, RemoteConnectionPhase::Connected)
    }

    pub(crate) const fn reconnecting(generation: u64) -> Self {
        Self::new(generation, RemoteConnectionPhase::Reconnecting)
    }

    pub(crate) const fn disconnected(generation: u64) -> Self {
        Self::new(generation, RemoteConnectionPhase::Disconnected)
    }

    pub(crate) const fn failed(generation: u64) -> Self {
        Self::new(generation, RemoteConnectionPhase::Failed)
    }

    pub(crate) const fn closing(generation: u64) -> Self {
        Self::new(generation, RemoteConnectionPhase::Closing)
    }

    const fn new(generation: u64, phase: RemoteConnectionPhase) -> Self {
        Self { generation, phase }
    }

    pub(crate) const fn generation(self) -> u64 {
        self.generation
    }

    pub(crate) const fn phase(self) -> RemoteConnectionPhase {
        self.phase
    }

    /// Reduces a completion or lifecycle observation through the explicit transition table.
    ///
    /// A reconnect may advance the generation only from Disconnected or Failed. Same-generation
    /// observations may complete or terminate the current attempt, while Closing accepts no
    /// successor. Rejection never mutates `self`.
    pub(crate) fn reduce(&mut self, next: Self) -> RemoteConnectionReduction {
        if next.generation < self.generation {
            return RemoteConnectionReduction::Stale;
        }
        let legal = if next.generation == self.generation {
            matches!(
                (self.phase, next.phase),
                (
                    RemoteConnectionPhase::Connected,
                    RemoteConnectionPhase::Disconnected | RemoteConnectionPhase::Closing
                ) | (
                    RemoteConnectionPhase::Reconnecting,
                    RemoteConnectionPhase::Connected
                        | RemoteConnectionPhase::Disconnected
                        | RemoteConnectionPhase::Failed
                        | RemoteConnectionPhase::Closing
                ) | (
                    RemoteConnectionPhase::Disconnected | RemoteConnectionPhase::Failed,
                    RemoteConnectionPhase::Closing
                )
            )
        } else {
            matches!(
                (self.phase, next.phase),
                (
                    RemoteConnectionPhase::Disconnected | RemoteConnectionPhase::Failed,
                    RemoteConnectionPhase::Reconnecting
                )
            )
        };
        if !legal {
            return RemoteConnectionReduction::Illegal;
        }
        *self = next;
        RemoteConnectionReduction::Applied
    }
}

/// Move-only child reservations. The owner validates the complete membership and every child's
/// live authority before the first restart effect. Dropping a rejected batch releases all tokens.
pub(crate) struct RemoteRestartBatch<T> {
    children: Vec<T>,
}

impl<T> RemoteRestartBatch<T> {
    pub(crate) fn new(children: Vec<T>) -> Self {
        Self { children }
    }

    pub(crate) fn validate<E>(
        &self,
        count: usize,
        changed: impl FnOnce() -> E,
        mut validate: impl FnMut(&T) -> Result<(), E>,
    ) -> Result<(), E> {
        if self.children.len() != count {
            return Err(changed());
        }
        for child in &self.children {
            validate(child)?;
        }
        Ok(())
    }

    pub(crate) fn commit<C, E>(
        self,
        count: usize,
        changed: impl FnOnce() -> E,
        cx: &mut C,
        mut validate: impl FnMut(&T, &C) -> Result<(), E>,
        mut restart: impl FnMut(T, &mut C),
    ) -> Result<(), E> {
        self.validate(count, changed, |child| validate(child, cx))?;
        for child in self.children {
            restart(child, cx);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_restart_authority_rechecks_session_epoch_and_disconnection() {
        let facts = RemotePaneFacts {
            remote: true,
            generation: Some(7),
            disconnected: true,
            epoch: 2,
        };
        let authority = RemoteRestartAuthority::prepare(facts, 8).unwrap();
        assert!(authority.validate(facts).is_ok());
        assert_eq!(
            authority.validate(RemotePaneFacts { epoch: 3, ..facts }),
            Err(RemotePaneLifecycleError::SessionChanged)
        );
        assert_eq!(
            authority.validate(RemotePaneFacts {
                disconnected: false,
                ..facts
            }),
            Err(RemotePaneLifecycleError::NotDisconnected)
        );
        assert!(matches!(
            RemoteRestartAuthority::prepare(facts, 7),
            Err(RemotePaneLifecycleError::StaleGeneration { .. })
        ));
        assert!(matches!(
            RemoteRestartAuthority::prepare(
                RemotePaneFacts {
                    remote: false,
                    ..facts
                },
                8
            ),
            Err(RemotePaneLifecycleError::LocalPane)
        ));
    }

    #[test]
    fn remote_restart_validates_every_child_before_effects() {
        let mut events = Vec::new();
        let batch = RemoteRestartBatch::new(vec![1, 2, 3]);
        let result = batch.commit(
            3,
            || "membership",
            &mut events,
            |child, _| {
                if *child == 3 {
                    Err("stale session")
                } else {
                    Ok(())
                }
            },
            |child, events| events.push(child),
        );
        assert_eq!((result, events), (Err("stale session"), vec![]));
    }

    #[test]
    fn remote_restart_preserves_order_and_rejects_membership_change() {
        let mut events = Vec::new();
        RemoteRestartBatch::new(vec![3, 1, 2])
            .commit(
                3,
                || (),
                &mut events,
                |_, _| Ok(()),
                |child, events| events.push(child),
            )
            .unwrap();
        assert_eq!(events, vec![3, 1, 2]);
        assert!(
            RemoteRestartBatch::new(vec![4])
                .commit(
                    2,
                    || (),
                    &mut events,
                    |_, _| panic!("membership before validation"),
                    |_, _| panic!("membership before restart")
                )
                .is_err()
        );
    }

    #[test]
    fn remote_generation_rejects_old_completion_and_close_is_terminal() {
        let mut state = RemoteConnectionState::disconnected(3);
        assert_eq!(
            state.begin_reconnect(),
            Some(RemoteConnectionReduction::Applied)
        );
        assert_eq!(
            state.reduce(RemoteConnectionState::connected(3)),
            RemoteConnectionReduction::Stale
        );
        assert_eq!(state.begin_close(), RemoteConnectionReduction::Applied);
        assert_eq!(
            state.reduce(RemoteConnectionState::connected(4)),
            RemoteConnectionReduction::Illegal
        );
        assert_eq!(
            state.begin_reconnect(),
            Some(RemoteConnectionReduction::Illegal)
        );
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
/// A typed rejection of a Remote Pane disconnect or restart lifecycle operation.
///
/// Errors leave the Pane's session epoch, presentation, and input state unchanged.
pub(crate) enum RemotePaneLifecycleError {
    #[error("the Pane does not own a remote Terminal Session")]
    LocalPane,
    #[error("remote connection generation {received} is stale; current generation is {current}")]
    StaleGeneration { current: u64, received: u64 },
    #[error("the remote Pane is not disconnected")]
    NotDisconnected,
    #[error("the prepared remote restart no longer matches the Pane session epoch")]
    SessionChanged,
    #[error(transparent)]
    ChannelUnavailable(#[from] RemoteChannelUnavailable),
}

#[derive(Clone, Copy)]
pub(crate) struct RemotePaneFacts {
    pub(crate) remote: bool,
    pub(crate) generation: Option<u64>,
    pub(crate) disconnected: bool,
    pub(crate) epoch: u64,
}

impl RemotePaneFacts {
    pub(crate) fn validate_generation(
        self,
        generation: u64,
    ) -> Result<(), RemotePaneLifecycleError> {
        if !self.remote {
            return Err(RemotePaneLifecycleError::LocalPane);
        }
        if let Some(current) = self.generation
            && generation < current
        {
            return Err(RemotePaneLifecycleError::StaleGeneration {
                current,
                received: generation,
            });
        }
        Ok(())
    }

    fn validate_successor(self, generation: u64) -> Result<(), RemotePaneLifecycleError> {
        if !self.disconnected {
            return Err(RemotePaneLifecycleError::NotDisconnected);
        }
        let current = self
            .generation
            .ok_or(RemotePaneLifecycleError::NotDisconnected)?;
        if generation <= current {
            return Err(RemotePaneLifecycleError::StaleGeneration {
                current,
                received: generation,
            });
        }
        Ok(())
    }
}

pub(crate) struct RemoteRestartAuthority {
    generation: u64,
    expected_epoch: u64,
}

impl RemoteRestartAuthority {
    pub(crate) fn prepare(
        facts: RemotePaneFacts,
        generation: u64,
    ) -> Result<Self, RemotePaneLifecycleError> {
        facts.validate_generation(generation)?;
        facts.validate_successor(generation)?;
        Ok(Self {
            generation,
            expected_epoch: facts.epoch,
        })
    }

    pub(crate) fn validate(&self, facts: RemotePaneFacts) -> Result<(), RemotePaneLifecycleError> {
        if facts.epoch != self.expected_epoch {
            return Err(RemotePaneLifecycleError::SessionChanged);
        }
        facts.validate_successor(self.generation)
    }

    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }
}
