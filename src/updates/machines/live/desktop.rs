//! The desktop's [`Sources`]: This Mac's or This Computer's account Core through the service
//! manager and the account-local lane, and each paired Machine from the direct connection
//! registry through its own service lane.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use spaceterm_core::protocol::version::Incompatible;
use spaceterm_core::service::Service;
use spaceterm_core::service::control::Controller;
use spaceterm_core::service::lane::admin;
use spaceterm_core::service::lane::client::{LaneError, Session};
use spaceterm_core::service::lane::{LaneAction, LaneReply, LaneRequest, WAIT_FACTS_LIMIT};
use spaceterm_core::update::facts::Version;

use super::{
    Changed, Known, LaneChange, Machine, MachineLane, Observed, Reach, Sources, failure_of_code,
};
use crate::direct_connections::{
    DirectConnections, DirectRouteId, DirectRouteStatus, RefusalReason,
};
use crate::machine_connection::{CoreError, MachineConnection};
use crate::updates::machines::{ActionFailure, MachineUpdatesError};
use crate::updates::push::{self, EmbeddedCore};

/// How long `WaitFacts` may take beyond the Core's own limit before the lane gives up.
const WAIT_MARGIN: Duration = Duration::from_secs(10);

#[cfg(target_os = "macos")]
const THIS_MACHINE: &str = "This Mac";
#[cfg(not(target_os = "macos"))]
const THIS_MACHINE: &str = "This Computer";

pub(crate) struct DesktopSources {
    /// The account Core's service and its lane handle. None when the identity has no service
    /// paths.
    local: Option<(Controller, MachineConnection)>,
    direct: DirectConnections,
}

impl DesktopSources {
    /// The Machines of the desktop build `identity` and its paired routes. Nothing is read
    /// until [`Sources::known`] or [`Sources::watch`].
    pub(crate) fn new(identity: &str, direct: DirectConnections) -> Self {
        let local = Service::for_identity(identity)
            .ok()
            .zip(MachineConnection::for_identity(identity).ok())
            .map(|(service, core)| (Controller::new(service), core));
        Self { local, direct }
    }
}

impl Sources for DesktopSources {
    fn known(&self) -> Vec<Known> {
        let mut known = Vec::new();
        if let Some((controller, core)) = &self.local {
            let installed = controller.status().is_ok_and(|status| status.installed);
            if installed && let Ok(id) = core.machine_id() {
                known.push(Known {
                    id,
                    name: THIS_MACHINE.into(),
                    local: true,
                    machine: Arc::new(LocalMachine {
                        controller: controller.clone(),
                        core: core.clone(),
                    }),
                });
            }
        }
        let mut paired: Vec<Known> = self
            .direct
            .routes()
            .into_iter()
            .filter_map(|(route, _)| {
                let saved = self.direct.route(&route)?;
                Some(Known {
                    id: saved.paired.pin.core,
                    name: saved.label.into(),
                    local: false,
                    machine: Arc::new(PairedMachine {
                        direct: self.direct.clone(),
                        route,
                    }),
                })
            })
            .collect();
        paired.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        known.extend(paired);
        known
    }

    fn watch(&self, changed: Changed) -> Result<(), MachineUpdatesError> {
        let routes = self.direct.subscribe();
        {
            let direct = self.direct.clone();
            let changed = Arc::clone(&changed);
            std::thread::Builder::new()
                .name("spaceterm-machine-routes".into())
                .spawn(move || {
                    while routes.recv_blocking().is_ok() {
                        if !changed() {
                            break;
                        }
                    }
                    direct.close_subscription(&routes);
                })
                .map_err(|_| MachineUpdatesError::Unavailable)?;
        }
        if let Some((controller, _)) = &self.local {
            let directories = controller
                .state_directories()
                .map(std::path::Path::to_path_buf)
                .to_vec();
            let process = controller.clone();
            super::service_changes::follow(
                crate::platform::service_watch,
                directories,
                move || process.core_process().ok().flatten(),
                move || changed(),
            )
            .map_err(|_| MachineUpdatesError::Unavailable)?;
        }
        Ok(())
    }
}

/// This Mac's or This Computer's account Core.
struct LocalMachine {
    controller: Controller,
    core: MachineConnection,
}

impl Machine for LocalMachine {
    fn reach(&self) -> Reach {
        match self.controller.status() {
            Ok(status) if status.installed && status.stopped => Reach::Stopped,
            Ok(status) if status.installed => Reach::Lane {
                lane: Arc::new(CoreLane::new(self.core.clone())),
                work: Ok(()),
            },
            _ => Reach::Offline,
        }
    }

    fn start(&self) -> Result<(), ActionFailure> {
        self.controller
            .start()
            .map(drop)
            .map_err(|error| core_failure(&error))
    }
}

/// A paired Machine, reached through the route the registry keeps for it.
struct PairedMachine {
    direct: DirectConnections,
    route: DirectRouteId,
}

impl Machine for PairedMachine {
    fn reach(&self) -> Reach {
        let incompatible = match self.direct.status(&self.route) {
            Some(DirectRouteStatus::Connected) => false,
            // The lane admits across work majors.
            Some(DirectRouteStatus::Refused(RefusalReason::Incompatible)) => true,
            _ => return Reach::Offline,
        };
        let Some(core) = self.direct.core(&self.route) else {
            return Reach::Offline;
        };
        let lane = CoreLane::new(core);
        let work = if incompatible {
            Err(lane.direction())
        } else {
            Ok(())
        };
        Reach::Lane {
            lane: Arc::new(lane),
            work,
        }
    }

    fn start(&self) -> Result<(), ActionFailure> {
        // A paired Core is started on its own computer.
        Err(ActionFailure::Failed)
    }
}

/// One Core's service lane through its [`MachineConnection`].
struct CoreLane {
    core: MachineConnection,
    /// The connection the latest read answered on, reused by the next `WaitFacts`.
    session: Mutex<Option<Session>>,
}

impl CoreLane {
    fn new(core: MachineConnection) -> Self {
        Self {
            core,
            session: Mutex::new(None),
        }
    }

    fn open(&self) -> Result<Session, ActionFailure> {
        self.core
            .service_lane()
            .and_then(|lane| lane.session())
            .map_err(|error| core_failure(&error))
    }

    /// Sends `request` on the kept connection or a new one, and keeps the connection when it
    /// answered.
    fn facts_call(
        &self,
        request: &LaneRequest,
        deadline: Instant,
    ) -> Result<Observed, ActionFailure> {
        let kept = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let mut session = match kept {
            Some(session) => session,
            None => self.open()?,
        };
        let observed = match session.call(request, deadline).map_err(lane_failure)? {
            LaneReply::Facts { boot, facts } => {
                facts.validate().map_err(|_| ActionFailure::Failed)?;
                Observed {
                    boot,
                    facts: *facts,
                }
            }
            _ => return Err(ActionFailure::Failed),
        };
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(session);
        Ok(observed)
    }

    /// Which side is too old when the Core shares no work major with this desktop. Work majors
    /// only rise from one release to the next, so the side on the older release is the older
    /// side. An unreadable or unordered version reads as this desktop being too old, which
    /// offers "Update SpaceTerm".
    fn direction(&self) -> Incompatible {
        let core = self
            .open()
            .ok()
            .and_then(|mut session| admin::status(&mut session).ok())
            .and_then(|status| Version::parse(&status.runtime.version).ok())
            .and_then(|version| version.release());
        let desktop = Version::parse(env!("SPACETERM_VERSION"))
            .ok()
            .and_then(|version| version.release());
        match (core, desktop) {
            (Some(core), Some(desktop)) if core < desktop => Incompatible::ListenerTooOld,
            _ => Incompatible::CallerTooOld,
        }
    }
}

impl MachineLane for CoreLane {
    fn facts(&self) -> Result<Observed, ActionFailure> {
        self.facts_call(
            &LaneRequest::Facts {},
            Instant::now() + admin::REQUEST_LIMIT,
        )
    }

    fn wait_facts(&self, boot: &str, revision: u64) -> Result<Observed, ActionFailure> {
        self.facts_call(
            &LaneRequest::WaitFacts {
                boot: boot.to_owned(),
                revision,
            },
            Instant::now() + WAIT_FACTS_LIMIT + WAIT_MARGIN,
        )
    }

    fn check(&self) -> Result<(), ActionFailure> {
        self.facts_call(&LaneRequest::Check, Instant::now() + admin::STAGE_LIMIT)
            .map(drop)
    }

    fn install(&self, build: Option<&EmbeddedCore>) -> Result<(), ActionFailure> {
        let mut session = self.open()?;
        match build {
            Some(build) => push::push(&mut session, build, &mut |_, _| {}).map(drop),
            None => admin::install_from_feed(&mut session)
                .map(drop)
                .map_err(lane_failure),
        }
    }

    fn change(&self, change: LaneChange, sessions: Option<u32>) -> Result<(), ActionFailure> {
        let action = match change {
            LaneChange::Restart => LaneAction::Restart,
            LaneChange::Stop => LaneAction::Stop,
        };
        let lane = self
            .core
            .service_lane()
            .map_err(|error| core_failure(&error))?;
        let pending = self
            .core
            .service_operations()
            .map_err(|error| core_failure(&error))?;
        let (mut session, _) = admin::open(&lane, &pending).map_err(lane_failure)?;
        let preview = admin::preview(&mut session, action).map_err(lane_failure)?;
        // The Core binds its token to what it previewed; this binds the preview to what the
        // person confirmed.
        if sessions.is_some_and(|confirmed| confirmed != preview.effects.ends_sessions) {
            return Err(ActionFailure::Changed);
        }
        let wait = admin::wait(&preview.action);
        let outcome = admin::execute(
            &lane,
            session,
            preview.action,
            preview.token,
            wait,
            &pending,
            &mut || Ok(()),
            // The flow finishes on its own thread whatever the desktop does meanwhile.
            &|| None,
        )
        .map_err(lane_failure)?;
        match admin::verdict(&outcome) {
            admin::Verdict::Done | admin::Verdict::Shutdown => Ok(()),
            admin::Verdict::Failed(Some(code)) => Err(failure_of_code(&code)),
            admin::Verdict::Failed(None) | admin::Verdict::NotPerformed => {
                Err(ActionFailure::Failed)
            }
            admin::Verdict::Uncertain => Err(ActionFailure::Unreachable),
        }
    }
}

fn core_failure(error: &CoreError) -> ActionFailure {
    failure_of_code(spaceterm_core::service::lane_codes::code(error))
}

fn lane_failure(error: LaneError) -> ActionFailure {
    match error {
        LaneError::Code(code) => failure_of_code(&code),
        LaneError::Core(error) => core_failure(&error),
        LaneError::Interrupted => ActionFailure::Failed,
    }
}
