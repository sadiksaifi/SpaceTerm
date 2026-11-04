"""Pair this commit with published SpaceTerm releases across the protocol.

`.github/compatibility.toml` lists the releases this commit must still talk to. For each one,
the compatibility workflow runs two pairs on x86_64 Linux: this commit's CLI against that
release's Core, and that release's CLI against this commit's Core. ADR 0035 owns the rule
that decides which releases belong in the list.
"""

import json
import os
import re
import shutil
import signal
import subprocess
import tarfile
import tempfile
import time
import tomllib
from contextlib import contextmanager
from pathlib import Path, PurePosixPath

from spaceterm_tasks import ROOT, TaskError, release
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.core_package import (
    PROTOCOL_MAJOR,
    SHARED_RESOURCES,
    TARGETS,
    artifact_public_key,
    compile_terminfo,
)
from spaceterm_tasks.packaging import SHELL_INTEGRATION, packaged_environment

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


def candidate_tag(listed):
    """A release tag for this commit's build: the next minor after the newest listed release."""
    if not listed:
        raise TaskError("a candidate pairs with a listed release")
    major, minor, _ = _order(listed[-1])
    return f"v{major}.{minor + 1}.0"


def expected(candidate, release_major, current=PROTOCOL_MAJOR):
    """The outcomes ADR 0035 allows each command of a pair, by the release's work major.

    A client speaks its work major and the previous one, and a Core speaks one, so this commit's
    CLI works with a Core one major behind, while that release's CLI is refused as too old. An
    outcome is `ok`, or `cli_too_old` for an `incompatible_protocol` refusal that names the CLI's
    side as the older one. The service lane has its own major, which no build document records, so
    a release CLI's `status` may also be refused as too old.
    """
    if release_major not in (current, current - 1):
        raise TaskError("a listed release's work major is this commit's or the previous one")
    if candidate == "client":
        return {"list": {"ok"}, "status": {"ok"}}
    return {
        "list": {"ok"} if release_major == current else {"cli_too_old"},
        "status": {"ok", "cli_too_old"},
    }


def pair(tag, candidate, path=RELEASES):
    """Run one pair: `candidate` names the side this commit plays against release `tag`."""
    listed = releases(path)
    if tag not in listed or candidate not in SIDES:
        raise TaskError("a pair names a listed release and a candidate side")
    with tempfile.TemporaryDirectory(prefix="spaceterm-compatibility-") as temporary:
        directory = Path(temporary)
        built = build_candidate(candidate_tag(listed), directory / "candidate")
        published, major = fetch(tag, directory / "release")
        allowed = expected(candidate, major)
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
    tags HEAD locally for the build and removes the tag after it.
    """
    if (
        subprocess.run(
            ["git", "rev-parse", "--verify", "--quiet", f"refs/tags/{tag}"],
            cwd=ROOT,
            capture_output=True,
        ).returncode
        == 0
    ):
        raise TaskError("the candidate's release tag already exists")
    identity = ["-c", "user.name=SpaceTerm compatibility", "-c", "user.email=compatibility@invalid"]
    _checked(["git", *identity, "tag", "-a", tag, "-m", "Compatibility candidate"], "tag")
    try:
        executable = build_executable(
            "--locked",
            "--package",
            "spaceterm-cli",
            "--no-default-features",
            binary="spaceterm",
            env=packaged_environment(tag),
        )
    finally:
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
    and its work major."""
    name = release.machine_archive_name(tag[1:], TARGET)
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
    release.verify_ed25519(artifact_public_key("spaceterm"), archive, signature)
    with tarfile.open(archive, "r:gz") as source:
        members = [
            member
            for member in source
            if PurePosixPath(member.name).parts[:1] == ("server",)
            and (member.isfile() or member.isdir())
        ]
        source.extractall(destination, members=members, filter="data")
    try:
        build = json.loads((destination / "server/build.json").read_bytes())
    except (OSError, json.JSONDecodeError):
        raise TaskError("the release archive has no build document") from None
    found = tuple(build.get(key) for key in ("identity", "version", "target"))
    if found != ("spaceterm", tag[1:], TARGET):
        raise TaskError("the release archive is not that release's Linux Core")
    major = build.get("protocol_major")
    if not isinstance(major, int):
        raise TaskError("the release archive names no work major")
    tree = destination / "server"
    shutil.copyfile(archive, tree / "artifact.tar.gz")
    shutil.copyfile(signature, tree / "artifact.sig")
    return tree, major


def install(tree, environment):
    """Install and start a release Core through its own installer steps: retain, then setup.

    Setup starts the Core as a systemd user service, which needs boot startup already
    authorized for the account.
    """
    _checked([str(tree / "bin/spaceterm"), "internal", "retain", str(tree)], "retain", environment)
    if (outcome := _outcome(Path.home() / ".local/bin/spaceterm", ["setup"], environment)) != "ok":
        raise TaskError(f"the release Core's setup failed ({outcome})")


def set_up_account(environment):
    """Create the account state this commit's foreground Core needs, as its setup would."""
    example = build_executable(
        "--locked", "--package", "spaceterm-cli", binary="account_setup", example=True
    )
    _checked([str(example), "spaceterm"], "account setup", environment)


def exercise(client, environment, allowed, core=None):
    """Require each of `client`'s commands to end in an outcome `allowed` names for it.

    `list` admits on the work protocol and `status` on the service lane, so each negotiates its
    own version. `list` is retried while the Core starts; `core` is its process when this pair
    started it.
    """
    deadline = time.monotonic() + STARTUP
    while (outcome := _outcome(client, ["list"], environment)) not in allowed["list"]:
        if (core is not None and core.poll() is not None) or time.monotonic() >= deadline:
            raise TaskError(f"the client's list ended in {outcome}")
        time.sleep(0.25)
    if (outcome := _outcome(client, ["status"], environment)) not in allowed["status"]:
        raise TaskError(f"the client's status ended in {outcome}")


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
