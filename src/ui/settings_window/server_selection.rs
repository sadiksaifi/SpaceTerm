//! The server Servers and Git present, retained across launches with its last-known facts.
//!
//! One app-owned selection serves both pages. Choosing a server only changes what they present:
//! it connects to, starts and installs nothing. The retained record lives outside the Settings
//! Document and is read and written on one background worker, in the order the window asked, so
//! rendering never waits for storage. A failure to retain the record leaves the choice in effect
//! for this run.

use std::collections::{BTreeMap, BTreeSet};
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
    /// Counts the window's changes. Only the snapshot answering the latest one is presented: an
    /// earlier snapshot predates a change the window already shows, and the latest one holds
    /// everything the earlier ones did.
    requested: u64,
    /// The servers that exist, once the saved connections have loaded. Until then every
    /// server's facts are kept.
    inventory: Option<Vec<ServerKey>>,
}

impl Global for ServerSelection {}

/// One change for the worker to retain, in order.
enum Change {
    Reconcile(Vec<ServerKey>),
    Select(ServerKey),
    /// Boxed, since the facts are much larger than the other changes.
    Remember(ServerKey, Box<LastKnownFacts>),
}

/// What the retained record holds after one change.
struct Snapshot {
    change: u64,
    selected: ServerKey,
    facts: BTreeMap<ServerKey, LastKnownFacts>,
}

/// The retained record, which the worker changes in the order the window asked.
struct Retainer {
    context: ServerContext,
    /// The servers the window said exist. Until it says, the record keeps every server's facts.
    inventory: Option<Vec<ServerKey>>,
    /// The servers this run selected or observed, whose facts are reported before the window
    /// says which servers exist.
    touched: BTreeSet<ServerKey>,
}

impl Retainer {
    fn load(paths: Arc<AppPaths>) -> Self {
        // A record SpaceTerm can't read is never replaced: the context refuses to write over it,
        // and the choices of this run stay in memory.
        let context =
            ServerContext::load(Arc::clone(&paths)).unwrap_or_else(|_| ServerContext::new(paths));
        Self {
            context,
            inventory: None,
            touched: BTreeSet::new(),
        }
    }

    fn apply(&mut self, change: u64, request: Change) -> Snapshot {
        let _: Result<(), ContextError> = match request {
            Change::Reconcile(keys) => {
                let reconciled = self.context.reconcile(&keys);
                self.inventory = Some(keys);
                reconciled
            }
            Change::Select(key) => {
                self.touched.insert(key.clone());
                self.context.select(key)
            }
            Change::Remember(key, facts) => {
                self.touched.insert(key.clone());
                self.context.remember(key, *facts)
            }
        };
        self.snapshot(change)
    }

    /// The selection and the facts of this computer's server, the selected server, and either
    /// every server that exists or, before the window says which do, every one this run touched.
    fn snapshot(&self, change: u64) -> Snapshot {
        let selected = self.context.selected().clone();
        let reported: BTreeSet<&ServerKey> = match &self.inventory {
            Some(keys) => keys.iter().collect(),
            None => self.touched.iter().collect(),
        };
        let facts = [&ServerKey::Local, &selected]
            .into_iter()
            .chain(reported)
            .filter_map(|key| Some((key.clone(), self.context.facts(key)?.clone())))
            .collect();
        Snapshot {
            change,
            selected,
            facts,
        }
    }
}

/// Starts the worker that loads and retains the selection in `paths`' private storage. Until it
/// loads, the window presents this computer's server.
pub(crate) fn configure_server_context(paths: Arc<AppPaths>, cx: &mut App) {
    let (changes, queue) = async_channel::unbounded::<(u64, Change)>();
    let (snapshots, published) = async_channel::unbounded::<Snapshot>();
    cx.set_global(ServerSelection::new(Some(changes)));
    cx.background_spawn(async move {
        let mut retainer = Retainer::load(paths);
        let _ = snapshots.send(retainer.snapshot(0)).await;
        while let Ok((change, request)) = queue.recv().await {
            if snapshots
                .send(retainer.apply(change, request))
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
            cx.update(|cx| present(snapshot, cx));
        }
    })
    .detach();
}

impl ServerSelection {
    fn new(retain: Option<async_channel::Sender<(u64, Change)>>) -> Self {
        Self {
            selected: ServerKey::Local,
            facts: BTreeMap::new(),
            retain,
            requested: 0,
            inventory: None,
        }
    }

    fn queue(&mut self, change: Change) {
        self.requested = self.requested.wrapping_add(1);
        if let Some(retain) = &self.retain {
            // An unbounded queue refuses only once its worker has ended with the application.
            let _ = retain.try_send((self.requested, change));
        }
    }

    /// Whether `key` may carry facts: this computer's server always, another once it is known
    /// to exist or while the saved connections are still loading.
    fn admits(&self, key: &ServerKey) -> bool {
        *key == ServerKey::Local
            || self
                .inventory
                .as_ref()
                .is_none_or(|inventory| inventory.contains(key))
    }

    /// What the window presents once `snapshot` arrives, or `None` when it changes nothing. A
    /// snapshot answering an earlier change changes nothing: the latest change's own snapshot is
    /// still on its way.
    fn presented(
        &self,
        snapshot: Snapshot,
    ) -> Option<(ServerKey, BTreeMap<ServerKey, LastKnownFacts>)> {
        if snapshot.change != self.requested {
            return None;
        }
        let facts: BTreeMap<_, _> = snapshot
            .facts
            .into_iter()
            .filter(|(key, _)| self.admits(key))
            .collect();
        (snapshot.selected != self.selected || facts != self.facts)
            .then_some((snapshot.selected, facts))
    }
}

// Updating the selection notifies its observers even when the update changes nothing. The
// Settings window records observations whenever it is notified, so every step below decides on a
// read of the selection first and updates it only to change it.

/// Presents what the record holds after one change, when that changes what the window shows.
fn present(snapshot: Snapshot, cx: &mut App) {
    let Some((selected, facts)) = cx
        .try_global::<ServerSelection>()
        .and_then(|selection| selection.presented(snapshot))
    else {
        return;
    };
    cx.update_global::<ServerSelection, _>(|selection, _| {
        selection.selected = selected;
        selection.facts = facts;
    });
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

/// Records an observation the window made of `key`, merged into what the selection holds as
/// the retained record merges it. An older observation never replaces newer facts, and a reading
/// that lacks network or device facts never erases them.
pub(crate) fn remember(key: ServerKey, facts: LastKnownFacts, cx: &mut App) {
    ensure(cx);
    let selection = cx.global::<ServerSelection>();
    // A removed server's late observation never brings its facts back.
    if !selection.admits(&key) {
        return;
    }
    let known = selection.facts.get(&key);
    let merged = known.map_or_else(|| facts.clone(), |known| known.merged(&facts));
    if known == Some(&merged) {
        return;
    }
    cx.update_global::<ServerSelection, _>(|selection, _| {
        selection.facts.insert(key.clone(), merged);
        // The worker merges the observation into the record on disk, which can hold facts this
        // window hasn't seen.
        selection.queue(Change::Remember(key, Box::new(facts)));
    });
}

/// Tells the record which servers exist now, once the saved connections have loaded. A selected
/// server that is gone gives way to this computer's, and the facts of forgotten servers are
/// dropped.
pub(crate) fn reconcile(available: Vec<ServerKey>, cx: &mut App) {
    ensure(cx);
    if cx.global::<ServerSelection>().inventory.as_ref() == Some(&available) {
        return;
    }
    cx.update_global::<ServerSelection, _>(|selection, _| {
        if !available.contains(&selection.selected) {
            selection.selected = ServerKey::Local;
        }
        selection.inventory = Some(available.clone());
        let facts = std::mem::take(&mut selection.facts);
        selection.facts = facts
            .into_iter()
            .filter(|(key, _)| selection.admits(key))
            .collect();
        selection.queue(Change::Reconcile(available));
    });
}

/// A build or test without retained storage still keeps the selection for its run.
fn ensure(cx: &mut App) {
    if !cx.has_global::<ServerSelection>() {
        cx.set_global(ServerSelection::new(None));
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::platform::app_directories::AppDirectoryEnvironment;
    use crate::platform::testing::{RecordingFilesystem, resolve_app_paths};

    fn studio() -> ServerKey {
        ServerKey::Direct([7; 32])
    }

    /// The window's selection with its worker run by hand over in-memory storage, so a test
    /// decides when each snapshot arrives.
    struct Delayed {
        queued: async_channel::Receiver<(u64, Change)>,
        retainer: Retainer,
    }

    impl Delayed {
        fn install(cx: &mut gpui::TestAppContext) -> Self {
            let paths = resolve_app_paths(
                &AppDirectoryEnvironment {
                    home: Some("/home/test".into()),
                    ..Default::default()
                },
                Some("/runtime".into()),
                200,
                Arc::new(RecordingFilesystem::default()),
            )
            .unwrap();
            let (changes, queued) = async_channel::unbounded();
            cx.update(|cx| cx.set_global(ServerSelection::new(Some(changes))));
            Self {
                queued,
                retainer: Retainer::load(Arc::new(paths)),
            }
        }

        /// Retains every change the window queued so far, returning each one's snapshot
        /// without presenting it.
        fn retain(&mut self) -> Vec<Snapshot> {
            std::iter::from_fn(|| self.queued.try_recv().ok())
                .map(|(change, request)| self.retainer.apply(change, request))
                .collect()
        }
    }

    fn arrive(snapshots: Vec<Snapshot>, cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            for snapshot in snapshots {
                present(snapshot, cx);
            }
        });
    }

    /// Counts the notifications the window's observer of the selection receives.
    fn count_notifications(cx: &mut gpui::TestAppContext) -> (Rc<Cell<usize>>, gpui::Subscription) {
        let notified = Rc::new(Cell::new(0));
        let counter = Rc::clone(&notified);
        let observer = cx.update(|cx| {
            cx.observe_global::<ServerSelection>(move |_| counter.set(counter.get() + 1))
        });
        (notified, observer)
    }

    fn observed(observed_at: u64) -> LastKnownFacts {
        LastKnownFacts {
            observed_at,
            machine: Some("Studio".to_owned()),
            ..LastKnownFacts::default()
        }
    }

    /// The window records observations whenever the selection notifies it, so a refused
    /// observation or an unchanged inventory that notified would make it record again forever.
    #[gpui::test]
    fn observations_and_inventories_that_change_nothing_notify_no_one(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            reconcile(vec![ServerKey::Local, studio()], cx);
            remember(studio(), observed(20), cx);
        });
        let (notified, _observer) = count_notifications(cx);

        cx.update(|cx| {
            // Older than what the selection holds, as after the clock moves back.
            remember(studio(), observed(10), cx);
            remember(studio(), observed(20), cx);
            remember(ServerKey::Direct([8; 32]), observed(30), cx);
            reconcile(vec![ServerKey::Local, studio()], cx);
        });
        assert_eq!(notified.get(), 0, "nothing changed, so nothing is told");

        cx.update(|cx| remember(studio(), observed(30), cx));
        assert_eq!(
            notified.get(),
            1,
            "a newer observation still tells the window"
        );
    }

    /// Network access is observed on its own, so a reading whose general facts are older than
    /// the record can still carry newer network facts. Those are kept, and a later reading
    /// without network facts never erases them.
    #[gpui::test]
    fn an_older_reading_still_brings_its_newer_network_access_once(cx: &mut gpui::TestAppContext) {
        use crate::server_context::{NetworkAccessFacts, NetworkListenerFacts};

        cx.update(|cx| {
            reconcile(vec![ServerKey::Local, studio()], cx);
            remember(studio(), observed(20), cx);
        });
        let (notified, _observer) = count_notifications(cx);
        let network = NetworkAccessFacts {
            observed_at: 30,
            server_name: "Studio".to_owned(),
            policy: None,
            listener: NetworkListenerFacts::Off,
            addresses: None,
        };
        let late = LastKnownFacts {
            network_access: Some(network.clone()),
            ..observed(10)
        };

        cx.update(|cx| remember(studio(), late.clone(), cx));
        assert_eq!(
            notified.get(),
            1,
            "the newer network reading is accepted once"
        );
        let held = cx.update(|cx| facts(&studio(), cx)).unwrap();
        assert_eq!(
            (held.observed_at, held.network_access.as_ref()),
            (20, Some(&network)),
            "the newer general facts stay"
        );

        cx.update(|cx| remember(studio(), late, cx));
        assert_eq!(notified.get(), 1, "the same reading again changes nothing");

        cx.update(|cx| {
            remember(
                studio(),
                LastKnownFacts {
                    observed_at: 40,
                    machine: Some("Studio Pro".to_owned()),
                    ..LastKnownFacts::default()
                },
                cx,
            );
        });
        assert_eq!(notified.get(), 2);
        let held = cx.update(|cx| facts(&studio(), cx)).unwrap();
        assert_eq!(
            (held.machine.as_deref(), held.network_access),
            (Some("Studio Pro"), Some(network)),
            "a reading without network facts keeps the ones observed"
        );
    }

    #[gpui::test]
    fn a_snapshot_that_changes_nothing_notifies_no_one(cx: &mut gpui::TestAppContext) {
        let mut worker = Delayed::install(cx);
        cx.update(|cx| {
            reconcile(vec![ServerKey::Local, studio()], cx);
            remember(studio(), observed(10), cx);
        });
        let earlier = worker.retain();
        cx.update(|cx| remember(studio(), observed(20), cx));
        let latest = worker.retain();
        let (notified, _observer) = count_notifications(cx);

        arrive(earlier, cx);
        assert_eq!(notified.get(), 0, "a late snapshot presents nothing");
        arrive(latest, cx);

        assert_eq!(
            notified.get(),
            0,
            "the latest snapshot holds what the window already presents"
        );
        assert_eq!(cx.update(|cx| facts(&studio(), cx)), Some(observed(20)));
    }

    #[gpui::test]
    fn a_removed_servers_facts_never_return_with_a_late_snapshot(cx: &mut gpui::TestAppContext) {
        let mut worker = Delayed::install(cx);
        let studio_facts = LastKnownFacts {
            observed_at: 10,
            machine: Some("Studio".to_owned()),
            ..LastKnownFacts::default()
        };
        cx.update(|cx| {
            reconcile(vec![ServerKey::Local, studio()], cx);
            remember(studio(), studio_facts, cx);
        });
        let before_removal = worker.retain();
        cx.update(|cx| reconcile(vec![ServerKey::Local], cx));
        let after_removal = worker.retain();

        arrive(before_removal, cx);
        assert_eq!(cx.update(|cx| facts(&studio(), cx)), None);
        arrive(after_removal, cx);

        cx.update(|cx| {
            assert_eq!(facts(&studio(), cx), None);
            remember(
                studio(),
                LastKnownFacts {
                    observed_at: 20,
                    ..LastKnownFacts::default()
                },
                cx,
            );
            assert_eq!(
                facts(&studio(), cx),
                None,
                "a removed server's late observation stays forgotten"
            );
        });
    }

    #[gpui::test]
    fn a_late_snapshot_never_undoes_a_change_observed_at_the_same_time(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut worker = Delayed::install(cx);
        let observed = LastKnownFacts {
            observed_at: 10,
            machine: Some("Studio".to_owned()),
            ..LastKnownFacts::default()
        };
        // A Git observation keeps the time the server's other facts were observed.
        let with_location = LastKnownFacts {
            worktree_location: Some("~/.worktrees/{repository}/{branch}".to_owned()),
            ..observed.clone()
        };
        cx.update(|cx| {
            reconcile(vec![ServerKey::Local, studio()], cx);
            remember(studio(), observed, cx);
        });
        let earlier = worker.retain();
        cx.update(|cx| remember(studio(), with_location.clone(), cx));

        arrive(earlier, cx);
        assert_eq!(
            cx.update(|cx| facts(&studio(), cx)).as_ref(),
            Some(&with_location)
        );
        let latest = worker.retain();
        arrive(latest, cx);

        assert_eq!(
            cx.update(|cx| facts(&studio(), cx)),
            Some(with_location),
            "the latest snapshot presents what the window asked for"
        );
    }

    #[gpui::test]
    fn facts_observed_before_the_saved_connections_load_are_kept(cx: &mut gpui::TestAppContext) {
        let mut worker = Delayed::install(cx);
        let observed = LastKnownFacts {
            observed_at: 10,
            machine: Some("Studio".to_owned()),
            ..LastKnownFacts::default()
        };
        cx.update(|cx| remember(studio(), observed.clone(), cx));

        let snapshots = worker.retain();
        arrive(snapshots, cx);
        assert_eq!(
            cx.update(|cx| facts(&studio(), cx)).as_ref(),
            Some(&observed)
        );
        cx.update(|cx| reconcile(vec![ServerKey::Local, studio()], cx));
        let snapshots = worker.retain();
        arrive(snapshots, cx);

        assert_eq!(cx.update(|cx| facts(&studio(), cx)), Some(observed));
    }

    #[gpui::test]
    fn removing_the_selected_server_presents_this_computer(cx: &mut gpui::TestAppContext) {
        let studio = ServerKey::Direct([7; 32]);
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
        let studio = ServerKey::Direct([7; 32]);
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
