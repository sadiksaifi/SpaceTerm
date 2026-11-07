"""Publish a complete, verified draft as the immutable Latest release.

Every attempt of a release run repackages its products, and archives, disk images and the
appcast record when they were built. So the bytes of two attempts differ. A draft takes this
attempt's verified bytes in place of an earlier attempt's. A release already published keeps
its bytes, and a later attempt only confirms it.

GitHub's release listing lags its writes, and its tag lookup returns published releases only.
So the listing only finds a release that an earlier attempt created, and every later request
names the release by its id. An update to a draft that omits its tag name detaches the draft
from its tag, so every update sends the tag name.
"""

import json
import subprocess
from pathlib import Path
from urllib.parse import quote

from spaceterm_tasks import ROOT, TaskError, release_plan, update_signing
from spaceterm_tasks.release_plan import REPOSITORY, api, pages

RELEASES = f"repos/{REPOSITORY}/releases"


def upload_asset(release, path):
    """Upload `path` to the release with id `release` and return GitHub's asset record."""
    result = subprocess.run(
        [
            "gh",
            "api",
            "--method",
            "POST",
            f"https://uploads.github.com/{RELEASES}/{release}/assets?name={quote(path.name)}",
            "--header",
            "Content-Type: application/octet-stream",
            "--input",
            str(path),
        ],
        capture_output=True,
        text=True,
        cwd=ROOT,
    )
    if result.returncode:
        raise TaskError("GitHub could not upload a release asset")
    return json.loads(result.stdout)


def verify_identity(record, tag, notes_file):
    if (
        record.get("tag_name") != tag
        or record.get("prerelease")
        or record.get("body") != Path(notes_file).read_text()
        or record.get("name") != f"SpaceTerm {tag}"
    ):
        raise TaskError("the release differs from the planned release notes and identity")


def verify_inventory(uploaded, assets):
    expected = {
        path.name: (path.stat().st_size, f"sha256:{update_signing.sha256(path)}") for path in assets
    }
    actual = {item["name"]: (item["size"], item.get("digest")) for item in uploaded}
    if len(uploaded) != len(expected) or actual != expected:
        raise TaskError("the uploaded release inventory differs from the verified assets")


def verify_published(record, plan, assets, notes_file, request, *, exact=True):
    """Require the published release of this plan. `exact` also requires these asset bytes;
    without it, the release must list exactly these asset names."""
    verify_identity(record, plan["tag"], notes_file)
    if record["draft"] or record.get("immutable") is not True:
        raise TaskError("GitHub did not publish an immutable release")
    release_plan.recheck(plan, request)
    uploaded = pages(f"{RELEASES}/{record['id']}/assets", request=request)
    if exact:
        verify_inventory(uploaded, assets)
    elif sorted(item["name"] for item in uploaded) != sorted(path.name for path in assets):
        raise TaskError("the published release lacks the planned asset inventory")


def stage_draft(plan, assets, notes_file, *, request=api, upload=upload_asset):
    """Return the draft of this plan holding exactly these asset bytes, or the release an
    earlier attempt already published."""
    tag = plan["tag"]
    existing = [item for item in pages(RELEASES, request=request) if item["tag_name"] == tag]
    if len(existing) > 1:
        raise TaskError("GitHub lists more than one release for the planned tag")
    # Keep a draft until the complete server-side asset inventory matches the local bytes.
    draft = existing[0] if existing else None
    if draft is None:
        draft = request(
            RELEASES,
            method="POST",
            fields={
                "tag_name": tag,
                "name": f"SpaceTerm {tag}",
                "body": Path(notes_file).read_text(),
                "draft": True,
                "prerelease": False,
                "make_latest": "false",
            },
        )
    if not draft["draft"]:
        return draft
    verify_identity(draft, tag, notes_file)
    uploaded = pages(f"{RELEASES}/{draft['id']}/assets", request=request)
    known = {item["name"]: item for item in uploaded}
    if set(known) - {path.name for path in assets}:
        raise TaskError("the draft release contains unexpected assets")
    # Upload what the draft lacks, and replace what an earlier attempt built differently.
    for path in assets:
        item = known.get(path.name)
        if (
            item is not None
            and item.get("digest") == f"sha256:{update_signing.sha256(path)}"
            and item["size"] == path.stat().st_size
        ):
            continue
        if item is not None:
            request(f"{RELEASES}/assets/{item['id']}", method="DELETE")
        upload(draft["id"], path)
    verify_inventory(pages(f"{RELEASES}/{draft['id']}/assets", request=request), assets)
    return draft


def publish_draft(plan, assets, notes_file, *, request=api, upload=upload_asset):
    tag = plan["tag"]
    draft = stage_draft(plan, assets, notes_file, request=request, upload=upload)
    if not draft["draft"]:
        verify_published(draft, plan, assets, notes_file, request, exact=False)
        print(f"SpaceTerm {tag} is already published; this attempt changes nothing.")
        return
    release_plan.recheck(plan, request)
    request(
        f"{RELEASES}/{draft['id']}",
        method="PATCH",
        fields={"tag_name": tag, "draft": False, "make_latest": "true"},
    )
    published = request(f"{RELEASES}/{draft['id']}")
    verify_published(published, plan, assets, notes_file, request)
    print(f"Published SpaceTerm {tag} with {len(assets)} assets.")
