"""Draft publication never exposes partial assets or overrides an immutable release."""

import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import (
    ROOT,
    TaskError,
    package_linux,
    release,
    release_plan,
    release_publish,
    update_signing,
)

from tests.test_release_floor import Repository, source


class GitHub:
    """GitHub's release API as the hosted publication job observes it.

    The release listing omits a release created since the last `settle`, which stands for the
    time between two attempts. The tag lookup returns published releases only. An update to a
    draft that omits its tag name detaches the draft from its tag. Requests by id observe every
    earlier write.
    """

    def __init__(self):
        self.release = None
        self.listed = False
        self.assets = []
        self.mutations = []
        self.tag_sha = "a" * 40
        self.next_asset = 1

    def settle(self):
        self.listed = self.release is not None

    def request(self, endpoint, *, method="GET", fields=None):
        releases = f"repos/{release_plan.REPOSITORY}/releases"
        if method == "GET" and "/git/ref/" in endpoint:
            return {"object": {"type": "tag", "sha": self.tag_sha}}
        if method == "GET" and endpoint.startswith(f"{releases}/1/assets?"):
            return [dict(asset) for asset in self.assets]
        if method == "GET" and endpoint.startswith(f"{releases}?"):
            return [dict(self.release)] if self.release and self.listed else []
        if method == "GET" and endpoint == f"{releases}/1":
            assert self.release is not None
            return dict(self.release)
        if method == "GET" and endpoint.startswith(f"{releases}/tags/"):
            if self.release is None or self.release["draft"]:
                raise TaskError("the published release was not found")
            return dict(self.release)
        if method == "POST" and endpoint == releases:
            assert fields is not None
            if self.release is not None:
                raise TaskError("the release already exists")
            if fields.get("draft") is not True or fields.get("make_latest") != "false":
                raise AssertionError(
                    "release became visible before its asset inventory was checked"
                )
            self.mutations.append("create")
            self.release = {
                "id": 1,
                "tag_name": fields["tag_name"],
                "draft": True,
                "prerelease": fields.get("prerelease", False),
                "body": fields["body"],
                "name": fields["name"],
            }
            return dict(self.release)
        if method == "DELETE" and endpoint.startswith(f"{releases}/assets/"):
            self.mutations.append("delete")
            asset = int(endpoint.rsplit("/", 1)[1])
            self.assets = [item for item in self.assets if item["id"] != asset]
            return None
        if method == "PATCH" and endpoint == f"{releases}/1":
            assert fields is not None and self.release is not None
            if "tag_name" not in fields and self.release["draft"]:
                self.release["tag_name"] = "untagged-0123456789abcdef0123"
            if fields.get("draft") is False:
                if fields.get("make_latest") != "true":
                    raise AssertionError("publication did not mark the release Latest")
                self.mutations.append("publish")
                self.release["immutable"] = True
            self.release.update(fields)
            self.release.pop("make_latest", None)
            return dict(self.release)
        raise AssertionError((method, endpoint))

    def upload(self, release, path):
        assert release == 1 and self.release is not None and self.release["draft"]
        if any(item["name"] == path.name for item in self.assets):
            raise TaskError("an asset with this name already exists")
        self.mutations.append("upload")
        asset = {
            "id": self.next_asset,
            "name": path.name,
            "size": path.stat().st_size,
            "digest": "sha256:" + update_signing.sha256(path),
        }
        self.next_asset += 1
        self.assets.append(asset)
        return dict(asset)


class PublishTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.assets = [self.directory / name for name in ("core.tar.gz", "SpaceTerm.dmg")]
        for path in self.assets:
            path.write_bytes(path.name.encode())
        (self.directory / "notes").write_text("First release of SpaceTerm.\n")
        self.github = GitHub()
        self.plan = {"tag": "v0.10.0", "tag_object": self.github.tag_sha}

    def publish(self, request=None):
        """One attempt, started after GitHub lists every release of earlier attempts."""
        self.github.settle()
        release_publish.publish_draft(
            self.plan,
            self.assets,
            self.directory / "notes",
            request=request or self.github.request,
            upload=self.github.upload,
        )

    def test_only_complete_verified_assets_become_the_published_latest_release(self):
        # The attempt never finds its new draft through the lagging listing or the tag lookup.
        self.publish()
        published = ["create", "upload", "upload", "publish"]
        self.assertEqual(self.github.mutations, published)
        assert self.github.release is not None
        self.assertFalse(self.github.release["draft"])
        self.assertEqual(self.github.release["tag_name"], "v0.10.0")
        self.assertEqual(len(self.github.assets), 2)
        self.publish()
        self.assertEqual(self.github.mutations, published)

    def test_reassembly_at_a_later_time_completes_the_same_partial_draft(self):
        repository = Repository(self.directory)
        for path in release_plan.INPUTS:
            destination = self.directory / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / path, destination)
        repository.commit(source())
        for name in ("core-data", "cli-state", "desktop-config", "desktop-state"):
            repository.fixtures(name, 1)
        repository.git("tag", "-a", "v0.10.0", "-m", "Release SpaceTerm 0.10.0")
        dist = self.directory / "dist"
        dist.mkdir()
        with patch.dict(os.environ, {update_signing.DRY_RUN: "1"}):
            self.plan = release_plan.create("v0.10.0", root=self.directory)
        self.plan.update(published_at=1700000000)
        self.github.tag_sha = self.plan["tag_object"]
        plan_file = dist / "release-plan.json"
        plan_file.write_text(json.dumps(self.plan))
        linux = dist / "linux"
        linux.mkdir()
        (linux / release.archive_name("desktop", "0.10.0", "linux-x86_64")).write_bytes(
            b"unchanged compiled archive"
        )
        environment = {
            update_signing.DRY_RUN: "1",
            update_signing.SIGNING_SECRET: update_signing.encoded_seed(bytes(range(32))),
            "SPACETERM_RELEASE_PLAN": str(plan_file),
        }
        run = subprocess.run

        def network(arguments, **kwargs):
            if arguments[:2] == ["gh", "api"]:
                return subprocess.CompletedProcess(arguments, 0, "", "")
            return run(arguments, **kwargs)

        with (
            patch.object(release, "ROOT", self.directory),
            patch.object(release, "DIST", dist),
            patch.object(release, "RELEASE", dist / "release"),
            patch.object(package_linux, "verify"),
            patch.object(subprocess, "run", side_effect=network),
        ):
            for clock in (1800000000, 1800000060):
                with patch("time.time", return_value=clock):
                    release.assemble(
                        "v0.10.0",
                        dist / "core",
                        dist / "macos",
                        linux,
                        complete=False,
                        environment=environment,
                    )
                current = {path.name: path.read_bytes() for path in release.RELEASE.iterdir()}
                if clock == 1800000000:
                    initial = current
                    self.github.release = self.draft()
                    self.github.upload(1, release.RELEASE / "SHA256SUMS")
                else:
                    self.assertEqual(current, initial)
            feed = json.loads(current["spaceterm-desktop-linux-x86_64.json"])
            self.assertEqual(feed["published_at"], 1700000000)
            self.assets = list(release.RELEASE.iterdir())
            self.publish()
            uploads = ["upload"] * (len(self.assets) - 1)
            self.assertEqual(self.github.mutations, ["upload", *uploads, "publish"])

    def test_retry_recovers_a_lost_publication_response_without_mutation(self):
        request = self.github.request

        def lost(endpoint, *, method="GET", fields=None):
            result = request(endpoint, method=method, fields=fields)
            if method == "PATCH":
                raise TaskError("GitHub response was lost")
            return result

        with self.assertRaises(TaskError):
            self.publish(lost)
        self.publish()
        self.assertEqual(self.github.mutations, ["create", "upload", "upload", "publish"])

    def test_retry_recovers_a_lost_draft_creation_response(self):
        request = self.github.request

        def lost(endpoint, *, method="GET", fields=None):
            result = request(endpoint, method=method, fields=fields)
            if method == "POST":
                raise TaskError("GitHub response was lost")
            return result

        with self.assertRaises(TaskError):
            self.publish(lost)
        self.publish()
        self.assertEqual(self.github.mutations, ["create", "upload", "upload", "publish"])

    def test_retry_recovers_a_failed_post_publication_read(self):
        request = self.github.request

        def unavailable(endpoint, *, method="GET", fields=None):
            if (
                method == "GET"
                and endpoint.endswith("/releases/1")
                and self.github.release
                and not self.github.release["draft"]
            ):
                raise TaskError("GitHub response was lost")
            return request(endpoint, method=method, fields=fields)

        with self.assertRaises(TaskError):
            self.publish(unavailable)
        self.publish()
        self.assertEqual(self.github.mutations, ["create", "upload", "upload", "publish"])

    def test_a_listing_with_two_releases_for_the_tag_stops_before_any_mutation(self):
        request = self.github.request

        def duplicated(endpoint, *, method="GET", fields=None):
            if endpoint.startswith(f"repos/{release_plan.REPOSITORY}/releases?"):
                return [self.draft(), {**self.draft(), "id": 2}]
            return request(endpoint, method=method, fields=fields)

        with self.assertRaises(TaskError):
            self.publish(duplicated)
        self.assertEqual(self.github.mutations, [])

    def test_retry_reuses_matching_draft_assets_and_uploads_only_missing_assets(self):
        self.github.release = self.draft()
        path = self.assets[0]
        self.github.assets = [
            {
                "id": 99,
                "name": path.name,
                "size": path.stat().st_size,
                "digest": "sha256:" + update_signing.sha256(path),
            }
        ]
        self.publish()
        self.assertEqual(self.github.mutations, ["upload", "publish"])
        self.assertEqual(len(self.github.assets), 2)

    def test_retry_replaces_draft_assets_an_earlier_attempt_built_differently(self):
        # Each attempt repackages, and archives and disk images record their build time.
        self.github.release = self.draft()
        self.github.assets = [
            {"id": 99, "name": self.assets[0].name, "size": 1, "digest": "sha256:" + "f" * 64}
        ]
        self.publish()
        self.assertEqual(self.github.mutations, ["delete", "upload", "upload", "publish"])
        self.assertEqual(
            sorted(asset["digest"] for asset in self.github.assets),
            sorted("sha256:" + update_signing.sha256(path) for path in self.assets),
        )

    def test_retry_after_publication_keeps_the_published_release(self):
        self.publish()
        published = [dict(asset) for asset in self.github.assets]
        for path in self.assets:
            path.write_bytes(path.name.encode() + b" rebuilt by a later attempt")
        self.github.mutations.clear()
        self.publish()
        self.assertEqual(self.github.mutations, [])
        self.assertEqual(self.github.assets, published)

    def test_a_tag_move_after_upload_never_publishes_the_draft(self):
        self.github.tag_sha = "b" * 40
        with self.assertRaises(TaskError):
            self.publish()
        self.assertEqual(self.github.mutations, ["create", "upload", "upload"])
        assert self.github.release is not None
        self.assertTrue(self.github.release["draft"])

    def draft(self):
        return {
            "id": 1,
            "tag_name": "v0.10.0",
            "draft": True,
            "body": "First release of SpaceTerm.\n",
            "name": "SpaceTerm v0.10.0",
        }

    def test_retry_refuses_different_notes_before_uploading_or_publishing(self):
        self.github.release = {**self.draft(), "body": "Different release notes"}
        with self.assertRaises(TaskError):
            self.publish()
        self.assertEqual(self.github.mutations, [])

    def test_an_existing_publication_must_match_every_planned_value(self):
        self.publish()
        assert self.github.release is not None
        original = dict(self.github.release)
        original_assets = [dict(asset) for asset in self.github.assets]
        for change in (
            {"body": "changed"},
            {"name": "changed"},
            {"prerelease": True},
            {"immutable": False},
            {"tag_sha": "b" * 40},
        ):
            with self.subTest(change=change):
                self.github.release = {
                    **original,
                    **{
                        key: value
                        for key, value in change.items()
                        if key not in ("digest", "tag_sha")
                    },
                }
                self.github.assets = [dict(asset) for asset in original_assets]
                tag_sha = change.get("tag_sha", self.plan["tag_object"])
                assert isinstance(tag_sha, str)
                self.github.tag_sha = tag_sha
                if "digest" in change:
                    self.github.assets[0]["digest"] = change["digest"]
                self.github.mutations.clear()
                with self.assertRaises(TaskError):
                    self.publish()
                self.assertEqual(self.github.mutations, [])

    def test_an_existing_publication_requires_the_complete_asset_inventory(self):
        self.publish()
        original = [dict(asset) for asset in self.github.assets]
        for assets in (
            original[:1],
            original + [original[0]],
            original + [{**original[0], "name": "unexpected"}],
        ):
            self.github.assets = assets
            self.github.mutations.clear()
            with self.subTest(assets=assets), self.assertRaises(TaskError):
                self.publish()
            self.assertEqual(self.github.mutations, [])
