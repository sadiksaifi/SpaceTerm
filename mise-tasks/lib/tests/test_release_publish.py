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
    def __init__(self):
        self.release = None
        self.assets = []
        self.mutations = []
        self.tag_sha = "a" * 40

    def request(self, endpoint):
        if "/git/ref/" in endpoint:
            return {"object": {"type": "tag", "sha": self.tag_sha}}
        if "/assets?" in endpoint:
            return self.assets
        if "/releases/tags/" in endpoint:
            return self.release
        if "/releases?" in endpoint:
            return [self.release] if self.release else []
        raise AssertionError(endpoint)

    def run(self, command, **kwargs):
        self.mutations.append(command[2])
        if command[2] == "create":
            if "--draft" not in command or "--latest=false" not in command:
                raise AssertionError(
                    "release became visible before its asset inventory was checked"
                )
            self.release = {
                "id": 1,
                "tag_name": command[3],
                "draft": True,
                "body": Path(command[-1]).read_text(),
                "name": f"SpaceTerm {command[3]}",
            }
        elif command[2] == "upload":
            for path in command[4:]:
                self.assets.append(
                    {
                        "name": path.name,
                        "size": path.stat().st_size,
                        "digest": "sha256:" + update_signing.sha256(path),
                    }
                )
        elif command[2] == "edit":
            if "--latest" not in command:
                raise AssertionError("publication did not mark the release Latest")
            assert self.release is not None
            self.release["draft"] = False
            self.release["immutable"] = True
        return subprocess.CompletedProcess(command, 0)


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

    def publish(self):
        release_publish.publish_draft(
            self.plan,
            self.assets,
            self.directory / "notes",
            request=self.github.request,
            run=self.github.run,
        )

    def test_only_complete_verified_assets_become_the_published_latest_release(self):
        self.publish()
        self.assertEqual(self.github.mutations, ["create", "upload", "edit"])
        assert self.github.release is not None
        self.assertFalse(self.github.release["draft"])
        self.assertEqual(len(self.github.assets), 2)
        self.publish()
        self.assertEqual(self.github.mutations, ["create", "upload", "edit"])

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
                    uploaded = release.RELEASE / "SHA256SUMS"
                    self.github.run(["gh", "release", "upload", "v0.10.0", uploaded])
                else:
                    self.assertEqual(current, initial)
            feed = json.loads(current["spaceterm-desktop-linux-x86_64.json"])
            self.assertEqual(feed["published_at"], 1700000000)
            self.assets = list(release.RELEASE.iterdir())
            self.publish()
            self.assertEqual(self.github.mutations, ["upload", "upload", "edit"])

    def test_retry_recovers_a_lost_publication_response_without_mutation(self):
        run = self.github.run

        def lost(command, **kwargs):
            result = run(command, **kwargs)
            return subprocess.CompletedProcess(command, 1) if command[2] == "edit" else result

        with self.assertRaises(TaskError):
            release_publish.publish_draft(
                self.plan,
                self.assets,
                self.directory / "notes",
                request=self.github.request,
                run=lost,
            )
        self.publish()
        self.assertEqual(self.github.mutations, ["create", "upload", "edit"])

    def test_retry_recovers_a_failed_post_publication_read(self):
        request = self.github.request

        def unavailable(endpoint):
            if (
                "/releases/tags/" in endpoint
                and self.github.release
                and not self.github.release["draft"]
            ):
                raise TaskError("GitHub response was lost")
            return request(endpoint)

        with self.assertRaises(TaskError):
            release_publish.publish_draft(
                self.plan,
                self.assets,
                self.directory / "notes",
                request=unavailable,
                run=self.github.run,
            )
        self.publish()
        self.assertEqual(self.github.mutations, ["create", "upload", "edit"])

    def test_retry_reuses_matching_draft_assets_and_uploads_only_missing_assets(self):
        self.github.release = self.draft()
        path = self.assets[0]
        self.github.assets = [
            {
                "name": path.name,
                "size": path.stat().st_size,
                "digest": "sha256:" + update_signing.sha256(path),
            }
        ]
        self.publish()
        self.assertEqual(self.github.mutations, ["upload", "edit"])
        self.assertEqual(len(self.github.assets), 2)

    def test_retry_refuses_a_draft_with_different_bytes(self):
        self.github.release = self.draft()
        self.github.assets = [
            {"name": self.assets[0].name, "size": 1, "digest": "sha256:" + "f" * 64}
        ]
        with self.assertRaises(TaskError):
            self.publish()
        self.assertEqual(self.github.mutations, [])

    def test_a_tag_move_after_upload_never_publishes_the_draft(self):
        self.github.tag_sha = "b" * 40
        with self.assertRaises(TaskError):
            self.publish()
        self.assertEqual(self.github.mutations, ["create", "upload"])
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
            {"digest": "sha256:" + "f" * 64},
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
