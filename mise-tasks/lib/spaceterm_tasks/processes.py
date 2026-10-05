"""Own a child's whole process group so an interrupted task leaves no descendants running."""

import os
import signal
import subprocess
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


def stop_group(process: subprocess.Popen, timeout: float = 5) -> None:
    """Terminate a child started with OWN_GROUP and its descendants, then reap it."""
    if process.poll() is not None:
        return
    try:
        if os.name == "posix":
            os.killpg(process.pid, signal.SIGTERM)
        else:
            process.terminate()
    except ProcessLookupError:
        process.wait()
        return
    try:
        process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        try:
            if os.name == "posix":
                os.killpg(process.pid, signal.SIGKILL)
            else:
                process.kill()
        except ProcessLookupError:
            pass
        process.wait()
