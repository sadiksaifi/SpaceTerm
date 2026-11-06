"""Pair this commit with published SpaceTerm releases across the protocol and the update path.

`.github/compatibility.toml` lists the releases this commit must still talk to. For each one,
the release workflow runs two pairs on x86_64 Linux: this commit's CLI against that
release's Core, and that release's CLI against this commit's Core. ADR 0035 owns the rule
that decides which releases belong in the list.

The release workflow also runs the update-from-floor pair: the Core of the release's Update
Floor, with Workspace organization and a pairing, updates to the release's own signed Core
archive and restarts into it. ADR 0064 owns the floor.
"""

import json
import os
import pty
import re
import select
import shutil
import signal
import subprocess
import tempfile
import time
import tomllib
from contextlib import contextmanager
from pathlib import Path, PurePosixPath

from spaceterm_tasks import ROOT, TaskError, release_feed, release_floor, update_signing
from spaceterm_tasks.bounded_tar import open_archive
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.core_package import (
    MAX_EXPANDED,
    MAX_MEMBERS,
    PROTOCOLS,
    SHARED_RESOURCES,
    TARGETS,
    artifact_trust,
    compile_terminfo,
    core_floor,
)
from spaceterm_tasks.packaging import RELEASE_FLOOR, SHELL_INTEGRATION, packaged_environment

RELEASES = ROOT / ".github/compatibility.toml"
TARGET = TARGETS["linux"]
# Each side of a pair is this commit or the listed release.
SIDES = ("client", "core")
STARTUP = 60
COMMAND = 60


def releases(path=RELEASES):
    """The listed release tags, oldest first. cliff.toml's tag_pattern decides what a release is."""
    with open(ROOT / "cliff.toml", "rb") as config:
        pattern = re.compile(tomllib.load(config)["git"]["tag_pattern"])
    try:
        listed = tomllib.loads(Path(path).read_text())["releases"]
    except (KeyError, tomllib.TOMLDecodeError):
        raise TaskError("the compatibility list needs one releases array") from None
    if not isinstance(listed, list) or not all(
        isinstance(tag, str) and pattern.match(tag) for tag in listed
    ):
        raise TaskError("each listed release is a stable release tag")
    if listed != sorted(set(listed), key=_order):
        raise TaskError("listed releases are distinct and oldest first")
    return listed


def matrix(listed):
    """The workflow's pair jobs: each listed release, once with this commit as each side."""
    return [{"release": tag, "candidate": side} for tag in listed for side in SIDES]


def plan(output, path=RELEASES):
    """Write the matrix and whether it has any job as GitHub step outputs."""
    include = matrix(releases(path))
    with open(output, "a") as outputs:
        outputs.write(f"matrix={json.dumps({'include': include}, separators=(',', ':'))}\n")
        outputs.write(f"any={'true' if include else 'false'}\n")
    print(f"{len(include)} compatibility pairs planned.")


def verdict(planning, planned, pairs):
    """Passes only when planning succeeded and the pairs ran as planned: every one passed when
    the plan had any, and none ran when it had none. The arguments are GitHub job results and
    the plan's `any` output."""
    if planning != "success":
        raise TaskError("the compatibility plan did not succeed")
    if (planned, pairs) not in (("true", "success"), ("false", "skipped")):
        raise TaskError(f"the planned pairs ended {pairs}")
    print("Every planned compatibility pair passed.")


def candidate_tag(listed):
    """A release tag for this commit's build: the next minor after the newest listed release."""
    if not listed:
        raise TaskError("a candidate pairs with a listed release")
    major, minor, _ = _order(listed[-1])
    return f"v{major}.{minor + 1}.0"


# Each command admits on one protocol: `list` on the work protocol, `status` on the service lane.
LANES = {"list": "work", "status": "service"}


def expected(candidate, release, current=PROTOCOLS):
    """The outcome ADR 0035 allows each command of a pair, from both builds' `protocols`.

    A client speaks its major and the previous one, and a Core speaks one, so this commit's CLI
    works with a Core one major behind, while that release's CLI is refused as too old. An
    outcome is `ok`, or `cli_too_old` for an `incompatible_protocol` refusal that names the
    CLI's side as the older one.
    """
    allowed = {}
    for command, lane in LANES.items():
        mine, theirs = current[lane]["major"], release[lane]["major"]
        if theirs not in (mine, mine - 1):
            raise TaskError(f"a listed release's {lane} major is this commit's or the previous one")
        allowed[command] = "ok" if candidate == "client" or theirs == mine else "cli_too_old"
    return allowed


def pair(tag, candidate, path=RELEASES, *, release_tag=None):
    """Run one pair: `candidate` names the side this commit plays against release `tag`."""
    listed = releases(path)
    if tag not in listed or candidate not in SIDES:
        raise TaskError("a pair names a listed release and a candidate side")
    with tempfile.TemporaryDirectory(prefix="spaceterm-compatibility-") as temporary:
        directory = Path(temporary)
        built = build_candidate(release_tag or candidate_tag(listed), directory / "candidate")
        published, protocols = fetch(tag, directory / "release")
        allowed = expected(candidate, protocols)
        environment = _environment(directory / "account")
        if candidate == "client":
            install(published, environment)
            exercise(built, environment, allowed)
        else:
            set_up_account(environment)
            with _serving(built, environment, directory / "core.log") as core:
                exercise(published / "bin/spaceterm", environment, allowed, core)
    print(f"This commit's {candidate} and {tag} behave as ADR 0035 requires.")


def build_candidate(tag, destination):
    """This commit's executable as a SpaceTerm release, in the installed Linux layout.

    build.rs gives the release identity only to a build of an annotated tag at HEAD, so this
    uses its planned tag, or creates and removes a local tag for a manual pair.
    """
    exists = (
        subprocess.run(
            ["git", "rev-parse", "--verify", "--quiet", f"refs/tags/{tag}"],
            cwd=ROOT,
            capture_output=True,
        ).returncode
        == 0
    )
    identity = ["-c", "user.name=SpaceTerm compatibility", "-c", "user.email=compatibility@invalid"]
    if not exists:
        _checked(["git", *identity, "tag", "-a", tag, "-m", "Compatibility candidate"], "tag")
    try:
        executable = build_executable(
            "--release",
            "--locked",
            "--package",
            "spaceterm-cli",
            "--no-default-features",
            binary="spaceterm",
            env={**packaged_environment(tag), RELEASE_FLOOR: core_floor(tag)},
        )
    finally:
        if not exists:
            _checked(["git", "tag", "-d", tag], "untag")
    binary = destination / "bin/spaceterm"
    binary.parent.mkdir(parents=True)
    shutil.copyfile(executable, binary)
    binary.chmod(0o755)
    resources = destination / SHARED_RESOURCES
    shutil.copytree(SHELL_INTEGRATION, resources / "shell-integration")
    compile_terminfo(resources / "terminfo", TARGET)
    return binary


def fetch(tag, destination):
    """Release `tag`'s authenticated Linux Core archive, unpacked as the installer stages it,
    and its protocol versions."""
    name = release_feed.archive_name("core", tag[1:], "linux-x86_64")
    destination.mkdir()
    download = destination / "download"
    _checked(
        [
            "gh",
            "release",
            "download",
            tag,
            "--pattern",
            name,
            "--pattern",
            f"{name}.sig",
            "--dir",
            str(download),
        ],
        "download",
    )
    archive, signature = download / name, download / f"{name}.sig"
    update_signing.verify(artifact_trust("spaceterm"), archive, signature)
    with open_archive(archive, maximum=MAX_EXPANDED, members=MAX_MEMBERS) as source:
        members = [
            member
            for member in source
            if PurePosixPath(member.name).parts[:1] == ("core",)
            and (member.isfile() or member.isdir())
        ]
        source.extractall(destination, members=members, filter="data")
    try:
        build = json.loads((destination / "core/build.json").read_bytes())
    except (OSError, json.JSONDecodeError):
        raise TaskError("the release archive has no build document") from None
    found = tuple(build.get(key) for key in ("identity", "version", "target"))
    if found != ("spaceterm", tag[1:], TARGET):
        raise TaskError("the release archive is not that release's Linux Core")
    protocols = build.get("protocols")
    if not isinstance(protocols, dict) or not all(
        isinstance((protocols.get(lane) or {}).get("major"), int) for lane in LANES.values()
    ):
        raise TaskError("the release archive names no protocol versions")
    tree = destination / "core"
    shutil.copyfile(archive, tree / "artifact.tar.gz")
    shutil.copyfile(signature, tree / "artifact.sig")
    return tree, protocols


def install(tree, environment, setup=()):
    """Install and start a release Core through its own installer steps: retain, then setup.

    Setup starts the Core as a systemd user service, which needs boot startup already
    authorized for the account. `setup` adds options to the setup command.
    """
    _checked([str(tree / "bin/spaceterm"), "internal", "retain", str(tree)], "retain", environment)
    if (outcome := _outcome(LAUNCHER, ["setup", *setup], environment)) != "ok":
        raise TaskError(f"the release Core's setup failed ({outcome})")


def set_up_account(environment):
    """Create the account state this commit's foreground Core needs, as its setup would."""
    example = build_executable(
        "--locked", "--package", "spaceterm-cli", binary="account_setup", example=True
    )
    _checked([str(example), "spaceterm"], "account setup", environment)


def exercise(client, environment, allowed, core=None):
    """Require each of `client`'s commands to end in the outcome `allowed` names for it.

    `list` admits on the work protocol and `status` on the service lane, so each negotiates its
    own version. `list` is retried while the Core starts; `core` is its process when this pair
    started it.
    """
    deadline = time.monotonic() + STARTUP
    while (outcome := _outcome(client, ["list"], environment)) != allowed["list"]:
        if (core is not None and core.poll() is not None) or time.monotonic() >= deadline:
            raise TaskError(f"the client's list ended in {outcome}")
        time.sleep(0.25)
    if (outcome := _outcome(client, ["status"], environment)) != allowed["status"]:
        raise TaskError(f"the client's status ended in {outcome}")


# The Command Line Tool that setup links for the release identity.
LAUNCHER = Path.home() / ".local/bin/spaceterm"
WORKSPACE = "update-from-floor"
SAVED_MACHINE = "floor"
RESTART = 120


def update(tag, archive, signature):
    """Update the Core of release `tag`'s Update Floor to `archive`, that release's signed Linux
    Core, as a person on the floor release would, and require their work to survive.

    The floor Core gets a Workspace and a pairing with this account's own CLI. It installs the
    archive, restarts into it, and must then report the release's version in `status`, list the
    Workspace, and answer over the pairing. A release that is its own floor has no release to
    update from, so the pair passes without running.
    """
    version = tag[1:]
    floor = release_floor.floor("core", version)
    if floor == version:
        print(f"{tag} is its own Update Floor; no published release updates to it.")
        return
    with tempfile.TemporaryDirectory(prefix="spaceterm-update-pair-") as temporary:
        directory = Path(temporary)
        published, _ = fetch(f"v{floor}", directory / "floor")
        environment = _environment(directory / "account")
        install(published, environment, ["--network", "127.0.0.1"])
        _envelope(LAUNCHER, ["workspace", "create", WORKSPACE], environment)
        pair_with_self(environment)
        updated = _envelope(
            LAUNCHER,
            ["update", "--archive", str(archive), "--signature", str(signature)],
            environment,
        )
        # The Core may already have started the release on its own.
        installed = (updated["data"].get("pending") or {}).get("version")
        if version not in (installed, updated["data"].get("running")):
            raise TaskError(f"the floor release {floor} did not install {tag}")
        restart_into(
            version,
            lambda arguments, status="ok", timeout=COMMAND: _envelope(
                LAUNCHER, arguments, environment, status, timeout
            ),
        )
        for arguments in (["list"], ["list", "--machine", SAVED_MACHINE]):
            listed = _envelope(LAUNCHER, arguments, environment)
            names = [workspace["ref"]["label"] for workspace in listed["data"]["workspaces"]]
            if WORKSPACE not in names:
                raise TaskError(f"the Workspace did not survive the update to {tag}")
    print(f"The Core of {floor} updated to {tag} with its Workspace and pairing intact.")


def restart_into(version, cli, deadline=RESTART, *, clock=time.monotonic, sleep=time.sleep):
    """Bring SpaceTerm Core onto `version`, its Pending Version, and return its status.

    With no live Terminal Session, the Core's update engine may restart into a nonbreaking
    Pending Version on its own at any moment under ADR 0034, which makes an issued confirmation
    stale. So this reads `status`, previews a restart only while `version` still waits and no
    confirmed restart has run, confirms it only when the preview still moves to `version`, and
    repeats until the Core runs `version`. Either transition passes, and the verdict depends
    only on where the Core ends. No command starts after the deadline, and each one's timeout
    ends by it. `cli(arguments, status, timeout)` returns the CLI's envelope or raises
    TaskError.
    """
    end = clock() + deadline

    def run(arguments, status="ok"):
        remaining = end - clock()
        if remaining <= 0:
            raise TaskError(f"SpaceTerm Core did not restart into {version}")
        return cli(arguments, status, min(remaining, COMMAND))

    restarted = False
    while True:
        try:
            status = run(["status"])
            data = status["data"]
            if data["runtime"]["version"] == version and data["running"]:
                return status
            if not restarted and (data.get("pending") or {}).get("version") == version:
                confirmation = run(["restart"], "confirmation_required")["data"]["confirmation"]
                # The Core may have started the release since `status`, leaving a preview that
                # would restart it again.
                if (confirmation["effects"].get("pending") or {}).get("to") == version:
                    run(["restart", "--confirm", confirmation["token"]])
                    restarted = True
        except TaskError:
            pass
        if clock() >= end:
            raise TaskError(f"SpaceTerm Core did not restart into {version}")
        sleep(1)


def pair_with_self(environment, timeout=60):
    """Pair this account's CLI with its own Core over Network Access, as another device would.

    `pair` needs a terminal, so it runs on a pseudo-terminal. `machine add` reads the
    Invitation Link from a pipe and reports the Comparison Code as a JSON event, which this
    types into `pair`.
    """
    deadline = time.monotonic() + timeout
    primary, secondary = pty.openpty()
    inviting = subprocess.Popen(
        [str(LAUNCHER), "pair"],
        stdin=secondary,
        stdout=secondary,
        stderr=secondary,
        env=environment,
        start_new_session=True,
    )
    os.close(secondary)
    joining = None
    try:
        link = _read_until(
            primary, re.compile(rb"spaceterm://pair/[^\s\x1b]+(?=[\s\x1b])"), deadline
        )
        joining = subprocess.Popen(
            [str(LAUNCHER), "--json", "machine", "add", SAVED_MACHINE, "--trust"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            env=environment,
        )
        stdin, stdout = joining.stdin, joining.stdout
        if stdin is None or stdout is None:
            raise TaskError("machine add has no pipes")
        stdin.write(link + b"\n")
        stdin.close()
        event = json.loads(stdout.readline())
        if event.get("event") != "comparison_code":
            raise TaskError("machine add reported no Comparison Code")
        _read_until(primary, re.compile(rb"Comparison Code it shows: "), deadline)
        os.write(primary, event["data"]["code"].encode() + b"\n")
        lines = stdout.read().splitlines()
        added = json.loads(lines[-1] if lines else b"{}")
        if joining.wait(timeout=max(deadline - time.monotonic(), 1)) or added.get("status") != "ok":
            raise TaskError("machine add did not pair with the floor Core")
        if inviting.wait(timeout=max(deadline - time.monotonic(), 1)):
            raise TaskError("pair did not approve the device")
    except (subprocess.TimeoutExpired, json.JSONDecodeError, KeyError, AttributeError):
        raise TaskError("pairing with the floor Core did not finish") from None
    finally:
        for process in (inviting, joining):
            if process is not None and process.poll() is None:
                process.kill()
                process.wait()
        os.close(primary)


def _read_until(descriptor, pattern, deadline):
    """Read a pseudo-terminal until `pattern` appears in its output, and return the match."""
    output = b""
    while (found := pattern.search(output)) is None:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TaskError("pair did not show what the pairing waits for")
        if select.select([descriptor], [], [], remaining)[0]:
            try:
                chunk = os.read(descriptor, 4096)
            except OSError:
                chunk = b""
            if not chunk:
                raise TaskError("pair ended before the pairing finished")
            output += chunk
    return found.group(0)


def _envelope(client, arguments, environment, status="ok", timeout=COMMAND):
    """Run `client` with `--json` and return its envelope, which must have `status`."""
    try:
        result = subprocess.run(
            [str(client), "--json", *arguments],
            env=environment,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            timeout=timeout,
        )
        envelope = json.loads(result.stdout)
    except (subprocess.TimeoutExpired, json.JSONDecodeError):
        raise TaskError(f"the {arguments[0]} command returned no envelope") from None
    if not isinstance(envelope, dict) or envelope.get("status") != status:
        code = ((envelope if isinstance(envelope, dict) else {}).get("error") or {}).get("code")
        raise TaskError(f"the {arguments[0]} command ended in {code}")
    return envelope


def _environment(account):
    """A minimal environment whose CLI state stays under `account`. The Core keeps its own in
    the account's home, and a user service reaches systemd through the runtime directory."""
    roots = {
        "HOME": Path.home(),
        "XDG_CONFIG_HOME": account / "config",
        "XDG_DATA_HOME": account / "data",
        "XDG_STATE_HOME": account / "state",
        "XDG_CACHE_HOME": account / "cache",
    }
    for key, directory in roots.items():
        if key != "HOME":
            directory.mkdir(parents=True, mode=0o700)
    runtime = Path(f"/run/user/{os.getuid()}")
    return {
        **{key: str(directory) for key, directory in roots.items()},
        **({"XDG_RUNTIME_DIR": str(runtime)} if runtime.is_dir() else {}),
        "PATH": "/usr/bin:/bin",
        "SHELL": "/bin/sh",
        "LANG": "C.UTF-8",
    }


@contextmanager
def _serving(core, environment, log):
    """Run `core` in the foreground until the block ends."""
    with open(log, "wb") as output:
        process = subprocess.Popen(
            [str(core), "internal", "serve", "--identity", "spaceterm"],
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=output,
            stderr=output,
            start_new_session=True,
        )
    try:
        yield process
    finally:
        if process.poll() is None:
            process.send_signal(signal.SIGTERM)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


def _outcome(client, arguments, environment):
    """`ok`, `cli_too_old` or `core_too_old` for an `incompatible_protocol` refusal by the side
    it names as older, or another error code."""
    try:
        result = subprocess.run(
            [str(client), "--json", *arguments],
            env=environment,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            timeout=COMMAND,
        )
    except subprocess.TimeoutExpired:
        return "timed out"
    try:
        envelope = json.loads(result.stdout)
        error = envelope.get("error") or {}
        code = error.get("code")
    except (json.JSONDecodeError, AttributeError):
        return f"exit {result.returncode} without an envelope"
    if envelope.get("status") == "ok" and result.returncode == 0:
        return "ok"
    if code == "incompatible_protocol":
        older = {"core": "cli_too_old", "machine": "core_too_old"}
        return older.get((error.get("details") or {}).get("update"), "an unnamed refusal")
    return code if isinstance(code, str) and code.isidentifier() else f"exit {result.returncode}"


def _order(tag):
    return tuple(int(part) for part in tag[1:].split("."))


def _checked(arguments, step, environment=None):
    if subprocess.run(arguments, cwd=ROOT, env=environment, capture_output=True).returncode:
        raise TaskError(f"the compatibility {step} step failed")
