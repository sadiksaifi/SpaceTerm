"""Run a real older server and a current client without exposing account state."""

import argparse
import os
import pwd
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from spaceterm_tasks import TaskError, main


def run() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("installation", type=Path)
    parser.add_argument("commit")
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("requires Linux bubblewrap user/mount/PID namespaces")
    if len(args.commit) != 40 or any(c not in "0123456789abcdef" for c in args.commit):
        parser.error("commit must be an exact lowercase Git SHA")
    installation = args.installation.resolve(strict=True)
    if not (installation / "bin/spaceterm-server").is_file():
        parser.error("installation must contain bin/spaceterm-server and its resources")
    target = Path(os.environ.get("CARGO_TARGET_DIR", "target")).resolve()
    client = target / "debug/examples/server_compatibility"
    if not client.is_file():
        parser.error("run mise run server:compatibility:build first")
    bwrap = shutil.which("bwrap")
    if not bwrap or os.geteuid() == 0:
        parser.error("requires bubblewrap and a non-root account")
    account = pwd.getpwuid(os.geteuid())
    with tempfile.TemporaryDirectory(prefix="spaceterm-compatibility-") as temporary:
        scope = Path(temporary)
        (scope / "disposable").write_text("spaceterm-compatibility-v1\n")
        # Replace the account record itself: HOME/XDG overrides do not isolate the server.
        (scope / "passwd").write_text(
            f"root:x:0:0:root:/root:/bin/sh\n"
            f"{account.pw_name}:x:{account.pw_uid}:{account.pw_gid}:compatibility:"
            "/home/spaceterm-compat:/bin/sh\n"
        )
        command = [
            bwrap,
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--uid",
            str(account.pw_uid),
            "--gid",
            str(account.pw_gid),
            "--cap-drop",
            "ALL",
            "--ro-bind",
            "/",
            "/",
            "--tmpfs",
            "/home",
            "--tmpfs",
            "/tmp",
            "--tmpfs",
            "/run",
            "--tmpfs",
            "/root",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--dir",
            "/home/spaceterm-compat",
            "--dir",
            "/tmp/compat",
            "--ro-bind",
            str(scope / "passwd"),
            "/etc/passwd",
            "--ro-bind",
            str(scope / "disposable"),
            "/tmp/compat/disposable",
            "--ro-bind",
            str(installation),
            "/tmp/compat/installation",
            "--ro-bind",
            str(client),
            "/tmp/compat/client",
            "--clearenv",
            "--setenv",
            "HOME",
            "/home/spaceterm-compat",
            "--setenv",
            "PATH",
            "/usr/bin:/bin",
            "--setenv",
            "SHELL",
            "/bin/sh",
            "--setenv",
            "SPACETERM_COMPATIBILITY_COMMIT",
            args.commit,
            "--chdir",
            "/home/spaceterm-compat",
            "--",
            "/tmp/compat/client",
        ]
        try:
            completed = subprocess.run(command, check=False, timeout=120)
        except subprocess.TimeoutExpired:
            raise TaskError("compatibility process deadline exceeded") from None
        if completed.returncode:
            raise TaskError(f"compatibility process failed (exit {completed.returncode})")


if __name__ == "__main__":
    main(run)
