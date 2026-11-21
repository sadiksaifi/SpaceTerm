#!/usr/bin/env python
# MISE description="Build libspaceterm from one annotated Git tag in an external Cargo project"
# USAGE arg "<target>" help="Mobile Rust target"
# USAGE arg "<tag>" help="Annotated libspaceterm Git tag"
"""Build libspaceterm from one annotated Git tag in an external Cargo project."""

import argparse
import json
import subprocess
import tempfile
import tomllib
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, main

CRATES = ("spaceterm-protocol", "spaceterm-client", "spaceterm-terminal-client")
TARGETS = ("aarch64-apple-ios", "aarch64-linux-android")


def build(target, tag):
    tag_type = subprocess.run(
        ["git", "cat-file", "-t", f"refs/tags/{tag}"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if tag_type.returncode or tag_type.stdout.strip() != "tag":
        raise TaskError("An annotated libspaceterm tag must exist in this Git checkout")

    source = ROOT.as_uri()
    cargo_tag = json.dumps(tag, ensure_ascii=False)
    with (ROOT / "rust-toolchain.toml").open("rb") as toolchain_file:
        toolchain = tomllib.load(toolchain_file)["toolchain"]["channel"]

    with tempfile.TemporaryDirectory(prefix="spaceterm-mobile-consumer-") as directory:
        consumer = Path(directory)
        # Keep Cargo's default features so the check matches a new mobile consumer.
        dependencies = "".join(
            f'{crate} = {{ git = "{source}", tag = {cargo_tag} }}\n' for crate in CRATES
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
    parser.add_argument("tag")
    args = parser.parse_args()
    main(lambda: build(args.target, args.tag))
