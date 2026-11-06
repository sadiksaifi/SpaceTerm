"""A served fixture release, fake external boundaries, and terminals for install.sh tests.

The network, sudo, loginctl and the SpaceTerm Core executable are the controlled boundaries.
The fake Core records each invocation, retains an installation beside earlier ones as
`internal retain` does, and previews then confirms `uninstall`. Everything else in the
installer runs the host's real tools.
"""

import fcntl
import hashlib
import io
import json
import os
import select
import subprocess
import sys
import tarfile
import termios
import time
from pathlib import Path

from spaceterm_tasks import ROOT
from spaceterm_tasks.release import INSTALLER_SOURCE, RELEASES

TRUST = json.loads((ROOT / "packaging/update-trust.json").read_text())
TEAM_ID = "ABCDE12345"
TOKEN = "c1-" + "ab" * 32

CURL = f"""#!{sys.executable}
import json, os, shutil, sys
arguments = sys.argv[1:]
url = next(argument for argument in arguments if argument.startswith("https://"))
with open(os.environ["REQUESTS"], "a") as log:
    log.write(url + "\\n")
source = json.loads(os.environ["SERVED"]).get(url)
if source is None:
    sys.exit(22)
shutil.copyfile(source, arguments[arguments.index("--output") + 1])
"""

# The fake SpaceTerm Core. Like the CLI, it selects the Machine from --machine, then
# SPACETERM_MACHINE, then local, and refuses its local-only commands for any other Machine.
# `internal retain` copies the unpacked installation into installations/<sha256 of the
# archive> and links the launcher only when none exists, so an existing installation and its
# launcher stay as they were. The log records each command without its --machine flag.
CORE = r"""#!/bin/sh
machine="${SPACETERM_MACHINE:-local}"
if [ "${1:-}" = --machine ]; then
    machine="$2"
    shift 2
fi
if [ "$machine" != local ]; then
    echo "error: local_only" >&2
    exit 1
fi
printf '%s\n' "$*" >>"$CORE_LOG"
case "$1:${2:-}" in
    internal:retain)
        digest="$(shasum -a 256 "$3/artifact.tar.gz" 2>/dev/null || sha256sum "$3/artifact.tar.gz")"
        destination="$HOME/.local/share/spaceterm/core/installations/${digest%% *}"
        mkdir -p "$destination"
        cp -R "$3/." "$destination/"
        launcher="$HOME/.local/bin/spaceterm"
        if [ ! -e "$launcher" ] && [ ! -L "$launcher" ]; then
            mkdir -p "$HOME/.local/bin"
            ln -s "$destination/$CORE_RELATIVE" "$launcher"
        fi
        echo "The CLI launcher is installed."
        ;;
    uninstall:--confirm)
        rm -f "$HOME/.local/bin/spaceterm"
        echo "SpaceTerm Core is uninstalled from this machine."
        ;;
    uninstall:)
        echo "Uninstall SpaceTerm Core from this machine? Every Terminal Session on it ends."
        echo "Confirm with: spaceterm uninstall --confirm TOKEN"
        exit 3
        ;;
    *) exit 64 ;;
esac
""".replace("TOKEN", TOKEN)


def render(directory, kind="release", team_id=TEAM_ID, current=None, backup=None):
    """Fill install.sh as the release pipeline does, with optional test update keys."""
    text = INSTALLER_SOURCE.read_text()
    text = text.replace("@RELEASE_KIND@", kind).replace("@TEAM_ID@", team_id)
    for name, key in (("current", current), ("backup", backup)):
        if key is not None:
            text = text.replace(TRUST[name], key)
    path = Path(directory) / "install.sh"
    path.write_text(text)
    return path


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def build_document(version, identity="spaceterm", floor="1.0.0"):
    return json.dumps(
        {
            "commit": "0" * 40,
            "floor": floor,
            "identity": identity,
            "protocol_major": 1,
            "schema": 1,
            "stapled": False,
            "source_sha256": None,
            "target": "fixture",
            "version": version,
        },
        sort_keys=True,
    )


def add(output, name, data=b"", mode=0o644, kind=tarfile.REGTYPE, link=""):
    member = tarfile.TarInfo(name)
    member.type = kind
    member.mode = mode
    member.linkname = link
    member.size = len(data) if kind == tarfile.REGTYPE else 0
    output.addfile(member, io.BytesIO(data) if kind == tarfile.REGTYPE else None)


def core_archive(path, version, relative="bin/spaceterm", extra=(), document=None, tree=None):
    """A Core archive: core/build.json, the fake executable at `relative`, and `extra` members,
    each a tuple of `add` arguments.

    `tree` is an already prepared core/ directory, such as a signed macOS bundle, to add as is.
    """
    with tarfile.open(path, "w:gz", format=tarfile.PAX_FORMAT) as output:
        add(output, "core", kind=tarfile.DIRTYPE, mode=0o755)
        add(output, "core/build.json", (document or build_document(version)).encode())
        if tree is not None:
            for item in sorted(Path(tree).rglob("*")):
                output.add(item, arcname=f"core/{item.relative_to(tree)}", recursive=False)
        else:
            parts = relative.split("/")
            for depth in range(1, len(parts)):
                add(output, "core/" + "/".join(parts[:depth]), kind=tarfile.DIRTYPE, mode=0o755)
            add(output, f"core/{relative}", CORE.encode(), mode=0o755)
        for member in extra:
            add(output, *member)
    return path


class Release:
    """Release assets served at their GitHub release URLs, with one SHA256SUMS per version."""

    def __init__(self, root):
        self.root = Path(root)
        self.served = {}
        self.assets = {}

    def publish(self, version, assets, latest=True, sums=None):
        """`assets` maps asset names to files. `sums` overrides the generated checksums."""
        self.assets[version] = dict(assets)
        for name, source in assets.items():
            self.served[f"{RELEASES}/download/v{version}/{name}"] = str(source)
        listing = self.root / f"SHA256SUMS-{version}"
        listing.write_text(
            sums
            if sums is not None
            else "".join(f"{sha256(source)}  {name}\n" for name, source in sorted(assets.items()))
        )
        self.served[f"{RELEASES}/download/v{version}/SHA256SUMS"] = str(listing)
        if latest:
            self.served[f"{RELEASES}/latest/download/SHA256SUMS"] = str(listing)


def fake(directory, name, contents):
    path = Path(directory) / name
    path.write_text(contents)
    path.chmod(0o755)
    return path


def run_with_tty(command, environment, answers="", timeout=120):
    """Run `command` in its own session with a terminal as its controlling terminal.

    Standard input stays /dev/null, as with `curl ... | sh`, so prompts reach the person only
    through /dev/tty. `answers` is typed on the terminal before the command starts.
    Returns the completed process and everything the command wrote to the terminal.
    """
    controller, terminal = os.openpty()

    def control():
        os.setsid()
        fcntl.ioctl(terminal, termios.TIOCSCTTY, 0)

    process = subprocess.Popen(
        command,
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        preexec_fn=control,
        pass_fds=(terminal,),
    )
    os.close(terminal)
    if answers:
        os.write(controller, answers.encode())
    transcript = b""
    deadline = time.monotonic() + timeout
    stdout = stderr = ""
    try:
        while process.poll() is None:
            if time.monotonic() > deadline:
                process.kill()
                raise TimeoutError(command)
            readable, _, _ = select.select([controller], [], [], 0.05)
            if readable:
                try:
                    transcript += os.read(controller, 4096)
                except OSError:
                    break
        stdout, stderr = process.communicate(timeout=timeout)
        while True:
            readable, _, _ = select.select([controller], [], [], 0)
            if not readable:
                break
            try:
                piece = os.read(controller, 4096)
            except OSError:
                break
            if not piece:
                break
            transcript += piece
    finally:
        os.close(controller)
    completed = subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
    return completed, transcript.decode(errors="replace")
