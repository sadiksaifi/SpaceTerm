"""Nonempty account/provider gates and explicit owner-only lifecycle commands.

Normal validation never installs, starts, stops, or uninstalls a service, enables
linger, or invokes an administrator tool. The lifecycle entry point requires a
separate mutation flag and delegates the fixed action to the account CLI.
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
            "two_client_control",
            "terminal_resources_wss",
        ),
    },
    "cli": {"spaceterm-client-native": ("cli_arguments", "cli_management", "cli_protocol")},
    "bootstrap": {
        "spaceterm-client-native": ("bootstrap",),
        "spaceterm-client": ("pairing_bootstrap",),
        "spaceterm-host": ("bootstrap_probe", "bootstrap_process", "bootstrap_linger"),
    },
}
MUTATIONS = {"install", "start", "stop", "uninstall"}


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
    if not announced or len(summaries) != len(announced) or completed is None:
        raise TaskError("the scoped test gate did not execute its target")
    passed, failed, ignored, measured = (int(count) for count in completed.groups())
    if failed or passed == 0 or passed + ignored + measured != int(announced[0]):
        raise TaskError("the scoped test gate did not execute its target")
    return passed


def test_gate(name):
    environment = signing_environment(os.environ)
    environment["CARGO_BUILD_JOBS"] = "5"
    total = 0
    target_count = 0
    for package, targets in GATES[name].items():
        for target in targets:
            if name == "account" and target == "pty_inheritance" and sys.platform != "darwin":
                continue
            command = ["cargo", "test", "--quiet", "--locked", "-p", package, "--test", target]
            if name == "account":
                command.extend(("--features", "test-support"))
            output = native.run_tool(
                command, cwd=ROOT, environment=environment, timeout=300, maximum=1024 * 1024
            )
            count = require_nonempty(output)
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


def lifecycle(executable, operation, platform, build, *, allow_mutation=False):
    if operation not in MUTATIONS | {"status"}:
        raise TaskError("unsupported account lifecycle operation")
    if operation in MUTATIONS and allow_mutation is not True:
        raise TaskError("explicit account-service mutation approval is required")
    if os.getuid() == 0:
        raise TaskError("SpaceTerm account commands cannot run as root")
    # The compiled build chooses the registration. No runtime identity or
    # executable path is sent to the provider as an installation authority.
    package_server.executable_metadata(executable, platform, build)
    output = native.run_tool(
        [executable, "--json", "--non-interactive", operation],
        timeout=60,
        maximum=64 * 1024,
        reject_stderr=True,
    )
    document = server_artifacts.strict_json(output, 64 * 1024)
    if (
        not isinstance(document, dict)
        or type(document.get("schema")) is not int
        or document["schema"] != 1
        or not isinstance(document.get("result"), dict)
    ):
        raise TaskError("account CLI returned an unsupported result")
    # Exit zero is the CLI's completion contract. This task does not manufacture
    # cleanup evidence or substitute for the coordinator's host observations.
    return document


def run(arguments):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=(*GATES, "startup", "lifecycle"))
    parser.add_argument("--executable", type=Path)
    parser.add_argument("--build", choices=server_artifacts.BUILDS, default="preflight")
    parser.add_argument("--operation", choices=sorted(MUTATIONS | {"status"}), default="status")
    parser.add_argument("--allow-mutation", action="store_true")
    args = parser.parse_args(arguments)
    if args.mode in GATES:
        test_gate(args.mode)
    else:
        if args.mode == "lifecycle" and args.executable is None:
            raise TaskError("account lifecycle requires an explicit installed executable")
        executable = args.executable or package_server.build()["spaceterm-server"]
        platform = "darwin-arm64" if sys.platform == "darwin" else "linux-x86_64"
        if args.mode == "startup":
            check_startup(executable, platform, args.build)
        else:
            lifecycle(
                executable, args.operation, platform, args.build, allow_mutation=args.allow_mutation
            )
        print(f"phase={args.mode} classification=cli-succeeded")
    return 0


if __name__ == "__main__":
    main(lambda: run(sys.argv[1:]))
