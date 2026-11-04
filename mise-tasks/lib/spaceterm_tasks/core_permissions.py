"""Owner check after signing: permission-dependent work in a pane of the installed Core.

Run it on a Mac whose installed SpaceTerm Core is the signed build under test, in that person's
login session, because macOS attributes a pane's programs to the Core LaunchAgent. It may show
the system's Local Network and Automation prompts naming SpaceTerm Core; allow them. Automated
tests never run it, because they must never cause a privacy prompt.
"""

import json
import plistlib
import re
import shlex
import subprocess
import tempfile
from pathlib import Path

from spaceterm_tasks import TaskError, macos_codesign

APPLE_EVENT = """osascript -e 'tell application "System Events" to get name of first process'"""
# ssh prints these only after it has reached the host.
REACHED = ("Permission denied", "Host key verification failed", "REMOTE HOST IDENTIFICATION")
LAUNCHER = Path.home() / ".local/bin/spaceterm"


def ssh_command(host):
    if not re.fullmatch(r"[A-Za-z0-9.:%\[\]_-]+", host) or host.startswith("-"):
        raise TaskError("the host must be a name or address")
    return (
        f"/usr/bin/ssh -F /dev/null -o BatchMode=yes -o ConnectTimeout=3 {shlex.quote(host)} true"
    )


def ssh_verdict(exit_status, output):
    """Whether ssh reached the host. A refusal before any connection is not a success."""
    return exit_status == 0 or any(text in output for text in REACHED)


def apple_event_verdict(exit_status, output):
    return exit_status == 0 and bool(output.strip())


def bundle_of(launcher):
    executable = Path(launcher).resolve()
    for parent in executable.parents:
        if parent.suffix == ".app":
            return parent
    raise TaskError("the launcher does not run a SpaceTerm Core bundle")


def describe_signature(bundle):
    """Print what the system checks; require the hardened runtime and every Core entitlement."""
    details = macos_codesign.details(bundle)
    for line in details.splitlines():
        if line.startswith(("Identifier=", "TeamIdentifier=", "Authority=", "Timestamp=")):
            print(f"  {line}")
    flags = re.search(r"^CodeDirectory .*flags=0x([0-9a-f]+)", details, re.MULTILINE)
    if not flags or not int(flags.group(1), 16) & macos_codesign.HARDENED_RUNTIME:
        raise TaskError("the installed SpaceTerm Core is not signed with the hardened runtime")
    expected = plistlib.loads(macos_codesign.CORE_ENTITLEMENTS.read_bytes())
    if macos_codesign.signed_entitlements(bundle) != expected:
        raise TaskError("the installed SpaceTerm Core lacks its entitlements")


def cli(launcher, *arguments, allowed=(0,)):
    result = subprocess.run(
        [str(launcher), "--json", "--timeout", "60s", *arguments],
        capture_output=True,
        text=True,
        timeout=90,
    )
    try:
        envelope = json.loads(result.stdout)
    except ValueError:
        raise TaskError(f"spaceterm {arguments[0]} returned no result") from None
    if result.returncode not in allowed:
        raise TaskError(f"spaceterm {' '.join(arguments[:2])} failed ({result.returncode})")
    return envelope


def run_in_pane(launcher, pane, command):
    data = cli(launcher, "pane", "run", command, "--pane", f"id:{pane}")["data"]
    output = "\n".join(row["text"] for row in data["output"]["rows"]).strip()
    return data["run"]["exit_status"], output


def check(host, launcher=LAUNCHER):
    command = ssh_command(host)
    bundle = bundle_of(launcher)
    print(f"SpaceTerm Core bundle: {bundle.name}")
    describe_signature(bundle)
    with tempfile.TemporaryDirectory(prefix="spaceterm-permissions-") as directory:
        workspace = cli(
            launcher, "workspace", "create", "Permissions check", "--directory", directory
        )["data"]["workspace"]["id"]
        try:
            pane = cli(launcher, "tab", "create", "--workspace", f"id:{workspace}")["data"]["pane"][
                "id"
            ]
            results = []
            for name, probe, verdict in (
                ("ssh", command, ssh_verdict),
                ("Apple Event", APPLE_EVENT, apple_event_verdict),
            ):
                status, output = run_in_pane(launcher, pane, probe)
                passed = verdict(status, output)
                results.append(passed)
                print(f"{name}: {'passed' if passed else 'FAILED'} (exit {status})")
                for line in output.splitlines()[-5:]:
                    print(f"  {line}")
        finally:
            preview = cli(
                launcher, "workspace", "kill", "--workspace", f"id:{workspace}", allowed=(3,)
            )
            token = preview["data"]["confirmation"]["token"]
            cli(launcher, "workspace", "kill", "--workspace", f"id:{workspace}", "--confirm", token)
    if not all(results):
        raise TaskError("a permission-dependent command failed in the SpaceTerm Core pane")
