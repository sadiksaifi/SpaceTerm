#!/usr/bin/env python
# MISE description="Exercise Linux packaging with controlled Cargo artifacts and real archives"
"""Exercise Linux packaging with controlled Cargo artifacts and real archives."""

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

from spaceterm_tasks import ROOT, server_package
from tests.server_artifact_fixture import configure_trust, prepare

FIXTURE = r"""
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifdef NEWER_GLIBC
extern int pidfd_getpid(int);
#endif
#ifdef WEAK_NEWER_GLIBC
extern int pidfd_getpid(int) __attribute__((weak));
#endif
int main(int argc, char **argv) {
    if (getenv("SPACETERM_SSH_ASKPASS_MODE")) return 2;
    if (argc == 2 && strcmp(argv[1], "--build-info") == 0) {
        puts(BUILD_INFO);
        return 0;
    }
    if (argc == 2 && strcmp(argv[1], "--version") == 0) {
        puts(IDENTITY);
        return 0;
    }
#ifdef NEWER_GLIBC
    return pidfd_getpid(-1);
#endif
#ifdef WEAK_NEWER_GLIBC
    if (pidfd_getpid) return pidfd_getpid(-1);
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
        major = self.root / server_package.PROTOCOL_MAJOR_SOURCE
        major.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / server_package.PROTOCOL_MAJOR_SOURCE, major)
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
        # Cargo is the controlled external boundary. Everything after its artifact report uses
        # the real terminfo compiler, archive writer, desktop entry validator, and verifier.
        (self.bin / "cargo").write_text(f"""#!{sys.executable}
import json
import os
from pathlib import Path
import sys
if sys.argv[1] != "zigbuild" or "--locked" not in sys.argv:
    os.execv({cargo!r}, [{cargo!r}, *sys.argv[1:]])
Path(os.environ["BUILD_RECORD"]).write_text(json.dumps({{
    "arguments": sys.argv[2:],
    "packaged": os.environ.get("SPACETERM_PACKAGED"),
    "release_tag": os.environ.get("SPACETERM_RELEASE_TAG"),
}}))
binary = sys.argv[sys.argv.index("--bin") + 1]
assert binary == "spaceterm-desktop", "the desktop builds no sibling server executable"
print(json.dumps({{"reason": "compiler-artifact", "target": {{"name": binary}},
                  "executable": os.environ["BUILD_EXECUTABLE"]}}))
sys.exit(int(os.environ.get("BUILD_EXIT", "0")))
""")
        (self.bin / "cargo").chmod(0o755)
        self.record = Path(self.directory.name) / "build.json"
        self.artifact = self.root / "target/release/spaceterm-desktop"
        self.env = {
            **os.environ,
            "PATH": f"{self.bin}{os.pathsep}{os.environ['PATH']}",
            "PYTHONPATH": str(self.root / "mise-tasks/lib"),
            "BUILD_RECORD": str(self.record),
            "BUILD_EXECUTABLE": str(self.artifact),
        }

    def compile_executable(self, identity, *definitions, candidate_digest=""):
        self.compiled_identity = identity
        self.compiled_definitions = definitions
        name, _, _ = identity.rpartition(" ")
        build = json.dumps(
            {
                "schema": 1,
                "identity": {
                    "SpaceTerm": "spaceterm",
                    "SpaceTerm Preflight": "spaceterm-preflight",
                    "SpaceTerm Development": "spaceterm-development",
                }[name],
                "target": "x86_64-unknown-linux-gnu",
                "server_candidate_sha256": candidate_digest,
            }
        )
        self.artifact.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            [
                "cc",
                "-x",
                "c",
                f"-DIDENTITY={json.dumps(identity)}",
                f"-DBUILD_INFO={json.dumps(build)}",
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

    def package(self, release_tag=None, *, candidate_digest=None):
        # Client and server releases are independent. Release fixtures reuse an older server.
        archive, _ = prepare(
            self.root, self.env, "v0.0.0" if release_tag else None, include_catalog=True
        )
        digest = (
            candidate_digest
            if candidate_digest is not None
            else hashlib.sha256(archive.read_bytes()).hexdigest()
        )
        self.compile_executable(
            self.compiled_identity, *self.compiled_definitions, candidate_digest=digest
        )
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
            "spaceterm-preflight/bin/spaceterm-desktop",
            "spaceterm-preflight/share/spaceterm/server-artifact/artifact.tar.gz",
            "spaceterm-preflight/share/spaceterm/server-artifact/artifact.sig",
            "spaceterm-preflight/share/spaceterm/server-artifacts/x86_64-unknown-linux-gnu/artifact.tar.gz",
            "spaceterm-preflight/share/spaceterm/server-artifacts/x86_64-unknown-linux-gnu/artifact.sig",
            "spaceterm-preflight/share/spaceterm/server-artifacts/x86_64-unknown-linux-gnu/artifact.sha256",
            "spaceterm-preflight/share/spaceterm/THIRD-PARTY-NOTICES.txt",
            "spaceterm-preflight/share/spaceterm/shell-integration/elvish/rc.elv",
            "spaceterm-preflight/share/spaceterm/terminfo/x/xterm-spaceterm",
            f"spaceterm-preflight/share/applications/{identifier}.desktop",
            f"spaceterm-preflight/share/icons/hicolor/scalable/apps/{identifier}.svg",
        ):
            self.assertIn(name, members)
        self.assertNotIn("spaceterm-preflight/bin/spaceterm", members)
        executable = members["spaceterm-preflight/bin/spaceterm-desktop"]
        self.assertEqual((executable.mode, executable.uid, executable.uname), (0o755, 0, ""))
        with tarfile.open(archive, "r:gz") as source:
            rc = source.extractfile(
                "spaceterm-preflight/share/spaceterm/shell-integration/elvish/rc.elv"
            )
            assert rc is not None
            self.assertEqual(
                rc.read(), (ROOT / "assets/shell-integration/elvish/rc.elv").read_bytes()
            )
            packaged = source.extractfile(executable)
            assert packaged is not None
            self.assertEqual(packaged.read(), self.artifact.read_bytes())
        record = json.loads(self.record.read_text())
        self.assertIn("--no-default-features", record["arguments"])
        self.assertEqual((record["packaged"], record["release_tag"]), ("1", None))

    def test_packaging_rejects_an_artifact_different_from_the_compiled_pin(self):
        self.compile_executable("SpaceTerm Preflight 0.0.0")
        result = self.package(candidate_digest="0" * 64)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("compiled candidate pin", result.stderr)
        self.assertFalse((self.root / "dist/spaceterm-preflight-linux-x86_64.tar.gz").exists())

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
        self.assertIn("spaceterm/bin/spaceterm-desktop", members)
        self.assertNotIn("spaceterm/bin/spaceterm", members)
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
    def test_preflight_rejects_even_a_weak_import_from_a_newer_glibc(self):
        self.compile_executable("SpaceTerm Preflight 0.0.0", "WEAK_NEWER_GLIBC")
        result = self.package()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("newer than 2.35", result.stderr)
        self.assertFalse((self.root / "dist/spaceterm-preflight-linux-x86_64.tar.gz").exists())


if __name__ == "__main__":
    unittest.main()
