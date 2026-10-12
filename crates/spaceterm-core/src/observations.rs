//! Engine-free server-internal observations. No protocol client can submit these.
//! Constructors check comparison structure, not native authority or Prompt Owner.
//! Terminal production must establish those facts before constructing observations.

use std::num::NonZeroU64;

use thiserror::Error;

pub use crate::authority::CloseWorkGeneration;
use crate::authority::SessionKey;
use crate::organization::ServerDirectory;

/// Milliseconds from the single monotonic source injected by server composition.
/// Ticks have meaning only within the server runtime that supplied them.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonotonicTick(u64);

impl MonotonicTick {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn checked_elapsed_since(self, earlier: Self) -> Option<u64> {
        self.0.checked_sub(earlier.0)
    }
}

/// Content-free observation conversion failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ObservationError {
    #[error("observation counter must be nonzero")]
    ZeroCounter,
    #[error("observation has inconsistent Session scope")]
    WrongSession,
}

macro_rules! observation_counter {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(NonZeroU64);

        impl $name {
            pub fn new(value: u64) -> Result<Self, ObservationError> {
                NonZeroU64::new(value)
                    .map(Self)
                    .ok_or(ObservationError::ZeroCounter)
            }

            pub const fn get(self) -> u64 {
                self.0.get()
            }

            pub fn checked_successor(self) -> Option<Self> {
                self.get()
                    .checked_add(1)
                    .and_then(NonZeroU64::new)
                    .map(Self)
            }
        }
    };
}

observation_counter!(
    RuntimeObservationSequence,
    "Monotonic source sequence within an exact Session; receipt does not advance it."
);

fn validate_session(session: SessionKey) -> Result<(), ObservationError> {
    if session.epoch.get() == 0 || session.generation.get() == 0 {
        return Err(ObservationError::WrongSession);
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DirectoryObservation {
    session: SessionKey,
    sequence: RuntimeObservationSequence,
    observed_at: MonotonicTick,
    directory: ServerDirectory,
}

impl DirectoryObservation {
    fn new(
        session: SessionKey,
        sequence: RuntimeObservationSequence,
        observed_at: MonotonicTick,
        directory: ServerDirectory,
    ) -> Result<Self, ObservationError> {
        validate_session(session)?;
        if session.pane.server() != directory.server() {
            return Err(ObservationError::WrongSession);
        }
        Ok(Self {
            session,
            sequence,
            observed_at,
            directory,
        })
    }
}

/// An owning-shell prompt commit confirmed by the matching Prompt Owner.
/// This is Session-qualified, not connection- or client-context-qualified.
/// Server-core alone turns this observation into a durable restoration event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfirmedDirectoryObservation(DirectoryObservation);

impl ConfirmedDirectoryObservation {
    pub fn new(
        session: SessionKey,
        sequence: RuntimeObservationSequence,
        observed_at: MonotonicTick,
        directory: ServerDirectory,
    ) -> Result<Self, ObservationError> {
        DirectoryObservation::new(session, sequence, observed_at, directory).map(Self)
    }

    pub const fn session(&self) -> SessionKey {
        self.0.session
    }
    pub const fn sequence(&self) -> RuntimeObservationSequence {
        self.0.sequence
    }
    pub const fn observed_at(&self) -> MonotonicTick {
        self.0.observed_at
    }
    pub fn directory(&self) -> &ServerDirectory {
        &self.0.directory
    }
}

/// Display classification only. Neither variant proves restoration authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectoryPresentation {
    OwningServer,
    ReportedRemote,
}

/// A display-directory report with its original source sequence and tick.
/// Even an OwningServer display report cannot update restoration by itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayDirectoryObservation {
    observation: DirectoryObservation,
    presentation: DirectoryPresentation,
}

impl DisplayDirectoryObservation {
    pub fn new(
        session: SessionKey,
        sequence: RuntimeObservationSequence,
        observed_at: MonotonicTick,
        directory: ServerDirectory,
        presentation: DirectoryPresentation,
    ) -> Result<Self, ObservationError> {
        Ok(Self {
            observation: DirectoryObservation::new(session, sequence, observed_at, directory)?,
            presentation,
        })
    }

    pub const fn session(&self) -> SessionKey {
        self.observation.session
    }
    pub const fn sequence(&self) -> RuntimeObservationSequence {
        self.observation.sequence
    }
    pub const fn observed_at(&self) -> MonotonicTick {
        self.observation.observed_at
    }
    pub fn directory(&self) -> &ServerDirectory {
        &self.observation.directory
    }
    pub const fn presentation(&self) -> DirectoryPresentation {
        self.presentation
    }
}

/// Content-free classification supplied by the terminal owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseWorkState {
    Inactive,
    Idle,
    RunningWork,
    Unknown,
}

/// Retained work observation. Polling or delivering it must not replace its tick.
/// The reducer checks the Session, source sequence, work generation, and freshness
/// against injected server time before using it in an exact assessment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CloseWorkObservation {
    session: SessionKey,
    sequence: RuntimeObservationSequence,
    generation: CloseWorkGeneration,
    observed_at: MonotonicTick,
    state: CloseWorkState,
}

impl CloseWorkObservation {
    pub fn new(
        session: SessionKey,
        sequence: RuntimeObservationSequence,
        generation: CloseWorkGeneration,
        observed_at: MonotonicTick,
        state: CloseWorkState,
    ) -> Result<Self, ObservationError> {
        validate_session(session)?;
        Ok(Self {
            session,
            sequence,
            generation,
            observed_at,
            state,
        })
    }

    pub const fn session(self) -> SessionKey {
        self.session
    }
    pub const fn sequence(self) -> RuntimeObservationSequence {
        self.sequence
    }
    pub const fn generation(self) -> CloseWorkGeneration {
        self.generation
    }
    pub const fn observed_at(self) -> MonotonicTick {
        self.observed_at
    }
    pub const fn state(self) -> CloseWorkState {
        self.state
    }

    /// A future tick is not fresh. The configured age belongs to server policy.
    pub fn is_fresh(self, now: MonotonicTick, max_age_ms: u64) -> bool {
        now.checked_elapsed_since(self.observed_at)
            .is_some_and(|age| age <= max_age_ms)
    }
}

/// One monotonic millisecond origin shared by composition and runtime owners.
pub trait RuntimeClock: Send + Sync {
    fn now(&self) -> Result<MonotonicTick, ObservationError>;
}

/// Retained producer snapshots never replace source ticks with delivery time.
/// None is missing evidence, not Idle or confirmed cleanup.
pub trait RuntimeObservationSource: Send + Sync {
    fn session(&self) -> SessionKey;
    fn display_directory_snapshot(&self) -> Option<DisplayDirectoryObservation>;
    fn confirmed_directory_snapshot(&self) -> Option<ConfirmedDirectoryObservation>;
    fn close_work_snapshot(&self) -> Option<CloseWorkObservation>;
    fn cleanup_snapshot(&self) -> Option<crate::retirement::SessionCleanupObservation>;
}
