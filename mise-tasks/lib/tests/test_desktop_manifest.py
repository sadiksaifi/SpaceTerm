"""A desktop authorizes exactly the finalized Core packages of its compiled release."""

import base64
import json
import shutil
import sys
import unittest
from unittest.mock import patch

from spaceterm_tasks import TaskError, core_package, desktop_manifest, update_signing

from tests import test_core_catalog as catalog_fixture


class DesktopManifestTests(unittest.TestCase):
    def setUp(self):
        self.fixture = catalog_fixture.CoreCatalogTests()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.root = self.fixture.root
        self.packages = {}
        self.info = {}
        for target in core_package.TARGETS.values():
            archive, signature, info = self.fixture.artifact(target, source_sha256="b" * 64)
            directory = self.root / target
            directory.mkdir()
            shutil.copyfile(archive, directory / "artifact.tar.gz")
            shutil.copyfile(signature, directory / "artifact.sig")
            (directory / "artifact.sha256").write_text(update_signing.sha256(archive) + "\n")
            self.packages[target] = directory
            self.info = info
        self.compiled: dict = {
            "schema": 1,
            "target": core_package.host_target(),
            "update_trust": {"current": "unused", "backup": "unused"},
            "build": {
                name: self.info[name]
                for name in ("identity", "commit", "version", "source_sha256", "protocols")
            },
        }
        self.compiled["build"].update(core_floor=None, desktop_floor=None, apple_team_id=None)
        self.app = self.root / "desktop"

    def stage(self):
        return desktop_manifest.stage_desktop_manifest(
            self.app, self.packages, self.compiled, macos=sys.platform == "darwin"
        )

    def test_stages_exact_signed_bytes_and_a_signed_manifest_after_compilation(self):
        with patch.object(
            desktop_manifest, "compiled_desktop", side_effect=AssertionError("must not compile")
        ):
            manifest = self.stage()
        signature = manifest.with_name("manifest.sig")
        seed = core_package.artifact_signing_seed(self.info["identity"])
        update_signing.verify((update_signing.derived_public_key(seed),), manifest, signature)
        document = json.loads(manifest.read_bytes())
        self.assertEqual(document["build"], self.compiled["build"])
        for entry in document["archives"]:
            package = self.packages[entry["target"]]
            directory = manifest.parent / entry["target"]
            self.assertEqual(entry["sha256"], update_signing.sha256(package / "artifact.tar.gz"))
            self.assertEqual(
                base64.b64decode(entry["signature"]), (package / "artifact.sig").read_bytes()
            )
            self.assertEqual(
                (directory / "artifact.tar.gz").read_bytes(),
                (package / "artifact.tar.gz").read_bytes(),
            )
        resources = self.app / (
            "Contents/Resources" if sys.platform == "darwin" else "share/spaceterm"
        )
        local = self.packages[self.compiled["target"]]
        self.assertEqual(
            (resources / "core-artifact/artifact.tar.gz").read_bytes(),
            (local / "artifact.tar.gz").read_bytes(),
        )
        manifest.write_bytes(manifest.read_bytes() + b" ")
        with self.assertRaises(update_signing.SignatureMismatch):
            update_signing.verify((update_signing.derived_public_key(seed),), manifest, signature)

    def test_refuses_wrong_source_version_identity_and_floors_before_publishing(self):
        for name in ("commit", "source_sha256", "version", "identity", "core_floor", "protocols"):
            with self.subTest(name=name):
                original = self.compiled["build"][name]
                self.compiled["build"][name] = "different"
                with self.assertRaises(TaskError):
                    self.stage()
                self.compiled["build"][name] = original
                self.assertFalse(any(self.app.rglob("manifest.json")))
                self.assertFalse(any(self.app.rglob("core-artifacts")))

    def test_refuses_missing_target_swapped_archive_and_unlisted_same_key_archive(self):
        target = next(iter(self.packages))
        directory = self.packages.pop(target)
        with self.assertRaises(TaskError):
            self.stage()
        self.packages[target] = directory
        other = next(value for native, value in self.packages.items() if native != target)
        shutil.copyfile(other / "artifact.tar.gz", directory / "artifact.tar.gz")
        shutil.copyfile(other / "artifact.sig", directory / "artifact.sig")
        with self.assertRaises(TaskError):
            self.stage()
        archive, signature, _ = self.fixture.artifact(target, source_sha256="c" * 64)
        shutil.copyfile(archive, directory / "artifact.tar.gz")
        shutil.copyfile(signature, directory / "artifact.sig")
        (directory / "artifact.sha256").write_text(update_signing.sha256(archive) + "\n")
        with self.assertRaises(TaskError):
            self.stage()

    def test_precompiled_input_never_invokes_the_compiler(self):
        directory = self.root / "compile/desktop"
        directory.mkdir(parents=True)
        executable = directory / "spaceterm-desktop"
        executable.write_bytes(b"unsigned executable")
        (directory / "desktop-build-info.json").write_text(json.dumps(self.compiled))

        def compiler(_):
            self.fail("precompiled packaging invoked Cargo")

        actual, info = desktop_manifest.compiled_desktop(directory.parent, {}, compiler)
        self.assertEqual(actual, executable)
        self.assertEqual(info, self.compiled)

    def test_release_manifest_requires_the_new_seed_and_exact_compiled_public_key(self):
        self.compiled["build"]["identity"] = "spaceterm"
        seed = bytes(range(32))
        self.compiled["update_trust"]["current"] = update_signing.derived_public_key(seed)
        for environment, message in (
            (
                {"UPDATE_SIGNING_KEY": update_signing.encoded_seed(seed)},
                desktop_manifest.SIGNING_SECRET,
            ),
            (
                {desktop_manifest.SIGNING_SECRET: update_signing.encoded_seed(bytes(range(1, 33)))},
                "differs from compiled",
            ),
        ):
            with self.assertRaisesRegex(TaskError, message):
                desktop_manifest.stage_desktop_manifest(
                    self.app,
                    self.packages,
                    self.compiled,
                    macos=sys.platform == "darwin",
                    environment=environment,
                )

    def test_manifest_signing_with_credentials_never_executes_a_product(self):
        import os
        import subprocess

        run = subprocess.run

        def guarded(arguments, **kwargs):
            if "internal" in arguments or "--build-info" in arguments or "--version" in arguments:
                self.fail("a secret-bearing manifest step executed a product")
            return run(arguments, **kwargs)

        with (
            patch.dict(os.environ, {desktop_manifest.SIGNING_SECRET: "credential-marker"}),
            patch.object(subprocess, "run", side_effect=guarded),
        ):
            manifest = self.stage()
            self.assertTrue(manifest.is_file())
            verified = desktop_manifest.verify_embedded_resources(
                manifest.parent.parent, self.info["identity"], execute=False
            )
            self.assertEqual(verified["build"], self.compiled["build"])
            self.assertEqual(
                verified["core_sha256"],
                {
                    target: update_signing.sha256(directory / "artifact.tar.gz")
                    for target, directory in self.packages.items()
                },
            )

    def test_full_verification_refuses_credentials_before_touching_a_product(self):
        import os

        with patch.dict(os.environ, {desktop_manifest.SIGNING_SECRET: "credential-marker"}):
            with self.assertRaisesRegex(TaskError, "credential-free"):
                desktop_manifest.verify_desktop_release(
                    self.root / "missing.dmg",
                    "v0.10.0",
                    macos=True,
                    source_sha="a" * 40,
                    plan_sha256="b" * 64,
                    core_digests={},
                )

    def test_final_evidence_binds_the_asset_source_plan_and_both_cores(self):
        from spaceterm_tasks import package_macos

        asset = self.root / "final.dmg"
        asset.write_bytes(b"final shipped asset")
        digests = {
            target: update_signing.sha256(directory / "artifact.tar.gz")
            for target, directory in self.packages.items()
        }
        result = {"build": self.compiled["build"], "core_sha256": digests}
        with patch.object(package_macos, "verify", return_value=result) as verify:
            evidence = desktop_manifest.verify_desktop_release(
                asset,
                None,
                macos=True,
                source_sha="a" * 40,
                plan_sha256="c" * 64,
                core_digests=digests,
            )
            verify.assert_called_once_with(dmg=asset, release_tag=None, execute=True)
        self.assertEqual(
            evidence,
            {
                "schema": 1,
                "kind": "desktop-verification",
                "source_sha": "a" * 40,
                "plan_sha256": "c" * 64,
                "asset_sha256": update_signing.sha256(asset),
                "core_sha256": digests,
            },
        )

        def changed(**_):
            asset.write_bytes(b"replaced after verification")
            return result

        with patch.object(package_macos, "verify", side_effect=changed):
            with self.assertRaisesRegex(TaskError, "evidence differs"):
                desktop_manifest.verify_desktop_release(
                    asset,
                    None,
                    macos=True,
                    source_sha="a" * 40,
                    plan_sha256="c" * 64,
                    core_digests=digests,
                )


class PrecompiledMacBundleTests(unittest.TestCase):
    def test_assembly_preserves_executable_resources_and_framework_links_without_cargo(self):
        import plistlib
        import tempfile
        from pathlib import Path

        from spaceterm_tasks import package_macos

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stage = root / "stage"
            stage.mkdir()
            (stage / "SpaceTerm.icns").write_bytes(b"icon")
            assets = root / "Assets.car"
            assets.write_bytes(b"compiled assets")
            executable = root / "spaceterm-desktop"
            executable.write_bytes(b"compiled desktop")
            framework = root / "Sparkle.framework"
            version = framework / "Versions/B"
            version.mkdir(parents=True)
            (version / "Sparkle").write_bytes(b"compiled updater")
            (framework / "Versions/Current").symlink_to("B")
            (framework / "Sparkle").symlink_to("Versions/Current/Sparkle")
            app = root / "SpaceTerm.app"
            with (
                patch.object(package_macos, "STAGE", stage),
                patch.object(
                    package_macos.subprocess, "run", side_effect=AssertionError("Cargo invoked")
                ),
            ):
                package_macos.assemble_bundle(
                    app,
                    executable,
                    {"CFBundleExecutable": "SpaceTerm", "CFBundleIdentifier": "test.spaceterm"},
                    {
                        "resources": [{"src": str(assets), "target": "Assets.car"}],
                        "macos": {"frameworks": [str(framework)]},
                    },
                )
            self.assertEqual(
                (app / "Contents/MacOS/SpaceTerm").read_bytes(), executable.read_bytes()
            )
            self.assertEqual(
                (app / "Contents/Resources/Assets.car").read_bytes(), assets.read_bytes()
            )
            self.assertTrue(
                (app / "Contents/Frameworks/Sparkle.framework/Versions/Current").is_symlink()
            )
            self.assertEqual(
                plistlib.loads((app / "Contents/Info.plist").read_bytes())["CFBundleIdentifier"],
                "test.spaceterm",
            )

    def test_a_compile_artifact_cannot_be_relabelled_for_another_source_or_release(self):
        import subprocess
        import tempfile
        from pathlib import Path

        from spaceterm_tasks.core_builds import _source

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)

            def git(*arguments):
                return subprocess.run(
                    [
                        "git",
                        "-c",
                        "user.name=Test",
                        "-c",
                        "user.email=test@example.com",
                        *arguments,
                    ],
                    cwd=root,
                    check=True,
                    capture_output=True,
                    text=True,
                ).stdout.strip()

            git("init", "--quiet")
            source = root / "source.rs"
            source.write_text("compiled source")
            git("add", "source.rs")
            git("commit", "--quiet", "-m", "source")
            git("tag", "-a", "v0.10.0", "-m", "release")
            git("tag", "v0.10.1")
            info = {
                "build": {
                    "identity": "spaceterm",
                    "commit": git("rev-parse", "HEAD"),
                    "version": "0.10.0",
                    "source_sha256": _source(root)[2],
                }
            }
            with patch.object(desktop_manifest, "ROOT", root):
                desktop_manifest.validate_compiled(info, "v0.10.0")
                info["build"]["version"] = "0.10.1"
                with self.assertRaisesRegex(TaskError, "selected release tag"):
                    desktop_manifest.validate_compiled(info, "v0.10.0")
                with self.assertRaisesRegex(TaskError, "selected release tag"):
                    desktop_manifest.validate_compiled(info, "v0.10.1")
                info["build"]["version"] = "0.10.0"
                source.write_text("different source")
                with self.assertRaisesRegex(TaskError, "packaging source"):
                    desktop_manifest.validate_compiled(info, "v0.10.0")
