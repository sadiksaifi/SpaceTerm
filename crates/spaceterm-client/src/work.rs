//! Shared work links, admission state and pushed updates for one Machine.
use crate::admission::{Admissions, Refused};
use crate::connection::link::{CoreLink, LinkEvent, LinkSink};
use crate::{CoreError, protocol};
use std::sync::{Arc, Mutex, Weak};

/// The one live link that a handle and its clones share with their server, and the sinks that
/// receive its pushed inventory and health.
#[derive(Default)]
pub struct WorkConnection {
    link: Mutex<Option<CoreLink>>,
    subscribers: Mutex<Vec<Weak<dyn LinkSink>>>,
    /// The latest admission on each lane, which the handle's capabilities follow.
    admissions: Arc<Mutex<Admissions>>,
    /// The generation of the latest link connected, or 0 before the first.
    generation: std::sync::atomic::AtomicU64,
    /// The last facts a link received while the Core restarted, and when they arrived. They
    /// outlast that link until another link's facts arrive, or for [`RESTART_FACTS_LIMIT`].
    restart: Mutex<Option<RestartFacts>>,
}

/// The facts a link received while the Core restarted, when they arrived, and the detailed
/// facts of the same revision from a Core of work
/// [`crate::update::facts::MINOR_DETAILED_FACTS`] or later.
type RestartFacts = (
    std::time::Instant,
    Arc<crate::update::facts::UpdateFacts>,
    Option<Arc<crate::update::facts::DetailedUpdateFacts>>,
);

/// How long [`WorkConnection::update_facts`] keeps a restart's facts without a link that
/// reports newer ones. It matches how long a client follows a Core that replaces itself.
pub const RESTART_FACTS_LIMIT: std::time::Duration = std::time::Duration::from_secs(180);

/// Whether `facts` say the Core is restarting, so its links are about to end.
fn restarting(facts: &crate::update::facts::UpdateFacts) -> bool {
    use crate::update::facts::Operation;
    matches!(
        facts.operation,
        Some(Operation::Restarting { .. } | Operation::RestartingSameVersion)
    )
}

/// Numbers every link this process connects, across all handles. A paired route's reconnect
/// creates a new handle, so a count per handle would repeat a generation an earlier link had.
static LINK_GENERATIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Delivers a shared link's events to every live subscriber. It holds the shared state weakly, so
/// the link it belongs to never keeps that state alive.
struct Fanout(Weak<WorkConnection>);
impl LinkSink for Fanout {
    fn deliver(&self, event: LinkEvent) {
        let Some(shared) = self.0.upgrade() else {
            return;
        };
        let subscribers: Vec<_> = {
            let mut subscribers = shared.subscribers.lock().unwrap_or_else(|e| e.into_inner());
            subscribers.retain(|sink| sink.strong_count() > 0);
            subscribers.iter().filter_map(Weak::upgrade).collect()
        };
        match event {
            LinkEvent::Inventory(revision) => {
                for sink in subscribers {
                    sink.deliver(LinkEvent::Inventory(Arc::clone(&revision)));
                }
            }
            LinkEvent::Health(health) => {
                for sink in subscribers {
                    sink.deliver(LinkEvent::Health(health));
                }
            }
            LinkEvent::UpdateFacts(facts) => {
                // The link stores its detailed facts before it reports the facts in them.
                let detailed = || {
                    let link = shared.link.try_lock().ok()?;
                    link.as_ref()?
                        .detailed_update_facts()
                        .filter(|detailed| detailed.facts == *facts)
                };
                *shared.restart.lock().unwrap_or_else(|e| e.into_inner()) = restarting(&facts)
                    .then(|| (std::time::Instant::now(), Arc::clone(&facts), detailed()));
                for sink in subscribers {
                    sink.deliver(LinkEvent::UpdateFacts(Arc::clone(&facts)));
                }
            }
        }
    }
}
impl WorkConnection {
    pub fn record_work(&self, admitted: &Result<protocol::Negotiated, Refused>) {
        self.admissions().record(protocol::Lane::Work, admitted);
    }
    pub fn admissions(&self) -> std::sync::MutexGuard<'_, Admissions> {
        self.admissions.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// What this Machine's connections can do now, from the versions their latest admissions
    /// negotiated: the shared live work connection, and the latest service-lane admission of
    /// this handle and its clones.
    pub fn capabilities(&self) -> protocol::capability::Capabilities {
        let live = self.live_link().is_some();
        self.admissions().capabilities(live)
    }
    /// The versions the Core offered at the latest admission on either lane, also when it
    /// refused this client. None before the first Offer.
    pub fn core_versions(&self) -> Option<protocol::CoreVersions> {
        self.admissions().core().cloned()
    }
    /// The newest update facts the shared live connection received. While the Core restarts,
    /// its link ends and the next one has no facts yet, so the facts that said it restarts
    /// stay until another link's facts arrive, for at most [`RESTART_FACTS_LIMIT`]. Otherwise
    /// None without a live connection or before its first facts. [`LinkEvent::UpdateFacts`]
    /// reports each change.
    pub fn update_facts(&self) -> Option<Arc<crate::update::facts::UpdateFacts>> {
        self.live_link()
            .and_then(|link| link.update_facts())
            .or_else(|| {
                self.restart
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .filter(|(at, _, _)| at.elapsed() < RESTART_FACTS_LIMIT)
                    .map(|(_, facts, _)| Arc::clone(facts))
            })
    }
    /// The newest detailed update facts the shared live connection received, kept through a
    /// restart as [`Self::update_facts`] keeps the facts. `restart` in the facts that announced
    /// a restart says why it began. None from a Core older than work
    /// [`crate::update::facts::MINOR_DETAILED_FACTS`].
    pub fn detailed_update_facts(&self) -> Option<Arc<crate::update::facts::DetailedUpdateFacts>> {
        self.live_link()
            .and_then(|link| link.detailed_update_facts())
            .or_else(|| {
                self.restart
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .filter(|(at, _, _)| at.elapsed() < RESTART_FACTS_LIMIT)
                    .and_then(|(_, _, detailed)| detailed.clone())
            })
    }
    /// Reads the shared live connection without opening one or waiting for a pending dial.
    /// Paired presentation follows its route owner's connection and backoff through this read.
    pub fn live_link(&self) -> Option<CoreLink> {
        let saved = match self.link.try_lock() {
            Ok(saved) => saved,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        saved
            .as_ref()
            .filter(|link| link.health() == crate::connection::link::Health::Live)
            .cloned()
    }
    /// Closes the shared connection, including links retained by this handle's clones.
    /// The route owner must disable its connector before disconnecting to prevent reconnects.
    pub fn disconnect(&self) {
        let link = self.link.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(link) = link {
            link.close();
        }
    }
    pub fn link_with(
        self: &Arc<Self>,
        connect: impl FnOnce(Arc<dyn LinkSink>) -> Result<CoreLink, CoreError>,
        cancelled: Option<&dyn Fn() -> bool>,
        deadline: Option<std::time::Instant>,
    ) -> Result<CoreLink, CoreError> {
        let passed = || deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline);
        // Callers share a successful connection. Paired presentation reads live_link instead
        // of queuing behind a dial.
        let mut saved = loop {
            if cancelled.is_some_and(|cancelled| cancelled()) {
                return Err(CoreError::ServiceBusy);
            }
            match self.link.try_lock() {
                Ok(saved) => break saved,
                Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock)
                    if cancelled.is_none() && deadline.is_none() =>
                {
                    break self.link.lock().unwrap_or_else(|e| e.into_inner());
                }
                // The callback has no wake handle. A bounded wait lets a queued caller
                // cancel without waiting for another caller's network deadline.
                Err(std::sync::TryLockError::WouldBlock) => {
                    if passed() {
                        return Err(CoreError::ServiceBusy);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        };
        if cancelled.is_some_and(|cancelled| cancelled()) {
            return Err(CoreError::ServiceBusy);
        }
        if let Some(link) = saved.as_ref()
            && link.health() == crate::connection::link::Health::Live
        {
            return Ok(link.clone());
        }
        *saved = None;
        // A dial that starts after the deadline could only outlive it.
        if passed() {
            return Err(CoreError::ServiceBusy);
        }
        let link = connect(Arc::new(Fanout(Arc::downgrade(self))))?;
        *saved = Some(link.clone());
        let generation = LINK_GENERATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        self.generation
            .store(generation, std::sync::atomic::Ordering::Release);
        Ok(link)
    }
    /// Makes `link` this handle's shared connection while it stays live, admitted at this build's
    /// work version by a Core of this build running `0.0.0`. Tests use it to serve
    /// a handle in process, without a socket.
    #[cfg(feature = "test-support")]
    pub fn adopt(&self, link: CoreLink) {
        let version = crate::update::facts::Version::parse("0.0.0")
            .unwrap_or_else(|_| unreachable!("valid version literal"));
        self.record_work(&Ok(protocol::Negotiated {
            selected: protocol::version::WORK,
            core: protocol::CoreVersions::current(version),
        }));
        link.select_work(protocol::version::WORK);
        *self.link.lock().unwrap_or_else(|e| e.into_inner()) = Some(link);
    }
    /// Identifies the shared link: it changes each time [`Self::link_with`] connects a new one, and no
    /// other handle's link in this process has the same generation. Facts read over an older link
    /// may describe a server that has since restarted or updated.
    pub fn link_generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Acquire)
    }
    /// Delivers the shared link's pushed inventory and health to `sink` while it lives, across
    /// reconnects.
    pub fn subscribe(&self, sink: Weak<dyn LinkSink>) {
        self.subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(sink);
    }
    /// Shares the admission state with the service lane.
    pub fn admitted_lanes(&self) -> Arc<Mutex<Admissions>> {
        Arc::clone(&self.admissions)
    }

    /// Starts the work I/O thread and sets its negotiated work version.
    pub fn start(
        &self,
        carrier: Box<dyn crate::connection::carrier::Carrier>,
        events: Arc<dyn LinkSink>,
    ) -> Result<CoreLink, CoreError> {
        let link =
            crate::connection::io::spawn_link(carrier, events).map_err(|_| CoreError::Protocol)?;
        if let Ok(work) = self.admissions().capabilities(true).work() {
            link.select_work(work);
        }
        Ok(link)
    }
}
