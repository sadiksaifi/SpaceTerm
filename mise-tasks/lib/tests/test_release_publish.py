"""Draft publication never exposes partial assets or overrides an immutable release."""

import subprocess
import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import TaskError, release_publish, update_signing


class GitHub:
    def __init__(self):
        self.release = None
        self.assets = []
        self.mutations = []
        self.tag_sha = "a" * 40

    def request(self, endpoint):
        if "immutable-releases" in endpoint:
            return {"enabled": True}
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
            self.release = {"id": 1, "tag_name": command[3], "draft": True}
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
            if "--latest=false" not in command:
                raise AssertionError("publication changed Latest")
            self.release["draft"] = False
        return subprocess.CompletedProcess(command, 0)


class PublishTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.assets = [self.directory / name for name in ("core.tar.gz", "SpaceTerm.dmg")]
        for path in self.assets:
            path.write_bytes(path.name.encode())
        self.github = GitHub()
        self.plan = {"tag": "v0.10.0", "mode": "publish", "tag_object": self.github.tag_sha}

    def publish(self):
        release_publish.publish_draft(
            self.plan,
            self.assets,
            self.directory / "notes",
            request=self.github.request,
            run=self.github.run,
        )

    def test_only_complete_verified_assets_become_published_and_latest_stays_held(self):
        self.publish()
        self.assertEqual(self.github.mutations, ["create", "upload", "edit"])
        self.assertFalse(self.github.release["draft"])
        self.assertEqual(len(self.github.assets), 2)
        with self.assertRaises(TaskError):
            self.publish()
        self.assertEqual(self.github.mutations, ["create", "upload", "edit"])

    def test_retry_reuses_matching_draft_assets_and_uploads_only_missing_assets(self):
        self.github.release = {"id": 1, "tag_name": "v0.10.0", "draft": True}
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
        self.github.release = {"id": 1, "tag_name": "v0.10.0", "draft": True}
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
        self.assertTrue(self.github.release["draft"])
