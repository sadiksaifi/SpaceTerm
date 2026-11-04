//! What SpaceTerm last observed about each server, so a server it isn't connected to shows its
//! last-known facts.
//!
//! An observation comes only from something that actually reported on the server: this account's
//! service, a paired connection, or the server's own facts over a live connection. Showing a server SpaceTerm isn't connected to is not an
//! observation, so it never replaces what was last observed. Nothing here connects to a server:
//! the server's own facts are read only over a connection SpaceTerm already has. Each observation
//! merges into the retained record, so facts another part of Settings keeps, such as Git and New
//! Worktree Location, stay.

use std::time::Duration;

use gpui::{App, Context, Task};
use spaceterm_core::worktree::{MachineFacts, Read, ReadReply};

use super::SettingsWindow;
use super::catalog::SettingsSectionId;
use super::machine_list::MachineChoice;
use super::machine_selection;
use super::machines::{LocalCoreStatus, ServicePhase};
use crate::core_context::{LastKnownFacts, ObservedCoreStatus};
use crate::direct_connections::{DirectRouteStatus, RefusalReason};
use crate::machine_connection::CoreError;
use crate::machine_connection::service::ServiceStatus;
use crate::ui::core_worktrees::Client;

/// How often the presented server's local observations are looked at again. Its own facts are
/// read once per connection, because they change only when the server restarts.
const LOOK_AGAIN: Duration = Duration::from_secs(5);
/// How long an unchanged live observation goes before it is retained again with its new time, so
/// the time a disconnected server's facts show stays close to when it was last connected.
const UNCHANGED_RETENTION: u64 = 60;

/// The task following the presented server's facts.
#[derive(Default)]
pub(super) struct FactsFollower {
    target: Option<MachineChoice>,
    generation: u64,
    poll: Option<Task<()>>,
}

/// What something that reported on a server found. A part it didn't report is `None`.
#[derive(Default)]
pub(super) struct Observation {
    pub(super) status: Option<ObservedCoreStatus>,
    /// The server's own facts, read over a live connection.
    pub(super) core: Option<MachineFacts>,
    pub(super) version: Option<String>,
    pub(super) starts_automatically: Option<bool>,
    pub(super) running_sessions: Option<u32>,
}

impl Observation {
    fn is_empty(&self) -> bool {
        self.status.is_none()
            && self.core.is_none()
            && self.version.is_none()
            && self.starts_automatically.is_none()
            && self.running_sessions.is_none()
    }
}

/// The status a service read reports.
pub(super) fn service_status(status: ServiceStatus) -> ObservedCoreStatus {
    match ServicePhase::of(status) {
        ServicePhase::Running => ObservedCoreStatus::Running,
        ServicePhase::Stopped => ObservedCoreStatus::Stopped,
        ServicePhase::NotInstalled => ObservedCoreStatus::NotInstalled,
        ServicePhase::NotRunning | ServicePhase::TurnedOff => ObservedCoreStatus::Unavailable,
    }
}

/// What this account's server reported: its connection and its service.
pub(super) fn local_status(
    connection: LocalCoreStatus,
    service: Option<ServiceStatus>,
) -> Option<ObservedCoreStatus> {
    match (connection, service) {
        (LocalCoreStatus::Failed(CoreError::IncompatibleProtocol(_)), _) => {
            Some(ObservedCoreStatus::NeedsUpdate)
        }
        (LocalCoreStatus::Connected(_), _) => Some(ObservedCoreStatus::Running),
        (_, Some(service)) => Some(service_status(service)),
        (LocalCoreStatus::Failed(CoreError::Stopped), None) => Some(ObservedCoreStatus::Stopped),
        (LocalCoreStatus::Failed(CoreError::NotInstalled | CoreError::NotSetUp), None) => {
            Some(ObservedCoreStatus::NotInstalled)
        }
        _ => None,
    }
}

/// What a paired server's connection reported.
pub(super) fn direct_status(route: DirectRouteStatus) -> Option<ObservedCoreStatus> {
    match route {
        DirectRouteStatus::Connected => Some(ObservedCoreStatus::Running),
        DirectRouteStatus::Refused(RefusalReason::Incompatible) => {
            Some(ObservedCoreStatus::NeedsUpdate)
        }
        _ => None,
    }
}

/// The record `previous` becomes with `observed`, as of `now`. A part the observation
/// doesn't report keeps its previous value.
pub(super) fn merged(
    previous: Option<LastKnownFacts>,
    now: u64,
    observed: Observation,
) -> LastKnownFacts {
    let previous = previous.unwrap_or_default();
    let core = observed.core;
    // Stop ends every Terminal Session, so a Core observed stopped or uninstalled runs none.
    let running_sessions = match observed.status {
        Some(ObservedCoreStatus::Stopped | ObservedCoreStatus::NotInstalled) => Some(0),
        _ => observed.running_sessions.or(previous.running_sessions),
    };
    LastKnownFacts {
        observed_at: now,
        status: observed.status.unwrap_or(previous.status),
        machine: core
            .as_ref()
            .map(|facts| facts.machine.clone())
            .or(previous.machine.clone()),
        operating_system: core
            .as_ref()
            .map(|facts| facts.operating_system.clone())
            .or(previous.operating_system.clone()),
        version: core
            .as_ref()
            .map(|facts| facts.version.clone())
            .or(observed.version)
            .or(previous.version.clone()),
        starts_automatically: observed
            .starts_automatically
            .or(previous.starts_automatically),
        running_sessions,
        ..previous
    }
}

/// Whether `next` is worth retaining over `previous`. A change in what was observed always is. A
/// live server's unchanged observation is retained again after a while, so its time stays
/// current; a server that isn't live has no new time to give.
pub(super) fn changed(
    previous: Option<&LastKnownFacts>,
    next: &LastKnownFacts,
    live: bool,
) -> bool {
    previous.is_none_or(|previous| {
        LastKnownFacts {
            observed_at: next.observed_at,
            ..previous.clone()
        } != *next
            || (live
                && next.observed_at.saturating_sub(previous.observed_at) >= UNCHANGED_RETENTION)
    })
}

impl SettingsWindow {
    /// What SpaceTerm holds now that actually reported on `target`, from typed reports only.
    pub(super) fn observation(&self, target: &MachineChoice) -> Observation {
        let selected = self.selected_machine().as_ref() == Some(target);
        let running_sessions = (selected && self.core_state(target).live)
            .then(|| self.running_sessions())
            .flatten()
            .and_then(|count| u32::try_from(count).ok());
        match target {
            MachineChoice::Local => {
                let connection = self
                    .local_core
                    .as_ref()
                    .map_or(LocalCoreStatus::NotChecked, |row| row.current_status());
                let service = self.core_service.as_ref().and_then(|row| row.state.status);
                let version = self
                    .core_update
                    .as_ref()
                    .and_then(|row| row.state.facts.as_ref())
                    .and_then(|facts| facts.approved.as_ref())
                    .map(|approved| approved.version.clone());
                Observation {
                    status: local_status(connection, service),
                    version,
                    starts_automatically: service
                        .filter(|status| status.installed)
                        .map(|status| status.enabled),
                    running_sessions,
                    ..Observation::default()
                }
            }
            MachineChoice::Direct(route) => Observation {
                status: self
                    .saved_direct_machines()
                    .into_iter()
                    .find(|(saved, _, _)| saved == route)
                    .and_then(|(_, status, _)| direct_status(status)),
                running_sessions,
                ..Observation::default()
            },
        }
    }

    /// Follows the observations of the server Servers or Git presents. Selection, section and
    /// connection changes call this; rendering never does.
    pub(super) fn sync_machine_facts(&mut self, cx: &mut Context<Self>) {
        let target = matches!(
            self.active_section,
            SettingsSectionId::Machines | SettingsSectionId::Git
        )
        .then(|| self.selected_machine())
        .flatten();
        let follower = &mut self.machine_facts;
        if follower.target == target && follower.poll.is_some() {
            // A connection or read changed. What it reports now is retained at once, so a
            // connection that ends before the next look still leaves its facts.
            if let Some(target) = target {
                self.remember_observation(&target, None, cx);
            }
            return;
        }
        follower.target = target.clone();
        follower.generation = follower.generation.wrapping_add(1);
        follower.poll = None;
        let Some(target) = target else {
            return;
        };
        let generation = follower.generation;
        follower.poll = Some(cx.spawn(async move |settings, cx| {
            let mut read = None;
            loop {
                // A connection presented after the window heard of it registers its reader
                // later, so each look finds it again until one read succeeds. A server that
                // isn't live is never read. Facts belong to the link they were read over: a new
                // link may reach a restarted server, so its facts are read again.
                let client = settings
                    .update(cx, |settings, cx| {
                        settings
                            .core_state(&target)
                            .live
                            .then(|| client_for(&target, cx))
                            .flatten()
                    })
                    .ok()
                    .flatten();
                let link = client.as_ref().map(|client| client.core.link_generation());
                if read
                    .as_ref()
                    .is_none_or(|(read_over, _)| Some(*read_over) != link)
                {
                    read = None;
                }
                if read.is_none()
                    && let (Some(client), Some(link)) = (client, link)
                    && let Ok(ReadReply::MachineFacts(facts)) =
                        cx.update(|cx| client.read(Read::MachineFacts, cx)).await
                {
                    read = Some((link, facts));
                }
                let core = read.as_ref().map(|(_, facts)| facts.clone());
                let followed = settings.update(cx, |settings, cx| {
                    if settings.machine_facts.generation != generation {
                        return false;
                    }
                    settings.remember_observation(&target, core, cx);
                    true
                });
                if !matches!(followed, Ok(true)) {
                    return;
                }
                cx.background_executor().timer(LOOK_AGAIN).await;
            }
        }));
    }

    /// Reads `target`'s own facts again now, over the connection SpaceTerm already has, and
    /// retains them. `None` when SpaceTerm isn't connected to it, so nothing is read.
    pub(super) fn reread_facts(
        &mut self,
        target: &MachineChoice,
        cx: &mut Context<Self>,
    ) -> Option<Task<()>> {
        if !self.core_state(target).live {
            return None;
        }
        let client = client_for(target, cx)?;
        let read = client.read(Read::MachineFacts, cx);
        let target = target.clone();
        Some(cx.spawn(async move |settings, cx| {
            let core = match read.await {
                Ok(ReadReply::MachineFacts(facts)) => Some(facts),
                _ => None,
            };
            let _ = settings.update(cx, |settings, cx| {
                settings.remember_observation(&target, core, cx);
            });
        }))
    }

    /// Retains what reported on `target`, merged into what SpaceTerm knew.
    fn remember_observation(
        &mut self,
        target: &MachineChoice,
        core: Option<MachineFacts>,
        cx: &mut Context<Self>,
    ) {
        let mut observation = self.observation(target);
        observation.core = core;
        if observation.is_empty() {
            return;
        }
        let live = self.core_state(target).live;
        let key = target.key();
        let previous = machine_selection::facts(&key, cx);
        let next = merged(
            previous.clone(),
            crate::ui::direct_access::unix_now(),
            observation,
        );
        if changed(previous.as_ref(), &next, live) {
            machine_selection::remember(key, next, cx);
        }
    }
}

/// The reader of `target`'s connection, when SpaceTerm presents it.
fn client_for(target: &MachineChoice, cx: &mut App) -> Option<Client> {
    match target {
        MachineChoice::Local => Client::get(cx),
        MachineChoice::Direct(route) => Client::presented_direct(route, cx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(machine: &str, version: &str) -> MachineFacts {
        serde_json::from_value(serde_json::json!({
            "machine": machine,
            "operating_system": "Linux",
            "version": version,
        }))
        .expect("server facts")
    }

    #[test]
    fn an_observation_keeps_facts_another_part_of_settings_retained() {
        use crate::core_context::{NetworkAccessFacts, NetworkListenerFacts, PairedDevicesFacts};

        let network_access = NetworkAccessFacts {
            observed_at: 8,
            machine_name: "penguin".to_owned(),
            policy: None,
            listener: NetworkListenerFacts::Off,
            addresses: None,
        };
        // A successful reading that found no devices, which differs from not knowing.
        let paired_devices = PairedDevicesFacts {
            observed_at: 9,
            devices: Vec::new(),
        };
        let previous = LastKnownFacts {
            observed_at: 10,
            worktree_location: Some("~/wt/{repository}/{branch}".to_owned()),
            running_sessions: Some(3),
            network_access: Some(network_access.clone()),
            paired_devices: Some(paired_devices.clone()),
            ..LastKnownFacts::default()
        };

        let next = merged(
            Some(previous),
            20,
            Observation {
                status: Some(ObservedCoreStatus::Running),
                core: Some(facts("penguin", "2.1.0")),
                starts_automatically: Some(true),
                ..Observation::default()
            },
        );

        assert_eq!(
            next.worktree_location.as_deref(),
            Some("~/wt/{repository}/{branch}")
        );
        assert_eq!(next.running_sessions, Some(3));
        assert_eq!(next.network_access, Some(network_access));
        assert_eq!(next.paired_devices, Some(paired_devices));
        assert_eq!(next.machine.as_deref(), Some("penguin"));
        assert_eq!(next.version.as_deref(), Some("2.1.0"));
        assert_eq!(next.observed_at, 20);
    }

    /// Stop ends every Terminal Session, so a Core observed stopped or uninstalled runs none,
    /// whatever count it reported while it ran.
    #[test]
    fn a_core_observed_stopped_runs_no_sessions() {
        for status in [
            ObservedCoreStatus::Stopped,
            ObservedCoreStatus::NotInstalled,
        ] {
            let previous = LastKnownFacts {
                observed_at: 10,
                status: ObservedCoreStatus::Running,
                running_sessions: Some(1),
                ..LastKnownFacts::default()
            };
            let next = merged(
                Some(previous),
                20,
                Observation {
                    status: Some(status),
                    ..Observation::default()
                },
            );
            assert_eq!(next.running_sessions, Some(0), "{status:?}");
        }
    }

    /// A Stop read from the service while SpaceTerm isn't connected is still what was observed
    /// last, so relaunch shows Stopped rather than the Running seen earlier.
    #[test]
    fn a_service_read_without_a_connection_replaces_an_earlier_running_observation() {
        let earlier = LastKnownFacts {
            observed_at: 10,
            status: ObservedCoreStatus::Running,
            starts_automatically: Some(true),
            ..LastKnownFacts::default()
        };
        let stopped = ServiceStatus {
            installed: true,
            enabled: false,
            running: false,
            stopped: true,
        };

        let next = merged(
            Some(earlier.clone()),
            20,
            Observation {
                status: local_status(LocalCoreStatus::Failed(CoreError::Stopped), Some(stopped)),
                starts_automatically: Some(false),
                ..Observation::default()
            },
        );

        assert!(changed(Some(&earlier), &next, false));
        assert_eq!(next.status, ObservedCoreStatus::Stopped);
        assert_eq!(next.starts_automatically, Some(false));
    }

    #[test]
    fn an_unchanged_observation_is_retained_again_only_while_live_and_after_a_while() {
        let observed = |at| {
            merged(
                None,
                at,
                Observation {
                    status: Some(ObservedCoreStatus::Running),
                    core: Some(facts("penguin", "2.1.0")),
                    running_sessions: Some(1),
                    ..Observation::default()
                },
            )
        };
        let first = observed(100);

        assert!(!changed(Some(&first), &observed(105), true));
        assert!(changed(
            Some(&first),
            &observed(100 + UNCHANGED_RETENTION),
            true
        ));
        assert!(
            !changed(Some(&first), &observed(100 + UNCHANGED_RETENTION), false),
            "a server that isn't live has no new time to give"
        );
        assert!(changed(
            Some(&first),
            &LastKnownFacts {
                running_sessions: Some(2),
                ..observed(105)
            },
            false
        ));
    }
}
