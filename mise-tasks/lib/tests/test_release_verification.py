"""Publication admits native verification only for the exact planned product bytes."""

import hashlib
import json
import os
import re
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import ROOT, TaskError, release, release_inputs, release_plan


class WorkflowBoundaryTests(unittest.TestCase):
    def test_secret_jobs_install_fresh_tools_and_native_checks_use_separate_jobs(self):
        # actionlint checks YAML syntax. These assertions guard the security boundary
        # against reintroducing caches or secrets while wiring new release stages.
        workflows = {}
        self.assertEqual(
            sorted(path.name for path in (ROOT / ".github/workflows").iterdir()), ["release.yml"]
        )
        for name in ("release",):
            text = (ROOT / f".github/workflows/{name}.yml").read_text()
            self.assertIn("MISE_LOCKED: 'true'", text)
            self.assertIn("\ncache-mode: none\n", text.split("jobs:\n", 1)[0])
            pieces = re.split(r"^  ([a-z][a-z-]*):\n", text.split("jobs:\n", 1)[1], flags=re.M)
            workflows[name] = dict(zip(pieces[1::2], pieces[2::2], strict=True))
            for job, block in workflows[name].items():
                if "environment: release" not in block:
                    continue
                with self.subTest(workflow=name, job=job):
                    self.assertNotRegex(block, r"uses: (?:actions/cache|Swatinem/rust-cache)")
                    self.assertNotIn("cache-mode:", block)
                    self.assertNotIn("uses: ./.github/actions/release-build-tools", block)
                    setup = block.split("uses: jdx/mise-action@", 1)[1].split("\n      -", 1)[0]
                    self.assertIn("cache: false", setup)
                    self.assertNotRegex(block, r"release:pipeline verify-(?:core|desktop)")
        for product in ("core", "desktop"):
            for platform in ("macos", "linux"):
                job = f"verify-{product}-{platform}"
                block = workflows["release"][job]
                with self.subTest(job=job):
                    self.assertNotIn("environment:", block)
                    self.assertNotIn("secrets.", block)
                    self.assertNotIn("cache-mode:", block)
                    self.assertIn(f"release:pipeline verify-{product}", block)
                    self.assertIn(
                        f"needs.{job}.result == 'success'", workflows["release"]["publish"]
                    )
        # Every job plans for itself, so nothing waits on a planning job, and signing waits
        # only on its compilation, never on validation.
        self.assertNotIn("plan", workflows["release"])
        for job in ("core-macos", "core-linux"):
            platform = job.split("-")[1]
            self.assertIn(f"needs: compile-{platform}\n", workflows["release"][job])
        self.assertIn("needs.validate.result == 'success'", workflows["release"]["publish"])
        # A rerun of a signer must reuse its finalized bytes: a packaged desktop resumed from an
        # earlier notarization embeds the earlier Core archives, and rebuilt archives differ.
        for job, lane in (
            ("core-macos", "core-macos"),
            ("core-linux", "core-linux"),
            ("desktop-macos", "desktop"),
        ):
            block = workflows["release"][job]
            with self.subTest(job=job):
                self.assertIn(
                    f"uses: ./.github/actions/release-resume\n        with:\n          lane: {lane}\n",
                    block,
                )
                retained = block.split(f"name: notary-{lane}-${{{{ github.run_attempt }}}}", 1)
                self.assertEqual(len(retained), 2)
                self.assertIn("if: always()", retained[0].rsplit("\n      - ", 1)[1])


class VerificationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.plan_file = self.root / "plan.json"
        self.plan_file.write_bytes(b"frozen plan")
        self.plan = {"source": "a" * 40, "tag": "v0.10.0", "version": "0.10.0"}
        self.targets = ("aarch64-apple-darwin", "x86_64-unknown-linux-gnu")
        for owner, name, value in (
            (release_inputs, "DIST", self.root),
            (release, "CORE", self.root / "core"),
            (release, "DESKTOP_MACOS", self.root / "macos"),
            (release, "DESKTOP_LINUX", self.root / "linux"),
            (release_plan, "PLAN", self.plan_file),
        ):
            mocked = patch.object(owner, name, value)
            mocked.start()
            self.addCleanup(mocked.stop)
        (self.root / "core-verification").mkdir()
        for target in self.targets:
            directory = self.root / "core" / target
            directory.mkdir(parents=True)
            for name in ("artifact.tar.gz", "artifact.sig", "artifact.sha256"):
                (directory / name).write_bytes((target + name).encode())
            self.write_proof(target)

    def write_proof(self, target):
        proof = {
            "source": "a" * 40,
            "plan_sha256": hashlib.sha256(b"frozen plan").hexdigest(),
            "target": target,
            "verified": True,
            "files": {
                name: hashlib.sha256((target + name).encode()).hexdigest()
                for name in ("artifact.tar.gz", "artifact.sig", "artifact.sha256")
            },
        }
        (self.root / "core-verification" / f"{target}.json").write_text(json.dumps(proof))

    def test_native_core_receipts_cannot_authorize_changed_bytes_or_another_plan(self):
        release_inputs.require_core_evidence(self.plan)
        for name in ("artifact.tar.gz", "artifact.sig", "artifact.sha256"):
            changed = self.root / "core" / self.targets[0] / name
            before = changed.read_bytes()
            changed.write_bytes(b"substituted bytes")
            with self.subTest(name=name), self.assertRaises(TaskError):
                release_inputs.require_core_evidence(self.plan)
            changed.write_bytes(before)
        self.plan_file.write_bytes(b"another plan")
        with self.assertRaises(TaskError):
            release_inputs.require_core_evidence(self.plan)

    def test_an_unverified_or_stale_source_receipt_grants_no_authority(self):
        path = self.root / "core-verification" / f"{self.targets[0]}.json"
        original = json.loads(path.read_bytes())
        for change in ({"verified": False}, {"source": "b" * 40}):
            path.write_text(json.dumps({**original, **change}))
            with self.subTest(change=change), self.assertRaises(TaskError):
                release_inputs.require_core_evidence(self.plan)

    def test_product_verification_refuses_a_process_with_private_release_inputs(self):
        for name in (
            "SPACETERM_RELEASE_UPDATE_ED25519_SEED",
            "SPACETERM_RELEASE_DEVID_P12_BASE64",
            "SPACETERM_RELEASE_DEVID_P12_PASSWORD",
            "SPACETERM_RELEASE_NOTARY_P8",
            "HOMEBREW_TAP_PAT",
        ):
            with patch.dict(os.environ, {name: "private fixture"}, clear=True):
                for verify in (release_inputs.verify_core, release_inputs.verify_desktop):
                    with (
                        self.subTest(name=name, verify=verify.__name__),
                        self.assertRaisesRegex(TaskError, "credential-free job"),
                    ):
                        verify()

    def test_native_desktop_receipt_must_bind_the_original_producer_asset_and_plan(self):
        core = {
            target: hashlib.sha256((target + "artifact.tar.gz").encode()).hexdigest()
            for target in self.targets
        }
        shared = {
            "source": "a" * 40,
            "plan_sha256": hashlib.sha256(b"frozen plan").hexdigest(),
            "core": core,
        }
        (self.root / "desktop-verification").mkdir()
        for target, directory, name in (
            (self.targets[0], release.DESKTOP_MACOS, "SpaceTerm-0.10.0-macos-arm64.dmg"),
            (
                self.targets[1],
                release.DESKTOP_LINUX,
                "spaceterm-desktop-0.10.0-linux-x86_64.tar.gz",
            ),
        ):
            directory.mkdir()
            payload = target.encode()
            (directory / name).write_bytes(payload)
            digest = hashlib.sha256(payload).hexdigest()
            (directory / "evidence.json").write_text(
                json.dumps({**shared, "files": {name: digest}})
            )
            proof = {
                "schema": 1,
                "kind": "desktop-verification",
                "source_sha": "a" * 40,
                "plan_sha256": shared["plan_sha256"],
                "asset_sha256": digest,
                "core_sha256": core,
            }
            (self.root / "desktop-verification" / f"{target}.json").write_text(json.dumps(proof))
        (self.root / "floor-smoke.json").write_text(
            json.dumps({"source": "a" * 40, "tag": "v0.10.0", "sha256": core[self.targets[1]]})
        )
        release_inputs.verify_evidence(self.plan)
        path = self.root / "desktop-verification" / f"{self.targets[0]}.json"
        original = json.loads(path.read_bytes())
        for changed in (
            {"asset_sha256": "f" * 64},
            {"plan_sha256": "f" * 64},
            {"source_sha": "b" * 40},
        ):
            path.write_text(json.dumps({**original, **changed}))
            with (
                self.subTest(changed=changed),
                self.assertRaisesRegex(TaskError, "native desktop verification differs"),
            ):
                release_inputs.verify_evidence(self.plan)
