"""Own a child's whole process group so an interrupted task leaves no descendants running."""

import ctypes
import errno
import os
import select
import signal
import subprocess
import sys
import time
from contextlib import closing, contextmanager
from pathlib import Path

from spaceterm_tasks import TaskError

# Popen start_new_session value that puts a child and its descendants in their own group.
OWN_GROUP = os.name == "posix"
# Signals that end a task; SIGINT already raises KeyboardInterrupt.
TERMINATING_SIGNALS = tuple(
    getattr(signal, name) for name in ("SIGHUP", "SIGTERM") if hasattr(signal, name)
)


def _exit(signum, _frame):
    raise SystemExit(128 + signum)


@contextmanager
def terminating_signals_raise():
    """Turn terminating signals into SystemExit so cleanup handlers run."""
    previous = {signum: signal.getsignal(signum) for signum in TERMINATING_SIGNALS}
    for signum in TERMINATING_SIGNALS:
        signal.signal(signum, _exit)
    try:
        yield
    finally:
        for signum, handler in previous.items():
            signal.signal(signum, handler)


def child_exited(process: subprocess.Popen) -> bool:
    """Observe exit without releasing the PID reserved by the unreaped child."""
    if process.returncode is not None:
        return True
    if hasattr(os, "WNOWAIT"):
        return os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT) is not None
    if sys.platform == "darwin":
        with closing(select.kqueue()) as queue:
            event = select.kevent(
                process.pid,
                filter=select.KQ_FILTER_PROC,
                flags=select.KQ_EV_ADD | select.KQ_EV_ONESHOT,
                fflags=select.KQ_NOTE_EXIT,
            )
            try:
                return bool(queue.control([event], 1, 0))
            except ProcessLookupError:
                # The caller still owns the wait status, so ESRCH means its child exited.
                return True
    raise TaskError("child-ownership-unavailable")


def _darwin_members(pgid):
    # Reuse the libproc group listing used by the bounded native-tool runner.
    native = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    native.proc_listpids.argtypes = (ctypes.c_uint, ctypes.c_uint, ctypes.c_void_p, ctypes.c_int)
    native.proc_listpids.restype = ctypes.c_int
    native.proc_pidinfo.argtypes = (
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_uint64,
        ctypes.c_void_p,
        ctypes.c_int,
    )
    native.proc_pidinfo.restype = ctypes.c_int
    members = (ctypes.c_int * 257)()
    size = native.proc_listpids(2, pgid, members, ctypes.sizeof(members))
    width = ctypes.sizeof(ctypes.c_int)
    if size < 0 or size >= ctypes.sizeof(members) or size % width:
        raise TaskError("tool-cleanup-unresolved")
    observed = []
    for pid in members[: size // width]:
        info = ctypes.create_string_buffer(136)  # Darwin proc_bsdinfo.
        ctypes.set_errno(0)
        count = native.proc_pidinfo(pid, 3, 0, info, len(info))
        if count == 0 and ctypes.get_errno() == errno.ESRCH:
            observed.append((pid, None))
            continue
        if count != len(info):
            raise TaskError("tool-cleanup-unresolved")
        # pbi_status follows pbi_flags; SZOMB cannot run or receive a signal.
        observed.append((pid, int.from_bytes(info.raw[4:8], sys.byteorder)))
    return observed


def darwin_zombie_group(pid):
    """Prove the native-tool runner's group contains only its reserved zombie."""
    if sys.platform != "darwin":
        return False
    try:
        return _darwin_members(pid) == [(pid, None)]
    except TaskError:
        # False makes the native-tool runner classify cleanup as unresolved.
        return False


def group_alive(pgid: int, *, exclude: int | None = None) -> bool:
    """Report live members, not zombies that Darwin cannot signal."""
    if sys.platform == "darwin":
        return any(
            pid != exclude and status not in (None, 5) for pid, status in _darwin_members(pgid)
        )
    if sys.platform.startswith("linux"):
        # The accessibility runner already reads this kernel identity record.
        # Only state and group ID are needed; no names or environment are inspected.
        for entry in Path("/proc").iterdir():
            if not entry.name.isdigit() or int(entry.name) == exclude:
                continue
            try:
                fields = (entry / "stat").read_text().rsplit(")", 1)[1].split()
            except (FileNotFoundError, ProcessLookupError):
                continue
            except (OSError, IndexError):
                raise TaskError("tool-cleanup-unresolved") from None
            try:
                if int(fields[2]) == pgid and fields[0] != "Z":
                    return True
            except (IndexError, ValueError):
                raise TaskError("tool-cleanup-unresolved") from None
        return False
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        # A member that changed credentials still exists.
        return True
    return True


def _signal_group(pgid, signum):
    try:
        os.killpg(pgid, signum)
    except ProcessLookupError:
        return
    except PermissionError:
        # A live member may exit between observation and signal. Prove that no
        # live member remains instead of ignoring a failed cleanup operation.
        if sys.platform != "darwin" or group_alive(pgid):
            raise TaskError("tool-cleanup-unresolved") from None


def stop_group(process: subprocess.Popen, timeout: float = 5) -> None:
    """Retire all group signals before consuming the leader's wait status."""
    if os.name != "posix":
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                process.kill()
        process.wait()
        return
    if process.returncode is not None:
        # A consuming caller has already released the leader's PID. Do not send
        # a signal to a possibly reused group, even if cleanup is unresolved.
        if group_alive(process.pid):
            raise TaskError("tool-cleanup-unresolved")
        return
    deadline = time.monotonic() + timeout
    if group_alive(process.pid):
        _signal_group(process.pid, signal.SIGTERM)
    while group_alive(process.pid):
        if time.monotonic() >= deadline:
            _signal_group(process.pid, signal.SIGKILL)
            break
        time.sleep(0.05)
    # Keep the leader reserved until the final signal's live recipients retire.
    deadline = time.monotonic() + 2
    while group_alive(process.pid):
        if time.monotonic() >= deadline:
            raise TaskError("tool-cleanup-unresolved")
        time.sleep(0.01)
    process.wait(timeout=max(0.001, deadline - time.monotonic()))
