"""The Update Floor of each product, computed from the Data Generations releases declared.

`crates/spaceterm-core/src/migration/generations.rs` declares every data root on one line, as
`pub const CORE_DATA: Generations = Generations { oldest: 1, current: 1 };`. A product's floor
is the oldest published, non-withdrawn release whose `current` in every root of that product is
at least the root's `oldest` today. Only a release GitHub lists as published counts, never a
tag alone. A release that predates the file is not eligible. The first release with the file
is its own floor, and a release whose roots no eligible release reaches is refused. ADR 0064
owns the rule.
"""

import re
import subprocess
import tomllib
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError
from spaceterm_tasks.packaging import STABLE_TAG

SOURCE = Path("crates/spaceterm-core/src/migration/generations.rs")
FIXTURES = Path("fixtures/migrations")
# Published releases that are never a floor, because a patch on their line replaced them.
WITHDRAWN = Path("packaging/withdrawn-releases.toml")
VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")
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


def published_releases():
    """The versions of every stable release GitHub lists as published, drafts excluded.

    The GitHub CLI follows every page of the releases API, so no release is left out however
    long the history grows.
    """
    result = subprocess.run(
        [
            "gh",
            "api",
            "--paginate",
            "repos/{owner}/{repo}/releases?per_page=100",
            "--jq",
            ".[] | select(.draft | not) | select(.prerelease | not) | .tag_name",
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode:
        raise TaskError("the GitHub CLI could not list the published releases")
    return {tag[1:] for tag in result.stdout.split() if STABLE_TAG.fullmatch(tag)}


def released(version, root=ROOT):
    """The Data Generations release `version` declared, or None when it predates the file."""
    tag = f"v{version}"
    if subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", f"refs/tags/{tag}"],
        cwd=root,
        capture_output=True,
    ).returncode:
        raise TaskError(f"the tag of published release {version} is missing; fetch every tag")
    if not _git(root, "ls-tree", "--name-only", tag, "--", str(SOURCE)).strip():
        return None
    return generations(_git(root, "show", f"{tag}:{SOURCE}"))


def withdrawn(root, published):
    """The withdrawn releases WITHDRAWN lists, each a distinct published release."""
    try:
        listed = tomllib.loads((Path(root) / WITHDRAWN).read_text())["releases"]
    except (OSError, KeyError, tomllib.TOMLDecodeError):
        raise TaskError("the withdrawn release list needs one releases array") from None
    if not isinstance(listed, list) or not all(
        isinstance(version, str) and VERSION.fullmatch(version) for version in listed
    ):
        raise TaskError("each withdrawn release is a MAJOR.MINOR.PATCH version")
    if len(set(listed)) != len(listed) or not set(listed) <= published:
        raise TaskError("each withdrawn release is a distinct published release")
    return set(listed)


def releases(version, root=ROOT, published=published_releases):
    """Every published release older than `version` that declares Data Generations, oldest
    first, as `(version, generations, withdrawn)`."""
    listed = set(published())
    skipped = withdrawn(root, listed)
    result = []
    for candidate in sorted(listed, key=_order):
        if _order(candidate) < _order(version):
            declared = released(candidate, root)
            if declared is not None:
                result.append((candidate, declared, candidate in skipped))
    return result


def _current(root):
    try:
        return generations((Path(root) / SOURCE).read_text())
    except OSError:
        raise TaskError("the Data Generation source is unavailable") from None


def floor(product, version, root=ROOT, published=published_releases, *, history=None):
    """The Update Floor of `product` for release `version`, built from the tree at `root`."""
    current = _current(root)
    history = releases(version, root, published) if history is None else history
    if not history:
        return version
    for candidate, declared, skipped in history:
        if not skipped and all(declared[name][1] >= current[name][0] for name in PRODUCTS[product]):
            return candidate
    raise TaskError(
        f"no published release reaches the oldest {product} Data Generations; "
        "a release must keep a step from a published one"
    )


def floors(version, root=ROOT, published=published_releases):
    """Both products' floors for release `version`."""
    history = releases(version, root, published)
    return {product: floor(product, version, root, history=history) for product in PRODUCTS}


def check(version, root=ROOT, published=published_releases):
    """The release-only Data Generation checks for release `version`; returns both floors.

    Against the previous published release, withdrawn or not, no root's `oldest` or `current`
    decreases. Every generation from `oldest` to `current` of every root has a fixture
    directory with a file in it. Each floor is a published release or this one.
    """
    current = _current(root)
    history = releases(version, root, published)
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
    return {product: floor(product, version, root, history=history) for product in PRODUCTS}
