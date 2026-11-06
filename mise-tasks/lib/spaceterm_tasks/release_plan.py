"""Freeze release authority and inputs before compilation or signing starts."""

import json
import os
import subprocess
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, release_floor, update_signing
from spaceterm_tasks.packaging import checked, release_version

REPOSITORY = "sadiksaifi/SpaceTerm"
PLAN = ROOT / "dist/release-plan.json"
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
        raise TaskError("GitHub could not verify release authority")
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


def immutable(request=api):
    if request(f"repos/{REPOSITORY}/immutable-releases").get("enabled") is not True:
        raise TaskError("enable repository release immutability before a signed release")


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


def create(tag, mode, *, root=ROOT, request=api):
    version = greenfield_tag(tag)
    source = git("rev-parse", "HEAD", root=root)
    if (
        git("cat-file", "-t", f"refs/tags/{tag}", root=root) != "tag"
        or git("rev-parse", f"refs/tags/{tag}^{{commit}}", root=root) != source
    ):
        raise TaskError("the release requires an annotated tag on the exact checkout")
    tag_object = git("rev-parse", f"refs/tags/{tag}", root=root)
    if mode != "unsigned":
        if os.environ.get("GITHUB_REPOSITORY", "").lower() != REPOSITORY.lower():
            raise TaskError("signed releases run only in the intended repository")
        immutable(request)
        if os.environ.get("GITHUB_REF") != "refs/heads/main":
            raise TaskError("signed release dispatches must select main")
        if os.environ.get("GITHUB_SHA") != source:
            raise TaskError("release compilation must use the dispatch's exact source")
        from spaceterm_tasks import macos_notary

        if not macos_notary.required(os.environ):
            raise TaskError("signed release plans require Developer ID signing")
        update_signing.release_trust(root)
    floors = release_floor.check(
        version, root, **({"published": lambda: set()} if mode == "unsigned" else {})
    )
    from spaceterm_tasks.release_feed import asset_names

    return {
        "schema": 1,
        "tag": tag,
        "version": version,
        "source": source,
        "tag_object": tag_object,
        "published_at": publication_time(tag, root),
        "tag_data": git("cat-file", "tag", tag_object, root=root) + "\n",
        "mode": mode,
        "floors": floors,
        "first_release_exemption": floors["core"] == version,
        "inputs": input_digests(root),
        "assets": asset_names(version),
        "team": os.environ.get("SPACETERM_RELEASE_APPLE_TEAM_ID", ""),
        "identity": os.environ.get("SPACETERM_RELEASE_APPLE_IDENTITY", ""),
        "notes_sha256": update_signing.sha256(root / "dist/release-notes.md"),
    }


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
    if value["notes_sha256"] != update_signing.sha256(root / "dist/release-notes.md"):
        raise TaskError("the release notes differ from the compiled release plan")
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
    if plan["mode"] != "publish":
        raise TaskError("an unsigned plan grants no publication authority")
    immutable(request)
    ref = request(f"repos/{REPOSITORY}/git/ref/tags/{plan['tag']}")
    if ref["object"]["type"] != "tag" or ref["object"]["sha"] != plan["tag_object"]:
        raise TaskError("the release tag moved after planning")


def restore(path=PLAN):
    """Restore the exact local annotated tag in shallow downstream checkouts."""
    plan = json.loads(Path(path).read_text())
    if git("rev-parse", "HEAD") != plan["source"]:
        raise TaskError("the release checkout differs from the planned source")
    result = checked(
        ["git", "hash-object", "-t", "tag", "-w", "--stdin"],
        "Git could not restore the local release tag",
        cwd=ROOT,
        input=plan["tag_data"],
        text=True,
    ).strip()
    if result != plan["tag_object"]:
        raise TaskError("the annotated release tag differs from the plan")
    checked(
        ["git", "update-ref", f"refs/tags/{plan['tag']}", result],
        "Git could not restore the release tag",
        cwd=ROOT,
    )
    return load(path)


def authorize_tag():
    """After validation, publish the planned tag without moving any existing ref."""
    plan = load()
    if plan["mode"] != "publish":
        print("Signed rehearsal: the annotated tag stays local; no remote tag is created.")
        return
    immutable()
    tags = pages(f"repos/{REPOSITORY}/git/matching-refs/tags/{plan['tag']}")
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
            "GitHub could not create the exact validated tag",
            cwd=ROOT,
        )
    recheck(plan)


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
