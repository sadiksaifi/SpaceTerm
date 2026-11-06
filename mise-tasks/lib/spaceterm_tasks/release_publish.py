"""Publish complete immutable drafts while holding Latest for explicit promotion."""

import subprocess

from spaceterm_tasks import ROOT, TaskError, release_plan, update_signing
from spaceterm_tasks.release_plan import REPOSITORY, api, pages


def publish_draft(plan, assets, notes_file, *, request=api, run=subprocess.run):
    tag = plan["tag"]
    existing = [
        item
        for item in pages(f"repos/{REPOSITORY}/releases", request=request)
        if item["tag_name"] == tag
    ]
    if existing and not existing[0]["draft"]:
        raise TaskError("a published release is immutable and cannot be replaced")
    # Keep a draft until the complete server-side asset inventory matches the local bytes.
    # A retry may reuse an exact draft; mismatched assets require explicit investigation.
    if (
        not existing
        and run(
            [
                "gh",
                "release",
                "create",
                tag,
                "--draft",
                "--verify-tag",
                "--latest=false",
                "--title",
                f"SpaceTerm {tag}",
                "--notes-file",
                notes_file,
            ],
            cwd=ROOT,
        ).returncode
    ):
        raise TaskError("GitHub could not create the draft release")
    draft = request(f"repos/{REPOSITORY}/releases/tags/{tag}")
    uploaded = pages(f"repos/{REPOSITORY}/releases/{draft['id']}/assets", request=request)
    known = {item["name"]: item for item in uploaded}
    if set(known) - {path.name for path in assets}:
        raise TaskError("the draft release contains unexpected assets")
    missing = []
    for path in assets:
        item = known.get(path.name)
        if item is None:
            missing.append(path)
        elif (
            item.get("digest") != f"sha256:{update_signing.sha256(path)}"
            or item["size"] != path.stat().st_size
        ):
            raise TaskError("the draft release contains different asset bytes")
    if missing and run(["gh", "release", "upload", tag, *missing], cwd=ROOT).returncode:
        raise TaskError("GitHub could not upload the draft release assets")
    uploaded = pages(f"repos/{REPOSITORY}/releases/{draft['id']}/assets", request=request)
    expected = {
        path.name: (path.stat().st_size, f"sha256:{update_signing.sha256(path)}") for path in assets
    }
    actual = {item["name"]: (item["size"], item.get("digest")) for item in uploaded}
    if len(uploaded) != len(expected) or actual != expected:
        raise TaskError("the uploaded release inventory differs from the verified assets")
    release_plan.recheck(plan, request)
    if run(
        [
            "gh",
            "release",
            "edit",
            tag,
            "--draft=false",
            "--latest=false",
        ],
        cwd=ROOT,
    ).returncode:
        raise TaskError("GitHub could not publish the release")
    print(f"Published SpaceTerm {tag} with {len(assets)} assets.")
