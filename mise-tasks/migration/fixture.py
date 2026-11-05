#!/usr/bin/env python
# MISE description="Capture a data root's fixture at its current Data Generation"
# USAGE arg "<root>" help="The data root" {
# USAGE   choices "core-data" "cli-state" "desktop-config" "desktop-state"
# USAGE }
"""Write fixtures/migrations/<root>/gen-<current>/ with this build's own writers."""

import argparse
import subprocess

from spaceterm_tasks import ROOT, TaskError, main

# Each root's capture is an ignored test in the crate that owns the root.
CAPTURES = {
    "core-data": (
        ["--package", "spaceterm-core", "--lib", "--features", "client"],
        "migration::core_data::tests::capture_core_data_fixture",
    ),
    "cli-state": (
        ["--package", "spaceterm-cli", "--lib"],
        "state::tests::capture_cli_state_migration_fixture",
    ),
    "desktop-config": (
        ["--package", "spaceterm", "--lib", "--all-features"],
        "migration::tests::capture_desktop_config_migration_fixture",
    ),
    "desktop-state": (
        ["--package", "spaceterm", "--lib", "--all-features"],
        "migration::tests::capture_desktop_state_migration_fixture",
    ),
}


def capture(root):
    package, test = CAPTURES[root]
    command = ["cargo", "test", "--locked", *package, "--", "--ignored", "--exact", test]
    if subprocess.run(command, cwd=ROOT, check=False).returncode != 0:
        raise TaskError(f"capturing the {root} fixture failed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", choices=sorted(CAPTURES))
    args = parser.parse_args()
    main(lambda: capture(args.root))
