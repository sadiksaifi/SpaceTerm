"""Derive one release plan, byte for byte, in every job of a release run.

Each job tags the exact source and computes the plan from that tag and the tracked public
inputs, so no job waits for another to plan. Artifacts carry the plan's digest, and each job
refuses inputs planned differently.
"""

import json
import os
import subprocess
from datetime import datetime
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, release_floor, update_signing
from spaceterm_tasks.packaging import checked, release_version

REPOSITORY = "sadiksaifi/SpaceTerm"
PLAN = ROOT / "dist/release-plan.json"
# Every job writes the same annotated tag object, so the tagger and message are fixed.
TAGGER = {
    "GIT_COMMITTER_NAME": "SpaceTerm releases",
    "GIT_COMMITTER_EMAIL": "releases@spaceterm.peblon.com",
}
INPUTS = (
    "mise.lock",
    ".mise.toml",
    "rust-toolchain.toml",
    "packaging/update-trust.json",
    "packaging/linux/toolchain.json",
    "packaging/withdrawn-releases.toml",
    ".tagsmith.jsonc",
    "cliff.toml",
)


def api(endpoint, *, method="GET", fields=None):
    command = ["gh", "api", "--method", method, endpoint]
    if fields:
        command.extend(["--input", "-"])
    result = subprocess.run(
        command,
        input=json.dumps(fields) if fields else None,
        capture_output=True,
        text=True,
        cwd=ROOT,
    )
    if result.returncode:
        raise TaskError("a GitHub release request failed")
    return json.loads(result.stdout) if result.stdout.strip() else None


def pages(endpoint, key=None, request=api):
    values = []
    for page in range(1, 101):
        result = request(f"{endpoint}{'&' if '?' in endpoint else '?'}per_page=100&page={page}")
        batch = result[key] if key else result
        values.extend(batch)
        if len(batch) < 100:
            return values
    raise TaskError("GitHub release evidence exceeded the pagination bound")


def greenfield_tag(tag):
    version = release_version(tag)
    if tuple(map(int, version.split("."))) < (0, 10, 0):
        raise TaskError("greenfield releases start at v0.10.0")
    return version


def check_policy(version, *, root=ROOT):
    """Check `version` against the Tagsmith release policy.

    A new tag must be the next valid release on main. A rerun after publication pushed the tag
    validates that tag instead, because Tagsmith refuses to plan a tag that exists.
    """
    tag = f"v{greenfield_tag(f'v{version}')}"
    if subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", f"refs/tags/{tag}"],
        cwd=root,
        capture_output=True,
    ).returncode:
        command = ["tagsmith", "tag", "--target", "spaceterm", "--channel", "stable"]
        command += ["--version", version, "--dry-run", "--json"]
    else:
        command = ["tagsmith", "validate", "--tag", tag]
    if subprocess.run(command, cwd=root).returncode:
        raise TaskError("the release version fails the Tagsmith release policy")


def git(*arguments, root=ROOT):
    return checked(
        ["git", *arguments], "Git could not verify the release source", cwd=root, text=True
    ).strip()


def input_digests(root=ROOT):
    return {path: update_signing.sha256(root / path) for path in INPUTS}


def publication_time(tag, root=ROOT):
    """Use the annotated tag's time so even a fresh plan for that tag retains feed bytes."""
    value = git("for-each-ref", "--format=%(taggerdate:unix)", f"refs/tags/{tag}", root=root)
    if not value.isdecimal() or int(value) <= 0:
        raise TaskError("the release tag must record its publication timestamp")
    return int(value)


def dispatch_time(request=api):
    """The creation time of this workflow run, which every job and every attempt shares."""
    run = request(f"repos/{REPOSITORY}/actions/runs/{int(os.environ['GITHUB_RUN_ID'])}")
    try:
        created = datetime.fromisoformat(run["created_at"].replace("Z", "+00:00"))
    except (KeyError, TypeError, ValueError):
        raise TaskError("GitHub reported no creation time for this release run") from None
    return int(created.timestamp())


def tag(version, *, root=ROOT, request=api):
    """Create the release tag at HEAD unless it exists, with the run's creation time.

    Every job of one run therefore writes the identical tag object, and a tag already
    published for this version is kept as it is.
    """
    name = f"v{greenfield_tag(f'v{version}')}"
    if not subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", f"refs/tags/{name}"],
        cwd=root,
        capture_output=True,
    ).returncode:
        return name
    if not os.environ.get("GITHUB_RUN_ID"):
        raise TaskError("outside the release workflow, tag the release before planning it")
    checked(
        [
            "git",
            "-c",
            "tag.gpgSign=false",
            "tag",
            "--annotate",
            "--no-sign",
            name,
            "--message",
            f"Release SpaceTerm {version}",
        ],
        "Git could not create the release tag",
        cwd=root,
        env={**os.environ, **TAGGER, "GIT_COMMITTER_DATE": f"{dispatch_time(request)} +0000"},
    )
    return name


def create(tag, *, root=ROOT, published=None):
    """The plan of release `tag` at HEAD. A dry run plans against no published release."""
    version = greenfield_tag(tag)
    source = git("rev-parse", "HEAD", root=root)
    if (
        git("cat-file", "-t", f"refs/tags/{tag}", root=root) != "tag"
        or git("rev-parse", f"refs/tags/{tag}^{{commit}}", root=root) != source
    ):
        raise TaskError("the release requires an annotated tag on the exact checkout")
    tag_object = git("rev-parse", f"refs/tags/{tag}", root=root)
    if update_signing.dry_run(os.environ):
        published = published or (lambda: set())
    else:
        if os.environ.get("GITHUB_REPOSITORY", "").lower() != REPOSITORY.lower():
            raise TaskError("releases run only in the intended repository")
        if os.environ.get("GITHUB_REF") != "refs/heads/main":
            raise TaskError("release dispatches must select main")
        if os.environ.get("GITHUB_SHA") != source:
            raise TaskError("a release must build the dispatch's exact source")
        from spaceterm_tasks import macos_notary

        if not macos_notary.required(os.environ):
            raise TaskError("a release requires Developer ID signing")
        update_signing.release_trust(root)
    floors = release_floor.check(version, root, **({"published": published} if published else {}))
    from spaceterm_tasks.release_feed import asset_names

    return {
        "schema": 1,
        "tag": tag,
        "version": version,
        "source": source,
        "tag_object": tag_object,
        "published_at": publication_time(tag, root),
        "tag_data": git("cat-file", "tag", tag_object, root=root) + "\n",
        "floors": floors,
        "first_release_exemption": floors["core"] == version,
        "inputs": input_digests(root),
        "assets": asset_names(version),
        "team": os.environ.get("SPACETERM_RELEASE_APPLE_TEAM_ID", ""),
        "identity": os.environ.get("SPACETERM_RELEASE_APPLE_IDENTITY", ""),
    }


def encode(plan):
    """The plan file's bytes, which artifacts bind by digest."""
    return json.dumps(plan, indent=2) + "\n"


def load(path=None, *, root=ROOT):
    path = path or os.environ.get("SPACETERM_RELEASE_PLAN") or PLAN
    value = json.loads(Path(path).read_text())
    if (
        value["schema"] != 1
        or type(value.get("published_at")) is not int
        or value["published_at"] <= 0
        or value["source"] != git("rev-parse", "HEAD", root=root)
        or value["inputs"] != input_digests(root)
    ):
        raise TaskError("the release plan differs from this exact source and tool configuration")
    if value["team"] != os.environ.get("SPACETERM_RELEASE_APPLE_TEAM_ID", "") or value[
        "identity"
    ] != os.environ.get("SPACETERM_RELEASE_APPLE_IDENTITY", ""):
        raise TaskError("the release signing identity differs from the plan")
    tag = value["tag"]
    if (
        git("rev-parse", f"refs/tags/{tag}", root=root) != value["tag_object"]
        or git("rev-parse", f"refs/tags/{tag}^{{commit}}", root=root) != value["source"]
    ):
        raise TaskError("the release tag differs from the plan")
    return value


def recheck(plan, request=api):
    """Require GitHub's release tag to be the planned tag object."""
    ref = request(f"repos/{REPOSITORY}/git/ref/tags/{plan['tag']}")
    if ref["object"]["type"] != "tag" or ref["object"]["sha"] != plan["tag_object"]:
        raise TaskError("the release tag moved after planning")


def push_tag(plan, request=api):
    """Publish the planned tag without moving an existing one."""
    tags = pages(f"repos/{REPOSITORY}/git/matching-refs/tags/{plan['tag']}", request=request)
    existing = [ref for ref in tags if ref["ref"] == f"refs/tags/{plan['tag']}"]
    if existing:
        if existing[0]["object"]["sha"] != plan["tag_object"]:
            raise TaskError("the release tag already exists with different bytes")
    else:
        checked(
            [
                "git",
                "-c",
                "credential.helper=!gh auth git-credential",
                "push",
                "origin",
                f"refs/tags/{plan['tag']}",
            ],
            "GitHub could not create the planned release tag",
            cwd=ROOT,
        )
    recheck(plan, request)


def resume_artifact(lane):
    """Select only a previous attempt's immutable evidence from this same workflow run."""
    attempt = int(os.environ["GITHUB_RUN_ATTEMPT"])
    artifacts = pages(
        f"repos/{REPOSITORY}/actions/runs/{os.environ['GITHUB_RUN_ID']}/artifacts", "artifacts"
    )
    prefix = f"notary-{lane}-"
    previous = [
        item
        for item in artifacts
        if not item["expired"]
        and item["name"].startswith(prefix)
        and item["name"][len(prefix) :].isdigit()
        and int(item["name"][len(prefix) :]) < attempt
    ]
    selected = max(previous, key=lambda item: int(item["name"][len(prefix) :]), default=None)
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"artifact-id={selected['id'] if selected else ''}\n")
