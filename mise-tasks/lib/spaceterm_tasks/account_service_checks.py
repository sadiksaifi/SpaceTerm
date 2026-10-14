"""Nonempty account/provider gates without installed-service effects.

Normal validation never installs, starts, stops, or uninstalls a service, enables
linger, or invokes an administrator tool. Real lifecycle checks use spaceterm-cli
explicitly, as documented in the coordinator's real-host runbook.
"""

import argparse
import os
import re
import sys
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, main, package_server, server_artifacts
from spaceterm_tasks import server_vm_linux as native
from spaceterm_tasks.macos_signing import signing_environment

GATES = {
    "account": {
        "spaceterm-host": (
            "account_service",
            "server_keys",
            "server_installation",
            "owner_enrollment",
            "ipc",
            "pty_inheritance",
        ),
    },
    "vertical": {
        "spaceterm-server": (
            "authenticated_local_vertical",
            "authenticated_route_equivalence",
            "production_listeners",
        ),
        "spaceterm-terminal": ("geometry_admission",),
    },
    # Real keys, SQLite, IPC and shell startup. Only service-manager and
    # authorization boundaries use fixtures; no installed service is touched.
    "startup": {"spaceterm-server": ("account_startup", "production_listeners")},
    "cli": {"spaceterm-client-native": ("cli_arguments", "cli_management", "cli_protocol")},
    "bootstrap": {
        "spaceterm-client-native": ("bootstrap",),
        "spaceterm-client": ("pairing_bootstrap",),
        "spaceterm-host": ("bootstrap_probe", "bootstrap_process", "bootstrap_linger"),
    },
    # Signed archive fixtures and injected service managers remain distinct from
    # the real Git subprocess, native PTY and authenticated replacement client.
    # This gate never supplies installed-service or release-signing evidence.
    "update": {
        "spaceterm-host": (
            "server_installation",
            "account_service_lifecycle",
            "account_service_managers",
        ),
        "spaceterm-server": ("git_shutdown",),
    },
}


def require_nonempty(output):
    # One Cargo invocation selects one target. Re-executed fixture harnesses can
    # inherit stdout; only the target's final summary supplies its executed count.
    announced = re.findall(rb"(?m)^running ([0-9]+) tests?[^\S\r\n]*$", output)
    summaries = re.findall(rb"(?m)^test result: (?:ok|FAILED)\. ", output)
    completed = re.search(
        rb"(?m)^test result: ok\. ([0-9]+) passed; ([0-9]+) failed; "
        rb"([0-9]+) ignored; ([0-9]+) measured; [0-9]+ filtered out; "
        rb"finished in [0-9.]+s\s*\Z",
        output,
    )
    if not announced or completed is None:
        raise TaskError("the scoped test gate did not execute its target")
    if len(summaries) != len(announced):
        # Power-loss/process-kill suites intentionally terminate one-test child
        # harnesses without a summary. Accept only an unambiguous larger parent
        # target whose final completed count still matches its own announcement.
        if len(summaries) > len(announced) or int(announced[0]) <= max(
            (int(count) for count in announced[1:]), default=0
        ):
            raise TaskError("the scoped test gate did not execute its target")
    passed, failed, ignored, measured = (int(count) for count in completed.groups())
    if failed or passed == 0 or passed + ignored + measured != int(announced[0]):
        raise TaskError("the scoped test gate did not execute its target")
    return passed


def test_gate(name):
    environment = signing_environment(os.environ)
    environment["CARGO_BUILD_JOBS"] = "4"
    if name == "startup":
        # Compile the fixture dependency graph before the bounded test runs.
        # A cold build is not evidence that runtime startup exceeded its bound.
        package_server.build_executable(
            "--locked",
            "-p",
            "spaceterm-server",
            "--features",
            "test-support",
            binary="spaceterm-server",
            env=environment,
        )
    total = 0
    target_count = 0
    for package, targets in GATES[name].items():
        for target in targets:
            if name == "account" and target == "pty_inheritance" and sys.platform != "darwin":
                continue
            command = ["cargo", "test", "--quiet", "--locked", "-p", package]
            if package == "spaceterm-client-native" and target == "bootstrap":
                # The native provider suite lives in the library, not tests/bootstrap.rs.
                command.extend(("--lib", "bootstrap::"))
            else:
                command.extend(("--test", target))
            if name in {"account", "update", "startup"}:
                command.extend(("--features", "test-support"))
            elif package == "spaceterm-terminal":
                command.extend(("--features", "runtime"))
            try:
                output = native.run_tool(
                    command,
                    cwd=ROOT,
                    environment=environment,
                    timeout=300,
                    maximum=1024 * 1024,
                    reject_stderr=False,
                )
            except TaskError as error:
                classification = str(error)
                if classification not in {
                    "tool-failed",
                    "tool-timeout",
                    "tool-output-limit",
                    "tool-cleanup-unresolved",
                    "child-ownership-unavailable",
                }:
                    classification = "tool-failed"
                raise TaskError(
                    f"package={package} target={target} classification={classification}"
                ) from None
            try:
                count = require_nonempty(output)
            except TaskError:
                raise TaskError(
                    f"package={package} target={target} classification=target-incomplete"
                ) from None
            total += count
            target_count += 1
            print(f"package={package} target={target} tests={count} classification=passed")
    print(f"targets={target_count} tests={total} classification=passed")
    return total


def check_startup(executable, platform, build):
    """Exercise real copied-metadata prerequisites without entering serve."""
    metadata = package_server.executable_metadata(executable, platform, build)
    help_text = native.run_tool([executable, "--help"], timeout=5, maximum=4096, reject_stderr=True)
    version = native.run_tool(
        [executable, "--version"], timeout=5, maximum=4096, reject_stderr=True
    )
    if not help_text.strip() or metadata["version"].encode() not in version:
        raise TaskError("server startup query did not match its metadata")
    for arguments in (("--distribution-metadata", "unexpected-argument"), ("--unknown",)):
        try:
            native.run_tool([executable, *arguments], timeout=5, maximum=4096)
        except TaskError as error:
            if str(error) != "tool-failed":
                raise
        else:
            raise TaskError("invalid server arguments were accepted")
    return metadata


def run(arguments):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=tuple(GATES))
    parser.add_argument("--executable", type=Path)
    parser.add_argument("--build", choices=server_artifacts.BUILDS, default="preflight")
    args = parser.parse_args(arguments)
    if args.mode in GATES and args.mode != "startup":
        test_gate(args.mode)
    else:
        executable = args.executable or package_server.build()["spaceterm-server"]
        platform = "darwin-arm64" if sys.platform == "darwin" else "linux-x86_64"
        check_startup(executable, platform, args.build)
        test_gate("startup")
        print(f"phase={args.mode} classification=cli-succeeded")
    return 0


if __name__ == "__main__":
    main(lambda: run(sys.argv[1:]))
