use super::*;
use std::io::Cursor;
use std::sync::{Condvar, Mutex, mpsc};

use crate::domain::SshDestination;
use crate::ssh::command::{
    OpenSshExecutable, SETUP_READY_PREAMBLE, SshCommandContext, ValidatedRemoteShellCommand,
};

#[derive(Default)]
struct State {
    blocked: bool,
    released: bool,
    killed: bool,
    finished: bool,
    reaped: usize,
}

#[derive(Clone)]
struct Adapter {
    state: Arc<(Mutex<State>, Condvar)>,
    before_ready: bool,
}

struct RemoteOutput {
    adapter: Adapter,
    bytes: Cursor<Vec<u8>>,
}
impl Read for RemoteOutput {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let (state, changed) = &*self.adapter.state;
        let mut state = state.lock().unwrap();
        if self.adapter.before_ready || self.bytes.position() as usize >= SETUP_READY_PREAMBLE.len()
        {
            state.blocked = true;
            changed.notify_all();
            while !state.released && !state.killed {
                state = changed.wait(state).unwrap();
            }
        }
        if state.killed {
            return Ok(0);
        }
        let read = self.bytes.read(output)?;
        if self.bytes.position() as usize == self.bytes.get_ref().len() {
            state.finished = true;
        }
        Ok(read)
    }
}

impl SshProcessAdapter for Adapter {
    type Process = ();
    fn spawn(
        &self,
        _: SshProcessSpawnRequest,
    ) -> Result<SpawnedSshProcess<()>, SshProcessMechanismError> {
        Ok(SpawnedSshProcess::new(
            (),
            SshProcessPipes::new(
                Some(Box::new(std::io::sink())),
                Some(Box::new(RemoteOutput {
                    adapter: self.clone(),
                    bytes: Cursor::new([SETUP_READY_PREAMBLE, b"ready"].concat()),
                })),
                None,
            ),
        ))
    }
    fn try_status(&self, _: &mut ()) -> Result<Option<ProcessExit>, SshProcessMechanismError> {
        Ok(self
            .state
            .0
            .lock()
            .unwrap()
            .finished
            .then_some(ProcessExit::successful()))
    }
    fn signal(&self, _: &mut (), _: ProcessSignal) -> Result<(), SshProcessMechanismError> {
        self.state.0.lock().unwrap().killed = true;
        self.state.1.notify_all();
        Ok(())
    }
    fn reap(&self, _: ()) -> Result<(), SshProcessMechanismError> {
        self.state.0.lock().unwrap().reaped += 1;
        Ok(())
    }
}

fn start(
    before_ready: bool,
) -> (
    Adapter,
    CancellationToken,
    mpsc::Receiver<Result<CapturedProcessOutput, CapturedProcessError>>,
    JoinHandle<()>,
) {
    let adapter = Adapter {
        state: Arc::default(),
        before_ready,
    };
    let cancellation = CancellationToken::default();
    let runner = adapter.clone();
    let token = cancellation.clone();
    let (completed, result) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let command = SshCommandContext::setup(
            OpenSshExecutable::new("/fixture/ssh".into()).unwrap(),
            Path::new("/fixture/config"),
            &SshDestination::new("configured-server".into()).unwrap(),
            ValidatedRemoteShellCommand::server_script("exec observed-command").unwrap(),
        )
        .unwrap();
        let environment =
            SshProcessEnvironment::new_without_authentication("/fixture/home".into(), None)
                .unwrap();
        let outcome = run_captured_process(
            &runner,
            &command,
            &environment,
            vec![],
            64,
            &token,
            Instant::now() + Duration::from_millis(200),
        );
        completed.send(outcome).unwrap();
    });
    let state = adapter.state.0.lock().unwrap();
    let (state, timeout) = adapter
        .state
        .1
        .wait_timeout_while(state, Duration::from_secs(2), |state| !state.blocked)
        .unwrap();
    assert!(
        !timeout.timed_out() && state.blocked,
        "remote output did not reach its controlled boundary"
    );
    drop(state);
    (adapter, cancellation, result, worker)
}

#[test]
fn slave_observation_is_bounded_before_the_remote_ready_preamble() {
    let (adapter, _, result, worker) = start(true);
    assert!(matches!(
        result.recv_timeout(Duration::from_secs(2)).unwrap(),
        Err(CapturedProcessError::TimedOut)
    ));
    worker.join().unwrap();
    assert_eq!(adapter.state.0.lock().unwrap().reaped, 1);
}

#[test]
fn remote_observation_is_bounded_after_the_authenticated_command_starts() {
    let (adapter, _, result, worker) = start(false);
    assert!(matches!(
        result.recv_timeout(Duration::from_secs(2)).unwrap(),
        Err(CapturedProcessError::TimedOut)
    ));
    worker.join().unwrap();
    assert_eq!(adapter.state.0.lock().unwrap().reaped, 1);
}

#[test]
fn cancellation_while_the_slave_waits_for_ready_reaps_the_owned_process() {
    let (adapter, cancellation, result, worker) = start(true);
    cancellation.cancel();
    assert!(matches!(
        result.recv_timeout(Duration::from_secs(2)).unwrap(),
        Err(CapturedProcessError::Cancelled)
    ));
    worker.join().unwrap();
    assert_eq!(adapter.state.0.lock().unwrap().reaped, 1);
}

#[test]
fn setup_master_retains_one_authentication_until_its_owner_closes_it() {
    let adapter = Adapter {
        state: Arc::default(),
        before_ready: false,
    };
    let command = SshCommandContext::setup_master(
        OpenSshExecutable::new("/fixture/ssh".into()).unwrap(),
        Path::new("/fixture/config"),
        Path::new("/fixture/m"),
        &SshDestination::new("configured-server".into()).unwrap(),
    )
    .unwrap();
    let environment =
        SshProcessEnvironment::new_without_authentication("/fixture/home".into(), None).unwrap();
    let mut master = SetupMaster::start(
        &adapter,
        &command,
        &environment,
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(adapter.state.0.lock().unwrap().reaped, 0);
    master.close();
    drop(master);
    assert_eq!(adapter.state.0.lock().unwrap().reaped, 1);
}

#[test]
fn cancelling_setup_master_authentication_reaps_before_returning() {
    let adapter = Adapter {
        state: Arc::default(),
        before_ready: true,
    };
    let cancellation = CancellationToken::default();
    let runner = adapter.clone();
    let token = cancellation.clone();
    let worker = std::thread::spawn(move || {
        let command = SshCommandContext::setup_master(
            OpenSshExecutable::new("/fixture/ssh".into()).unwrap(),
            Path::new("/fixture/config"),
            Path::new("/fixture/m"),
            &SshDestination::new("configured-server".into()).unwrap(),
        )
        .unwrap();
        let environment =
            SshProcessEnvironment::new_without_authentication("/fixture/home".into(), None)
                .unwrap();
        SetupMaster::start(&runner, &command, &environment, &token).map(|_| ())
    });
    let state = adapter.state.0.lock().unwrap();
    let (state, timeout) = adapter
        .state
        .1
        .wait_timeout_while(state, Duration::from_secs(2), |state| !state.blocked)
        .unwrap();
    assert!(!timeout.timed_out() && state.blocked);
    drop(state);
    cancellation.cancel();
    assert!(matches!(
        worker.join().unwrap(),
        Err(CapturedProcessError::Cancelled)
    ));
    assert_eq!(adapter.state.0.lock().unwrap().reaped, 1);
}

#[test]
fn setup_master_authentication_waits_independently_of_slave_observation_deadlines() {
    let adapter = Adapter {
        state: Arc::default(),
        before_ready: true,
    };
    let runner = adapter.clone();
    let (completed, outcome) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let command = SshCommandContext::setup_master(
            OpenSshExecutable::new("/fixture/ssh".into()).unwrap(),
            Path::new("/fixture/config"),
            Path::new("/fixture/m"),
            &SshDestination::new("configured-server".into()).unwrap(),
        )
        .unwrap();
        let environment =
            SshProcessEnvironment::new_without_authentication("/fixture/home".into(), None)
                .unwrap();
        let result = SetupMaster::start(
            &runner,
            &command,
            &environment,
            &CancellationToken::default(),
        )
        .map(|_| ());
        completed.send(result).unwrap();
    });
    let state = adapter.state.0.lock().unwrap();
    let (state, timeout) = adapter
        .state
        .1
        .wait_timeout_while(state, Duration::from_secs(2), |state| !state.blocked)
        .unwrap();
    assert!(!timeout.timed_out() && state.blocked);
    drop(state);
    let waiting = matches!(
        outcome.recv_timeout(Duration::from_millis(400)),
        Err(mpsc::RecvTimeoutError::Timeout)
    );
    adapter.state.0.lock().unwrap().released = true;
    adapter.state.1.notify_all();
    outcome
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    worker.join().unwrap();
    assert!(
        waiting,
        "master authentication inherited a slave observation deadline"
    );
    assert_eq!(adapter.state.0.lock().unwrap().reaped, 1);
}
