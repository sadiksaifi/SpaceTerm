"""Publish complete immutable drafts while holding Latest for explicit promotion."""

import subprocess
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, release_plan, update_signing
from spaceterm_tasks.release_plan import REPOSITORY, api, pages


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


def verify_published(record, plan, assets, notes_file, request):
    verify_identity(record, plan["tag"], notes_file)
    if record["draft"] or record.get("immutable") is not True:
        raise TaskError("GitHub did not publish an immutable release")
    release_plan.recheck(plan, request)
    uploaded = pages(f"repos/{REPOSITORY}/releases/{record['id']}/assets", request=request)
    verify_inventory(uploaded, assets)


def publish_draft(plan, assets, notes_file, *, request=api, run=subprocess.run):
    tag = plan["tag"]
    existing = [
        item
        for item in pages(f"repos/{REPOSITORY}/releases", request=request)
        if item["tag_name"] == tag
    ]
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
    if not draft["draft"]:
        verify_published(draft, plan, assets, notes_file, request)
        print(f"Verified already-published SpaceTerm {tag}; no mutation was needed.")
        return
    verify_identity(draft, tag, notes_file)
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
    verify_inventory(uploaded, assets)
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
    published = request(f"repos/{REPOSITORY}/releases/tags/{tag}")
    verify_published(published, plan, assets, notes_file, request)
    print(f"Published SpaceTerm {tag} with {len(assets)} assets.")
