"""Point pinned Git dependencies at local SpaceTerm fork checkouts through .cargo/config.toml.

Each Override owns one marked block in the file and leaves everything else untouched, so
GPUI and AccessKit overrides can be enabled and disabled in any order.
"""

import json
import subprocess
import tomllib
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError

ZED_FORK = "https://github.com/sadiksaifi/zed"
ACCESSKIT_CRATES = {
    "accesskit": "common",
    "accesskit_consumer": "consumer",
    "accesskit_atspi_common": "platforms/atspi-common",
    "accesskit_macos": "platforms/macos",
}


@dataclass(frozen=True)
class Override:
    name: str
    # The patch table key, for example "crates-io" for [patch.crates-io].
    source: str
    # Maps each patched crate to its directory in the checkout.
    crates: Callable[[Path, Path], dict[str, Path]]

    @property
    def start(self):
        return f"# BEGIN SpaceTerm local {self.name} patches\n"

    @property
    def start_with_separator(self):
        return f"# BEGIN SpaceTerm local {self.name} patches (added newline)\n"

    @property
    def end(self):
        return f"# END SpaceTerm local {self.name} patches\n"


def zed_crates(root, checkout):
    """Every crate the committed lockfile takes from the Zed fork, found in the checkout."""
    committed = subprocess.run(
        ["git", "show", "HEAD:Cargo.lock"], cwd=root, capture_output=True, text=True
    )
    if committed.returncode:
        raise TaskError("the committed Cargo.lock is unavailable")
    names = sorted(
        package["name"]
        for package in tomllib.loads(committed.stdout)["package"]
        if package.get("source", "").startswith(f"git+{ZED_FORK}")
    )
    available = {}
    for directory in ("crates", "tooling"):
        manifests = sorted(
            (checkout / directory).glob("**/Cargo.toml"), key=lambda path: len(path.parts)
        )
        for manifest in manifests:
            package = tomllib.loads(manifest.read_text()).get("package")
            if package:
                available.setdefault(package["name"], manifest.parent)
    missing = sorted(set(names) - available.keys())
    if missing:
        raise TaskError(f"fork crates missing from the checkout: {', '.join(missing)}")
    return {name: available[name] for name in names}


def accesskit_crates(_root, checkout):
    crates = {}
    for name, directory in ACCESSKIT_CRATES.items():
        manifest = checkout / directory / "Cargo.toml"
        if (
            not manifest.is_file()
            or tomllib.loads(manifest.read_text()).get("package", {}).get("name") != name
        ):
            raise TaskError(f"AccessKit crate missing from the checkout: {name}")
        crates[name] = manifest.parent
    return crates


GPUI = Override("GPUI", ZED_FORK, zed_crates)
ACCESSKIT = Override("AccessKit", "crates-io", accesskit_crates)


def config_path(root):
    return root / ".cargo" / "config.toml"


def without_owned_block(override, contents):
    lines = contents.splitlines(keepends=True)
    starts = [
        index
        for index, line in enumerate(lines)
        if line in (override.start, override.start_with_separator)
    ]
    ends = [index for index, line in enumerate(lines) if line == override.end]
    if not starts and not ends:
        return contents, False
    if len(starts) != 1 or len(ends) != 1 or starts[0] >= ends[0]:
        raise TaskError(f"local {override.name} patch markers are incomplete")
    start = sum(map(len, lines[: starts[0]]))
    end = sum(map(len, lines[: ends[0] + 1]))
    if lines[starts[0]] == override.start_with_separator:
        # This marker owns the newline immediately before it.
        if start == 0 or contents[start - 1] != "\n":
            raise TaskError(f"local {override.name} patch separator is missing")
        start -= 1
    return contents[:start] + contents[end:], True


def read_config(override, root):
    path = config_path(root)
    contents = path.read_text() if path.exists() else ""
    remaining, owned = without_owned_block(override, contents)
    try:
        configuration = tomllib.loads(remaining)
    except tomllib.TOMLDecodeError:
        raise TaskError("the Cargo configuration is invalid") from None
    return remaining, owned, configuration


def enable(override, checkout, root=ROOT):
    """Patch the dependency to a local checkout, replacing this override's previous block."""
    remaining, _, configuration = read_config(override, root)
    if override.source in configuration.get("patch", {}):
        raise TaskError(f"a {override.name} patch table already exists outside the owned block")
    checkout = (root / checkout).resolve()
    if not (checkout / "SPACETERM.md").is_file():
        raise TaskError(f"not a SpaceTerm {override.name} checkout")
    table = override.source if override.source == "crates-io" else json.dumps(override.source)
    lines = [f"[patch.{table}]"]
    for name, directory in override.crates(root, checkout).items():
        path = json.dumps(directory.as_posix(), ensure_ascii=False)
        lines.append(f"{name} = {{ path = {path} }}")
    # Each block owns its leading newline, including beside another owned block.
    generated = (
        remaining + "\n" + override.start_with_separator + "\n".join(lines) + "\n" + override.end
    )
    try:
        tomllib.loads(generated)
    except tomllib.TOMLDecodeError:
        raise TaskError(f"the generated local {override.name} configuration is invalid") from None
    config_path(root).parent.mkdir(exist_ok=True)
    config_path(root).write_text(generated)


def disable(override, root=ROOT):
    """Restore the pinned dependency by removing only this override's block."""
    remaining, owned, _ = read_config(override, root)
    if not owned:
        return
    if remaining:
        config_path(root).write_text(remaining)
    else:
        config_path(root).unlink()


def refresh_metadata(root=ROOT):
    """Let Cargo resolve the changed sources now rather than at the next build."""
    if subprocess.run(
        ["cargo", "metadata", "--format-version", "1"], cwd=root, stdout=subprocess.DEVNULL
    ).returncode:
        raise TaskError("Cargo could not resolve the selected dependencies")
