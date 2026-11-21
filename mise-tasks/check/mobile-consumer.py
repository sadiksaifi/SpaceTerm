#!/usr/bin/env python
# MISE description="Build the mobile crates as Git dependencies in an external Cargo project"
# USAGE arg "<target>" help="Mobile Rust target"
# USAGE arg "<revision>" help="Candidate Git commit"
"""Build the mobile crates as Git dependencies in an external Cargo project."""

import argparse
import subprocess
import tempfile
import tomllib
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, main

CRATES = ("spaceterm-protocol", "spaceterm-client", "spaceterm-terminal-client")
TARGETS = ("aarch64-apple-ios", "aarch64-linux-android")


def build(target, revision):
    candidate = subprocess.run(
        ["git", "rev-parse", "--verify", "--end-of-options", f"{revision}^{{commit}}"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if candidate.returncode:
        raise TaskError("Candidate commit is unavailable in this Git checkout")

    commit = candidate.stdout.strip()
    source = ROOT.as_uri()
    with (ROOT / "rust-toolchain.toml").open("rb") as toolchain_file:
        toolchain = tomllib.load(toolchain_file)["toolchain"]["channel"]

    with tempfile.TemporaryDirectory(prefix="spaceterm-mobile-consumer-") as directory:
        consumer = Path(directory)
        # Keep Cargo's default features so the check matches a new mobile consumer.
        dependencies = "".join(
            f'{crate} = {{ git = "{source}", rev = "{commit}" }}\n' for crate in CRATES
        )
        (consumer / "Cargo.toml").write_text(
            '[package]\nname = "spaceterm-mobile-consumer"\nversion = "0.0.0"\n'
            'edition = "2024"\npublish = false\n\n[dependencies]\n' + dependencies,
            encoding="utf-8",
        )
        (consumer / "src").mkdir()
        imports = "".join(f"use {crate.replace('-', '_')} as _;\n" for crate in CRATES)
        (consumer / "src/main.rs").write_text(imports + "\nfn main() {}\n", encoding="utf-8")
        return subprocess.run(
            ["cargo", f"+{toolchain}", "build", "--target", target], cwd=consumer
        ).returncode


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=TARGETS)
    parser.add_argument("revision")
    args = parser.parse_args()
    main(lambda: build(args.target, args.revision))
