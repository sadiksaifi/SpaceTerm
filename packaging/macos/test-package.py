#!/usr/bin/env python3
"""Exercise packaging with controlled Cargo artifacts and real macOS bundles."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="spaceterm-package-test-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name) / "repo with spaces"
        self.root.mkdir()
        for directory in ("packaging", "assets", "scripts"):
            shutil.copytree(ROOT / directory, self.root / directory)
        shutil.copyfile(ROOT / "Cargo.toml", self.root / "Cargo.toml")
        shutil.copyfile(ROOT / ".gitignore", self.root / ".gitignore")
        self.run_command("git", "init", "-q")
        self.run_command("git", "config", "user.name", "Package test")
        self.run_command("git", "config", "user.email", "test@example.invalid")
        self.run_command("git", "add", ".")
        self.run_command("git", "commit", "-qm", "test(packaging): create fixture")
        self.run_command("git", "tag", "-a", "v0.0.1", "-m", "Fixture release")

        self.bin = Path(self.directory.name) / "bin"
        self.bin.mkdir()
        (self.bin / "python3").symlink_to(sys.executable)
        cargo = shutil.which("cargo")
        self.assertIsNotNone(cargo)
        # Cargo is the controlled external boundary. Everything after its artifact
        # report uses the real icon compiler, packager, signing tools, and verifier.
        (self.bin / "cargo").write_text(f"""#!{sys.executable}
import json
import os
from pathlib import Path
import sys
if sys.argv[1] != "build":
    os.execv({cargo!r}, [{cargo!r}, *sys.argv[1:]])
Path(os.environ["BUILD_RECORD"]).write_text(json.dumps({{
    "arguments": sys.argv[2:],
    "packaged": os.environ.get("SPACETERM_PACKAGED"),
    "release_tag": os.environ.get("SPACETERM_RELEASE_TAG"),
}}))
print(json.dumps({{"reason": "compiler-artifact", "target": {{"name": "spaceterm"}},
                  "executable": os.environ["BUILD_EXECUTABLE"]}}))
sys.exit(int(os.environ.get("BUILD_EXIT", "0")))
""")
        (self.bin / "cargo").chmod(0o755)
        self.record = Path(self.directory.name) / "build.json"
        self.env = {**os.environ, "PATH": f"{self.bin}:{os.environ['PATH']}",
                    "BUILD_RECORD": str(self.record)}
        self.artifact = self.root / "target/aarch64-apple-darwin/release/spaceterm"
        self.env["BUILD_EXECUTABLE"] = str(self.artifact)
        self.stale = self.root / "target/release/spaceterm"
        self.compile_executable(self.stale, "SpaceTerm Development")

    def run_command(self, *args, **kwargs):
        return subprocess.run(args, cwd=self.root, capture_output=True, text=True,
                              check=True, **kwargs)

    def compile_executable(self, output, identity):
        output.parent.mkdir(parents=True, exist_ok=True)
        self.run_command("xcrun", "clang", "-arch", "arm64", "-mmacosx-version-min=26.0",
                         "-x", "c", "-", "-o", str(output), input=f"""
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
int main(int argc, char **argv) {{
    if (getenv("SPACETERM_SSH_ASKPASS_MODE")) return 2;
    if (argc == 2 && strcmp(argv[1], "--version") == 0) {{
        puts("{identity} 0.0.1");
        return 0;
    }}
    return 1;
}}
""")

    def package(self, *arguments):
        return subprocess.run(["bash", "packaging/macos/package.sh", *arguments],
                              cwd=self.root, env=self.env, capture_output=True,
                              text=True, timeout=180)

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
        record = json.loads(self.record.read_text())
        self.assertIn("--message-format=json-render-diagnostics", record["arguments"])
        self.assertEqual(record["packaged"], "1")

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
        result = self.package("--release", "v0.0.1")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        bundled = self.root / "dist/SpaceTerm.app/Contents/MacOS/SpaceTerm"
        self.assertEqual(self.executable_uuid(bundled), self.executable_uuid(self.artifact))
        self.assertNotEqual(self.executable_uuid(bundled), self.executable_uuid(self.stale))
        self.assertEqual(json.loads(self.record.read_text())["release_tag"], "v0.0.1")

    def test_release_packaging_rejects_a_preflight_identity_with_the_same_name_prefix(self):
        self.compile_executable(self.artifact, "SpaceTerm Preflight")
        result = self.package("--release", "v0.0.1")
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
        self.compile_executable(disk_app / "Contents/MacOS/SpaceTerm Preflight",
                                "SpaceTerm Development")
        self.run_command("codesign", "--force", "--sign", "-", "--timestamp=none", str(disk_app))
        (disk_root / "Applications").symlink_to("/Applications")
        dmg = Path(self.directory.name) / "wrong-identity.dmg"
        self.run_command("hdiutil", "create", "-srcfolder", str(disk_root),
                         "-format", "UDZO", str(dmg))
        result = subprocess.run(["bash", "packaging/macos/verify-package.sh",
                                 "--app", str(app), "--dmg", str(dmg)],
                                cwd=self.root, env=self.env, capture_output=True,
                                text=True, timeout=60)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("dmg executable application identity", result.stderr)


if __name__ == "__main__":
    unittest.main()
