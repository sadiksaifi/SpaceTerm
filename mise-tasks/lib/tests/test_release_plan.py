"""Release authority binds a stable greenfield tag, exact source, notes and public inputs."""

import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import ROOT, TaskError, release_floor, release_plan

from tests.test_release_floor import Repository, source


class ReleasePlanTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.repository = Repository(self.root)
        for path in release_plan.INPUTS:
            destination = self.root / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / path, destination)
        self.repository.commit(source())
        for root in release_floor.ROOTS.values():
            self.repository.fixtures(root, 1)
        (self.root / "dist").mkdir()
        (self.root / "dist/release-notes.md").write_text("First release of SpaceTerm.\n")
        self.repository.git("tag", "-a", "v0.10.0", "-m", "Release SpaceTerm 0.10.0")
        # Do not inherit a caller's plan or signing identity in this disposable repository.
        environment = patch.dict(os.environ, {}, clear=True)
        environment.start()
        self.addCleanup(environment.stop)

    def plan(self):
        return release_plan.create("v0.10.0", "unsigned", root=self.root)

    def test_unsigned_plan_has_no_publication_authority_and_freezes_exact_inputs(self):
        plan = self.plan()
        self.assertEqual(
            plan["published_at"],
            int(
                self.repository.git(
                    "for-each-ref", "--format=%(taggerdate:unix)", "refs/tags/v0.10.0"
                ).strip()
            ),
        )
        with patch("time.time", return_value=plan["published_at"] + 3600):
            self.assertEqual(self.plan()["published_at"], plan["published_at"])
        self.assertEqual(plan["floors"], {"core": "0.10.0", "desktop": "0.10.0"})
        self.assertTrue(plan["first_release_exemption"])
        with self.assertRaises(TaskError):
            release_plan.recheck(plan, lambda endpoint: self.fail("unsigned plan used GitHub"))
        path = self.root / "dist/release-plan.json"
        path.write_text(json.dumps(plan))
        self.assertEqual(release_plan.load(path, root=self.root), plan)
        for relative in ("dist/release-notes.md", "mise.lock"):
            with self.subTest(relative=relative):
                changed = self.root / relative
                before = changed.read_bytes()
                changed.write_bytes(before + b"changed")
                with self.assertRaises(TaskError):
                    release_plan.load(path, root=self.root)
                changed.write_bytes(before)
        self.repository.git("commit", "--allow-empty", "-qm", "Different source")
        with self.assertRaises(TaskError):
            release_plan.load(path, root=self.root)

    def test_release_refuses_lightweight_tags_and_tags_on_other_commits(self):
        self.repository.git("tag", "-d", "v0.10.0")
        self.repository.git("tag", "v0.10.0")
        with self.assertRaises(TaskError):
            self.plan()
        self.repository.git("tag", "-d", "v0.10.0")
        self.repository.git("tag", "-a", "v0.10.0", "-m", "Release")
        self.repository.git("commit", "--allow-empty", "-qm", "Different source")
        with self.assertRaises(TaskError):
            self.plan()

    def test_tag_policy_excludes_legacy_prerelease_and_noncanonical_versions(self):
        for tag in (
            "v0.9.99",
            "v0.6.8",
            "v0.10.0-rc.1",
            "v00.10.0",
            "v0.10.01",
            "0.10.0",
            "v0.10.0\n",
        ):
            with self.subTest(tag=tag), self.assertRaises(TaskError):
                release_plan.greenfield_tag(tag)
        for tag in ("v0.10.0", "v0.100.2", "v1.0.0"):
            self.assertEqual(release_plan.greenfield_tag(tag), tag[1:])

    def test_signed_plan_requires_the_intended_repository_main_and_exact_dispatch_sha(self):
        source_sha = self.repository.git("rev-parse", "HEAD").strip()
        environment = {
            "GITHUB_REPOSITORY": release_plan.REPOSITORY,
            "GITHUB_REF": "refs/heads/main",
            "GITHUB_SHA": source_sha,
        }
        for changed in (
            {"GITHUB_REPOSITORY": "other/fork"},
            {"GITHUB_REF": "refs/heads/feature"},
            {"GITHUB_SHA": "f" * 40},
        ):
            with patch.dict(os.environ, {**environment, **changed}, clear=True):
                with self.assertRaises(TaskError):
                    release_plan.create(
                        "v0.10.0",
                        "signed",
                        root=self.root,
                        request=lambda endpoint: {"enabled": True},
                    )

    def test_publication_rechecks_tag_identity_and_repository_immutability(self):
        plan = {**self.plan(), "mode": "publish"}
        for enabled, digest in ((False, plan["tag_object"]), (True, "b" * 40)):
            with self.subTest(enabled=enabled, digest=digest), self.assertRaises(TaskError):
                release_plan.recheck(
                    plan,
                    lambda endpoint: (
                        {"enabled": enabled}
                        if endpoint.endswith("immutable-releases")
                        else {"object": {"type": "tag", "sha": digest}}
                    ),
                )

    def test_local_tag_object_can_be_restored_without_recreating_its_timestamp(self):
        plan = self.plan()
        self.repository.git("tag", "-d", "v0.10.0")
        result = subprocess.run(
            ["git", "hash-object", "-t", "tag", "-w", "--stdin"],
            cwd=self.root,
            input=plan["tag_data"],
            text=True,
            capture_output=True,
            check=True,
        )
        self.assertEqual(result.stdout.strip(), plan["tag_object"])


if __name__ == "__main__":
    unittest.main()
