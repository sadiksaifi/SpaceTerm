//! The server Servers and Git present, retained across launches with its last-known facts.
//!
//! One app-owned selection serves both pages. Choosing a server only changes what they present:
//! it connects to, starts and installs nothing. The retained record lives outside the Settings
//! Document and is read and written on one background worker, in the order the window asked, so
//! rendering never waits for storage. A failure to retain the record leaves the choice in effect
//! for this run.

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{App, Global};

use crate::platform::app_paths::AppPaths;
use crate::server_context::{ContextError, LastKnownFacts, ServerContext, ServerKey};

/// The selection and facts the window presents, with the worker that retains them.
pub(crate) struct ServerSelection {
    selected: ServerKey,
    facts: BTreeMap<ServerKey, LastKnownFacts>,
    /// The worker's queue. `None` keeps the selection for this run only.
    retain: Option<async_channel::Sender<(u64, Change)>>,
    /// Counts the window's changes, so a snapshot answering an earlier one never undoes a later
    /// choice.
    requested: u64,
    /// The servers the latest reconciliation listed, which the worker reports facts for.
    available: Vec<ServerKey>,
}

impl Global for ServerSelection {}

/// One change for the worker to retain, in order.
enum Change {
    Reconcile(Vec<ServerKey>),
    Select(ServerKey),
    Remember(ServerKey, LastKnownFacts),
}

/// What the retained record holds after one change.
struct Snapshot {
    change: u64,
    selected: ServerKey,
    facts: BTreeMap<ServerKey, LastKnownFacts>,
}

/// Starts the worker that loads and retains the selection in `paths`' private storage. Until it
/// loads, the window presents this computer's server.
pub(crate) fn configure_server_context(paths: Arc<AppPaths>, cx: &mut App) {
    let (changes, queue) = async_channel::unbounded::<(u64, Change)>();
    let (snapshots, published) = async_channel::unbounded::<Snapshot>();
    cx.set_global(ServerSelection {
        selected: ServerKey::Local,
        facts: BTreeMap::new(),
        retain: Some(changes),
        requested: 0,
        available: Vec::new(),
    });
    cx.background_spawn(async move {
        // A record SpaceTerm can't read is never replaced: the context refuses to write over it,
        // and the choices of this run stay in memory.
        let mut context = match ServerContext::load(Arc::clone(&paths)) {
            Ok(context) => context,
            Err(_) => ServerContext::new(paths),
        };
        let mut available: Vec<ServerKey> = vec![ServerKey::Local];
        let _ = snapshots.send(snapshot(0, &context, &available)).await;
        while let Ok((change, request)) = queue.recv().await {
            let _: Result<(), ContextError> = match request {
                Change::Reconcile(keys) => {
                    available = keys;
                    context.reconcile(&available)
                }
                Change::Select(key) => context.select(key),
                Change::Remember(key, facts) => context.remember(key, facts),
            };
            if snapshots
                .send(snapshot(change, &context, &available))
                .await
                .is_err()
            {
                return;
            }
        }
    })
    .detach();
    cx.spawn(async move |cx| {
        while let Ok(snapshot) = published.recv().await {
            cx.update_global::<ServerSelection, _>(|selection, _| {
                // A choice made since this change was queued is still on its way.
                if snapshot.change >= selection.requested {
                    selection.selected = snapshot.selected;
                }
                for (key, facts) in snapshot.facts {
                    let newer = selection
                        .facts
                        .get(&key)
                        .is_none_or(|known| facts.observed_at >= known.observed_at);
                    if newer {
                        selection.facts.insert(key, facts);
                    }
                }
            });
        }
    })
    .detach();
}

fn snapshot(change: u64, context: &ServerContext, available: &[ServerKey]) -> Snapshot {
    Snapshot {
        change,
        selected: context.selected().clone(),
        facts: available
            .iter()
            .filter_map(|key| Some((key.clone(), context.facts(key)?.clone())))
            .collect(),
    }
}

impl ServerSelection {
    fn queue(&mut self, change: Change) {
        self.requested = self.requested.wrapping_add(1);
        if let Some(retain) = &self.retain {
            // An unbounded queue refuses only once its worker has ended with the application.
            let _ = retain.try_send((self.requested, change));
        }
    }
}

/// The selected server. This computer's server until the record loads or when none is retained.
pub(crate) fn selected(cx: &App) -> ServerKey {
    cx.try_global::<ServerSelection>()
        .map_or(ServerKey::Local, |selection| selection.selected.clone())
}

/// What SpaceTerm last observed about `key`, as of the time the facts carry.
pub(crate) fn facts(key: &ServerKey, cx: &App) -> Option<LastKnownFacts> {
    cx.try_global::<ServerSelection>()?.facts.get(key).cloned()
}

/// Presents `key` on Servers and Git, now and after relaunch.
pub(crate) fn select(key: ServerKey, cx: &mut App) {
    ensure(cx);
    cx.update_global::<ServerSelection, _>(|selection, _| {
        selection.selected = key.clone();
        selection.queue(Change::Select(key));
    });
}

/// Records an observation the window made of `key`. An older observation never replaces a newer
/// one.
pub(crate) fn remember(key: ServerKey, facts: LastKnownFacts, cx: &mut App) {
    ensure(cx);
    cx.update_global::<ServerSelection, _>(|selection, _| {
        let newer = selection
            .facts
            .get(&key)
            .is_none_or(|known| facts.observed_at >= known.observed_at);
        if !newer || selection.facts.get(&key) == Some(&facts) {
            return;
        }
        selection.facts.insert(key.clone(), facts.clone());
        selection.queue(Change::Remember(key, facts));
    });
}

/// Tells the record which servers exist now. A selected server that is gone gives way to this
/// computer's, and the facts of forgotten servers are dropped.
pub(crate) fn reconcile(available: Vec<ServerKey>, cx: &mut App) {
    ensure(cx);
    cx.update_global::<ServerSelection, _>(|selection, _| {
        if selection.available == available {
            return;
        }
        if !available.contains(&selection.selected) {
            selection.selected = ServerKey::Local;
        }
        selection
            .facts
            .retain(|key, _| *key == ServerKey::Local || available.contains(key));
        selection.available = available.clone();
        selection.queue(Change::Reconcile(available));
    });
}

/// A build or test without retained storage still keeps the selection for its run.
fn ensure(cx: &mut App) {
    if !cx.has_global::<ServerSelection>() {
        cx.set_global(ServerSelection {
            selected: ServerKey::Local,
            facts: BTreeMap::new(),
            retain: None,
            requested: 0,
            available: Vec::new(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn removing_the_selected_server_presents_this_computer(cx: &mut gpui::TestAppContext) {
        let studio = ServerKey::Ssh("studio".to_owned());
        cx.update(|cx| {
            reconcile(vec![ServerKey::Local, studio.clone()], cx);
            select(studio.clone(), cx);
            assert_eq!(selected(cx), studio);

            reconcile(vec![ServerKey::Local], cx);

            assert_eq!(selected(cx), ServerKey::Local);
        });
    }

    #[gpui::test]
    fn an_older_observation_never_replaces_a_newer_one(cx: &mut gpui::TestAppContext) {
        let studio = ServerKey::Ssh("studio".to_owned());
        let observed = |observed_at, version: &str| LastKnownFacts {
            observed_at,
            version: Some(version.to_owned()),
            ..LastKnownFacts::default()
        };
        cx.update(|cx| {
            remember(studio.clone(), observed(20, "2.1.0"), cx);
            remember(studio.clone(), observed(10, "2.0.0"), cx);

            assert_eq!(
                facts(&studio, cx).and_then(|facts| facts.version),
                Some("2.1.0".to_owned())
            );
        });
    }
}
