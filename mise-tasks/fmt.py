#!/usr/bin/env python
# MISE description="Format Rust and Python sources"
# USAGE flag "--check" help="Report unformatted files without changing them"
"""Format Rust and Python sources."""

import argparse
import subprocess

from spaceterm_tasks import ROOT, TaskError, main

# rustfmt does not follow main.rs's include!, and Adapter tests compile only on their
# platform, so name those files explicitly to format every platform's sources on any host.
UNREACHED_RUST = ("src/application_modules.rs", "src/platform/*_adapter_tests/*.rs")


def run(check):
    rustfmt = ["--check"] if check else []
    files = sorted(
        str(path.relative_to(ROOT)) for pattern in UNREACHED_RUST for path in ROOT.glob(pattern)
    )
    commands = (
        ["cargo", "fmt", "--all", *(["--", "--check"] if check else [])],
        ["rustfmt", "--edition", "2024", *rustfmt, *files],
        ["ruff", "format", *rustfmt],
    )
    failed = [command[0] for command in commands if subprocess.run(command, cwd=ROOT).returncode]
    if failed:
        raise TaskError(
            f"{', '.join(failed)} found unformatted files"
            if check
            else f"{', '.join(failed)} could not format every file"
        )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    main(lambda: run(args.check))
