#!/usr/bin/env python3
"""Resolve application identity from Git. Release builds require an annotated tag."""

import argparse
import json
import re
import subprocess
from pathlib import Path

STABLE_VERSION = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\Z")


class VersionError(Exception):
    pass


def git(root, *args):
    result = subprocess.run(
        ["git", "-C", str(root), *args], capture_output=True, text=True, check=False
    )
    if result.returncode:
        raise VersionError("Git release identity is unavailable")
    return result.stdout.strip()


def resolve(root, tag=None, require_clean=False):
    commit = git(root, "rev-parse", "HEAD")
    dirty = bool(git(root, "status", "--porcelain", "--untracked-files=normal"))
    if tag is not None:
        if not tag.startswith("v") or not STABLE_VERSION.fullmatch(tag[1:]):
            raise VersionError("release tag must be v followed by a canonical stable SemVer")
        ref = f"refs/tags/{tag}"
        if git(root, "cat-file", "-t", ref) != "tag":
            raise VersionError("release tag must be annotated")
        if git(root, "rev-parse", f"{ref}^{{commit}}") != commit:
            raise VersionError("release tag must identify the build commit")
        if require_clean and dirty:
            raise VersionError("release packaging requires a clean checkout")
        return {"version": tag[1:], "bundle_version": tag[1:], "commit": commit, "release": True}
    return {
        "version": f"dev.{commit[:12]}" + (".dirty" if dirty else ""),
        "bundle_version": "0.0.0",
        "commit": commit,
        "release": False,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--tag")
    parser.add_argument("--require-clean", action="store_true")
    parser.add_argument("--field", choices=("version", "bundle_version", "commit"))
    parser.add_argument("--cargo", action="store_true")
    args = parser.parse_args()
    try:
        identity = resolve(args.root, args.tag, args.require_clean)
    except VersionError as error:
        parser.exit(1, f"error: {error}\n")
    if args.cargo:
        print(f"cargo:rustc-env=SPACETERM_VERSION={identity['version']}")
        print(f"cargo:rustc-env=SPACETERM_BUNDLE_VERSION={identity['bundle_version']}")
    elif args.field:
        print(identity[args.field])
    else:
        print(json.dumps(identity))


if __name__ == "__main__":
    main()
