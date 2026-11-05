//! The desktop's [`Sources`]: This Mac's or This Computer's account Core through the service
//! manager and the account-local lane, and each paired Machine, across every Saved Connection to
//! it, from the direct connection registry through its own service lane.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use spaceterm_core::protocol::version::{self as protocol, Incompatible, ProtocolVersion};
use spaceterm_core::service::control::Controller;
use spaceterm_core::service::lane::admin;
use spaceterm_core::service::lane::client::{self as lane_client, LaneError, Session};
use spaceterm_core::service::lane::{LaneAction, LaneReply, LaneRequest, WAIT_FACTS_LIMIT};
use spaceterm_core::service::{Service, ServiceStatus};

use super::{
    Changed, Known, LaneChange, Machine, MachineLane, Observed, Reach, Sources, Watching,
    failure_of_code,
};
use crate::direct_connections::{
    DirectConnections, DirectRouteId, DirectRouteStatus, RefusalReason,
};
use crate::machine_connection::{CoreError, MachineConnection};
use crate::updates::machines::{ActionFailure, MachineId, MachineUpdatesError};
use crate::updates::push::{self, EmbeddedCore};

/// How long `WaitFacts` may take beyond the Core's own limit before the lane gives up.
const WAIT_MARGIN: Duration = Duration::from_secs(10);
/// The thread a cancellable long poll's lane I/O runs on.
const WAIT_IO: &str = "spaceterm-machine-wait";

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
                let core = core.clone();
                known.push(Known {
                    id,
                    name: THIS_MACHINE.into(),
                    local: true,
                    machine: Arc::new(LocalMachine {
                        service: Arc::new(controller.clone()),
                        connect: Arc::new(move || {
                            let lane = CoreLane::new(core.clone());
                            let work = lane.work()?;
                            Ok((Arc::new(lane) as Arc<dyn MachineLane>, work))
                        }),
                    }),
                });
            }
        }
        let mut routes: Vec<(MachineId, String, DirectRouteId)> = self
            .direct
            .routes()
            .into_iter()
            .filter_map(|(route, _)| {
                let saved = self.direct.route(&route)?;
                Some((saved.paired.pin.core, saved.label, route))
            })
            .collect();
        routes.sort_by(|a, b| a.1.cmp(&b.1).then(a.2.cmp(&b.2)));
        known.extend(paired_machines(&self.direct, routes));
        known
    }

    fn watch(&self, changed: Changed) -> Result<Watching, MachineUpdatesError> {
        let routes = self.direct.subscribe();
        let followed = {
            let routes = routes.clone();
            let changed = Arc::clone(&changed);
            std::thread::Builder::new()
                .name("spaceterm-machine-routes".into())
                .spawn(move || {
                    while routes.recv_blocking().is_ok() {
                        if !changed() {
                            break;
                        }
                    }
                })
        };
        if followed.is_err() {
            self.direct.close_subscription(&routes);
            return Err(MachineUpdatesError::Unavailable);
        }
        let mut watching = DesktopWatching {
            direct: self.direct.clone(),
            routes,
            service: None,
        };
        if let Some((controller, _)) = &self.local {
            let directories = controller
                .state_directories()
                .map(std::path::Path::to_path_buf)
                .to_vec();
            let process = controller.clone();
            let service = super::service_changes::follow(
                crate::platform::service_watch,
                directories,
                move || process.core_process().ok().flatten(),
                move || changed(),
            )
            .map_err(|_| MachineUpdatesError::Unavailable)?;
            watching.service = Some(service);
        }
        Ok(Box::new(watching))
    }
}

/// The desktop's watches, which end when dropped: the route subscription closes, which ends
/// its thread's wait, and the service watch's stop pipe closes.
struct DesktopWatching {
    direct: DirectConnections,
    routes: async_channel::Receiver<()>,
    service: Option<super::service_changes::Following>,
}

impl Drop for DesktopWatching {
    fn drop(&mut self) {
        self.direct.close_subscription(&self.routes);
    }
}

/// One Machine per Machine ID, named for the first of its routes in `routes`' order.
fn paired_machines(
    direct: &DirectConnections,
    routes: Vec<(MachineId, String, DirectRouteId)>,
) -> Vec<Known> {
    let mut known: Vec<(MachineId, String, Vec<DirectRouteId>)> = Vec::new();
    for (id, label, route) in routes {
        match known.iter_mut().find(|(known, _, _)| *known == id) {
            Some((_, _, routes)) => routes.push(route),
            None => known.push((id, label, vec![route])),
        }
    }
    known
        .into_iter()
        .map(|(id, label, routes)| Known {
            id,
            name: label.into(),
            local: false,
            machine: Arc::new(PairedMachine {
                routes: Arc::new(RegistryRoutes(direct.clone())),
                ids: routes,
            }),
        })
        .collect()
}

/// What This Mac's or This Computer's service manager reports and does.
pub(crate) trait LocalService: Send + Sync {
    fn status(&self) -> Result<ServiceStatus, CoreError>;
    fn start(&self) -> Result<(), CoreError>;
}

impl LocalService for Controller {
    fn status(&self) -> Result<ServiceStatus, CoreError> {
        Controller::status(self)
    }

    fn start(&self) -> Result<(), CoreError> {
        Controller::start(self).map(drop)
    }
}

/// Opens the local Core's lane and learns whether it shares a work major.
type Connect = Arc<
    dyn Fn() -> Result<(Arc<dyn MachineLane>, Result<(), Incompatible>), ActionFailure>
        + Send
        + Sync,
>;

/// This Mac's or This Computer's account Core.
pub(crate) struct LocalMachine {
    pub(crate) service: Arc<dyn LocalService>,
    pub(crate) connect: Connect,
}

impl Machine for LocalMachine {
    fn reach(&self) -> Reach {
        match self.service.status() {
            // A deliberately stopped Core is read from the service manager alone.
            Ok(status) if status.installed && status.stopped => Reach::Stopped,
            Ok(status) if status.installed => match (self.connect)() {
                Ok((lane, work)) => Reach::Lane { lane, work },
                Err(_) => Reach::Offline,
            },
            _ => Reach::Offline,
        }
    }

    fn start(&self) -> Result<(), ActionFailure> {
        self.service.start().map_err(|error| core_failure(&error))
    }
}

/// What the route registry reports about one Saved Connection.
pub(crate) trait Routes: Send + Sync {
    fn status(&self, route: &DirectRouteId) -> Option<DirectRouteStatus>;
    /// A handle whose service lane answers, while the route is connected or the Core refuses
    /// only this desktop's work protocol.
    fn lane(&self, route: &DirectRouteId) -> Option<Arc<dyn RouteLane>>;
}

/// A route's service lane, and the work version its Core offered at admission.
pub(crate) trait RouteLane: MachineLane {
    fn offered_work(&self) -> Result<ProtocolVersion, ActionFailure>;
}

struct RegistryRoutes(DirectConnections);

impl Routes for RegistryRoutes {
    fn status(&self, route: &DirectRouteId) -> Option<DirectRouteStatus> {
        self.0.status(route)
    }

    fn lane(&self, route: &DirectRouteId) -> Option<Arc<dyn RouteLane>> {
        Some(Arc::new(CoreLane::new(self.0.service(route)?)))
    }
}

/// A paired Machine, reached through whichever of its Saved Connections answers now.
pub(crate) struct PairedMachine {
    pub(crate) routes: Arc<dyn Routes>,
    /// Its routes, in the order they are tried.
    pub(crate) ids: Vec<DirectRouteId>,
}

impl Machine for PairedMachine {
    fn reach(&self) -> Reach {
        // A connected route shares the work major. Only when none does is a route whose Core
        // refuses this desktop's work protocol used, over the service lane alone.
        let connected = self
            .ids
            .iter()
            .filter(|route| self.routes.status(route) == Some(DirectRouteStatus::Connected))
            .find_map(|route| self.routes.lane(route));
        if let Some(lane) = connected {
            return Reach::Lane {
                lane: lane as Arc<dyn MachineLane>,
                work: Ok(()),
            };
        }
        let incompatible = self
            .ids
            .iter()
            .filter(|route| {
                self.routes.status(route)
                    == Some(DirectRouteStatus::Refused(RefusalReason::Incompatible))
            })
            .find_map(|route| self.routes.lane(route));
        let Some(lane) = incompatible else {
            return Reach::Offline;
        };
        match lane.offered_work() {
            Ok(offered) => Reach::Lane {
                work: work_with(offered),
                lane: lane as Arc<dyn MachineLane>,
            },
            Err(_) => Reach::Offline,
        }
    }

    fn start(&self) -> Result<(), ActionFailure> {
        // A paired Core is started on its own computer.
        Err(ActionFailure::Failed)
    }
}

/// Whether this desktop shares a work major with a Core that listens with `offered`, and if
/// not, which side is too old. This is the negotiation the Core runs at work admission.
pub(crate) fn work_with(offered: ProtocolVersion) -> Result<(), Incompatible> {
    let mine: Vec<ProtocolVersion> = std::iter::once(protocol::WORK)
        .chain(protocol::PREVIOUS_WORK)
        .collect();
    protocol::negotiate(&mine, offered).map(drop)
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

    /// Whether the Core shares a work major with this desktop, from its admission Offer.
    fn work(&self) -> Result<Result<(), Incompatible>, ActionFailure> {
        self.offered_work().map(work_with)
    }

    fn kept(&self) -> Result<Session, ActionFailure> {
        let kept = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        match kept {
            Some(session) => Ok(session),
            None => self.open(),
        }
    }

    fn keep(&self, session: Session) {
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(session);
    }
}

fn facts_of(reply: LaneReply) -> Result<Observed, ActionFailure> {
    match reply {
        LaneReply::Facts { boot, facts } => {
            facts.validate().map_err(|_| ActionFailure::Failed)?;
            Ok(Observed {
                boot,
                facts: *facts,
            })
        }
        _ => Err(ActionFailure::Failed),
    }
}

impl RouteLane for CoreLane {
    /// Admits a lane session, kept for the next read, so the Core's Offer is current.
    fn offered_work(&self) -> Result<ProtocolVersion, ActionFailure> {
        let session = self.kept()?;
        self.keep(session);
        self.core
            .core_versions()
            .map(|versions| versions.work)
            .ok_or(ActionFailure::Failed)
    }
}

impl MachineLane for CoreLane {
    fn facts(&self) -> Result<Observed, ActionFailure> {
        let mut session = self.kept()?;
        let reply = session
            .call(
                &LaneRequest::Facts {},
                Instant::now() + admin::REQUEST_LIMIT,
            )
            .map_err(lane_failure)?;
        let observed = facts_of(reply)?;
        self.keep(session);
        Ok(observed)
    }

    fn wait_facts(
        &self,
        boot: &str,
        revision: u64,
        stale: &dyn Fn() -> bool,
    ) -> Result<Observed, ActionFailure> {
        let mut session = self.kept()?;
        let request = LaneRequest::WaitFacts {
            boot: boot.to_owned(),
            revision,
        };
        let fixed = Instant::now() + WAIT_FACTS_LIMIT + WAIT_MARGIN;
        // The request runs on its own thread, so a change the sources report abandons it at
        // once. An abandoned request ends at its own deadline, and its connection with it.
        let (session, reply) = lane_client::within(
            WAIT_IO,
            &|| Some(if stale() { Instant::now() } else { fixed }),
            move || {
                let reply = session.call(&request, fixed);
                (session, reply)
            },
        )
        .ok_or(ActionFailure::Unreachable)?;
        let observed = facts_of(reply.map_err(lane_failure)?)?;
        self.keep(session);
        Ok(observed)
    }

    fn check(&self) -> Result<(), ActionFailure> {
        let mut session = self.kept()?;
        let reply = session
            .call(&LaneRequest::Check, Instant::now() + admin::STAGE_LIMIT)
            .map_err(lane_failure)?;
        facts_of(reply)?;
        self.keep(session);
        Ok(())
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

    fn preview(&self, change: LaneChange) -> Result<admin::Preview, ActionFailure> {
        let action = match change {
            LaneChange::Restart => LaneAction::Restart,
            LaneChange::Stop => LaneAction::Stop,
        };
        let mut session = self.open()?;
        admin::preview(&mut session, action).map_err(lane_failure)
    }

    fn execute(&self, preview: admin::Preview) -> Result<(), ActionFailure> {
        let lane = self
            .core
            .service_lane()
            .map_err(|error| core_failure(&error))?;
        let pending = self
            .core
            .service_operations()
            .map_err(|error| core_failure(&error))?;
        let (session, _) = admin::open(&lane, &pending).map_err(lane_failure)?;
        let wait = admin::wait(&preview.action);
        let outcome = admin::execute(
            &lane,
            session,
            preview.action,
            preview.token,
            wait,
            &pending,
            &mut admin::Unshared,
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
        // The Machine's facts changed since the push was offered.
        LaneError::BelowFloor(_) => ActionFailure::Changed,
        LaneError::Core(error) => core_failure(&error),
        LaneError::Interrupted => ActionFailure::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::updates::machines::live::Observed;

    struct Service(ServiceStatus);

    impl LocalService for Service {
        fn status(&self) -> Result<ServiceStatus, CoreError> {
            Ok(self.0)
        }

        fn start(&self) -> Result<(), CoreError> {
            Ok(())
        }
    }

    /// A lane that answers nothing and whose Core offered work major `major`.
    struct Lane {
        major: u16,
    }

    impl MachineLane for Lane {
        fn facts(&self) -> Result<Observed, ActionFailure> {
            Err(ActionFailure::Unreachable)
        }
        fn wait_facts(
            &self,
            _: &str,
            _: u64,
            _: &dyn Fn() -> bool,
        ) -> Result<Observed, ActionFailure> {
            Err(ActionFailure::Unreachable)
        }
        fn check(&self) -> Result<(), ActionFailure> {
            Ok(())
        }
        fn install(&self, _: Option<&EmbeddedCore>) -> Result<(), ActionFailure> {
            Ok(())
        }
        fn preview(&self, _: LaneChange) -> Result<admin::Preview, ActionFailure> {
            Err(ActionFailure::Unreachable)
        }
        fn execute(&self, _: admin::Preview) -> Result<(), ActionFailure> {
            Ok(())
        }
    }

    impl RouteLane for Lane {
        fn offered_work(&self) -> Result<ProtocolVersion, ActionFailure> {
            Ok(ProtocolVersion {
                major: self.major,
                minor: 0,
            })
        }
    }

    fn local(status: ServiceStatus, dialed: &Arc<AtomicUsize>) -> LocalMachine {
        let dialed = Arc::clone(dialed);
        LocalMachine {
            service: Arc::new(Service(status)),
            connect: Arc::new(move || {
                dialed.fetch_add(1, Ordering::SeqCst);
                Ok((Arc::new(Lane { major: 1 }) as Arc<dyn MachineLane>, Ok(())))
            }),
        }
    }

    struct OffInLoginItems;

    impl LocalService for OffInLoginItems {
        fn status(&self) -> Result<ServiceStatus, CoreError> {
            Ok(ServiceStatus {
                installed: true,
                enabled: true,
                running: false,
                stopped: true,
            })
        }

        fn start(&self) -> Result<(), CoreError> {
            Err(CoreError::LoginItemOff)
        }
    }

    /// The menu bar's Start reaches the service, which refuses a Core off in Login Items. The
    /// refusal stays typed, so the menu can open Login Items instead of reporting a failure.
    #[test]
    fn a_start_refused_by_login_items_stays_that_failure() {
        let machine = LocalMachine {
            service: Arc::new(OffInLoginItems),
            connect: Arc::new(|| Err(ActionFailure::Unreachable)),
        };
        assert_eq!(machine.start(), Err(ActionFailure::LoginItemOff));
    }

    /// A deliberately stopped local Core reads Stopped from the service manager and is never
    /// dialed; a running one is.
    #[test]
    fn a_stopped_local_core_is_read_from_the_service_manager_without_connecting() {
        let dialed = Arc::new(AtomicUsize::new(0));
        let stopped = local(
            ServiceStatus {
                installed: true,
                enabled: true,
                running: false,
                stopped: true,
            },
            &dialed,
        );
        assert!(matches!(stopped.reach(), Reach::Stopped));
        assert_eq!(dialed.load(Ordering::SeqCst), 0);

        let running = local(
            ServiceStatus {
                installed: true,
                enabled: true,
                running: true,
                stopped: false,
            },
            &dialed,
        );
        assert!(matches!(running.reach(), Reach::Lane { .. }));
        assert_eq!(dialed.load(Ordering::SeqCst), 1);
    }

    struct FakeRoutes(Vec<(DirectRouteId, DirectRouteStatus, Option<u16>)>);

    impl Routes for FakeRoutes {
        fn status(&self, route: &DirectRouteId) -> Option<DirectRouteStatus> {
            self.0
                .iter()
                .find(|(id, ..)| id == route)
                .map(|(_, s, _)| *s)
        }

        fn lane(&self, route: &DirectRouteId) -> Option<Arc<dyn RouteLane>> {
            let (_, _, major) = self.0.iter().find(|(id, ..)| id == route)?;
            Some(Arc::new(Lane { major: (*major)? }))
        }
    }

    fn paired(routes: Vec<(DirectRouteId, DirectRouteStatus, Option<u16>)>) -> PairedMachine {
        PairedMachine {
            ids: routes.iter().map(|(id, ..)| *id).collect(),
            routes: Arc::new(FakeRoutes(routes)),
        }
    }

    const INCOMPATIBLE: DirectRouteStatus = DirectRouteStatus::Refused(RefusalReason::Incompatible);

    /// A Core that refuses this desktop's work protocol is still reached over its service lane,
    /// and the work major its Offer names says which side is too old.
    #[test]
    fn an_incompatible_core_is_reached_over_its_service_lane_with_the_offered_direction() {
        let older = paired(vec![(
            [1; 32],
            INCOMPATIBLE,
            Some(protocol::WORK.major - 1),
        )]);
        assert!(matches!(
            older.reach(),
            Reach::Lane {
                work: Err(Incompatible::ListenerTooOld),
                ..
            }
        ));
        let newer = paired(vec![(
            [1; 32],
            INCOMPATIBLE,
            Some(protocol::WORK.major + 1),
        )]);
        assert!(matches!(
            newer.reach(),
            Reach::Lane {
                work: Err(Incompatible::CallerTooOld),
                ..
            }
        ));
        let unreachable = paired(vec![([1; 32], INCOMPATIBLE, None)]);
        assert!(matches!(unreachable.reach(), Reach::Offline));
    }

    /// Of a Machine's Saved Connections, a connected one is used before one that is
    /// disconnected or refuses the work protocol.
    #[test]
    fn a_machine_is_reached_through_whichever_route_answers() {
        let machine = paired(vec![
            ([1; 32], DirectRouteStatus::Disconnected, Some(1)),
            ([2; 32], INCOMPATIBLE, Some(protocol::WORK.major + 1)),
            ([3; 32], DirectRouteStatus::Connected, Some(1)),
        ]);
        assert!(matches!(machine.reach(), Reach::Lane { work: Ok(()), .. }));
        let none = paired(vec![([1; 32], DirectRouteStatus::Disconnected, Some(1))]);
        assert!(matches!(none.reach(), Reach::Offline));
    }

    /// Two Saved Connections to one Machine list it once, under the first route's name.
    #[test]
    fn saved_connections_to_one_machine_list_it_once() {
        let (direct, _) = crate::direct_connections::tests::offline("studio");
        let known = paired_machines(
            &direct,
            vec![
                ([7; 32], "penguin".into(), [1; 32]),
                ([8; 32], "build-box".into(), [2; 32]),
                ([7; 32], "penguin (LAN)".into(), [3; 32]),
            ],
        );
        let listed: Vec<(MachineId, String)> = known
            .iter()
            .map(|known| (known.id, known.name.to_string()))
            .collect();
        assert_eq!(
            listed,
            [
                ([7; 32], "penguin".to_owned()),
                ([8; 32], "build-box".to_owned())
            ]
        );
    }

    #[test]
    fn the_offered_work_version_decides_compatibility() {
        let major = |major| ProtocolVersion { major, minor: 0 };
        assert_eq!(work_with(protocol::WORK), Ok(()));
        assert_eq!(
            work_with(major(protocol::WORK.major - 1)),
            Err(Incompatible::ListenerTooOld)
        );
        assert_eq!(
            work_with(major(protocol::WORK.major + 1)),
            Err(Incompatible::CallerTooOld)
        );
    }
}
