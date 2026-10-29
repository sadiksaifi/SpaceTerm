#!/usr/bin/env python
# MISE description="Exercise macOS packaging with controlled Cargo artifacts and real bundles"
"""Exercise macOS packaging with controlled Cargo artifacts and real bundles."""

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import ROOT, server_package
from tests.macos_signing_fixture import certificate
from tests.server_artifact_fixture import configure_trust, prepare


class PackageTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        temporary = tempfile.TemporaryDirectory(prefix="spaceterm-package-certificate-")
        cls.addClassCleanup(temporary.cleanup)
        cls.fingerprint, cls.credentials = certificate(Path(temporary.name))

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
        config = self.root / "packaging/macos/spaceterm/Packager.toml"
        config.write_text(
            re.sub(
                r'signing-identity = ".*"',
                f'signing-identity = "{self.fingerprint}"',
                config.read_text(),
            )
        )
        configure_trust(self.root)
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
        cargo = shutil.which("cargo")
        self.assertIsNotNone(cargo)
        # Cargo is the controlled external boundary. Everything after its artifact
        # report uses the real icon compiler, packager, signing tools, and verifier.
        (self.bin / "cargo").write_text(f"""#!{sys.executable}
import json
import os
from pathlib import Path
import subprocess
import sys
if sys.argv[1] != "build" or "--locked" not in sys.argv:
    os.execv({cargo!r}, [{cargo!r}, *sys.argv[1:]])
Path(os.environ["BUILD_RECORD"]).write_text(json.dumps({{
    "arguments": sys.argv[2:],
    "packaged": os.environ.get("SPACETERM_PACKAGED"),
    "release_tag": os.environ.get("SPACETERM_RELEASE_TAG"),
    "private_inputs": [name for name in (
        "MACOS_SIGNING_CERTIFICATE_P12", "MACOS_SIGNING_CERTIFICATE_PASSWORD",
        "APPLE_CERTIFICATE", "APPLE_CERTIFICATE_PASSWORD", "UPDATE_SIGNING_KEY",
    ) if name in os.environ],
    "signing_available": os.environ["BUILD_SIGNING_FINGERPRINT"] in subprocess.run(
        ["security", "find-identity", "-p", "codesigning"],
        capture_output=True, text=True, check=True,
    ).stdout,
}}))
binary = sys.argv[sys.argv.index("--bin") + 1]
assert binary == "spaceterm-desktop", "the desktop builds no sibling server executable"
print(json.dumps({{"reason": "compiler-artifact", "target": {{"name": binary}},
                  "executable": os.environ["BUILD_EXECUTABLE"]}}))
sys.exit(int(os.environ.get("BUILD_EXIT", "0")))
""")
        (self.bin / "cargo").chmod(0o755)
        self.record = Path(self.directory.name) / "build.json"
        self.env = {
            **os.environ,
            **self.credentials,
            "PATH": f"{self.bin}{os.pathsep}{os.environ['PATH']}",
            "PYTHONPATH": str(self.root / "mise-tasks/lib"),
            "BUILD_RECORD": str(self.record),
            "BUILD_SIGNING_FINGERPRINT": self.fingerprint,
        }
        self.artifact = self.root / "target/aarch64-apple-darwin/release/spaceterm"
        self.env["BUILD_EXECUTABLE"] = str(self.artifact)
        self.compiled_identities = {}
        self.stale = self.root / "target/release/spaceterm"
        self.compile_executable(self.stale, "SpaceTerm Development")

    def run_command(self, *args, **kwargs):
        return subprocess.run(
            args, cwd=self.root, capture_output=True, text=True, check=True, **kwargs
        )

    def compile_executable(self, output, identity, candidate_digest=""):
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
                "server_candidate_sha256": candidate_digest,
            }
        )
        output.parent.mkdir(parents=True, exist_ok=True)
        self.run_command(
            "xcrun",
            "clang",
            "-arch",
            "arm64",
            "-mmacosx-version-min=26.0",
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
        puts("{identity} 0.0.1");
        return 0;
    }}
    return 1;
}}
""",
        )

    def task(self, path, *arguments):
        return subprocess.run(
            [sys.executable, str(self.root / "mise-tasks" / path), *arguments],
            cwd=self.root,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=300,
        )

    def package(self, release_tag=None, *, candidate_digest=None):
        # Client and server releases are independent. Release fixtures reuse an older server.
        server_tag = "v0.0.0" if release_tag else None
        archive, _ = prepare(
            self.root,
            self.env,
            server_tag,
            self.fingerprint,
            self.credentials,
            include_catalog=True,
        )
        digest = (
            candidate_digest
            if candidate_digest is not None
            else hashlib.sha256(archive.read_bytes()).hexdigest()
        )
        self.compile_executable(self.artifact, self.compiled_identities[self.artifact], digest)
        if release_tag:
            return self.task("release/package/macos.py", release_tag)
        return self.task("preflight/package/macos.py")

    def executable_uuid(self, executable):
        # Signing can change Mach-O layout even after removing the signature.
        # The linker's UUID identifies the compiled artifact across signing.
        result = self.run_command("xcrun", "dwarfdump", "--uuid", str(executable))
        return result.stdout.split()[1]

    def test_packaging_installs_reported_artifact_and_ignores_a_stale_binary(self):
        self.compile_executable(self.artifact, "SpaceTerm Preflight")
        self.env["CARGO_BUILD_TARGET"] = "aarch64-apple-darwin"
        result = self.package()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        bundled = self.root / "dist/SpaceTerm Preflight.app/Contents/MacOS/SpaceTerm Preflight"
        self.assertEqual(self.executable_uuid(bundled), self.executable_uuid(self.artifact))
        self.assertNotEqual(self.executable_uuid(bundled), self.executable_uuid(self.stale))
        server = bundled.with_name("spaceterm")
        self.assertFalse(server.exists())
        artifact = bundled.parent.parent / "Resources/server-artifact"
        self.assertEqual(
            (artifact / "artifact.tar.gz").read_bytes(),
            Path(self.env["SPACETERM_SERVER_ARCHIVE"]).read_bytes(),
        )
        self.assertEqual(
            (artifact / "artifact.sig").read_bytes(),
            Path(self.env["SPACETERM_SERVER_SIGNATURE"]).read_bytes(),
        )
        catalog = artifact.with_name("server-artifacts") / "aarch64-apple-darwin"
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
        record = json.loads(self.record.read_text())
        self.assertIn("--message-format=json-render-diagnostics", record["arguments"])
        self.assertEqual(record["packaged"], "1")

    def test_packaging_rejects_an_artifact_different_from_the_compiled_pin(self):
        self.compile_executable(self.artifact, "SpaceTerm Preflight")
        result = self.package(candidate_digest="0" * 64)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("compiled candidate pin", result.stderr)
        self.assertFalse((self.root / "dist/SpaceTerm Preflight.app").exists())

    def test_packaging_rejects_development_identity_in_a_preflight_bundle(self):
        self.compile_executable(self.artifact, "SpaceTerm Development")
        result = self.package()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("application identity", result.stderr)
        self.assertFalse((self.root / "dist/SpaceTerm Preflight.app").exists())

    def test_failed_build_does_not_package_a_stale_executable(self):
        self.compile_executable(self.artifact, "SpaceTerm Preflight")
        self.env["BUILD_EXIT"] = "1"
        result = self.package()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "dist/SpaceTerm Preflight.app").exists())

    def test_release_packaging_installs_the_reported_release_identity(self):
        self.compile_executable(self.artifact, "SpaceTerm")
        self.env["CARGO_BUILD_TARGET"] = "aarch64-apple-darwin"
        result = self.package("v0.0.1")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        bundled = self.root / "dist/SpaceTerm.app/Contents/MacOS/SpaceTerm"
        self.assertEqual(self.executable_uuid(bundled), self.executable_uuid(self.artifact))
        self.assertNotEqual(self.executable_uuid(bundled), self.executable_uuid(self.stale))
        self.assertNotIn("spaceterm", {path.name for path in bundled.parent.iterdir()})
        self.assertEqual(json.loads(self.record.read_text())["release_tag"], "v0.0.1")
        self.assertEqual(json.loads(self.record.read_text())["private_inputs"], [])
        self.assertFalse(json.loads(self.record.read_text())["signing_available"])
        catalog = bundled.parent.parent / "Resources/server-artifacts"
        self.assertEqual(
            set(path.name for path in catalog.iterdir()),
            {"aarch64-apple-darwin", "x86_64-unknown-linux-gnu"},
        )
        for value in self.credentials.values():
            self.assertNotIn(value, result.stdout + result.stderr)
        requirement = self.run_command(
            "codesign", "--display", "--requirements", "-", str(bundled)
        ).stdout
        self.assertEqual(
            requirement.strip(),
            f'designated => identifier "io.github.sadiksaifi.spaceterm" and certificate leaf = H"{self.fingerprint.lower()}"',
        )

    def test_verification_rejects_a_tampered_server_catalog(self):
        self.compile_executable(self.artifact, "SpaceTerm Preflight")
        result = self.package()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        catalog = self.root / "dist/SpaceTerm Preflight.app/Contents/Resources/server-artifacts"
        (catalog / "aarch64-apple-darwin/artifact.sha256").write_text("0" * 64 + "\n")
        verified = self.task("preflight/verify/macos.py")
        self.assertNotEqual(verified.returncode, 0)
        self.assertIn("compiled candidate pin", verified.stderr)

    def test_release_requires_signing_secrets_before_building(self):
        self.compile_executable(self.artifact, "SpaceTerm")
        self.env.pop("MACOS_SIGNING_CERTIFICATE_P12", None)
        self.env.pop("MACOS_SIGNING_CERTIFICATE_PASSWORD", None)
        result = self.package("v0.0.1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("release signing", result.stderr)
        self.assertFalse(self.record.exists())
        self.assertFalse((self.root / "dist/SpaceTerm.app").exists())

    def test_release_packaging_rejects_a_dirty_checkout_before_building(self):
        self.compile_executable(self.artifact, "SpaceTerm")
        with (self.root / "Cargo.toml").open("a") as manifest:
            manifest.write("\n")
        result = self.package("v0.0.1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("a release package requires a clean checkout", result.stderr)
        self.assertFalse(self.record.exists())
        self.assertFalse((self.root / "dist/SpaceTerm.app").exists())

    def test_release_packaging_rejects_a_preflight_identity_with_the_same_name_prefix(self):
        self.compile_executable(self.artifact, "SpaceTerm Preflight")
        result = self.package("v0.0.1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("application identity must be SpaceTerm", result.stderr)

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
        dmg = Path(self.directory.name) / "wrong-identity.dmg"
        self.run_command(
            "hdiutil", "create", "-srcfolder", str(disk_root), "-format", "UDZO", str(dmg)
        )
        result = self.task("preflight/verify/macos.py", "--app", str(app), "--dmg", str(dmg))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("dmg executable application identity", result.stderr)


class CurrentServerPackageTests(unittest.TestCase):
    def test_current_server_passes_signed_package_verification(self):
        # Build the real server, including its PTY/shell check, rather than the C fixture
        # used to isolate desktop packaging. Only the artifact destination is disposable.
        with tempfile.TemporaryDirectory(prefix="spaceterm-current-server-package-") as temporary:
            with patch.object(server_package, "DIST", Path(temporary)):
                archive, signature = server_package.package("spaceterm-preflight")
            destination = Path(temporary) / "desktop"
            server_package.stage_desktop_artifact(
                archive, signature, destination, identity="spaceterm-preflight"
            )
            self.assertEqual((destination / "artifact.tar.gz").read_bytes(), archive.read_bytes())
            self.assertEqual((destination / "artifact.sig").read_bytes(), signature.read_bytes())
            verified = Path(temporary) / "verified"
            verified.mkdir()
            tree, _, _ = server_package.verified_copy(
                archive, signature, verified, None, identity="spaceterm-preflight"
            )
            launcher = Path(temporary) / "bin/spaceterm"
            launcher.parent.mkdir()
            launcher.symlink_to(tree / "bin/spaceterm")
            result = subprocess.run(
                [launcher, "server", "provision", "check"],
                env={},
                cwd=tree,
                capture_output=True,
                timeout=15,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, b"Server checks passed.\n")
            self.assertEqual(result.stderr, b"")


if __name__ == "__main__":
    unittest.main()
