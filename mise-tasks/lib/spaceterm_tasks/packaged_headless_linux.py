"""Execute packaged queries in a minimal, unprivileged Linux filesystem.

This is not service or authenticated-runtime acceptance. Bubblewrap supplies the
filesystem, process and network isolation. Only the archive and its approved GNU
runtime libraries enter the sandbox. No service manager or administrator tool
runs, and no host HOME, runtime socket, display, client or checkout is mounted.
"""

import argparse
import json
import os
import re
import shutil
import sys
import tarfile
import tempfile
from pathlib import Path

from spaceterm_tasks import TaskError, main, package_server, server_artifacts
from spaceterm_tasks import package_server_linux as linux
from spaceterm_tasks import server_vm_linux as native


def host_facts():
    """Record versions, not hostnames, environment values or native diagnostics."""
    glibc = native.run_tool(["getconf", "GNU_LIBC_VERSION"], maximum=4096)
    systemd = native.run_tool(["systemctl", "--version"], maximum=4096)
    libc_match = re.fullmatch(rb"glibc ([0-9]+)\.([0-9]+)\n", glibc)
    systemd_match = re.match(rb"systemd ([0-9]+)\b", systemd)
    kernel_match = re.match(r"([0-9]+)\.([0-9]+)\.([0-9]+)\b", os.uname().release)
    if not libc_match or not systemd_match or not kernel_match:
        raise TaskError("unsupported-headless-host-versions")
    return {
        "glibc": [int(value) for value in libc_match.groups()],
        "kernel": [int(value) for value in kernel_match.groups()],
        "systemd": int(systemd_match.group(1)),
    }


def baseline_host(facts):
    """Newer hosts do not prove execution on the Ubuntu 22.04 baseline."""
    return facts["glibc"] == [2, 35] and facts["kernel"][:2] == [5, 15] and facts["systemd"] == 249


def copy_runtime(root, libraries):
    for name in libraries:
        source = Path("/lib/x86_64-linux-gnu") / name
        # The ELF policy has already restricted names to GNU runtime libraries.
        # Resolve the host's distribution symlink, but copy bytes, not links.
        package_server.copy_file(source.resolve(strict=True), root / "lib/x86_64-linux-gnu" / name)
    package_server.copy_file(
        Path("/lib64/ld-linux-x86-64.so.2").resolve(strict=True),
        root / "lib64/ld-linux-x86-64.so.2",
        0o755,
    )


def sandbox_command(root, home, runtime, arguments):
    return [
        "bwrap",
        "--unshare-all",
        "--die-with-parent",
        "--cap-drop",
        "ALL",
        "--clearenv",
        "--ro-bind",
        root,
        "/",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--bind",
        home,
        "/home/account",
        "--bind",
        runtime,
        "/runtime",
        "--setenv",
        "HOME",
        "/home/account",
        "--setenv",
        "XDG_DATA_HOME",
        "/home/account/data",
        "--setenv",
        "XDG_RUNTIME_DIR",
        "/runtime",
        "--setenv",
        "LC_ALL",
        "C",
        "--chdir",
        "/",
        "--",
        "/opt/spaceterm-server/bin/spaceterm-server",
        *arguments,
    ]


def run(archive, source_commit, *, require_baseline_host=False):
    """Return bounded artifact and query facts, without installing any service.

    source_commit is the caller's recorded source revision. build_commit comes
    independently from the executable. A sync checkout can have a different Git
    commit, so neither field is silently substituted for the other.
    """
    if (
        sys.platform != "linux"
        or os.getuid() == 0
        or os.geteuid() == 0
        or os.uname().machine != "x86_64"
    ):
        raise TaskError("unsupported-headless-host")
    if (
        not isinstance(source_commit, str)
        or re.fullmatch(r"[0-9a-f]{40}", source_commit) is None
        or source_commit == "0" * 40
    ):
        raise TaskError("invalid-source-commit")
    facts = host_facts()
    is_baseline = baseline_host(facts)
    if require_baseline_host and not is_baseline:
        raise TaskError("baseline-host-required")
    if shutil.which("bwrap") is None:
        raise TaskError("headless-sandbox-unavailable")
    # Reject unsafe input before copying, then verify the private snapshot used
    # for extraction and execution rather than reopening the caller's pathname.
    package_server.verify_archive(archive)
    with tempfile.TemporaryDirectory(prefix="spaceterm-packaged-headless-") as temporary:
        directory = Path(temporary)
        snapshot = directory / "archive.tar.gz"
        package_server.copy_file(archive, snapshot, 0o600)
        manifest = package_server.verify_archive(snapshot)
        expected = {key: manifest[key] for key in server_artifacts.METADATA_FIELDS}
        server_artifacts.validate_metadata(expected, "linux-x86_64")
        root = directory / "root"
        (root / "opt").mkdir(parents=True)
        with tarfile.open(snapshot, "r:gz") as source:
            source.extractall(root / "opt", filter="data")
        package = root / "opt/spaceterm-server"
        if (package / "bin/spaceterm-cli").exists():
            raise TaskError("packaged-client-present")
        linux.verify_elf_closure(package)
        executable = package / "bin/spaceterm-server"
        libraries = linux.verify_elf(executable)
        copy_runtime(root, libraries)
        home = directory / "home"
        runtime = directory / "runtime"
        for path in (home, runtime):
            path.mkdir(mode=0o700)
        for path in ("home/account", "runtime", "proc", "dev", "tmp"):
            (root / path).mkdir(parents=True, exist_ok=True)

        def query(arguments, *, reject_stderr=False):
            return native.run_tool(
                sandbox_command(root, home, runtime, arguments),
                timeout=5,
                maximum=4096,
                cwd="/",
                environment=native.ENVIRONMENT,
                reject_stderr=reject_stderr,
            )

        actual = server_artifacts.validate_metadata(
            server_artifacts.strict_json(
                query(["--distribution-metadata"], reject_stderr=True), 4096
            ),
            "linux-x86_64",
        )
        if actual != expected:
            raise TaskError("packaged-metadata-mismatch")
        help_text = query(["--help"], reject_stderr=True)
        version = query(["--version"], reject_stderr=True)
        if (
            b"usage: spaceterm-server" not in help_text
            or version != ("spaceterm-server " + expected["version"] + "\n").encode()
        ):
            raise TaskError("packaged-query-mismatch")
        invalid = (
            [],
            ["--unknown"],
            ["--distribution-metadata", "unexpected-argument"],
            ["serve", "unexpected-argument"],
        )
        for arguments in invalid:
            try:
                query(arguments)
            except TaskError as error:
                if str(error) != "tool-failed":
                    raise
            else:
                raise TaskError("invalid-packaged-arguments-accepted")
        if any(home.iterdir()) or any(runtime.iterdir()):
            raise TaskError("packaged-query-mutated-state")
        return {
            "schema": 1,
            "classification": "packaged-queries-passed",
            "source_commit": source_commit,
            "archive_sha256": server_artifacts.digest(snapshot),
            "metadata": actual,
            "host": facts,
            "baseline_host": is_baseline,
            "uid": os.getuid(),
            "libraries": sorted(libraries),
            "queries": 3 + len(invalid),
            "service_lifecycle_tested": False,
            "runtime_readiness_tested": False,
        }


def cli(arguments):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--require-baseline-host", action="store_true")
    args, unknown = parser.parse_known_args(arguments)
    if unknown:
        raise TaskError("invalid-headless-arguments")
    result = run(
        args.archive,
        args.source_commit,
        require_baseline_host=args.require_baseline_host,
    )
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    main(lambda: cli(sys.argv[1:]))
