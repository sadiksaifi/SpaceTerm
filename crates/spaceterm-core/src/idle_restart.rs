//! Receipt-time activity and the revocable receipt gate for a Core restart.
use std::collections::BTreeSet;
use std::io;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

/// Work and service minor that add the Stop grant and the retained-input refusal.
pub const MINOR: u16 = 3;

/// Input was not admitted because Stop has been granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Restarting;

#[derive(Clone)]
pub struct Activity(Arc<Mutex<State>>);
struct State {
    generation: Option<u64>,
    last: Instant,
    pending: BTreeSet<u64>,
    output: Option<u64>,
    lease: Weak<LeaseState>,
    granted: bool,
}
impl Activity {
    pub fn new(now: Instant) -> Self {
        Self(Arc::new(Mutex::new(State {
            generation: Some(0),
            last: now,
            pending: BTreeSet::new(),
            output: None,
            lease: Weak::new(),
            granted: false,
        })))
    }
    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Records admission before input enters a worker queue. The receipt stays with that work.
    pub fn input(&self, now: Instant) -> Result<Receipt, Restarting> {
        let mut state = self.lock();
        if state.granted {
            return Err(Restarting);
        }
        let generation = state.record(now);
        Ok(self.receipt(generation))
    }
    /// The native adapter calls this around one nonblocking read into a nonempty buffer,
    /// outside its readiness wait. EOF or a read failure makes the observation unreadable.
    pub fn read(&self, read: impl FnOnce() -> io::Result<usize>) -> io::Result<usize> {
        let mut state = self.lock();
        if state.granted {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let result = read();
        if matches!(result, Ok(n) if n > 0) {
            // One reader publishes each read before reading again.
            if state.output.is_some() {
                state.generation = None;
            }
            state.output = state.record(Instant::now());
        } else if !matches!(&result, Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted))
        {
            state.generation = None;
            state.last = Instant::now();
            state.revoke();
        }
        result
    }
    /// Transfers the native read's receipt to the output queue.
    pub fn output_receipt(&self) -> Option<Receipt> {
        let generation = self.lock().output.take()?;
        Some(self.receipt(Some(generation)))
    }
    fn receipt(&self, generation: Option<u64>) -> Receipt {
        Receipt {
            _completion: Arc::new(Completion {
                activity: Arc::downgrade(&self.0),
                generation,
            }),
        }
    }
    pub fn quiet(&self, now: Instant, period: Duration) -> bool {
        let state = self.lock();
        state.generation.is_some()
            && state.pending.is_empty()
            && now.saturating_duration_since(state.last) >= period
    }
    /// Store already-read native facts only before an idle lease owns this activity. The
    /// callback must only record its observation, without native I/O or activity locks.
    pub fn record_idle_observation(&self, record: impl FnOnce()) -> Result<(), Restarting> {
        let state = self.lock();
        if state.generation.is_none()
            || !state.pending.is_empty()
            || state.granted
            || state.lease.upgrade().is_some()
        {
            return Err(Restarting);
        }
        record();
        drop(state);
        Ok(())
    }
    pub fn generation(&self) -> Option<u64> {
        self.lock().generation
    }
}
impl State {
    fn record(&mut self, now: Instant) -> Option<u64> {
        self.last = self.last.max(now);
        self.generation = self.generation.and_then(|n| n.checked_add(1));
        if let Some(n) = self.generation {
            self.pending.insert(n);
        }
        self.revoke();
        self.generation
    }
    fn revoke(&self) {
        if let Some(lease) = self.lease.upgrade() {
            let _ = lease
                .phase
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
        }
    }
}
/// Completion shared by commands generated from one receipt. Dropping the last one settles it.
#[derive(Clone)]
pub struct Receipt {
    _completion: Arc<Completion>,
}
struct Completion {
    activity: Weak<Mutex<State>>,
    generation: Option<u64>,
}
impl Drop for Completion {
    fn drop(&mut self) {
        if let Some(activity) = self.activity.upgrade() {
            let mut state = activity.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(n) = self.generation {
                state.pending.remove(&n);
            }
            state.last = state.last.max(Instant::now());
        }
    }
}
/// The runtime supplies fresh prompt, foreground and process observations.
pub trait Session: Send + Sync {
    fn activity(&self) -> Activity;
    fn working(&self, now: Instant) -> bool;
    /// Freeze native execution, then verify prompt, foreground, membership, slave holders
    /// and pending output. Dropping the returned guard thaws exactly this attempt's processes.
    /// Receipt locks are held here; this operation must not acquire them again.
    fn freeze(&self, _now: Instant) -> Result<Box<dyn Frozen>, Restarting> {
        Err(Restarting)
    }
}
pub trait Frozen: Send + Sync {
    /// Recheck the same native population immediately before Stop, without thawing it.
    fn verify(&self) -> Result<(), Restarting> {
        Err(Restarting)
    }
}
struct LeaseState {
    phase: AtomicU8,
}
/// The receipt gate is bound to the exact activities inspected by one scan. Its owner also
/// binds it to the operation, installation and Core boot under the panes lock. It cannot
/// establish native idleness or authorize Stop without the native freeze and final recheck.
pub struct Lease {
    state: Arc<LeaseState>,
    sessions: Vec<(Activity, u64)>,
    native: Vec<Arc<dyn Session>>,
    frozen: Mutex<Vec<Box<dyn Frozen>>>,
}
impl Lease {
    pub fn scan(sessions: &[Arc<dyn Session>], now: Instant, period: Duration) -> Option<Self> {
        let observed: Option<Vec<_>> = sessions
            .iter()
            .map(|s| {
                let activity = s.activity();
                let generation = activity.generation()?;
                Some((activity, generation))
            })
            .collect();
        let observed = observed?;
        let working = sessions
            .iter()
            .map(|session| session.working(now))
            .collect::<Vec<_>>();
        if working.into_iter().any(|working| working) {
            return None;
        }
        let mut lease = Self::admit(observed, now, period)?;
        lease.native = sessions.to_vec();
        Some(lease)
    }
    fn admit(sessions: Vec<(Activity, u64)>, now: Instant, period: Duration) -> Option<Self> {
        for (index, (activity, _)) in sessions.iter().enumerate() {
            if sessions[..index]
                .iter()
                .any(|(other, _)| Arc::ptr_eq(&activity.0, &other.0))
            {
                return None;
            }
        }
        let mut guards = sessions.iter().map(|(a, _)| a.lock()).collect::<Vec<_>>();
        if guards
            .iter()
            .zip(&sessions)
            .any(|(state, (_, generation))| {
                state.generation != Some(*generation)
                    || !state.pending.is_empty()
                    || state.granted
                    || state.lease.upgrade().is_some()
                    || now.saturating_duration_since(state.last) < period
            })
        {
            return None;
        }
        let state = Arc::new(LeaseState {
            phase: AtomicU8::new(0),
        });
        for activity in &mut guards {
            activity.lease = Arc::downgrade(&state);
        }
        drop(guards);
        Some(Self {
            state,
            sessions,
            native: Vec::new(),
            frozen: Mutex::new(Vec::new()),
        })
    }
    pub fn freeze_and_grant(&self) -> Result<(), Restarting> {
        if self.granted() {
            return self.verify_frozen();
        }
        let mut guards = self
            .sessions
            .iter()
            .map(|(a, _)| a.lock())
            .collect::<Vec<_>>();
        if self.state.phase.load(Ordering::Acquire) != 0
            || guards
                .iter()
                .zip(&self.sessions)
                .any(|(state, (_, generation))| {
                    state.generation != Some(*generation) || !state.pending.is_empty()
                })
        {
            return Err(Restarting);
        }
        let mut frozen = Vec::new();
        for (index, session) in self.native.iter().enumerate() {
            match session.freeze(Instant::now()) {
                Ok(guard) => frozen.push(guard),
                Err(error) => {
                    let state = &mut guards[index];
                    state.last = Instant::now();
                    state.generation = state.generation.and_then(|n| n.checked_add(1));
                    state.revoke();
                    return Err(error);
                }
            }
        }
        self.commit(&mut guards)?;
        *self.frozen.lock().unwrap_or_else(|e| e.into_inner()) = frozen;
        Ok(())
    }
    #[cfg(test)]
    fn grant(&self) -> Result<(), Restarting> {
        let mut guards = self
            .sessions
            .iter()
            .map(|(a, _)| a.lock())
            .collect::<Vec<_>>();
        if guards
            .iter()
            .zip(&self.sessions)
            .any(|(state, (_, generation))| {
                state.generation != Some(*generation) || !state.pending.is_empty()
            })
        {
            return Err(Restarting);
        }
        self.commit(&mut guards)
    }
    pub fn verify_frozen(&self) -> Result<(), Restarting> {
        let mut guards = self
            .sessions
            .iter()
            .map(|(a, _)| a.lock())
            .collect::<Vec<_>>();
        let mut frozen = self.frozen.lock().unwrap_or_else(|e| e.into_inner());
        let valid = self.granted()
            && frozen.len() == self.native.len()
            && guards
                .iter()
                .zip(&self.sessions)
                .all(|(state, (_, generation))| {
                    state.granted
                        && state.generation == Some(*generation)
                        && state.pending.is_empty()
                })
            && frozen.iter().all(|native| native.verify().is_ok());
        if valid {
            return Ok(());
        }
        self.refuse_locked(&mut guards, &mut frozen);
        Err(Restarting)
    }
    pub(crate) fn refuse(&self) {
        let mut guards = self
            .sessions
            .iter()
            .map(|(a, _)| a.lock())
            .collect::<Vec<_>>();
        let mut frozen = self.frozen.lock().unwrap_or_else(|e| e.into_inner());
        self.refuse_locked(&mut guards, &mut frozen);
    }
    fn refuse_locked(
        &self,
        guards: &mut [MutexGuard<'_, State>],
        frozen: &mut Vec<Box<dyn Frozen>>,
    ) {
        self.state.phase.store(1, Ordering::Release);
        frozen.clear();
        for state in guards {
            state.granted = false;
            state.last = Instant::now();
            state.generation = state.generation.and_then(|n| n.checked_add(1));
        }
    }
    fn commit(&self, guards: &mut [MutexGuard<'_, State>]) -> Result<(), Restarting> {
        self.state
            .phase
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Restarting)?;
        for state in guards {
            // Grant consumes the revocable lease and leaves the input barrier until Stop
            // completes, or its owner proves that the helper can no longer Stop.
            state.lease = Weak::new();
            state.granted = true;
        }
        Ok(())
    }
    pub fn revoke(&self) {
        let _ = self
            .state
            .phase
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn granted(&self) -> bool {
        self.state.phase.load(Ordering::Acquire) == 2
    }
    /// Only after the helper is proven unable to Stop, or its activation has settled.
    pub fn release(self) {
        drop(self);
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        // Native execution resumes before input admission resumes. On a completed Stop these
        // exact process handles are already dead, so thaw cannot signal a reused PID.
        self.frozen
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        for (activity, _) in &self.sessions {
            let mut state = activity.lock();
            state.granted = false;
            state.lease = Weak::new();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct NativeSession {
        activity: Activity,
        index: usize,
        refuse: bool,
        events: Arc<Mutex<Vec<(usize, bool)>>>,
    }
    struct Stopped {
        index: usize,
        events: Arc<Mutex<Vec<(usize, bool)>>>,
    }
    impl Frozen for Stopped {
        fn verify(&self) -> Result<(), Restarting> {
            Ok(())
        }
    }
    impl Drop for Stopped {
        fn drop(&mut self) {
            self.events.lock().unwrap().push((self.index, false));
        }
    }
    impl Session for NativeSession {
        fn activity(&self) -> Activity {
            self.activity.clone()
        }
        fn working(&self, _: Instant) -> bool {
            false
        }
        fn freeze(&self, _: Instant) -> Result<Box<dyn Frozen>, Restarting> {
            self.events.lock().unwrap().push((self.index, true));
            let stopped = Stopped {
                index: self.index,
                events: self.events.clone(),
            };
            if self.refuse {
                return Err(Restarting);
            }
            Ok(Box::new(stopped))
        }
    }
    #[test]
    fn native_freezes_are_owned_until_release_or_shutdown_and_every_refusal_thaws() {
        for refuse in [Some(0), Some(1), None] {
            for explicit_release in [false, true] {
                let start = Instant::now();
                let events = Arc::new(Mutex::new(Vec::new()));
                let activities = [Activity::new(start), Activity::new(start)];
                let sessions: Vec<Arc<dyn Session>> = activities
                    .iter()
                    .enumerate()
                    .map(|(index, activity)| {
                        Arc::new(NativeSession {
                            activity: activity.clone(),
                            index,
                            refuse: refuse == Some(index),
                            events: events.clone(),
                        }) as Arc<dyn Session>
                    })
                    .collect();
                let lease = Lease::scan(
                    &sessions,
                    start + Duration::from_secs(7200),
                    Duration::from_secs(3600),
                )
                .unwrap();
                assert_eq!(
                    lease.freeze_and_grant(),
                    if refuse.is_some() {
                        Err(Restarting)
                    } else {
                        Ok(())
                    }
                );
                if let Some(index) = refuse {
                    assert_ne!(
                        activities[index].generation(),
                        Some(0),
                        "a refused native observation records work"
                    );
                    assert!(activities[0].input(Instant::now()).is_ok());
                } else {
                    assert_eq!(*events.lock().unwrap(), vec![(0, true), (1, true)]);
                    assert!(activities[0].input(Instant::now()).is_err());
                }
                if explicit_release {
                    lease.release();
                } else {
                    drop(lease);
                }
                let recorded = events.lock().unwrap();
                for index in 0..=refuse.unwrap_or(1) {
                    assert_eq!(
                        recorded
                            .iter()
                            .filter(|entry| **entry == (index, true))
                            .count(),
                        1
                    );
                    assert_eq!(
                        recorded
                            .iter()
                            .filter(|entry| **entry == (index, false))
                            .count(),
                        1
                    );
                }
                assert!(activities[0].input(Instant::now()).is_ok());
            }
        }
    }
    struct Shell(Activity);
    impl Session for Shell {
        fn activity(&self) -> Activity {
            self.0.clone()
        }
        fn working(&self, _: Instant) -> bool {
            false
        }
    }
    #[test]
    fn a_granted_stop_refuses_input_until_the_stop_is_proven_cancelled() {
        let start = Instant::now();
        let activity = Activity::new(start);
        let shell: Arc<dyn Session> = Arc::new(Shell(activity.clone()));
        let lease = Lease::scan(
            &[shell],
            start + Duration::from_secs(3600),
            Duration::from_secs(3600),
        )
        .expect("an idle shell");
        lease.grant().unwrap();
        assert!(activity.input(start).is_err());
        lease.release();
        assert!(activity.input(start).is_ok());
    }
}

#[cfg(test)]
mod scan_tests {
    use super::*;
    use std::sync::Barrier;
    struct Shell {
        activity: Activity,
        pause: Option<Arc<Barrier>>,
    }
    impl Session for Shell {
        fn activity(&self) -> Activity {
            self.activity.clone()
        }
        fn working(&self, _: Instant) -> bool {
            if let Some(pause) = &self.pause {
                pause.wait();
                pause.wait();
            }
            false
        }
    }
    const HOUR: Duration = Duration::from_secs(3600);
    #[test]
    fn a_late_work_refresh_cannot_replace_the_observation_owned_by_a_lease() {
        let start = Instant::now();
        let activity = Activity::new(start);
        let value = std::sync::atomic::AtomicUsize::new(0);
        activity
            .record_idle_observation(|| {
                value.store(1, Ordering::SeqCst);
            })
            .unwrap();
        let sessions: Vec<Arc<dyn Session>> = vec![Arc::new(Shell {
            activity: activity.clone(),
            pause: None,
        })];
        let lease = Lease::scan(&sessions, start + HOUR * 2, HOUR).unwrap();
        assert_eq!(
            activity.record_idle_observation(|| {
                value.store(2, Ordering::SeqCst);
            }),
            Err(Restarting)
        );
        assert_eq!(value.load(Ordering::SeqCst), 1);
        lease.grant().unwrap();
        assert_eq!(
            activity.record_idle_observation(|| {
                value.store(2, Ordering::SeqCst);
            }),
            Err(Restarting)
        );
        assert_eq!(value.load(Ordering::SeqCst), 1);
        lease.release();
        activity
            .record_idle_observation(|| {
                value.store(3, Ordering::SeqCst);
            })
            .unwrap();
        assert_eq!(value.load(Ordering::SeqCst), 3);
    }
    #[test]
    fn activity_in_either_session_during_either_probe_refuses_the_scan() {
        for paused in 0..2 {
            for changed in 0..2 {
                for output in [false, true] {
                    let start = Instant::now();
                    let activities = [Activity::new(start), Activity::new(start)];
                    let pause = Arc::new(Barrier::new(2));
                    let sessions = activities
                        .iter()
                        .enumerate()
                        .map(|(index, activity)| {
                            Arc::new(Shell {
                                activity: activity.clone(),
                                pause: (index == paused).then(|| pause.clone()),
                            }) as Arc<dyn Session>
                        })
                        .collect::<Vec<_>>();
                    let scan =
                        std::thread::spawn(move || Lease::scan(&sessions, start + HOUR * 2, HOUR));
                    pause.wait();
                    if output {
                        activities[changed].read(|| Ok(1)).unwrap();
                        drop(activities[changed].output_receipt());
                    } else {
                        drop(activities[changed].input(Instant::now()).unwrap());
                    }
                    pause.wait();
                    assert!(
                        scan.join().unwrap().is_none(),
                        "paused={paused}, changed={changed}, output={output}"
                    );
                }
            }
        }
    }
    #[test]
    fn activity_between_the_lease_and_grant_is_delivered_and_revokes_stop() {
        for output in [false, true] {
            let start = Instant::now();
            let activity = Activity::new(start);
            let sessions: Vec<Arc<dyn Session>> = vec![Arc::new(Shell {
                activity: activity.clone(),
                pause: None,
            })];
            let lease = Lease::scan(&sessions, start + HOUR, HOUR).unwrap();
            if output {
                activity.read(|| Ok(3)).unwrap();
                drop(activity.output_receipt());
            } else {
                drop(activity.input(start).unwrap());
            }
            assert_eq!(lease.grant(), Err(Restarting));
            assert!(activity.input(start).is_ok());
        }
    }
    #[test]
    fn a_native_read_cannot_cross_the_final_grant_unnoticed() {
        let start = Instant::now();
        let activity = Activity::new(start);
        let sessions: Vec<Arc<dyn Session>> = vec![Arc::new(Shell {
            activity: activity.clone(),
            pause: None,
        })];
        let lease = Lease::scan(&sessions, start + HOUR, HOUR).unwrap();
        let pause = Arc::new(Barrier::new(2));
        let reader_pause = Arc::clone(&pause);
        let reader = std::thread::spawn(move || {
            activity.read(|| {
                reader_pause.wait();
                reader_pause.wait();
                Ok(1)
            })
        });
        pause.wait();
        let grant = std::thread::spawn(move || lease.grant());
        pause.wait();
        reader.join().unwrap().unwrap();
        assert_eq!(grant.join().unwrap(), Err(Restarting));
    }
    #[test]
    fn native_read_failures_are_work_and_revoke_an_ungranted_lease() {
        for failure in [
            None,
            Some(io::ErrorKind::PermissionDenied),
            Some(io::ErrorKind::BrokenPipe),
        ] {
            let start = Instant::now();
            let activity = Activity::new(start);
            let sessions: Vec<Arc<dyn Session>> = vec![Arc::new(Shell {
                activity: activity.clone(),
                pause: None,
            })];
            let lease = Lease::scan(&sessions, start + HOUR, HOUR).unwrap();
            let _ = activity.read(|| match failure {
                None => Ok(0),
                Some(kind) => Err(kind.into()),
            });
            assert_eq!(lease.grant(), Err(Restarting));
            assert!(!activity.quiet(start + HOUR * 24, HOUR));
        }
    }

    #[test]
    fn queued_work_is_never_idle_even_when_its_receipt_is_an_hour_old() {
        let start = Instant::now();
        let activity = Activity::new(start);
        let pending = activity.input(start).unwrap();
        let sessions: Vec<Arc<dyn Session>> = vec![Arc::new(Shell {
            activity: activity.clone(),
            pause: None,
        })];
        assert!(Lease::scan(&sessions, start + HOUR * 2, HOUR).is_none());
        drop(pending);
        assert!(Lease::scan(&sessions, start + HOUR * 2, HOUR).is_some());
    }
    #[test]
    fn a_granted_receipt_gate_never_reads_output_or_errors_past_commitment() {
        let start = Instant::now();
        let activity = Activity::new(start);
        let sessions: Vec<Arc<dyn Session>> = vec![Arc::new(Shell {
            activity: activity.clone(),
            pause: None,
        })];
        let lease = Lease::scan(&sessions, start + HOUR, HOUR).unwrap();
        lease.grant().unwrap();
        let result =
            activity.read(|| panic!("a native read crossed the granted input/receipt gate"));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
        lease.release();
        assert_eq!(activity.read(|| Ok(1)).unwrap(), 1);
    }
}
