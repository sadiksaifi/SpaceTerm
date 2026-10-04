#!/usr/bin/env python3
"""Select a local AccessKit checkout for SpaceTerm's pinned accessibility crates."""

import argparse
import json
from pathlib import Path
import tomllib


ROOT = Path(__file__).resolve().parent.parent
CONFIG = ROOT / ".cargo" / "config.toml"
BLOCK_START = "# BEGIN SpaceTerm local AccessKit patches\n"
BLOCK_START_WITH_SEPARATOR = "# BEGIN SpaceTerm local AccessKit patches (added newline)\n"
BLOCK_END = "# END SpaceTerm local AccessKit patches\n"
CRATES = {
    "accesskit": "common",
    "accesskit_consumer": "consumer",
    "accesskit_atspi_common": "platforms/atspi-common",
}


def without_owned_block(contents: str) -> tuple[str, bool]:
    lines = contents.splitlines(keepends=True)
    starts = [i for i, line in enumerate(lines) if line in (BLOCK_START, BLOCK_START_WITH_SEPARATOR)]
    ends = [i for i, line in enumerate(lines) if line == BLOCK_END]
    if not starts and not ends:
        return contents, False
    if len(starts) != 1 or len(ends) != 1 or starts[0] >= ends[0]:
        raise ValueError("local AccessKit patch markers are incomplete")
    start = sum(map(len, lines[: starts[0]]))
    end = sum(map(len, lines[: ends[0] + 1]))
    if lines[starts[0]] == BLOCK_START_WITH_SEPARATOR:
        # This marker owns the newline immediately before it.
        if start == 0 or contents[start - 1] != "\n":
            raise ValueError("local AccessKit patch separator is missing")
        start -= 1
    return contents[:start] + contents[end:], True


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("on", "off"))
    parser.add_argument("checkout", nargs="?", default="../accesskit")
    args = parser.parse_args()
    contents = CONFIG.read_text() if CONFIG.exists() else ""
    try:
        remaining, owned = without_owned_block(contents)
        configuration = tomllib.loads(remaining)
    except (ValueError, tomllib.TOMLDecodeError) as error:
        parser.error(str(error))

    if args.mode == "off":
        if owned:
            if remaining:
                CONFIG.write_text(remaining)
            else:
                CONFIG.unlink()
        return

    if "crates-io" in configuration.get("patch", {}):
        parser.error("crates.io patch table already exists outside the owned block")
    checkout = (ROOT / args.checkout).resolve()
    if not (checkout / "SPACETERM.md").is_file():
        parser.error(f"not a SpaceTerm AccessKit checkout: {checkout}")
    lines = ["[patch.crates-io]"]
    for name, directory in CRATES.items():
        manifest = checkout / directory / "Cargo.toml"
        if not manifest.is_file() or tomllib.loads(manifest.read_text()).get("package", {}).get("name") != name:
            parser.error(f"AccessKit crate missing from checkout: {name}")
        lines.append(f"{name} = {{ path = {json.dumps(manifest.parent.as_posix())} }}")
    CONFIG.parent.mkdir(exist_ok=True)
    # Each block owns its leading newline, including beside another owned block.
    separator = "\n"
    marker = BLOCK_START_WITH_SEPARATOR
    CONFIG.write_text(remaining + separator + marker + "\n".join(lines) + "\n" + BLOCK_END)


if __name__ == "__main__":
    main()
