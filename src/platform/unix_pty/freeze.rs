//! Native ownership for the final idle-restart grant. It never signals an outside holder.
use super::*;

struct Resources {
    stopped: Vec<Arc<dyn ProcessControl>>,
    inspection: Option<u64>,
    serial: u64,
    thawed: bool,
}
struct Cleanup(Mutex<Resources>);
impl Cleanup {
    fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(Resources {
            stopped: Vec::new(),
            inspection: None,
            serial: 0,
            thawed: false,
        })))
    }
    fn thaw(&self, inspection: Option<u64>) {
        let stopped = {
            let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            if state.thawed || inspection.is_some_and(|id| state.inspection != Some(id)) {
                return;
            }
            state.thawed = true;
            state.inspection = None;
            std::mem::take(&mut state.stopped)
        };
        for process in stopped.into_iter().rev() {
            if let Err(error) = process.signal(libc::SIGCONT)
                && error.raw_os_error() != Some(libc::ESRCH)
            {
                eprintln!("failed to thaw a terminal process: {:?}", error.kind());
            }
        }
    }

    fn stop(&self, process: Arc<dyn ProcessControl>) -> io::Result<()> {
        if process.stopped()? {
            return Err(io::Error::other("a session process was already stopped"));
        }
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.thawed {
            return Err(io::ErrorKind::TimedOut.into());
        }
        // The watchdog cannot pass SIGSTOP between sending it and owning its exact thaw.
        process.signal(libc::SIGSTOP)?;
        state.stopped.push(process);
        Ok(())
    }

    fn stopped(&self) -> Vec<Arc<dyn ProcessControl>> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopped
            .clone()
    }
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        self.thaw(None);
    }
}

/// This owner never calls a native inspector. It can restore permissions and send exact CONT
/// while an inspector is blocked, and invalidates that attempt before any process resumes.
struct Inspection {
    cleanup: Arc<Cleanup>,
    id: u64,
    deadline: Instant,
    cancel: std::sync::mpsc::Sender<()>,
    finished: bool,
}
impl Inspection {
    fn start(cleanup: Arc<Cleanup>, deadline: Instant) -> io::Result<Self> {
        let id = {
            let mut state = cleanup.0.lock().unwrap_or_else(|e| e.into_inner());
            if state.thawed || state.inspection.is_some() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            state.serial = state
                .serial
                .checked_add(1)
                .ok_or_else(|| io::Error::other("inspection generation exhausted"))?;
            let id = state.serial;
            state.inspection = Some(id);
            id
        };
        let (cancel, cancelled) = std::sync::mpsc::channel();
        let watchdog = cleanup.clone();
        if let Err(error) = thread::Builder::new()
            .name("terminal-freeze-deadline".into())
            .spawn(move || {
                if cancelled
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .is_err()
                {
                    watchdog.thaw(Some(id));
                }
            })
        {
            cleanup.thaw(None);
            return Err(error);
        }
        Ok(Self {
            cleanup,
            id,
            deadline,
            cancel,
            finished: false,
        })
    }
    fn finish(mut self) -> io::Result<()> {
        let valid = {
            let mut state = self.cleanup.0.lock().unwrap_or_else(|e| e.into_inner());
            let valid = !state.thawed
                && state.inspection == Some(self.id)
                && Instant::now() < self.deadline;
            if valid {
                state.inspection = None;
            }
            valid
        };
        self.finished = true;
        let _ = self.cancel.send(());
        if valid {
            Ok(())
        } else {
            self.cleanup.thaw(None);
            Err(io::ErrorKind::TimedOut.into())
        }
    }
}
impl Drop for Inspection {
    fn drop(&mut self) {
        let _ = self.cancel.send(());
        if !self.finished {
            self.cleanup.thaw(None);
        }
    }
}
fn bounded<T: Send + 'static>(
    cleanup: Arc<Cleanup>,
    deadline: Instant,
    inspect: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    let inspection = Inspection::start(cleanup, deadline)?;
    let (sent, received) = std::sync::mpsc::channel();
    thread::Builder::new()
        .name("terminal-freeze-inspection".into())
        .spawn(move || {
            let _ = sent.send(inspect());
        })?;
    let result = received
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
    inspection.finish()?;
    Ok(result)
}

struct Facts {
    host: Arc<dyn UnixPtyHost>,
    session: ProcessSessionIdentity,
    master: std::sync::Weak<OwnedFd>,
    slave: std::path::PathBuf,
    observed: Vec<ProcessObservation>,
}
pub(super) struct Population {
    facts: Arc<Facts>,
    cleanup: Arc<Cleanup>,
}
impl spaceterm_core::idle_restart::Frozen for Population {
    fn verify(&self) -> Result<(), spaceterm_core::idle_restart::Restarting> {
        let facts = self.facts.clone();
        let cleanup = self.cleanup.clone();
        bounded(
            self.cleanup.clone(),
            Instant::now() + Duration::from_millis(250),
            move || facts.verify_native(&cleanup),
        )
        .map_err(|_| spaceterm_core::idle_restart::Restarting)
    }
}
impl Drop for Population {
    fn drop(&mut self) {
        self.cleanup.thaw(None);
    }
}
impl Population {
    pub(super) fn capture(
        host: Arc<dyn UnixPtyHost>,
        session: ProcessSessionIdentity,
        master: &Arc<OwnedFd>,
        slave: &std::path::Path,
        deadline: Instant,
    ) -> io::Result<Self> {
        let cleanup = Cleanup::new();
        let owned = cleanup.clone();
        let master = Arc::downgrade(master);
        let slave = slave.to_owned();
        let facts = bounded(cleanup.clone(), deadline, move || {
            let observed = members(host.as_ref(), session)?;
            let facts = Arc::new(Facts {
                host,
                session,
                master,
                slave,
                observed,
            });
            for member in &facts.observed {
                let process: Arc<dyn ProcessControl> =
                    Arc::from(facts.host.pin_process(member.identity)?);
                owned.stop(process.clone())?;
                while !process.stopped()? {
                    if Instant::now() >= deadline {
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    thread::sleep(Duration::from_millis(1));
                }
            }
            facts.verify_native(&owned)?;
            Ok(facts)
        })?;
        Ok(Self { facts, cleanup })
    }
    pub(super) fn controls(&self) -> Vec<std::sync::Weak<dyn ProcessControl>> {
        self.cleanup.stopped().iter().map(Arc::downgrade).collect()
    }
}
impl Facts {
    fn verify_native(&self, cleanup: &Cleanup) -> io::Result<()> {
        let fresh = members(self.host.as_ref(), self.session)?;
        if self.observed != fresh || fresh.len() != 1 || fresh[0].identity != self.session.leader {
            return Err(io::Error::other(
                "the terminal session has work or changed membership",
            ));
        }
        let stopped = cleanup.stopped();
        if stopped.len() != fresh.len() || stopped.iter().any(|p| !matches!(p.stopped(), Ok(true)))
        {
            return Err(io::Error::other(
                "a frozen process resumed or became unreadable",
            ));
        }

        let holders = self.host.slave_holders(&self.slave)?;
        if !holders.contains(&self.session.leader) {
            return Err(io::Error::other(
                "the shell's terminal descriptor could not be established",
            ));
        }
        for holder in holders {
            if !stopped.iter().any(|p| p.identity() == holder) {
                return Err(io::Error::other("an outside process holds the terminal"));
            }
        }
        let master = self
            .master
            .upgrade()
            .ok_or_else(|| io::Error::other("the terminal is gone"))?;
        if self.host.foreground(master.as_raw_fd())? != self.session.leader.process
            || self.host.pending_output(master.as_raw_fd())?
        {
            return Err(io::Error::other(
                "the terminal has foreground work or pending output",
            ));
        }
        if members(self.host.as_ref(), self.session)? != fresh
            || stopped.iter().any(|p| !matches!(p.stopped(), Ok(true)))
        {
            return Err(io::Error::other("the frozen population changed"));
        }
        // Include output produced by a privileged outside holder during the other final reads.
        if self.host.pending_output(master.as_raw_fd())? {
            return Err(io::Error::other("the terminal has pending output"));
        }
        Ok(())
    }
}

fn members(
    host: &dyn UnixPtyHost,
    session: ProcessSessionIdentity,
) -> io::Result<Vec<ProcessObservation>> {
    let mut members = Vec::new();
    for pid in host.session_candidates(session.session)? {
        if let Some(observation) = host.observe_exactly(pid)?
            && observation.session == session.session
        {
            members.push(observation);
        }
    }
    members.sort_by_key(|member| member.identity.process);
    if !members
        .iter()
        .any(|member| member.identity == session.leader)
    {
        return Err(io::Error::other("the session leader is gone or changed"));
    }
    Ok(members)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_core::idle_restart::{Activity, Frozen, Lease, Restarting, Session};
    use std::collections::BTreeSet;
    use std::sync::atomic::AtomicUsize;
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Fault {
        None,
        DelayedAck,
        Stall,
        Members(usize),
        Observation,
        Pin(i32),
        Stop(i32),
        AlreadyStopped(i32),
        StopRead(usize),
        Timeout,
        Joined,
        Job,
        Inventory,
        EmptyInventory,
        Descriptor,
        Outside,
        Foreground,
        OtherForeground,
        Pending,
        Output,
        Reused,
    }
    struct Data {
        fault: Mutex<Fault>,
        scans: AtomicUsize,
        reads: AtomicUsize,
        stopped: Mutex<BTreeSet<i32>>,
        signals: Mutex<Vec<(i32, i32)>>,
        stall: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    }
    #[derive(Clone)]
    struct Facts(Arc<Data>);
    impl Facts {
        fn new(fault: Fault) -> Self {
            Self(Arc::new(Data {
                fault: Mutex::new(fault),
                stall: Mutex::new(None),
                scans: AtomicUsize::new(0),
                reads: AtomicUsize::new(0),
                stopped: Mutex::new(BTreeSet::new()),
                signals: Mutex::new(Vec::new()),
            }))
        }
        fn fault(&self) -> Fault {
            *self.0.fault.lock().unwrap()
        }
        fn observation(&self, pid: i32) -> ProcessObservation {
            ProcessObservation {
                identity: ProcessIdentity {
                    process: pid,
                    start: ProcessStart {
                        coarse: if self.fault() == Fault::Reused { 2 } else { 1 },
                        fine: 0,
                    },
                },
                process_group: pid,
                session: if pid == 200 { 200 } else { 100 },
            }
        }
    }
    impl UnixPtyHost for Facts {
        fn observe_process(&self, pid: i32) -> Option<ProcessObservation> {
            Some(self.observation(pid))
        }
        fn observe_exactly(&self, pid: i32) -> io::Result<Option<ProcessObservation>> {
            if self.fault() == Fault::Observation {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(Some(self.observation(pid)))
        }
        fn process_ids(&self) -> io::Result<Vec<i32>> {
            Ok(vec![100])
        }
        fn session_candidates(&self, _: i32) -> io::Result<Vec<i32>> {
            let scan = self.0.scans.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fault() == Fault::Members(scan) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(
                if matches!(
                    self.fault(),
                    Fault::Job | Fault::Pin(101) | Fault::Stop(101) | Fault::AlreadyStopped(101)
                ) || (self.fault() == Fault::Joined && scan > 1)
                {
                    vec![100, 101]
                } else {
                    vec![100]
                },
            )
        }
        fn complete_process_ids(&self) -> io::Result<Vec<i32>> {
            if self.fault() == Fault::Inventory {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            if self.fault() == Fault::EmptyInventory {
                return Ok(Vec::new());
            }
            Ok(if self.fault() == Fault::Outside {
                vec![100, 200]
            } else {
                vec![100]
            })
        }
        fn holds_slave(&self, _: i32, _: &std::path::Path) -> io::Result<bool> {
            if let Some((entered, resume)) = self.0.stall.lock().unwrap().take() {
                entered.send(()).unwrap();
                resume.recv().unwrap();
            }
            if self.fault() == Fault::Descriptor {
                Err(io::ErrorKind::PermissionDenied.into())
            } else {
                Ok(true)
            }
        }
        fn foreground(&self, _: RawFd) -> io::Result<i32> {
            match self.fault() {
                Fault::Foreground => Err(io::ErrorKind::PermissionDenied.into()),
                Fault::OtherForeground => Ok(101),
                _ => Ok(100),
            }
        }
        fn pending_output(&self, _: RawFd) -> io::Result<bool> {
            if self.fault() == Fault::Pending {
                Err(io::ErrorKind::PermissionDenied.into())
            } else {
                Ok(self.fault() == Fault::Output)
            }
        }
        fn pin_process(&self, identity: ProcessIdentity) -> io::Result<Box<dyn ProcessControl>> {
            if self.fault() == Fault::Pin(identity.process) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(Box::new(Pinned {
                facts: self.clone(),
                identity,
            }))
        }
        fn master_read_error(&self, _: &io::Error) -> MasterReadError {
            MasterReadError::Failure
        }
    }
    struct Pinned {
        facts: Facts,
        identity: ProcessIdentity,
    }
    impl ProcessControl for Pinned {
        fn identity(&self) -> ProcessIdentity {
            self.identity
        }
        fn stopped(&self) -> io::Result<bool> {
            let read = self.facts.0.reads.fetch_add(1, Ordering::SeqCst) + 1;
            if self.facts.fault() == Fault::DelayedAck && read == 2 {
                return Ok(false);
            }
            if self.facts.fault() == Fault::StopRead(read) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            if self.facts.fault() == Fault::AlreadyStopped(self.identity.process) {
                return Ok(true);
            }
            if self.facts.fault() == Fault::Timeout {
                return Ok(false);
            }
            Ok(self
                .facts
                .0
                .stopped
                .lock()
                .unwrap()
                .contains(&self.identity.process))
        }
        fn signal(&self, signal: i32) -> io::Result<()> {
            if self.facts.fault() == Fault::Reused {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            if signal == libc::SIGSTOP && self.facts.fault() == Fault::Stop(self.identity.process) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            self.facts
                .0
                .signals
                .lock()
                .unwrap()
                .push((self.identity.process, signal));
            let mut stopped = self.facts.0.stopped.lock().unwrap();
            if signal == libc::SIGSTOP {
                stopped.insert(self.identity.process);
            } else {
                stopped.remove(&self.identity.process);
            }
            Ok(())
        }
    }
    fn master() -> Arc<OwnedFd> {
        let (socket, _) = std::os::unix::net::UnixStream::pair().unwrap();
        Arc::new(socket.into())
    }
    fn session(facts: &Facts) -> ProcessSessionIdentity {
        ProcessSessionIdentity {
            session: 100,
            leader: facts.observation(100).identity,
        }
    }
    fn capture(facts: &Facts, master: &Arc<OwnedFd>) -> io::Result<Population> {
        let deadline = if facts.fault() == Fault::Timeout {
            Instant::now() + Duration::from_millis(80)
        } else {
            Instant::now() + Duration::from_secs(2)
        };
        Population::capture(
            Arc::new(facts.clone()),
            session(facts),
            master,
            std::path::Path::new("/dev/test-terminal"),
            deadline,
        )
    }
    #[test]
    fn every_native_refusal_thaws_only_the_processes_this_attempt_stopped() {
        for (fault, expected) in [
            (Fault::Members(1), vec![]),
            (Fault::Observation, vec![]),
            (Fault::Pin(100), vec![]),
            (Fault::Pin(101), vec![100]),
            (Fault::Stop(100), vec![]),
            (Fault::Stop(101), vec![100]),
            (Fault::AlreadyStopped(100), vec![]),
            (Fault::AlreadyStopped(101), vec![100]),
            (Fault::StopRead(1), vec![]),
            (Fault::StopRead(2), vec![100]),
            (Fault::StopRead(4), vec![100]),
            (Fault::Timeout, vec![100]),
            (Fault::Members(2), vec![100]),
            (Fault::Members(3), vec![100]),
            (Fault::Joined, vec![100]),
            (Fault::Job, vec![100, 101]),
            (Fault::Inventory, vec![100]),
            (Fault::EmptyInventory, vec![100]),
            (Fault::Descriptor, vec![100]),
            (Fault::Outside, vec![100]),
            (Fault::Foreground, vec![100]),
            (Fault::OtherForeground, vec![100]),
            (Fault::Pending, vec![100]),
            (Fault::Output, vec![100]),
        ] {
            let facts = Facts::new(fault);
            assert!(capture(&facts, &master()).is_err(), "{fault:?}");
            let signals = facts.0.signals.lock().unwrap();
            let stopped = signals
                .iter()
                .filter(|(_, signal)| *signal == libc::SIGSTOP)
                .map(|(pid, _)| *pid)
                .collect::<Vec<_>>();
            let mut thawed = signals
                .iter()
                .filter(|(_, signal)| *signal == libc::SIGCONT)
                .map(|(pid, _)| *pid)
                .collect::<Vec<_>>();
            thawed.sort_unstable();
            assert_eq!(
                stopped, expected,
                "{fault:?}: stopped exactly the owned prefix"
            );
            assert_eq!(
                thawed, expected,
                "{fault:?}: every stopped process is thawed once"
            );
        }
    }
    struct NativeSession {
        activity: Activity,
        facts: Facts,
        master: Arc<OwnedFd>,
    }
    impl Session for NativeSession {
        fn activity(&self) -> Activity {
            self.activity.clone()
        }
        fn working(&self, _: Instant) -> bool {
            false
        }
        fn freeze(&self, _: Instant) -> Result<Box<dyn Frozen>, Restarting> {
            capture(&self.facts, &self.master)
                .map(|guard| Box::new(guard) as Box<dyn Frozen>)
                .map_err(|_| Restarting)
        }
    }
    #[test]
    fn a_resumed_process_new_member_or_output_before_stop_revokes_and_thaws_the_same_grant() {
        for after in [Fault::None, Fault::Joined, Fault::Output, Fault::Descriptor] {
            let facts = Facts::new(Fault::None);
            let start = Instant::now();
            let activity = Activity::new(start);
            let sessions: Vec<Arc<dyn Session>> = vec![Arc::new(NativeSession {
                activity: activity.clone(),
                facts: facts.clone(),
                master: master(),
            })];
            let lease = Lease::scan(
                &sessions,
                start + Duration::from_secs(7200),
                Duration::from_secs(3600),
            )
            .unwrap();
            lease.freeze_and_grant().unwrap();
            assert!(activity.input(Instant::now()).is_err());
            if after == Fault::None {
                facts.0.stopped.lock().unwrap().remove(&100);
            } // outside SIGCONT
            *facts.0.fault.lock().unwrap() = after;
            assert_eq!(lease.verify_frozen(), Err(Restarting), "{after:?}");
            assert!(!lease.granted());
            assert!(activity.input(Instant::now()).is_ok());
            drop(lease);
            assert_eq!(
                *facts.0.signals.lock().unwrap(),
                vec![(100, libc::SIGSTOP), (100, libc::SIGCONT)]
            );
        }
    }
    #[test]
    fn an_unstopped_reused_pid_is_never_signalled_by_thaw() {
        let facts = Facts::new(Fault::None);
        let master = master();
        let frozen = capture(&facts, &master).unwrap();
        *facts.0.fault.lock().unwrap() = Fault::Reused;
        assert!(frozen.verify().is_err());
        drop(frozen);
        assert_eq!(*facts.0.signals.lock().unwrap(), vec![(100, libc::SIGSTOP)]);
    }
    #[test]
    fn a_successful_freeze_stays_owned_until_shutdown_or_release() {
        for fault in [Fault::None, Fault::DelayedAck] {
            let facts = Facts::new(fault);
            let master = master();
            let frozen = capture(&facts, &master).unwrap();
            frozen.verify().unwrap();
            assert_eq!(*facts.0.signals.lock().unwrap(), vec![(100, libc::SIGSTOP)]);
            drop(frozen);
            assert_eq!(
                *facts.0.signals.lock().unwrap(),
                vec![(100, libc::SIGSTOP), (100, libc::SIGCONT)]
            );
        }
    }
    #[test]
    fn a_stalled_inspection_thaws_without_waiting_for_the_inspection_to_return() {
        let fault = Fault::Stall;
        let facts = Facts::new(fault);
        let (entered, paused) = std::sync::mpsc::channel();
        let (resume, inspection) = std::sync::mpsc::channel();
        *facts.0.stall.lock().unwrap() = Some((entered, inspection));
        let host = facts.clone();
        let worker = thread::spawn(move || {
            Population::capture(
                Arc::new(host.clone()),
                session(&host),
                &master(),
                std::path::Path::new("/dev/test-terminal"),
                Instant::now() + Duration::from_millis(80),
            )
            .is_err()
        });
        paused.recv_timeout(Duration::from_secs(3)).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        let thawed = loop {
            if facts
                .0
                .signals
                .lock()
                .unwrap()
                .contains(&(100, libc::SIGCONT))
            {
                break true;
            }
            if Instant::now() >= until {
                break false;
            }
            thread::sleep(Duration::from_millis(2));
        };

        resume.send(()).unwrap();
        assert!(
            worker.join().unwrap(),
            "an expired inspection can never grant"
        );
        assert!(
            thawed,
            "cleanup must run while the native inspector is still stalled"
        );
        assert_eq!(
            *facts.0.signals.lock().unwrap(),
            vec![(100, libc::SIGSTOP), (100, libc::SIGCONT)]
        );
    }
}
