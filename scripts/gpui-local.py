#!/usr/bin/env python3
"""Select a local Zed checkout for SpaceTerm's pinned GPUI dependencies."""

import argparse
from pathlib import Path
import subprocess
import tomllib


FORK_URL = "https://github.com/sadiksaifi/zed"
ROOT = Path(__file__).resolve().parent.parent
CONFIG = ROOT / ".cargo" / "config.toml"


def local_crates(checkout: Path) -> dict[str, Path]:
    crates = {}
    for directory in ("crates", "tooling"):
        manifests = sorted((checkout / directory).glob("**/Cargo.toml"), key=lambda path: len(path.parts))
        for manifest in manifests:
            package = tomllib.loads(manifest.read_text()).get("package")
            if package:
                crates.setdefault(package["name"], manifest.parent)
    return crates


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("on", "off"))
    parser.add_argument("checkout", nargs="?", default="../zed")
    args = parser.parse_args()

    if args.mode == "off":
        CONFIG.unlink(missing_ok=True)
        return

    checkout = (ROOT / args.checkout).resolve()
    if not (checkout / "SPACETERM.md").is_file():
        parser.error(f"not a SpaceTerm Zed checkout: {checkout}")

    committed_lock = subprocess.run(
        ["git", "show", "HEAD:Cargo.lock"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    lock = tomllib.loads(committed_lock)
    names = sorted(
        package["name"]
        for package in lock["package"]
        if package.get("source", "").startswith(f"git+{FORK_URL}")
    )
    crates = local_crates(checkout)
    missing = sorted(set(names) - crates.keys())
    if missing:
        parser.error(f"fork crates missing from checkout: {', '.join(missing)}")

    lines = [f'[patch."{FORK_URL}"]']
    for name in names:
        path = crates[name].as_posix().replace("\\", "\\\\").replace('"', '\\"')
        lines.append(f'{name} = {{ path = "{path}" }}')
    CONFIG.parent.mkdir(exist_ok=True)
    CONFIG.write_text("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
