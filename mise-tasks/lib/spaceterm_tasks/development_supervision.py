"""Own one source Development run without retained PIDs or service mutation."""

import fcntl
import os
import selectors
import signal
import socket
import stat
import subprocess
import tempfile
import time
from contextlib import contextmanager
from pathlib import Path

from spaceterm_tasks import TaskError, developer_environment
from spaceterm_tasks.processes import OWN_GROUP, stop_group, terminating_signals_raise

READY = b"spaceterm-development-ready-v1\n"


def source_environment(environment):
    result = dict(environment)
    for name in (
        "SPACETERM_SERVER",
        "SPACETERM_AS",
        "SPACETERM_IDENTITY",
        "SPACETERM_SERVER_ID",
        "SPACETERM_PANE",
        "SPACETERM_SESSION",
        "SPACETERM_CLI",
        "SPACETERM_PACKAGED",
        "SPACETERM_RELEASE_TAG",
        "SPACETERM_CLI_ARCHIVE",
        "SPACETERM_CLI_SIGNATURE",
        "SPACETERM_DEVELOPMENT_FOREGROUND",
        "SPACETERM_DEVELOPMENT_OWNER_SOCKET",
    ):
        result.pop(name, None)
    return result


@contextmanager
def development_run():
    """Refuse a second source command before either command can replace staged executables."""
    path = Path("/tmp") / f"spaceterm-development-owner-{os.geteuid()}.lock"
    descriptor = os.open(path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        metadata = os.fstat(descriptor)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_uid != os.geteuid()
            or stat.S_IMODE(metadata.st_mode) != 0o600
            or metadata.st_nlink != 1
        ):
            raise TaskError("Development ownership storage is unsafe")
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise TaskError("another source Development run owns this account") from None
        with terminating_signals_raise():
            yield
    finally:
        # Keep the inode: unlinking an advisory lock would allow two concurrent owners.
        os.close(descriptor)


def _ready(server):
    assert server.stdout is not None
    with selectors.DefaultSelector() as selector:
        selector.register(server.stdout, selectors.EVENT_READ)
        deadline = time.monotonic() + 10
        received = b""
        while received != READY:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not selector.select(timeout=max(0, remaining)):
                raise TaskError("the foreground Development Server did not become ready")
            chunk = os.read(server.stdout.fileno(), len(READY) + 1 - len(received))
            received += chunk
            if not chunk or not READY.startswith(received):
                raise TaskError(
                    "the foreground Development Server could not acquire ownership or start"
                )


@contextmanager
def _cleanup_signals():
    signals = (signal.SIGINT, signal.SIGHUP, signal.SIGTERM)
    previous = {signum: signal.getsignal(signum) for signum in signals}
    try:
        for signum in signals:
            signal.signal(signum, signal.SIG_IGN)
        yield
    finally:
        for signum, handler in previous.items():
            signal.signal(signum, handler)


def _finish(server):
    assert server.stdin is not None
    server.stdin.close()
    try:
        # Admitted Git writes have no deadline. The Server reconciles them before ending
        # Terminal Sessions, so only its completed exit proves cleanup has finished.
        server.wait()
    finally:
        assert server.stdout is not None
        server.stdout.close()
    # The server performs PTY cleanup before reporting successful exit.
    if server.returncode:
        raise TaskError("Development Server cleanup could not be confirmed")


def supervise(server_command, client_command, environment):
    """Launch the client only after this exact child Server owns its endpoint.

    The Server owns PTY shutdown and retains organization. Its stdin is an owner pipe;
    supervisor death closes it. The client uses a private connection instead because
    LaunchServices, rather than this task, must launch the macOS responsible process.
    """
    environment = developer_environment(source_environment(environment))
    with (
        terminating_signals_raise(),
        tempfile.TemporaryDirectory(prefix="st-dev-", dir="/tmp") as root,
    ):
        with socket.socket(socket.AF_UNIX) as listener:
            endpoint = str(Path(root) / "owner")
            listener.bind(endpoint)
            listener.listen(1)
            listener.settimeout(0.1)
            client_environment = {
                **environment,
                "SPACETERM_DEVELOPMENT_FOREGROUND": "1",
                "SPACETERM_DEVELOPMENT_OWNER_SOCKET": endpoint,
            }
            server = subprocess.Popen(
                server_command,
                env=environment,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                start_new_session=OWN_GROUP,
            )
            client = None
            connection = None
            try:
                _ready(server)
                client = subprocess.Popen(
                    client_command, env=client_environment, start_new_session=OWN_GROUP
                )
                deadline = time.monotonic() + 30
                while connection is None:
                    if server.poll() is not None:
                        raise TaskError("the foreground Development Server exited")
                    if client.poll() is not None:
                        return client.returncode
                    if time.monotonic() >= deadline:
                        raise TaskError("the Development client did not establish ownership")
                    try:
                        connection, _ = listener.accept()
                    except TimeoutError:
                        pass
                connection.settimeout(0.1)
                while True:
                    if server.poll() is not None:
                        raise TaskError("the foreground Development Server exited")
                    if client.poll() is not None:
                        return client.returncode
                    try:
                        if not connection.recv(1):
                            return 0
                        raise TaskError("the Development owner connection was invalid")
                    except TimeoutError:
                        pass
            finally:
                with _cleanup_signals():
                    try:
                        if connection is not None:
                            connection.close()
                        if client is not None:
                            # EOF reaches a LaunchServices client outside our process group.
                            try:
                                client.wait(timeout=5)
                            except subprocess.TimeoutExpired:
                                pass
                            stop_group(client)
                    finally:
                        # Readiness may already be published even if _ready was interrupted.
                        # Close only this child's owner pipe and let its shutdown finish.
                        _finish(server)
