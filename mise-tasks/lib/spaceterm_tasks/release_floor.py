"""The Update Floor of each product, computed from the Data Generations releases declared.

`crates/spaceterm-core/src/migration/generations.rs` declares every data root on one line, as
`pub const CORE_DATA: Generations = Generations { oldest: 1, current: 1 };`. A product's floor
is the oldest published release whose `current` in every root of that product is at least the
root's `oldest` today. A release that predates the file is not eligible. The first release
with the file is its own floor, and a release whose roots no published release reaches is
refused. ADR 0064 owns the rule.
"""

import re
import subprocess
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError
from spaceterm_tasks.packaging import STABLE_TAG

SOURCE = Path("crates/spaceterm-core/src/migration/generations.rs")
FIXTURES = Path("fixtures/migrations")
# Each data root's constant and its fixture directory.
ROOTS = {
    "CORE_DATA": "core-data",
    "CLI_STATE": "cli-state",
    "DESKTOP_CONFIG": "desktop-config",
    "DESKTOP_STATE": "desktop-state",
}
# The roots whose data each product's updater carries forward.
PRODUCTS = {"core": ("CORE_DATA", "CLI_STATE"), "desktop": ("DESKTOP_CONFIG", "DESKTOP_STATE")}


def generations(text):
    """Each root's `(oldest, current)` as `text`, a generations.rs source, declares them."""
    result = {}
    for name in ROOTS:
        found = re.findall(
            rf"^pub const {name}: Generations = "
            r"Generations \{ oldest: ([1-9][0-9]*), current: ([1-9][0-9]*) \};$",
            text,
            re.MULTILINE,
        )
        if len(found) != 1:
            raise TaskError(f"the Data Generation source must declare {name} once")
        oldest, current = (int(part) for part in found[0])
        if oldest > current:
            raise TaskError(f"{name} has an oldest generation above its current one")
        result[name] = (oldest, current)
    return result


def _git(root, *arguments):
    result = subprocess.run(["git", *arguments], cwd=root, capture_output=True, text=True)
    if result.returncode:
        raise TaskError("Git could not read the released Data Generations")
    return result.stdout


def _order(version):
    return tuple(int(part) for part in version.split("."))


def released(version, root=ROOT):
    """The Data Generations release `version` declared, or None when it predates the file."""
    tag = f"v{version}"
    if not _git(root, "ls-tree", "--name-only", tag, "--", str(SOURCE)).strip():
        return None
    return generations(_git(root, "show", f"{tag}:{SOURCE}"))


def releases(version, root=ROOT):
    """Every stable release older than `version` that declares Data Generations, oldest first."""
    older = sorted(
        (tag[1:] for tag in _git(root, "tag", "--list").split() if STABLE_TAG.fullmatch(tag)),
        key=_order,
    )
    result = []
    for candidate in older:
        if _order(candidate) < _order(version):
            declared = released(candidate, root)
            if declared is not None:
                result.append((candidate, declared))
    return result


def _current(root):
    try:
        return generations((Path(root) / SOURCE).read_text())
    except OSError:
        raise TaskError("the Data Generation source is unavailable") from None


def floor(product, version, root=ROOT, *, history=None):
    """The Update Floor of `product` for release `version`, built from the tree at `root`."""
    current = _current(root)
    history = releases(version, root) if history is None else history
    if not history:
        return version
    for candidate, declared in history:
        if all(declared[name][1] >= current[name][0] for name in PRODUCTS[product]):
            return candidate
    raise TaskError(
        f"no published release reaches the oldest {product} Data Generations; "
        "a release must keep a step from a published one"
    )


def floors(version, root=ROOT):
    """Both products' floors for release `version`."""
    history = releases(version, root)
    return {product: floor(product, version, root, history=history) for product in PRODUCTS}


def published(version):
    """Whether GitHub lists `version` as a published, non-draft release."""
    result = subprocess.run(
        ["gh", "release", "view", f"v{version}", "--json", "isDraft", "--jq", ".isDraft"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    return result.returncode == 0 and result.stdout.strip() == "false"


def check(version, root=ROOT, published=published):
    """The release-only Data Generation checks for release `version`; returns both floors.

    Against the previous release, no root's `oldest` or `current` decreases. Every generation
    from `oldest` to `current` of every root has a fixture directory with a file in it. Each
    floor is this release or a published one.
    """
    current = _current(root)
    history = releases(version, root)
    if history:
        previous = history[-1][1]
        for name, (oldest, newest) in current.items():
            if oldest < previous[name][0] or newest < previous[name][1]:
                raise TaskError(f"{name} decreased against the previous release")
    for name, (oldest, newest) in current.items():
        for generation in range(oldest, newest + 1):
            fixture = Path(root) / FIXTURES / ROOTS[name] / f"gen-{generation}"
            if not fixture.is_dir() or not any(path.is_file() for path in fixture.rglob("*")):
                raise TaskError(f"no migration fixture in {ROOTS[name]}/gen-{generation}")
    result = {product: floor(product, version, root, history=history) for product in PRODUCTS}
    for product, found in result.items():
        if found != version and not published(found):
            raise TaskError(f"the {product} floor {found} is not a published release")
    return result
