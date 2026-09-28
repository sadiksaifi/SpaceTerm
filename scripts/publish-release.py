#!/usr/bin/env python3
"""Upload complete assets to a draft before publishing the GitHub release."""

import argparse
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REPOSITORY = "sadiksaifi/SpaceTerm"


def gh(*arguments, check=True):
    return subprocess.run(["gh", *arguments, "--repo", REPOSITORY], capture_output=True, text=True, check=check)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    args = parser.parse_args()
    directory = ROOT / "dist/release"
    assets = [directory / f"SpaceTerm-{args.tag}-darwin-arm64.dmg", directory / "appcast.xml", directory / "SHA256SUMS"]
    if not all(path.is_file() for path in assets):
        parser.exit(1, "error: complete release assets are required\n")
    existing = gh("release", "view", args.tag, "--json", "isDraft,tagName", check=False)
    if existing.returncode == 0:
        release = json.loads(existing.stdout)
        if not release["isDraft"] or release["tagName"] != args.tag:
            parser.exit(1, "error: published releases must never be overwritten\n")
    else:
        gh("release", "create", args.tag, "--draft", "--verify-tag", "--title", f"SpaceTerm {args.tag}", "--generate-notes")
    gh("release", "upload", args.tag, *(str(path) for path in assets), "--clobber")
    # The public latest/download/appcast.xml URL changes only after every asset is present.
    gh("release", "edit", args.tag, "--draft=false", "--latest")
    print(f"Published SpaceTerm {args.tag} with complete update assets.")


if __name__ == "__main__":
    main()
