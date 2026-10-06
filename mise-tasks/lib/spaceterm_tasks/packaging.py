"""Identity selection and package checks that every platform's packaging shares."""

import filecmp
import os
import re
import subprocess

from spaceterm_tasks import ROOT, TaskError

DIST = ROOT / "dist"
TERMINFO = ROOT / "assets" / "terminfo" / "xterm-spaceterm.terminfo"
TERMINFO_IDENTITY = "xterm-spaceterm|SpaceTerm truthful xterm-compatible terminal"
NOTICES = ROOT / "assets" / "THIRD-PARTY-NOTICES.txt"
SHELL_INTEGRATION = ROOT / "assets" / "shell-integration"
PREFLIGHT_VERSION = "0.0.0"
STABLE_TAG = re.compile(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")
ASKPASS_HELPER_MODE = "broker-v1"


def release_version(tag):
    """Return a release tag's version. build.rs proves the tag is annotated, on HEAD, and clean."""
    if not STABLE_TAG.fullmatch(tag):
        raise TaskError("a release tag is v followed by a canonical stable SemVer")
    return tag[1:]


def selected_identity(release_tag):
    return "spaceterm" if release_tag else "preflight"


def checked(arguments, failure, **kwargs):
    result = subprocess.run(arguments, capture_output=True, **kwargs)
    if result.returncode:
        raise TaskError(failure)
    return result.stdout


def require_clean_checkout():
    """Packaging reads files Cargo never sees, and Cargo can reuse a build script's last check."""
    status = checked(
        ["git", "--no-optional-locks", "status", "--porcelain", "--untracked-files=normal"],
        "Git could not report the checkout state",
        cwd=ROOT,
    )
    if status.strip():
        raise TaskError("a release package requires a clean checkout")


def packaged_environment(release_tag, removed=()):
    """The build environment of a packaged identity, without inherited packaging inputs."""
    environment = {
        key: value
        for key, value in os.environ.items()
        if key not in ("SPACETERM_RELEASE_TAG", *removed)
    }
    # Leaves the SpaceTerm Development identity.
    environment["SPACETERM_PACKAGED"] = "1"
    if release_tag:
        environment["SPACETERM_RELEASE_TAG"] = release_tag
    return environment


def verify_application_identity(executable, expected, label, version=None):
    # An old executable may launch the application instead of answering --version.
    # Clear helper inputs and bound the query before inspecting its compiled name.
    try:
        result = subprocess.run([executable, "--version"], env={}, capture_output=True, timeout=5)
    except (OSError, subprocess.TimeoutExpired):
        raise TaskError(f"{label} executable application identity query failed") from None
    name, _, reported = result.stdout.removesuffix(b"\n").rpartition(b" ")
    if (
        result.returncode
        or result.stderr
        or name != expected.encode()
        or not reported
        or any(byte in b" \t\r\n" for byte in reported)
    ):
        raise TaskError(f"{label} executable application identity must be {expected}")
    if version is not None and reported != version.encode():
        raise TaskError(f"{label} executable version must be {version}")


def verify_askpass_helper_mode(executable, label):
    """The packaged executable must enter AskPass helper mode before anything else."""
    for prompt in ("SpaceTerm package verifier prompt", "--version"):
        try:
            result = subprocess.run(
                [executable, prompt],
                capture_output=True,
                timeout=5,
                env={"SPACETERM_SSH_ASKPASS_MODE": ASKPASS_HELPER_MODE},
            )
        except (OSError, subprocess.TimeoutExpired):
            raise TaskError(f"{label} AskPass helper mode did not finish") from None
        if result.returncode != 2 or result.stdout or result.stderr:
            raise TaskError(
                f"{label} AskPass helper mode must reject missing transport "
                "inputs silently with exit 2"
            )


def verify_resources(resources, label):
    """Verify the notices, shell integration, and terminfo entry a package carries."""
    notices = resources / NOTICES.name
    if not notices.is_file() or not filecmp.cmp(NOTICES, notices, shallow=False):
        raise TaskError(f"{label} third-party notices differ from the tracked resource")
    if not (resources / "shell-integration").is_dir() or not same_tree(
        SHELL_INTEGRATION, resources / "shell-integration"
    ):
        raise TaskError(f"{label} shell integration differs from the tracked resources")
    terminfo = checked(
        ["infocmp", "-x", "-1", "xterm-spaceterm"],
        f"{label} xterm-spaceterm entry is not discoverable",
        env={**os.environ, "TERMINFO": str(resources / "terminfo")},
        text=True,
    )
    if TERMINFO_IDENTITY not in terminfo:
        raise TaskError(f"{label} terminfo entry has the wrong identity")


def same_tree(expected, actual):
    comparison = filecmp.dircmp(expected, actual, ignore=[])
    if comparison.left_only or comparison.right_only or comparison.funny_files:
        return False
    _, mismatch, errors = filecmp.cmpfiles(expected, actual, comparison.common_files, shallow=False)
    return (
        not mismatch
        and not errors
        and all(
            same_tree(expected / directory, actual / directory)
            for directory in comparison.common_dirs
        )
    )
