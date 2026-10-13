//! Pause the real native PTY allocation before portable-pty applies CLOEXEC.

#![cfg(target_os = "macos")]

use std::{
    cell::RefCell,
    ffi::OsString,
    io::{self, BufRead, BufReader, Read, Write},
    mem::MaybeUninit,
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, OnceLock, mpsc},
    time::Duration,
};

use spaceterm_host::{
    process::spawn,
    pty::{
        NativePtyAdapterFactory, NativePtyExit, NativePtySize, UnixNativePtyAdapterFactory,
        macos_pty_host::MacosPtyHost,
    },
    shell_launch::{
        PreparedRemoteShellCommand, PreparedShellLaunch, RemoteShellLaunchParts, ShellLaunchFailure,
    },
};

const FIXTURE: &str = "SPACETERM_PTY_INHERITANCE_FIXTURE";
const CHILD_FACT: &str = "SPACETERM_PTY_SLAVE_INHERITED=";
const WAIT: Duration = Duration::from_secs(5);

type OpenPty = unsafe extern "C" fn(
    *mut libc::c_int,
    *mut libc::c_int,
    *mut libc::c_char,
    *mut libc::termios,
    *mut libc::winsize,
) -> libc::c_int;

struct AllocationPause {
    allocated: mpsc::Sender<String>,
    released: mpsc::Receiver<()>,
}

thread_local! {
    static ALLOCATION: RefCell<Option<AllocationPause>> = const { RefCell::new(None) };
}

// Interpose only this test executable's native boundary, not a production test hook.
// portable-pty still owns allocation, CLOEXEC setup, the master, and the slave.
#[unsafe(export_name = "openpty")]
unsafe extern "C" fn paused_openpty(
    master: *mut libc::c_int,
    slave: *mut libc::c_int,
    name: *mut libc::c_char,
    attributes: *mut libc::termios,
    size: *mut libc::winsize,
) -> libc::c_int {
    static NATIVE: OnceLock<OpenPty> = OnceLock::new();
    let native = NATIVE.get_or_init(|| {
        // SAFETY: RTLD_NEXT selects libSystem's operation rather than this executable's symbol.
        let address = unsafe { libc::dlsym(libc::RTLD_NEXT, c"openpty".as_ptr()) };
        assert!(!address.is_null(), "native PTY allocation is unavailable");
        // SAFETY: libSystem exports openpty with the native signature above.
        unsafe { std::mem::transmute::<*mut libc::c_void, OpenPty>(address) }
    });
    // SAFETY: the caller supplies exactly the native openpty pointers and storage.
    let result = unsafe { native(master, slave, name, attributes, size) };
    if result == 0 {
        ALLOCATION.with(|pause| {
            if let Some(pause) = pause.borrow_mut().take() {
                // SAFETY: native openpty just initialized slave with a live descriptor.
                let fd = unsafe { *slave };
                assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, 0);
                let mut status = MaybeUninit::<libc::stat>::uninit();
                assert_eq!(unsafe { libc::fstat(fd, status.as_mut_ptr()) }, 0);
                let status = unsafe { status.assume_init() };
                pause
                    .allocated
                    .send(format!("{fd},{},{}", status.st_dev as u64, status.st_ino))
                    .unwrap();
                pause.released.recv_timeout(WAIT).unwrap();
            }
        });
    }
    result
}

#[test]
fn desktop_pty_allocation_excludes_unrelated_child_inheritance() {
    if child_fixture() {
        return;
    }
    assert_allocation_excludes_inheritance(
        Arc::new(UnixNativePtyAdapterFactory::new(Arc::new(MacosPtyHost))),
        "desktop_pty_allocation_excludes_unrelated_child_inheritance",
    );
}

#[test]
fn server_pty_allocation_excludes_unrelated_child_inheritance() {
    if child_fixture() {
        return;
    }
    assert_allocation_excludes_inheritance(
        Arc::new(UnixNativePtyAdapterFactory::new(Arc::new(MacosPtyHost))),
        "server_pty_allocation_excludes_unrelated_child_inheritance",
    );
}

fn child_fixture() -> bool {
    let Ok(facts) = std::env::var(FIXTURE) else {
        return false;
    };
    let facts = facts
        .split(',')
        .map(|value| value.parse::<u64>().unwrap())
        .collect::<Vec<_>>();
    let mut status = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat inspects the numeric descriptor without taking ownership.
    let inherited = unsafe { libc::fstat(facts[0] as i32, status.as_mut_ptr()) } == 0 && {
        let status = unsafe { status.assume_init() };
        (status.st_dev as u64, status.st_ino) == (facts[1], facts[2])
    };
    println!("{CHILD_FACT}{inherited}");
    io::stdout().flush().unwrap();
    // Keep any inherited slave open until the parent has checked terminal EOF.
    io::stdin().read_exact(&mut [0]).unwrap();
    true
}

fn assert_allocation_excludes_inheritance(
    factory: Arc<dyn NativePtyAdapterFactory>,
    test_name: &'static str,
) {
    let (allocated, allocation) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (created, creation) = mpsc::channel();
    let allocator = std::thread::spawn(move || {
        ALLOCATION.with(|pause| {
            *pause.borrow_mut() = Some(AllocationPause {
                allocated,
                released,
            });
        });
        created
            .send(factory.create(
                launch(),
                NativePtySize {
                    rows: 31,
                    columns: 97,
                    ..NativePtySize::default()
                },
            ))
            .unwrap();
    });
    let facts = allocation.recv_timeout(WAIT).unwrap();
    let (starting, started) = mpsc::channel();
    let (spawned, spawning) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", test_name, "--nocapture"])
            .env(FIXTURE, facts)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // SAFETY: force the fork path used by desktop SSH, with no unrelated-fd cleanup.
        unsafe { command.pre_exec(|| Ok(())) };
        starting.send(()).unwrap();
        spawned.send(spawn(&mut command)).unwrap();
    });
    started.recv_timeout(WAIT).unwrap();
    let early = spawning.recv_timeout(Duration::from_millis(100));
    let waited = matches!(&early, Err(mpsc::RecvTimeoutError::Timeout));
    release.send(()).unwrap();
    let mut child = match early {
        Ok(child) => child.unwrap(),
        Err(mpsc::RecvTimeoutError::Timeout) => spawning.recv_timeout(WAIT).unwrap().unwrap(),
        Err(error) => panic!("spawn worker disconnected: {error}"),
    };
    worker.join().unwrap();
    let mut parts = creation.recv_timeout(WAIT).unwrap().unwrap();
    allocator.join().unwrap();

    let stdout = child.stdout.take().unwrap();
    let (observed, observation) = mpsc::channel();
    let observer = std::thread::spawn(move || {
        let mut lines = BufReader::new(stdout).lines();
        let fact = lines
            .by_ref()
            .map(Result::unwrap)
            .find_map(|line| line.strip_prefix(CHILD_FACT).map(str::to_owned))
            .unwrap();
        observed.send(fact).unwrap();
        for line in lines {
            line.unwrap();
        }
    });
    let inherited = observation.recv_timeout(WAIT).unwrap();
    let mut reader = parts.adapter.take_reader().unwrap();
    let (ended, ending) = mpsc::channel();
    let reading = std::thread::spawn(move || {
        ended.send(io::copy(&mut reader, &mut io::sink())).unwrap();
    });
    parts.adapter.write_all(b"done\n").unwrap();
    assert_eq!(
        parts.adapter.wait_for_exit(WAIT).unwrap(),
        NativePtyExit::Success
    );
    let eof = ending.recv_timeout(Duration::from_millis(250));
    let alive = child.try_wait().unwrap().is_none();
    // Clean up both children before reporting the regression, even on the red run.
    child.stdin.take().unwrap().write_all(&[0]).unwrap();
    let status = child.wait().unwrap();
    observer.join().unwrap();
    reading.join().unwrap();
    if eof.is_err() {
        ending.recv_timeout(WAIT).unwrap().unwrap();
    }
    assert!(status.success());
    assert!(
        alive,
        "the unrelated child must outlive the terminal EOF check"
    );
    assert_eq!(
        inherited, "false",
        "the unrelated child retained the PTY slave"
    );
    assert!(
        eof.is_ok_and(|result| result.is_ok()),
        "the unrelated child delayed terminal EOF"
    );
    assert!(waited, "spawn must wait for complete PTY CLOEXEC setup");
}

struct Launch;
impl PreparedRemoteShellCommand for Launch {
    fn into_remote_shell_parts(self) -> Result<RemoteShellLaunchParts, ShellLaunchFailure> {
        Ok(RemoteShellLaunchParts {
            executable: "/bin/sh".into(),
            arguments: ["-c", "read line"].map(OsString::from).into(),
            working_directory: "/tmp".into(),
            environment: vec![],
            terminal_name: "xterm-256color",
        })
    }
}

fn launch() -> PreparedShellLaunch {
    PreparedShellLaunch::remote(Path::new("/tmp"), Launch).unwrap()
}
