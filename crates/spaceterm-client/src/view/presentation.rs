//! Portable Pane presentation policy.
//!
//! The reducer derives what one Pane presents, and what it may paint, expose,
//! copy and accept, from authoritative replica facts, its attachment, the
//! explicit-use state and the Session the projection currently displays. It
//! retains Session keys and the prior presentation only, never a screen,
//! terminal text or native context. Desktop C1 maps each decision to its
//! native presentation DTO.
//!
//! Precedence, first match wins:
//!
//! 1. A missing target, detached or suppressed attachment, removed pairing or
//!    resource-limited Workspace is unavailable and clears its content and
//!    native context.
//! 2. Known control held by another Client is a Takeover Screen, offline,
//!    reconciling or reconciled.
//! 3. An offline or connecting replica is Offline. A connected replica that is
//!    not reconciled is Reconciling. Either keeps a prior Takeover or
//!    Unclaimed screen, or keeps the last live frame frozen only while the
//!    projection still displays that Session and the replica has not
//!    replaced it.
//! 4. An explicit startup bound to the current Session and connection awaits
//!    measurement or starts.
//! 5. A running or starting Session that no Client controls is the Unclaimed
//!    screen, which withholds content until this Client acquires control.
//! 6. Ended, Not Started, Failed and Directory Unavailable are content-free,
//!    also when unclaimed: their own Start Session or Retry action
//!    compare-acquires, and a restarted Server restores every Pane unclaimed.
//! 7. A running Session controlled by this Client is Live.

use spaceterm_core::authority::{ConnectionBinding, ControlGeneration, SessionKey};
use spaceterm_core::identity::{ClientId, PaneId, WorkspaceId};
use spaceterm_core::organization::{ControlRecord, EndReason, FailureClass, SessionPhase};

use super::replica_bridge::{ReplicaObservation, ReplicaReadiness, ServerViewStatus};
use super::startup::{StartupIntents, StartupState, UseKind};
use super::state::{ClientViewState, InvalidationReason, ProjectionInvalidation, ViewAvailability};
use super::types::ViewError;

/// What an offline or reconciling Pane keeps from before the interruption.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OfflinePrior {
    /// The last live screen, frozen, when one is retained.
    Frozen,
    /// The Takeover Screen. An older screen is never revealed.
    Takeover,
    /// The Unclaimed screen. An older screen is never revealed.
    Unclaimed,
}

/// What one Pane presents in place of, or as, its live Terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PanePresentation {
    Live,
    /// No Session runs and no explicit use is pending. Explicit use starts one.
    NotStarted,
    AwaitingMeasurement,
    Starting,
    /// Another Client controls the Pane. The decision names it when known.
    /// The screen's action is the explicit Take Over.
    Takeover,
    /// No Client controls the running or starting Session, and its content is
    /// withheld. The screen's action is the Pane's own selection,
    /// `StartupIntents::begin` with `ExplicitUseSource::PaneSelection`, which
    /// compare-acquires. It is never Take Over, and the screen does not say
    /// another device uses the Pane. `acquiring` is set while this Client's
    /// compare-acquire is in flight on the current connection.
    Unclaimed {
        acquiring: bool,
    },
    Ended {
        reason: EndReason,
    },
    Failed {
        class: FailureClass,
    },
    DirectoryUnavailable,
    Offline {
        prior: OfflinePrior,
    },
    Reconciling {
        prior: OfflinePrior,
    },
    /// The Pane is not represented by a current attachment.
    Unavailable {
        reason: InvalidationReason,
    },
}

impl PanePresentation {
    /// Whether keys, text, pastes and pointer reports may reach the Session.
    pub const fn accepts_input(&self) -> bool {
        matches!(self, Self::Live)
    }

    /// A coarse rendering guard: whether the presentation can show terminal
    /// content at all. The decision's eligibility and content disposition are
    /// authoritative; `true` never authorizes a read or a native effect.
    pub const fn content_eligible(&self) -> bool {
        matches!(
            self,
            Self::Live
                | Self::Offline {
                    prior: OfflinePrior::Frozen
                }
                | Self::Reconciling {
                    prior: OfflinePrior::Frozen
                }
        )
    }

    /// The control screen this presentation shows, kept through an
    /// interruption.
    const fn control_screen(&self) -> Option<OfflinePrior> {
        match self {
            Self::Takeover
            | Self::Offline {
                prior: OfflinePrior::Takeover,
            }
            | Self::Reconciling {
                prior: OfflinePrior::Takeover,
            } => Some(OfflinePrior::Takeover),
            Self::Unclaimed { .. }
            | Self::Offline {
                prior: OfflinePrior::Unclaimed,
            }
            | Self::Reconciling {
                prior: OfflinePrior::Unclaimed,
            } => Some(OfflinePrior::Unclaimed),
            _ => None,
        }
    }
}

/// What the Pane's terminal content may be used for. Live use stays subject to
/// prepared-copy, focus and effect fences.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct PresentationEligibility {
    pub paint: bool,
    pub accessibility: bool,
    pub copy: bool,
    pub ime: bool,
    pub input: bool,
    pub native_effects: bool,
}

impl PresentationEligibility {
    pub const NONE: Self = Self {
        paint: false,
        accessibility: false,
        copy: false,
        ime: false,
        input: false,
        native_effects: false,
    };

    pub const LIVE: Self = Self {
        paint: true,
        accessibility: true,
        copy: true,
        ime: true,
        input: true,
        native_effects: true,
    };

    /// Whether `next` withdraws copy, IME, input or native effects.
    const fn interaction_revoked_by(self, next: Self) -> bool {
        (self.copy && !next.copy)
            || (self.ime && !next.ime)
            || (self.input && !next.input)
            || (self.native_effects && !next.native_effects)
    }
}

/// Which Session's content the projection may display.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ContentDisposition {
    /// Display current content of exactly this Session.
    Allow { session: SessionKey },
    /// Keep the last displayed frame of this Session, unchanged.
    KeepFrozen { session: SessionKey },
    /// Display no terminal content.
    Clear,
}

impl ContentDisposition {
    pub const fn session(self) -> Option<SessionKey> {
        match self {
            Self::Allow { session } | Self::KeepFrozen { session } => Some(session),
            Self::Clear => None,
        }
    }
}

/// One presentation decision for one Pane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentationDecision {
    pub presentation: PanePresentation,
    pub eligibility: PresentationEligibility,
    pub content: ContentDisposition,
    /// The other Client a Takeover Screen names, when known. `None` for every
    /// other presentation.
    pub controller: Option<ClientId>,
    /// Discard interaction context now: IME composition and marked text,
    /// prepared copies and copy leases, Services and other native effect
    /// registrations, and pending input. Set when copy, IME, input or native
    /// effects are withdrawn, and whenever `clear_content` is set. It never
    /// discards a frame that `content` keeps.
    pub invalidate_native_context: bool,
    /// Discard the retained frame of the Session displayed before this
    /// decision now: its screen, accessibility facts, selection, Find results
    /// and graphics references. Set for an unavailable Pane and whenever
    /// `content` no longer names the Session last kept or displayed, as after
    /// Takeover, End All Sessions, a replacement Session or a new epoch. A
    /// frame kept frozen through an interruption of the same Session is never
    /// cleared; `eligibility` alone withdraws its accessibility exposure while
    /// reconciling.
    pub clear_content: bool,
}

/// A startup the presentation shows, bound to the Session it began with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StartupProgress {
    session: SessionKey,
    phase: SessionPhase,
    measured: bool,
}

/// This Client's in-flight compare-acquire, bound to the unclaimed state it
/// was requested against.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AcquisitionProgress {
    session: SessionKey,
    phase: SessionPhase,
    control_generation: ControlGeneration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RuntimeFacts {
    session: SessionKey,
    phase: SessionPhase,
    control: ControlRecord,
    control_generation: ControlGeneration,
}

impl RuntimeFacts {
    /// Whether a Session runs or starts while no Client controls it.
    const fn unclaimed_session(&self) -> bool {
        matches!(self.control, ControlRecord::Unclaimed)
            && matches!(
                self.phase,
                SessionPhase::Running { .. } | SessionPhase::Starting { .. }
            )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Represented {
    readiness: ReplicaReadiness,
    status: ServerViewStatus,
    client: ClientId,
    connection: Option<ConnectionBinding>,
    runtime: Option<RuntimeFacts>,
    startup: Option<StartupProgress>,
    acquisition: Option<AcquisitionProgress>,
    displayed: Option<SessionKey>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Membership {
    Represented(Box<Represented>),
    Unavailable(InvalidationReason),
}

/// Checked facts about one Pane, read from the view, the replica of its
/// Server, the explicit-use state and the projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresentationFacts {
    pane: PaneId,
    membership: Membership,
}

impl PresentationFacts {
    /// Observes a Pane of an attached Workspace. `displayed` is the Session
    /// whose content the projection currently shows, if any.
    pub fn observe(
        view: &ClientViewState,
        replica: &ReplicaObservation<'_>,
        workspace: WorkspaceId,
        pane: PaneId,
        startup: &StartupIntents,
        displayed: Option<SessionKey>,
    ) -> Result<Self, ViewError> {
        let server = replica.server();
        if workspace.server() != server || pane.server() != server {
            return Err(ViewError::WrongServer);
        }
        if displayed.is_some_and(|session| session.pane != pane) {
            return Err(ViewError::WrongMembership);
        }
        let unavailable = |reason| Self {
            pane,
            membership: Membership::Unavailable(reason),
        };
        let Some(workspace_view) = view.workspace(workspace) else {
            return Ok(unavailable(InvalidationReason::TargetDeleted));
        };
        let attachment = workspace_view.attachment();
        if attachment.pairing() != replica.pairing() || attachment.client() != replica.client() {
            return Ok(unavailable(InvalidationReason::PairingRemoved));
        }
        if let ViewAvailability::ResourceLimited { .. } = workspace_view.availability() {
            return Ok(unavailable(InvalidationReason::ResourceLimited));
        }
        let represented = workspace_view.tab_order().iter().any(|&tab| {
            workspace_view
                .arrangement(tab)
                .is_some_and(|arrangement| arrangement.contains_pane(pane))
        });
        if !represented {
            return Ok(unavailable(InvalidationReason::TargetDeleted));
        }
        let in_workspace = replica.pane_tab(pane).is_some_and(|tab| {
            replica
                .tab_record(tab)
                .is_some_and(|record| record.workspace == workspace)
        });
        let readiness = replica.readiness();
        // A Ready replica is authoritative about deletion; a retained one is not.
        if readiness == ReplicaReadiness::Ready && !in_workspace {
            return Ok(unavailable(InvalidationReason::TargetDeleted));
        }
        let runtime = replica
            .pane_runtime(pane)
            .filter(|_| in_workspace)
            .map(|record| RuntimeFacts {
                session: SessionKey {
                    epoch: replica.epoch(),
                    pane,
                    generation: record.session_generation,
                },
                phase: record.phase,
                control: record.control,
                control_generation: record.control_generation,
            });
        if readiness == ReplicaReadiness::Ready && runtime.is_none() {
            return Ok(unavailable(InvalidationReason::TargetDeleted));
        }
        // Only a use of this attachment requested on the current connection
        // is shown.
        let pane_use = startup.use_for(pane).filter(|pane_use| {
            pane_use.context.attachment() == attachment
                && pane_use.context.target().workspace == workspace
                && replica.connection() == Some(pane_use.context.connection())
        });
        let startup = pane_use
            .filter(|pane_use| pane_use.kind == UseKind::Startup)
            .map(|pane_use| StartupProgress {
                session: pane_use.context.session(),
                phase: pane_use.context.phase(),
                measured: !matches!(
                    pane_use.state,
                    StartupState::AwaitingAuthorization { measured: false }
                        | StartupState::AwaitingMeasurement
                ),
            });
        let acquisition = pane_use
            .filter(|pane_use| pane_use.kind == UseKind::Acquisition)
            .map(|pane_use| AcquisitionProgress {
                session: pane_use.context.session(),
                phase: pane_use.context.phase(),
                control_generation: pane_use.context.control_generation(),
            });
        Ok(Self {
            pane,
            membership: Membership::Represented(Box::new(Represented {
                readiness,
                status: replica.status(),
                client: replica.client(),
                connection: replica.connection(),
                runtime,
                startup,
                acquisition,
                displayed,
            })),
        })
    }

    /// Facts for a Pane whose projection the view invalidated: deleted,
    /// detached, unpaired, suppressed or resource-limited.
    pub const fn invalidated(invalidation: ProjectionInvalidation) -> Self {
        Self {
            pane: invalidation.pane,
            membership: Membership::Unavailable(invalidation.reason),
        }
    }

    pub const fn pane(&self) -> PaneId {
        self.pane
    }
}

/// Presentation memory of one Pane: its prior presentation, the Session last
/// allowed live, and the last eligibility and content, for edge-triggered
/// invalidation. It retains no screen, text or native context.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PresentationState {
    pane: Option<PaneId>,
    prior: Option<PanePresentation>,
    live: Option<SessionKey>,
    last: Option<(PresentationEligibility, ContentDisposition)>,
}

impl PresentationState {
    pub fn new() -> Self {
        Self::default()
    }

    pub const fn prior(&self) -> Option<PanePresentation> {
        self.prior
    }

    /// Decides the Pane's presentation. A state serves the first Pane it
    /// reduces and rejects facts of another.
    pub fn reduce(&mut self, facts: &PresentationFacts) -> Result<PresentationDecision, ViewError> {
        if self.pane.is_some_and(|pane| pane != facts.pane) {
            return Err(ViewError::WrongMembership);
        }
        let (presentation, content, controller, displayed) = match &facts.membership {
            &Membership::Unavailable(reason) => (
                PanePresentation::Unavailable { reason },
                ContentDisposition::Clear,
                None,
                None,
            ),
            Membership::Represented(represented) => {
                let (presentation, content, controller) = self.decide(represented);
                (presentation, content, controller, represented.displayed)
            }
        };
        let eligibility = eligibility(presentation, content);
        // A retained frame of another Session than the one `content` names is
        // discarded. Keeping or resuming the same Session discards nothing.
        let replaced = |session: SessionKey| content.session() != Some(session);
        let clear_content = matches!(presentation, PanePresentation::Unavailable { .. })
            || self
                .last
                .and_then(|(_, last_content)| last_content.session())
                .is_some_and(replaced)
            || displayed.is_some_and(replaced);
        let invalidate_native_context = clear_content
            || self.last.is_some_and(|(last_eligibility, _)| {
                last_eligibility.interaction_revoked_by(eligibility)
            });
        match content {
            ContentDisposition::Allow { session } => self.live = Some(session),
            ContentDisposition::KeepFrozen { .. } => {}
            ContentDisposition::Clear => self.live = None,
        }
        self.pane = Some(facts.pane);
        self.prior = Some(presentation);
        self.last = Some((eligibility, content));
        Ok(PresentationDecision {
            presentation,
            eligibility,
            content,
            controller,
            invalidate_native_context,
            clear_content,
        })
    }

    fn decide(
        &self,
        facts: &Represented,
    ) -> (PanePresentation, ContentDisposition, Option<ClientId>) {
        let other = facts.runtime.and_then(|runtime| match runtime.control {
            ControlRecord::Held { client, .. } if client != facts.client => Some(client),
            _ => None,
        });
        let interrupted = match facts.readiness {
            ReplicaReadiness::Ready => None,
            ReplicaReadiness::Synchronizing => Some(Interruption::Reconciling),
            ReplicaReadiness::Retained if facts.status == ServerViewStatus::Connected => {
                Some(Interruption::Reconciling)
            }
            ReplicaReadiness::Retained => Some(Interruption::Offline),
        };
        if let Some(interruption) = interrupted {
            // The retained replica's control, then the control screen shown
            // before the interruption. Neither reveals an older frame.
            let screen = if other.is_some() {
                Some(OfflinePrior::Takeover)
            } else {
                self.prior
                    .and_then(|prior| prior.control_screen())
                    .or_else(|| {
                        facts
                            .runtime
                            .filter(RuntimeFacts::unclaimed_session)
                            .map(|_| OfflinePrior::Unclaimed)
                    })
            };
            let (prior, content, controller) = match screen {
                Some(screen) => (screen, ContentDisposition::Clear, other),
                None => {
                    let content = self
                        .frozen(facts)
                        .map_or(ContentDisposition::Clear, |session| {
                            ContentDisposition::KeepFrozen { session }
                        });
                    (OfflinePrior::Frozen, content, None)
                }
            };
            let presentation = match interruption {
                Interruption::Offline => PanePresentation::Offline { prior },
                Interruption::Reconciling => PanePresentation::Reconciling { prior },
            };
            return (presentation, content, controller);
        }
        let (Some(runtime), Some(_)) = (facts.runtime, facts.connection) else {
            return (
                PanePresentation::Reconciling {
                    prior: OfflinePrior::Frozen,
                },
                ContentDisposition::Clear,
                None,
            );
        };
        if let Some(controller) = other {
            return (
                PanePresentation::Takeover,
                ContentDisposition::Clear,
                Some(controller),
            );
        }
        if let Some(startup) = facts
            .startup
            .filter(|startup| startup.session == runtime.session && startup.phase == runtime.phase)
        {
            let presentation = if startup.measured {
                PanePresentation::Starting
            } else {
                PanePresentation::AwaitingMeasurement
            };
            return (presentation, ContentDisposition::Clear, None);
        }
        if runtime.unclaimed_session() {
            let acquiring = facts.acquisition.is_some_and(|acquisition| {
                acquisition.session == runtime.session
                    && acquisition.phase == runtime.phase
                    && acquisition.control_generation == runtime.control_generation
            });
            return (
                PanePresentation::Unclaimed { acquiring },
                ContentDisposition::Clear,
                None,
            );
        }
        // Control held by another Client returned above: the remaining
        // phases are this Client's or have no Session to withhold.
        let presentation = match runtime.phase {
            SessionPhase::Running { .. } => {
                return (
                    PanePresentation::Live,
                    ContentDisposition::Allow {
                        session: runtime.session,
                    },
                    None,
                );
            }
            SessionPhase::Starting { .. } => PanePresentation::Starting,
            SessionPhase::NotStarted => PanePresentation::NotStarted,
            SessionPhase::Ended { reason } => PanePresentation::Ended { reason },
            SessionPhase::Failed { class } => PanePresentation::Failed { class },
            SessionPhase::DirectoryUnavailable => PanePresentation::DirectoryUnavailable,
        };
        (presentation, ContentDisposition::Clear, None)
    }

    /// The Session whose last live frame may stay frozen: the one last allowed
    /// live, still displayed, and not replaced in the replica.
    fn frozen(&self, facts: &Represented) -> Option<SessionKey> {
        let session = self.live?;
        let displayed = facts.displayed == Some(session);
        let current = facts
            .runtime
            .is_none_or(|runtime| runtime.session == session);
        (displayed && current).then_some(session)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Interruption {
    Offline,
    Reconciling,
}

fn eligibility(
    presentation: PanePresentation,
    content: ContentDisposition,
) -> PresentationEligibility {
    match (presentation, content) {
        (PanePresentation::Live, ContentDisposition::Allow { .. }) => PresentationEligibility::LIVE,
        (PanePresentation::Offline { .. }, ContentDisposition::KeepFrozen { .. }) => {
            PresentationEligibility {
                paint: true,
                accessibility: true,
                ..PresentationEligibility::NONE
            }
        }
        (PanePresentation::Reconciling { .. }, ContentDisposition::KeepFrozen { .. }) => {
            PresentationEligibility {
                paint: true,
                ..PresentationEligibility::NONE
            }
        }
        _ => PresentationEligibility::NONE,
    }
}
