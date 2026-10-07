#!/usr/bin/env python
# MISE description="Exercise Linux packaging with controlled Cargo artifacts and real archives"
"""Exercise Linux packaging with controlled Cargo artifacts and real archives."""

import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import ROOT

FIXTURE = r"""
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifdef NEWER_GLIBC
extern int pidfd_getpid(int);
#endif
int main(int argc, char **argv) {
    if (getenv("SPACETERM_SSH_ASKPASS_MODE")) return 2;
    if (argc == 2 && strcmp(argv[1], "--version") == 0) {
        puts(IDENTITY);
        return 0;
    }
#ifdef NEWER_GLIBC
    return pidfd_getpid(-1);
#endif
    return 1;
}
"""


def host_glibc():
    try:
        version = os.confstr("CS_GNU_LIBC_VERSION") or ""
    except ValueError:
        return (0, 0)
    major, minor = (version.removeprefix("glibc ").split(".") + ["0", "0"])[:2]
    return int(major), int(minor)


class PackageTests(unittest.TestCase):
    def setUp(self):
        for tool in ("cc", "tic", "infocmp", "objdump", "desktop-file-validate"):
            self.assertIsNotNone(shutil.which(tool), f"{tool} is required; run mise doctor project")
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
        # Cargo is the controlled external boundary. Everything after its artifact report uses
        # the real terminfo compiler, archive writer, desktop entry validator, and verifier.
        (self.bin / "cargo").write_text(f"""#!{sys.executable}
import json
import os
from pathlib import Path
import sys
if sys.argv[1] != "build" or "--locked" not in sys.argv:
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
        self.artifact = self.root / "target/release/spaceterm"
        self.env = {
            **os.environ,
            "PATH": f"{self.bin}{os.pathsep}{os.environ['PATH']}",
            "PYTHONPATH": str(self.root / "mise-tasks/lib"),
            "BUILD_RECORD": str(self.record),
            "BUILD_EXECUTABLE": str(self.artifact),
        }

    def compile_executable(self, identity, *definitions):
        self.artifact.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            [
                "cc",
                "-x",
                "c",
                f"-DIDENTITY={json.dumps(identity)}",
                *(f"-D{definition}" for definition in definitions),
                "-",
                "-o",
                str(self.artifact),
            ],
            input=FIXTURE,
            text=True,
            check=True,
            capture_output=True,
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

    def package(self, release_tag=None):
        if release_tag:
            return self.task("release/package/linux.py", release_tag)
        return self.task("preflight/package/linux.py")

    def members(self, archive):
        with tarfile.open(archive, "r:gz") as source:
            return {member.name: member for member in source.getmembers()}

    def test_preflight_archive_holds_the_reported_artifact_in_the_installed_layout(self):
        self.compile_executable("SpaceTerm Preflight 0.0.0")
        result = self.package()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        archive = self.root / "dist/spaceterm-preflight-linux-x86_64.tar.gz"
        members = self.members(archive)
        self.assertTrue(all(name.split("/")[0] == "spaceterm-preflight" for name in members))
        identifier = "io.github.sadiksaifi.spaceterm-preflight"
        for name in (
            "spaceterm-preflight/bin/spaceterm",
            "spaceterm-preflight/share/spaceterm/THIRD-PARTY-NOTICES.txt",
            "spaceterm-preflight/share/spaceterm/terminfo/x/xterm-spaceterm",
            f"spaceterm-preflight/share/applications/{identifier}.desktop",
            f"spaceterm-preflight/share/icons/hicolor/scalable/apps/{identifier}.svg",
        ):
            self.assertIn(name, members)
        executable = members["spaceterm-preflight/bin/spaceterm"]
        self.assertEqual((executable.mode, executable.uid, executable.uname), (0o755, 0, ""))
        with tarfile.open(archive, "r:gz") as source:
            packaged = source.extractfile(executable)
            assert packaged is not None
            self.assertEqual(packaged.read(), self.artifact.read_bytes())
        record = json.loads(self.record.read_text())
        self.assertIn("--no-default-features", record["arguments"])
        self.assertEqual((record["packaged"], record["release_tag"]), ("1", None))

    def test_packaging_rejects_development_identity_in_a_preflight_archive(self):
        self.compile_executable("SpaceTerm Development 0.0.0")
        result = self.package()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("application identity", result.stderr)
        self.assertFalse((self.root / "dist/spaceterm-preflight-linux-x86_64.tar.gz").exists())

    def test_failed_build_does_not_package_a_stale_executable(self):
        self.compile_executable("SpaceTerm Preflight 0.0.0")
        self.env["BUILD_EXIT"] = "1"
        result = self.package()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "dist/spaceterm-preflight-linux-x86_64.tar.gz").exists())

    def test_release_archive_holds_the_tagged_release_identity(self):
        self.compile_executable("SpaceTerm 0.0.1")
        result = self.package("v0.0.1")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        members = self.members(self.root / "dist/spaceterm-linux-x86_64.tar.gz")
        self.assertIn("spaceterm/bin/spaceterm", members)
        self.assertEqual(json.loads(self.record.read_text())["release_tag"], "v0.0.1")

    def test_release_archive_rejects_an_executable_of_another_version(self):
        self.compile_executable("SpaceTerm 0.0.2")
        result = self.package("v0.0.1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("executable version must be 0.0.1", result.stderr)

    def test_release_packaging_rejects_a_dirty_checkout_before_building(self):
        self.compile_executable("SpaceTerm 0.0.1")
        with (self.root / "Cargo.toml").open("a") as manifest:
            manifest.write("\n")
        result = self.package("v0.0.1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("a release package requires a clean checkout", result.stderr)
        self.assertFalse(self.record.exists())

    @unittest.skipUnless(host_glibc() >= (2, 39), "the host glibc predates pidfd_getpid")
    def test_release_packaging_rejects_an_executable_that_needs_a_newer_glibc(self):
        self.compile_executable("SpaceTerm 0.0.1", "NEWER_GLIBC")
        result = self.package("v0.0.1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("newer than 2.35", result.stderr)
        self.assertFalse((self.root / "dist/spaceterm-linux-x86_64.tar.gz").exists())

    @unittest.skipUnless(host_glibc() >= (2, 39), "the host glibc predates pidfd_getpid")
    def test_preflight_runs_on_its_build_host_whatever_its_glibc(self):
        self.compile_executable("SpaceTerm Preflight 0.0.0", "NEWER_GLIBC")
        result = self.package()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
