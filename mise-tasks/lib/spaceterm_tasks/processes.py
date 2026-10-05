"""Own a child's whole process group so an interrupted task leaves no descendants running."""

import os
import signal
import subprocess
import time
from contextlib import contextmanager

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


def group_alive(pgid: int) -> bool:
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        # A member that changed credentials still exists.
        return True
    return True


def stop_group(process: subprocess.Popen, timeout: float = 5) -> None:
    """Terminate a child started with OWN_GROUP and every descendant, then reap the child.

    The leader can exit before its descendants, so completion waits for the whole group.
    """
    if os.name != "posix":
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                process.kill()
        process.wait()
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        process.wait()
        return
    deadline = time.monotonic() + timeout
    # Reaping the leader lets the group disappear once its descendants have exited.
    while process.poll() is None or group_alive(process.pid):
        if time.monotonic() >= deadline:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            break
        time.sleep(0.05)
    process.wait()
