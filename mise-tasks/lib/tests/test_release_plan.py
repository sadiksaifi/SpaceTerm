"""Every release job derives the same plan from a stable greenfield tag, exact source and
public inputs."""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import ROOT, TaskError, release_floor, release_plan, update_signing

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
        self.repository.git("tag", "-a", "v0.10.0", "-m", "Release SpaceTerm 0.10.0")
        # Do not inherit a caller's plan or signing identity in this disposable repository.
        environment = patch.dict(os.environ, {update_signing.DRY_RUN: "1"}, clear=True)
        environment.start()
        self.addCleanup(environment.stop)

    def plan(self):
        return release_plan.create("v0.10.0", root=self.root)

    def test_plan_freezes_the_tag_time_floors_and_exact_inputs(self):
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
        path = self.root / "dist/release-plan.json"
        path.write_text(release_plan.encode(plan))
        self.assertEqual(release_plan.load(path, root=self.root), plan)
        changed = self.root / "mise.lock"
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

    def test_release_plan_requires_the_intended_repository_main_and_exact_dispatch_sha(self):
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
                    release_plan.create("v0.10.0", root=self.root, published=lambda: set())

    def test_publication_rechecks_the_tag_object_with_the_ordinary_token(self):
        plan = self.plan()
        endpoints = []

        def request(endpoint):
            endpoints.append(endpoint)
            return {"object": {"type": "tag", "sha": digest}}

        digest = plan["tag_object"]
        release_plan.recheck(plan, request)
        digest = "b" * 40
        with self.assertRaises(TaskError):
            release_plan.recheck(plan, request)
        self.assertEqual(endpoints, [f"repos/{release_plan.REPOSITORY}/git/ref/tags/v0.10.0"] * 2)

    def test_every_job_of_one_run_writes_the_identical_tag_and_plan(self):
        self.repository.git("tag", "-d", "v0.10.0")
        created = "2026-11-06T15:00:00Z"
        runs = []

        def request(endpoint):
            runs.append(endpoint)
            return {"created_at": created}

        # A second job's checkout of the same source.
        other = self.root.parent / f"{self.root.name}-job"
        self.addCleanup(shutil.rmtree, other, True)
        shutil.copytree(self.root, other, symlinks=True)
        plans = []
        with patch.dict(os.environ, {"GITHUB_RUN_ID": "42"}):
            for root, clock in ((self.root, "1900000000 +0000"), (other, "1900000600 +0000")):
                # Jobs start at different times; only the run's creation time counts.
                with patch.dict(os.environ, {"GIT_COMMITTER_DATE": clock}):
                    self.assertEqual(
                        release_plan.tag("0.10.0", root=root, request=request), "v0.10.0"
                    )
                plans.append(release_plan.create("v0.10.0", root=root))
        self.assertEqual(plans[0], plans[1])
        self.assertEqual(plans[0]["published_at"], 1793977200)
        self.assertEqual(runs, [f"repos/{release_plan.REPOSITORY}/actions/runs/42"] * 2)
        # An existing tag, such as one an earlier attempt published, is kept unchanged.
        with patch.dict(os.environ, {"GITHUB_RUN_ID": "43"}):
            release_plan.tag("0.10.0", root=self.root, request=lambda endpoint: self.fail())
        self.assertEqual(release_plan.create("v0.10.0", root=self.root), plans[0])

    def test_tagging_outside_the_release_workflow_requires_an_existing_tag(self):
        self.repository.git("tag", "-d", "v0.10.0")
        with self.assertRaises(TaskError):
            release_plan.tag("0.10.0", root=self.root, request=lambda endpoint: self.fail())

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


@unittest.skipUnless(shutil.which("tagsmith"), "Tagsmith runs through mise run")
class TagPolicyTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        origin, self.root = Path(temporary.name) / "origin.git", Path(temporary.name) / "work"
        self.git(Path(temporary.name), "init", "-q", "--bare", "-b", "main", str(origin))
        self.git(Path(temporary.name), "clone", "-q", str(origin), str(self.root))
        shutil.copyfile(ROOT / ".tagsmith.jsonc", self.root / ".tagsmith.jsonc")
        self.git(self.root, "add", ".tagsmith.jsonc")
        self.git(self.root, "commit", "-qm", "chore: start")
        self.git(self.root, "push", "-q", "origin", "main")
        self.git(self.root, "checkout", "-q", "--detach")

    def git(self, root, *arguments):
        subprocess.run(
            ["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", *arguments],
            cwd=root,
            check=True,
            capture_output=True,
        )

    def test_a_rerun_after_the_tag_was_pushed_still_passes_the_policy(self):
        release_plan.check_policy("0.11.0", root=self.root)
        self.git(self.root, "tag", "-a", "v0.11.0", "-m", "Release SpaceTerm 0.11.0")
        self.git(self.root, "push", "-q", "origin", "v0.11.0")
        release_plan.check_policy("0.11.0", root=self.root)
        with self.assertRaises(TaskError):
            release_plan.check_policy("0.9.0", root=self.root)

    def test_the_deleted_0_10_0_fails_the_policy(self):
        with self.assertRaises(TaskError):
            release_plan.check_policy("0.10.0", root=self.root)


if __name__ == "__main__":
    unittest.main()
