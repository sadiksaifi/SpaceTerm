#!/usr/bin/env python3
"""Build one executable while recording Cargo's authoritative artifact path."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--bin", required=True)
    parser.add_argument("cargo_args", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.cargo_args[:1] == ["--"]:
        args.cargo_args = args.cargo_args[1:]
    return args


def main() -> int:
    args = parse_args()
    command = [
        "cargo",
        "build",
        "--bin",
        args.bin,
        "--message-format=json-render-diagnostics",
        *args.cargo_args,
    ]
    process = subprocess.Popen(command, stdout=subprocess.PIPE, text=True)
    assert process.stdout is not None
    executables: set[Path] = set()
    for line in process.stdout:
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            print(line, end="")
            continue
        rendered = message.get("message", {}).get("rendered")
        if rendered:
            print(rendered, end="", file=sys.stderr)
        if (
            message.get("reason") == "compiler-artifact"
            and message.get("target", {}).get("name") == args.bin
            and message.get("executable")
        ):
            executables.add(Path(message["executable"]).resolve())

    status = process.wait()
    if status != 0:
        return status
    if len(executables) != 1:
        print(
            f"error: Cargo reported {len(executables)} executable paths for {args.bin}",
            file=sys.stderr,
        )
        return 2

    args.output.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(dir=args.output.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            output.write(f"{executables.pop()}\n")
        os.replace(temporary, args.output)
    except BaseException:
        Path(temporary).unlink(missing_ok=True)
        raise
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
