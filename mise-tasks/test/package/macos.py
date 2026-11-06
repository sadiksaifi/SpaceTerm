#!/usr/bin/env python
# MISE description="Exercise macOS packaging with precompiled fixtures and real bundles"
"""Exercise macOS packaging with precompiled fixtures and real bundles."""

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import (
    ROOT,
    core_package,
    macos_codesign,
    macos_signing,
    release_floor,
    sparkle,
    update_signing,
)
from tests.core_artifact_fixture import SEED, configure_trust, prepare
from tests.macos_signing_fixture import NAME, TEAM, certificate


class PackageTests(unittest.TestCase):
    """Package precompiled fixtures statically, then execute in a credential-free process.

    Release fixtures use ad hoc signing and the fixture update key, without notarization.
    """

    @classmethod
    def setUpClass(cls):
        temporary = tempfile.TemporaryDirectory(prefix="spaceterm-package-certificate-")
        cls.addClassCleanup(temporary.cleanup)
        _, cls.credentials = certificate(Path(temporary.name))

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="spaceterm-package-test-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name) / "repo with spaces"
        self.root.mkdir()
        for directory in ("packaging", "assets", "mise-tasks"):
            shutil.copytree(
                ROOT / directory,
                self.root / directory,
                ignore=shutil.ignore_patterns("__pycache__"),
            )
        shutil.copyfile(ROOT / "Cargo.toml", self.root / "Cargo.toml")
        major = self.root / core_package.PROTOCOL_SOURCE
        major.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / core_package.PROTOCOL_SOURCE, major)
        configure_trust(self.root)
        # The appcast's Update Floor comes from the declared Data Generations.
        generations = self.root / release_floor.SOURCE
        generations.parent.mkdir(parents=True)
        generations.write_text(
            "".join(
                f"pub const {name}: Generations = Generations {{ oldest: 1, current: 1 }};\n"
                for name in release_floor.ROOTS
            )
        )
        # Release packaging requires a clean Git checkout.
        (self.root / ".gitignore").write_text("/target/\n/dist/\n__pycache__/\n")
        for arguments in (("init", "-q"), ("add", "-A"), ("commit", "-qm", "fixture")):
            subprocess.run(
                [
                    "git",
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                    *arguments,
                ],
                cwd=self.root,
                check=True,
                capture_output=True,
            )

        self.bin = Path(self.directory.name) / "bin"
        self.bin.mkdir()
        # Static packaging must never compile. Bundling, signing, and verification use
        # real native tools, with product execution checked in a separate process.
        (self.bin / "cargo").write_text(f"""#!{sys.executable}
raise AssertionError("static packaging invoked Cargo")
""")
        (self.bin / "cargo").chmod(0o755)
        # GitHub is the other external boundary: it lists no published release, so the
        # release is its own Update Floor and its appcast continues no earlier one.
        (self.bin / "gh").write_text(f"""#!{sys.executable}
import sys
sys.exit(0 if sys.argv[1:3] == ["api", "--paginate"] else 1)
""")
        (self.bin / "gh").chmod(0o755)
        self.notes = Path(self.directory.name) / "release-notes.md"
        self.notes.write_text("### Features\n\n- Add terminal panes\n")
        self.env = {
            **os.environ,
            **self.credentials,
            "PATH": f"{self.bin}{os.pathsep}{os.environ['PATH']}",
            "PYTHONPATH": str(self.root / "mise-tasks/lib"),
            # Release packaging here is the dry run: a self-signed certificate has no Team ID.
            update_signing.DRY_RUN: "1",
            update_signing.SIGNING_SECRET: update_signing.encoded_seed(SEED),
            macos_codesign.TEAM_ID: TEAM,
            macos_codesign.SIGNING_IDENTITY: macos_codesign.ADHOC,
        }
        self.compiled = self.root / "target/compiled"
        self.artifact = self.compiled / "desktop/spaceterm-desktop"
        self.compiled_identities = {}
        self.stale = self.root / "target/release/spaceterm"
        self.compile_executable(self.stale, "SpaceTerm Development")

    def run_command(self, *args, **kwargs):
        return subprocess.run(
            args, cwd=self.root, capture_output=True, text=True, check=True, **kwargs
        )

    def compile_executable(self, output, identity, candidate_digest="", provenance=None):
        self.compiled_identities[output] = identity
        build = json.dumps(
            {
                "schema": 1,
                "identity": {
                    "SpaceTerm": "spaceterm",
                    "SpaceTerm Preflight": "spaceterm-preflight",
                    "SpaceTerm Development": "spaceterm-development",
                }[identity],
                "target": "aarch64-apple-darwin",
                "core_candidate_sha256": candidate_digest,
                "commit": provenance["commit"] if provenance else None,
                "core_provenance": provenance,
            }
        )
        output.parent.mkdir(parents=True, exist_ok=True)
        # The release executable links Sparkle like the real desktop, so the launch check
        # proves that its signature lets it load the bundled framework.
        updater = (
            [
                "-F",
                str(sparkle.directory()),
                "-framework",
                "Sparkle",
                "-Wl,-rpath,@executable_path/../Frameworks",
            ]
            if identity == "SpaceTerm"
            else []
        )
        self.run_command(
            "xcrun",
            "clang",
            "-arch",
            "arm64",
            "-mmacosx-version-min=26.0",
            *updater,
            "-x",
            "c",
            "-",
            "-o",
            str(output),
            input=f"""
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
int main(int argc, char **argv) {{
    if (getenv("SPACETERM_SSH_ASKPASS_MODE")) return 2;
    if (argc == 2 && strcmp(argv[1], "--build-info") == 0) {{
        puts({json.dumps(build)});
        return 0;
    }}
    if (argc == 2 && strcmp(argv[1], "--version") == 0) {{
        puts("{identity} {provenance["version"] if provenance else "0.0.0"}");
        return 0;
    }}
    return 1;
}}
""",
        )

    def python(self, code, *, environment):
        return subprocess.run(
            [sys.executable, "-c", code],
            cwd=self.root,
            env=environment,
            capture_output=True,
            text=True,
            timeout=300,
        )

    def package(self, release_tag=None, *, candidate_digest=None, missing_executable=False):
        # Both Core packages and the desktop must have the same compiled provenance.
        archive, _ = prepare(
            self.root,
            self.env,
            release_tag,
            include_catalog=True,
        )
        digest = (
            candidate_digest
            if candidate_digest is not None
            else hashlib.sha256(archive.read_bytes()).hexdigest()
        )
        with tarfile.open(archive) as packaged:
            document = packaged.extractfile("core/build.json")
            assert document is not None
            core = json.load(document)
        provenance = {
            name: core[name]
            for name in ("identity", "commit", "version", "source_sha256", "protocols")
        }
        provenance.update(
            core_floor=core["floor"],
            desktop_floor=release_tag[1:] if release_tag else None,
            apple_team_id=TEAM if release_tag else None,
        )
        self.compile_executable(
            self.artifact, self.compiled_identities[self.artifact], digest, provenance
        )
        (self.artifact.parent / "desktop-build-info.json").write_text(
            json.dumps(
                {
                    "schema": 1,
                    "target": core["target"],
                    "update_trust": json.loads((self.root / update_signing.TRUST).read_text()),
                    "build": provenance,
                }
            )
        )
        if missing_executable:
            self.artifact.unlink()
        if release_tag:
            self.run_command(
                "git",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "tag",
                "-fa",
                release_tag,
                "-m",
                "fixture",
            )
        # Detect product execution even if an implementation clears the child's env.
        return self.python(
            f"""
import subprocess
from pathlib import Path
from spaceterm_tasks import main, package_macos, release
run = subprocess.run
def static_run(arguments, **kwargs):
    executable = Path(arguments[0]).name.lower()
    if executable.startswith("spaceterm") or any(
        argument in ("internal", "--build-info", "--version") for argument in arguments[1:]
    ):
        raise AssertionError("secret-bearing packaging executed a product")
    return run(arguments, **kwargs)
subprocess.run = static_run
def package():
    package_macos.package(
        {release_tag!r}, {str(self.notes)!r}, compiled={str(self.compiled)!r}, execute=False
    )
    if {release_tag!r}:
        release.appcast({release_tag!r})
main(package)
""",
            environment=self.env,
        )

    def verify(self, release_tag=None, *, app=None, dmg=None, credentials=False):
        # No signing key or certificate enters the executable verification process.
        environment = macos_signing.signing_environment(self.env)
        environment.pop("HOMEBREW_TAP_PAT", None)
        if credentials:
            environment.update(self.credentials)
        if release_tag is None:
            arguments = []
            if app is not None:
                arguments.extend(("--app", str(app)))
            if dmg is not None:
                arguments.extend(("--dmg", str(dmg)))
            return subprocess.run(
                [
                    sys.executable,
                    str(self.root / "mise-tasks/preflight/verify/macos.py"),
                    *arguments,
                ],
                cwd=self.root,
                env=environment,
                capture_output=True,
                text=True,
                timeout=300,
            )
        return self.python(
            f"""
from spaceterm_tasks import main, package_macos
def verify():
    package_macos.verify({str(app) if app else None!r},
                         {str(dmg) if dmg else None!r}, {release_tag!r})
main(verify)
""",
            environment=environment,
        )

    def executable_uuid(self, executable):
        # Signing can change Mach-O layout even after removing the signature.
        # The linker's UUID identifies the compiled artifact across signing.
        result = self.run_command("xcrun", "dwarfdump", "--uuid", str(executable))
        return result.stdout.split()[1]

    def test_static_packaging_installs_compiled_artifact_and_ignores_a_stale_binary(self):
        self.compile_executable(self.artifact, "SpaceTerm Preflight")
        result = self.package()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        bundled = self.root / "dist/SpaceTerm Preflight.app/Contents/MacOS/SpaceTerm Preflight"
        self.assertEqual(self.executable_uuid(bundled), self.executable_uuid(self.artifact))
        self.assertNotEqual(self.executable_uuid(bundled), self.executable_uuid(self.stale))
        core = bundled.with_name("spaceterm")
        self.assertFalse(core.exists())
        self.assertEqual(
            (bundled.parent.parent / "Resources/shell-integration/elvish/rc.elv").read_bytes(),
            (ROOT / "assets/shell-integration/elvish/rc.elv").read_bytes(),
        )
        artifact = bundled.parent.parent / "Resources/core-artifact"
        self.assertEqual(
            (artifact / "artifact.tar.gz").read_bytes(),
            Path(self.env["SPACETERM_CLI_ARCHIVE"]).read_bytes(),
        )
        self.assertEqual(
            (artifact / "artifact.sig").read_bytes(),
            Path(self.env["SPACETERM_CLI_SIGNATURE"]).read_bytes(),
        )
        catalog = artifact.with_name("core-artifacts") / "aarch64-apple-darwin"
        self.assertEqual(
            (catalog / "artifact.tar.gz").read_bytes(), (artifact / "artifact.tar.gz").read_bytes()
        )
        self.assertEqual(
            (catalog / "artifact.sig").read_bytes(), (artifact / "artifact.sig").read_bytes()
        )
        self.assertEqual(
            (catalog / "artifact.sha256").read_text(),
            hashlib.sha256((artifact / "artifact.tar.gz").read_bytes()).hexdigest() + "\n",
        )
        verified = self.verify()
        self.assertEqual(verified.returncode, 0, verified.stdout + verified.stderr)
        refused = self.verify(credentials=True)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn(
            "product execution requires a credential-free verification job", refused.stderr
        )

    def test_execution_rejects_an_artifact_different_from_the_compiled_pin(self):
        self.compile_executable(self.artifact, "SpaceTerm Preflight")
        result = self.package(candidate_digest="0" * 64)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        verified = self.verify()
        self.assertNotEqual(verified.returncode, 0)
        self.assertIn("desktop provenance differs from its resources", verified.stderr)

    def test_execution_rejects_development_identity_in_a_preflight_bundle(self):
        self.compile_executable(self.artifact, "SpaceTerm Development")
        result = self.package()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        verified = self.verify()
        self.assertNotEqual(verified.returncode, 0)
        self.assertIn("application identity", verified.stderr)

    def test_missing_compiled_artifact_does_not_package_a_stale_executable(self):
        self.compile_executable(self.artifact, "SpaceTerm Preflight")
        result = self.package(missing_executable=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("compiled desktop executable is unavailable", result.stderr)
        self.assertFalse((self.root / "dist/SpaceTerm Preflight.app").exists())

    def test_release_packaging_installs_the_reported_release_identity(self):
        self.compile_executable(self.artifact, "SpaceTerm")
        result = self.package("v0.10.0")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        bundled = self.root / "dist/SpaceTerm.app/Contents/MacOS/SpaceTerm"
        self.assertEqual(self.executable_uuid(bundled), self.executable_uuid(self.artifact))
        self.assertNotEqual(self.executable_uuid(bundled), self.executable_uuid(self.stale))
        self.assertNotIn("spaceterm", {path.name for path in bundled.parent.iterdir()})
        catalog = bundled.parent.parent / "Resources/core-artifacts"
        self.assertEqual(
            set(path.name for path in catalog.iterdir()),
            {"aarch64-apple-darwin", "x86_64-unknown-linux-gnu", "manifest.json", "manifest.sig"},
        )
        for value in self.credentials.values():
            self.assertNotIn(value, result.stdout + result.stderr)
        app = self.root / "dist/SpaceTerm.app"
        framework = app / "Contents/Frameworks/Sparkle.framework"
        self.assertFalse((framework / "Versions/B/XPCServices").exists())
        for code in (
            app,
            framework / "Versions/B/Autoupdate",
            framework / "Versions/B/Updater.app",
        ):
            # A dry run signs without the hardened runtime, so ad hoc code can load Sparkle.
            self.assertIn("flags=0x2(adhoc)", macos_codesign.details(code))
        verified = self.verify("v0.10.0")
        self.assertEqual(verified.returncode, 0, verified.stdout + verified.stderr)
        self.assertEqual(macos_codesign.signed_entitlements(app), {})
        self.assertIn("Dry run: notarization and stapling skipped for SpaceTerm.app", result.stdout)
        self.assertIn(
            "Dry run: notarization and stapling skipped for SpaceTerm.dmg",
            result.stdout,
        )
        self.assertEqual(
            {path.name for path in (self.root / "dist/release-desktop-macos").iterdir()},
            {"SpaceTerm-0.10.0-macos-arm64.dmg", "appcast.xml"},
        )

    def test_release_packaging_requires_the_release_notes_before_building(self):
        self.compile_executable(self.artifact, "SpaceTerm")
        self.notes.unlink()
        result = self.package("v0.10.0")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("embeds its release notes", result.stderr)
        self.assertFalse((self.root / "target/package-macos").exists())

    def test_verification_rejects_a_tampered_core_catalog(self):
        self.compile_executable(self.artifact, "SpaceTerm Preflight")
        result = self.package()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        catalog = self.root / "dist/SpaceTerm Preflight.app/Contents/Resources/core-artifacts"
        (catalog / "aarch64-apple-darwin/artifact.sha256").write_text("0" * 64 + "\n")
        verified = self.verify()
        self.assertNotEqual(verified.returncode, 0)
        self.assertIn("digest mismatch", verified.stderr)

    def test_release_requires_the_team_and_its_signing_secrets_before_building(self):
        self.compile_executable(self.artifact, "SpaceTerm")
        for changes, message in (
            ({macos_codesign.TEAM_ID: None}, macos_codesign.TEAM_ID),
            ({macos_codesign.SIGNING_IDENTITY: None}, macos_codesign.SIGNING_IDENTITY),
            ({update_signing.DRY_RUN: None}, "a release signs with Developer ID"),
            (
                {
                    update_signing.DRY_RUN: None,
                    macos_codesign.SIGNING_IDENTITY: NAME,
                    "SPACETERM_RELEASE_DEVID_P12_BASE64": None,
                    "SPACETERM_RELEASE_DEVID_P12_PASSWORD": None,
                },
                "release signing certificate and password are required",
            ),
        ):
            with self.subTest(message=message):
                environment = dict(self.env)
                for key, value in changes.items():
                    if value is None:
                        environment.pop(key, None)
                    else:
                        environment[key] = value
                with patch.object(self, "env", environment):
                    result = self.package("v0.10.0")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertFalse((self.root / "target/package-macos/binaries").exists())
                self.assertFalse((self.root / "dist/SpaceTerm.app").exists())

    def test_release_packaging_rejects_a_dirty_checkout_before_building(self):
        self.compile_executable(self.artifact, "SpaceTerm")
        with (self.root / "Cargo.toml").open("a") as manifest:
            manifest.write("\n")
        result = self.package("v0.10.0")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("a release package requires a clean checkout", result.stderr)
        self.assertFalse((self.root / "target/package-macos").exists())
        self.assertFalse((self.root / "dist/SpaceTerm.app").exists())

    def test_release_execution_rejects_a_preflight_identity_with_the_same_name_prefix(self):
        self.compile_executable(self.artifact, "SpaceTerm Preflight")
        result = self.package("v0.10.0")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        verified = self.verify("v0.10.0")
        self.assertNotEqual(verified.returncode, 0)
        self.assertIn("application identity must be SpaceTerm", verified.stderr)

    def test_verification_rejects_a_different_identity_in_the_disk_image(self):
        self.compile_executable(self.artifact, "SpaceTerm Preflight")
        result = self.package()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        disk_root = Path(self.directory.name) / "disk"
        disk_root.mkdir()
        app = self.root / "dist/SpaceTerm Preflight.app"
        disk_app = disk_root / app.name
        shutil.copytree(app, disk_app)
        self.compile_executable(
            disk_app / "Contents/MacOS/SpaceTerm Preflight", "SpaceTerm Development"
        )
        self.run_command("codesign", "--force", "--sign", "-", "--timestamp=none", str(disk_app))
        (disk_root / "Applications").symlink_to("/Applications")
        # The window layout comes from the packaged image; hdiutil drops an invalid .DS_Store.
        packaged = Path(self.directory.name) / "packaged"
        packaged.mkdir()
        self.run_command(
            "hdiutil",
            "attach",
            "-nobrowse",
            "-readonly",
            "-mountpoint",
            str(packaged),
            str(self.root / "dist/SpaceTerm Preflight.dmg"),
        )
        try:
            for name in (".background.tiff", ".DS_Store"):
                shutil.copyfile(packaged / name, disk_root / name)
        finally:
            self.run_command("hdiutil", "detach", str(packaged))
        dmg = Path(self.directory.name) / "wrong-identity.dmg"
        self.run_command(
            "hdiutil", "create", "-srcfolder", str(disk_root), "-format", "UDZO", str(dmg)
        )
        result = self.verify(app=app, dmg=dmg)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("dmg executable application identity", result.stderr)


class CurrentMachinePackageTests(unittest.TestCase):
    def test_current_machine_passes_signed_package_verification(self):
        # Build the real server, including its PTY/shell check, rather than the C fixture
        # used to isolate desktop packaging. Only the artifact destination is disposable.
        with tempfile.TemporaryDirectory(prefix="spaceterm-current-core-package-") as temporary:
            with patch.object(core_package, "DIST", Path(temporary)):
                archive, signature = core_package.package("spaceterm-preflight")
            destination = Path(temporary) / "desktop"
            core_package.stage_desktop_artifact(
                archive, signature, destination, identity="spaceterm-preflight"
            )
            self.assertEqual((destination / "artifact.tar.gz").read_bytes(), archive.read_bytes())
            self.assertEqual((destination / "artifact.sig").read_bytes(), signature.read_bytes())
            verified = Path(temporary) / "verified"
            verified.mkdir()
            tree, _, _ = core_package.verified_copy(
                archive, signature, verified, None, identity="spaceterm-preflight"
            )
            launcher = Path(temporary) / "bin/spaceterm"
            launcher.parent.mkdir()
            launcher.symlink_to(
                tree
                / core_package.executable_path(core_package.host_target(), "spaceterm-preflight")
            )
            result = subprocess.run(
                [launcher, "internal", "check"],
                env={},
                cwd=tree,
                capture_output=True,
                timeout=15,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, b"SpaceTerm Core checks passed.\n")
            self.assertEqual(result.stderr, b"")


if __name__ == "__main__":
    unittest.main()
