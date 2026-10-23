//! What SpaceTerm last observed about each server, so a server it isn't connected to shows its
//! last-known facts.
//!
//! While Servers or Git shows a connected server, this reads that server's own facts over the
//! connection SpaceTerm already has: its machine name, operating system and running version. It
//! keeps them with the state, automatic start and running Terminal Sessions the window already
//! knows. Nothing here connects to a server, and a server SpaceTerm isn't connected to keeps the
//! facts it last showed. Each observation merges into the retained record, so facts another part
//! of Settings keeps, such as Git and New Worktree Location, stay.

use std::time::Duration;

use gpui::{App, Context, Task};
use spaceterm_server::worktree::{Read, ReadReply, ServerFacts};

use super::SettingsWindow;
use super::catalog::SettingsSectionId;
use super::server_card::observed_status;
use super::server_list::ServerChoice;
use super::server_selection;
use crate::server_context::LastKnownFacts;
use crate::ui::server_worktrees::Client;

/// How often the facts of a connected, presented server are read again.
const POLL: Duration = Duration::from_secs(5);
/// How long an unchanged observation goes before it is retained again with its new time, so the
/// time a disconnected server's facts show stays close to when it was last connected.
const UNCHANGED_RETENTION: u64 = 60;

/// The poll following the presented server's facts.
#[derive(Default)]
pub(super) struct FactsFollower {
    target: Option<ServerChoice>,
    generation: u64,
    poll: Option<Task<()>>,
}

/// The observation `previous` becomes with what SpaceTerm observed now. Facts this observation
/// doesn't carry keep their previous values.
pub(super) fn merged(
    previous: Option<LastKnownFacts>,
    observed_at: u64,
    observed: Observation,
) -> LastKnownFacts {
    let previous = previous.unwrap_or_default();
    LastKnownFacts {
        observed_at,
        status: observed.status,
        machine: observed
            .server
            .as_ref()
            .map(|facts| facts.machine.clone())
            .or(previous.machine.clone()),
        operating_system: observed
            .server
            .as_ref()
            .map(|facts| facts.operating_system.clone())
            .or(previous.operating_system.clone()),
        version: observed
            .server
            .as_ref()
            .map(|facts| facts.version.clone())
            .or(previous.version.clone()),
        starts_automatically: observed
            .starts_automatically
            .or(previous.starts_automatically),
        running_sessions: observed.running_sessions.or(previous.running_sessions),
        ..previous
    }
}

/// Whether `next` says something `previous` didn't, apart from when it was observed.
pub(super) fn changed(previous: Option<&LastKnownFacts>, next: &LastKnownFacts) -> bool {
    previous.is_none_or(|previous| {
        LastKnownFacts {
            observed_at: next.observed_at,
            ..previous.clone()
        } != *next
            || next.observed_at.saturating_sub(previous.observed_at) >= UNCHANGED_RETENTION
    })
}

/// What one look at a connected server found.
pub(super) struct Observation {
    pub(super) status: crate::server_context::ObservedServerStatus,
    pub(super) server: Option<ServerFacts>,
    pub(super) starts_automatically: Option<bool>,
    pub(super) running_sessions: Option<u32>,
}

impl SettingsWindow {
    /// Follows the facts of the server Servers or Git presents. Selection, section and connection
    /// changes call this; rendering never does.
    pub(super) fn sync_server_facts(&mut self, cx: &mut Context<Self>) {
        let target = matches!(
            self.active_section,
            SettingsSectionId::Servers | SettingsSectionId::Git
        )
        .then(|| self.selected_server())
        .flatten();
        let follower = &mut self.server_facts;
        if follower.target == target && follower.poll.is_some() {
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
            loop {
                // A connection presented after the window heard of it registers its reader
                // later, so each look finds it again. A server that isn't live is never read.
                let client = settings
                    .update(cx, |settings, cx| {
                        settings
                            .server_state(&target)
                            .live
                            .then(|| client_for(&target, cx))
                            .flatten()
                    })
                    .ok()
                    .flatten();
                let server = match client {
                    Some(client) => {
                        match cx.update(|cx| client.read(Read::ServerFacts, cx)).await {
                            Ok(ReadReply::ServerFacts(facts)) => Some(Some(facts)),
                            _ => Some(None),
                        }
                    }
                    None => None,
                };
                let followed = settings.update(cx, |settings, cx| {
                    if settings.server_facts.generation != generation {
                        return false;
                    }
                    if let Some(server) = server {
                        settings.remember_server_facts(&target, server, cx);
                    }
                    true
                });
                if !matches!(followed, Ok(true)) {
                    return;
                }
                cx.background_executor().timer(POLL).await;
            }
        }));
    }

    /// Retains what SpaceTerm knows about the connected `target` now, merged into what it knew.
    fn remember_server_facts(
        &mut self,
        target: &ServerChoice,
        server: Option<ServerFacts>,
        cx: &mut Context<Self>,
    ) {
        let state = self.server_state(target);
        if !state.live {
            return;
        }
        let selected = self.selected_server().as_ref() == Some(target);
        let observation = Observation {
            status: observed_status(&state),
            server,
            starts_automatically: selected.then(|| self.automatic_start(cx)).flatten(),
            running_sessions: selected
                .then(|| self.running_sessions())
                .flatten()
                .and_then(|count| u32::try_from(count).ok()),
        };
        let key = target.key();
        let previous = server_selection::facts(&key, cx);
        let next = merged(
            previous.clone(),
            crate::ui::direct_access::unix_now(),
            observation,
        );
        if changed(previous.as_ref(), &next) {
            server_selection::remember(key, next, cx);
        }
    }
}

/// The reader of `target`'s connection, when SpaceTerm presents it.
fn client_for(target: &ServerChoice, cx: &mut App) -> Option<Client> {
    match target {
        ServerChoice::Local => Client::get(cx),
        ServerChoice::Ssh(destination) => Client::presented_ssh(destination, cx),
        ServerChoice::Direct(route) => Client::presented_direct(route, cx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server_context::ObservedServerStatus;

    fn facts(machine: &str, version: &str) -> ServerFacts {
        serde_json::from_value(serde_json::json!({
            "machine": machine,
            "operating_system": "Linux",
            "version": version,
        }))
        .expect("server facts")
    }

    #[test]
    fn an_observation_keeps_facts_another_part_of_settings_retained() {
        let previous = LastKnownFacts {
            observed_at: 10,
            worktree_location: Some("~/wt/{repository}/{branch}".to_owned()),
            running_sessions: Some(3),
            ..LastKnownFacts::default()
        };

        let next = merged(
            Some(previous),
            20,
            Observation {
                status: ObservedServerStatus::Running,
                server: Some(facts("penguin", "2.1.0")),
                starts_automatically: Some(true),
                running_sessions: None,
            },
        );

        assert_eq!(
            next.worktree_location.as_deref(),
            Some("~/wt/{repository}/{branch}")
        );
        assert_eq!(next.running_sessions, Some(3));
        assert_eq!(next.machine.as_deref(), Some("penguin"));
        assert_eq!(next.version.as_deref(), Some("2.1.0"));
        assert_eq!(next.observed_at, 20);
    }

    #[test]
    fn an_unchanged_observation_is_retained_again_only_after_a_while() {
        let observed = |at| {
            merged(
                None,
                at,
                Observation {
                    status: ObservedServerStatus::Running,
                    server: Some(facts("penguin", "2.1.0")),
                    starts_automatically: None,
                    running_sessions: Some(1),
                },
            )
        };
        let first = observed(100);

        assert!(!changed(Some(&first), &observed(105)));
        assert!(changed(Some(&first), &observed(100 + UNCHANGED_RETENTION)));
        assert!(changed(
            Some(&first),
            &LastKnownFacts {
                running_sessions: Some(2),
                ..observed(105)
            }
        ));
    }
}
