"""Run archive entry points in a real unprivileged, source-free Linux filesystem."""

import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import TaskError, package_server
from spaceterm_tasks import packaged_headless_linux as acceptance

from tests.test_package_server import metadata


class PackagedHeadlessAdmissionTests(unittest.TestCase):
    def test_unknown_cli_arguments_do_not_disclose_paths_or_enter_archive_execution(self):
        diagnostics = io.StringIO()
        with (
            contextlib.redirect_stderr(diagnostics),
            patch.object(acceptance, "run") as execute,
            self.assertRaisesRegex(TaskError, "invalid-headless-arguments"),
        ):
            acceptance.cli(
                [
                    "--archive",
                    "/unused/archive.tar.gz",
                    "--source-commit",
                    "1" * 40,
                    "--unknown=/private/account/credential",
                ]
            )
        execute.assert_not_called()
        self.assertNotIn("/private/account/credential", diagnostics.getvalue())
        self.assertNotIn("/unused/archive.tar.gz", diagnostics.getvalue())

    def test_root_and_unsupported_hosts_are_refused_before_reading_the_archive(self):
        for platform, uid in (("darwin", 501), ("linux", 0)):
            with (
                self.subTest(platform=platform, uid=uid),
                patch.object(acceptance.sys, "platform", platform),
                patch.object(acceptance.os, "getuid", return_value=uid),
                patch.object(acceptance.package_server, "verify_archive") as verify,
            ):
                with self.assertRaises(TaskError):
                    acceptance.run(Path("/unused"), "1" * 40)
                verify.assert_not_called()

    def test_invalid_source_revision_is_refused_before_reading_the_archive(self):
        for revision in ("HEAD", "1e6efff", "0" * 40, "1" * 40 + "\n"):
            with (
                self.subTest(revision=revision),
                patch.object(acceptance.sys, "platform", "linux"),
                patch.object(acceptance.os, "getuid", return_value=1000),
                patch.object(acceptance.os, "geteuid", return_value=1000),
                patch.object(
                    acceptance.os,
                    "uname",
                    return_value=os.uname_result(("Linux", "unused", "6.12.0", "unused", "x86_64")),
                ),
                patch.object(acceptance.package_server, "verify_archive") as verify,
            ):
                with self.assertRaises(TaskError):
                    acceptance.run(Path("/unused"), revision)
                verify.assert_not_called()

    def test_newer_debian_host_cannot_be_reported_as_jammy_baseline_evidence(self):
        facts = {"glibc": [2, 41], "kernel": [6, 12, 0], "systemd": 257}
        with (
            patch.object(acceptance, "host_facts", return_value=facts),
            patch.object(acceptance.sys, "platform", "linux"),
            patch.object(acceptance.os, "getuid", return_value=1000),
            patch.object(acceptance.os, "geteuid", return_value=1000),
            patch.object(
                acceptance.os,
                "uname",
                return_value=os.uname_result(("Linux", "unused", "6.12.0", "unused", "x86_64")),
            ),
            patch.object(acceptance.package_server, "verify_archive") as verify,
        ):
            with self.assertRaisesRegex(TaskError, "baseline-host-required"):
                acceptance.run(Path("/unused"), "1" * 40, require_baseline_host=True)
            verify.assert_not_called()


if sys.platform == "linux":

    class PackagedHeadlessExecutionTests(unittest.TestCase):
        def setUp(self):
            temporary = tempfile.TemporaryDirectory(prefix="spaceterm-headless-test-")
            self.addCleanup(temporary.cleanup)
            self.directory = Path(temporary.name)
            self.root = self.directory / "spaceterm-server"
            self.binary = self.root / "bin/spaceterm-server"
            self.binary.parent.mkdir(parents=True)
            self.archive = self.directory / "server.tar.gz"
            self.document = metadata()

        def archive_probe(
            self,
            *,
            document=None,
            check_environment=True,
            mutate=False,
            version=None,
            help_text="usage: spaceterm-server serve | --help",
        ):
            document = self.document if document is None else document
            encoded = json.dumps(json.dumps(document))
            # No shell or Python enters the sandbox. The executable checks the
            # filesystem, inherited environment and account credentials itself.
            checks = (
                f"if (getuid() != {os.getuid()} || geteuid() != {os.geteuid()} "
                "|| getuid() == 0) return 10;\n"
                'if (getenv("DISPLAY") || getenv("WAYLAND_DISPLAY") '
                '|| getenv("LD_PRELOAD") || getenv("PYTHONPATH")) return 11;\n'
                'if (access("/bin/sh", F_OK) == 0 || access("/usr/bin", F_OK) == 0 '
                '|| access("/usr/share/fonts", F_OK) == 0 '
                '|| access("/opt/spaceterm-server/bin/spaceterm-cli", F_OK) == 0 '
                f"|| access({json.dumps(str(Path(__file__).resolve()))}, F_OK) == 0) return 12;\n"
                'char cwd[256]; if (!getcwd(cwd, sizeof(cwd)) || strcmp(cwd, "/")) '
                "return 13;\n"
            )
            source = (
                "#include <stdio.h>\n#include <stdlib.h>\n#include <string.h>\n"
                "#include <unistd.h>\nint main(int argc, char **argv) {\n"
                + (checks if check_environment else "")
                + ('fclose(fopen("/home/account/side-effect", "w"));\n' if mutate else "")
                + 'if (argc == 2 && !strcmp(argv[1], "--distribution-metadata")) {\n'
                + f"puts({encoded}); return 0; }}\n"
                + 'if (argc == 2 && !strcmp(argv[1], "--help")) { '
                + f"puts({json.dumps(help_text)}); return 0; }}\n"
                + 'if (argc == 2 && !strcmp(argv[1], "--version")) { '
                + f"puts({json.dumps('spaceterm-server ' + (version or document['version']))}); "
                + "return 0; }\n"
                + 'fputs("private native error /account/secret", stderr); return 2; }\n'
            )
            subprocess.run(
                ["cc", "-x", "c", "-", "-o", self.binary],
                input=source.encode(),
                check=True,
                capture_output=True,
                timeout=30,
            )
            self.binary.chmod(0o700)
            package_server.write_manifest(self.root, self.document)
            package_server.write_archive(self.root, self.archive)

        def test_archive_runs_without_display_gui_libraries_client_or_source_checkout(self):
            self.archive_probe()
            with patch.dict(
                os.environ,
                {"DISPLAY": "unavailable", "WAYLAND_DISPLAY": "unavailable"},
            ):
                result = acceptance.run(self.archive, "2" * 40)
            self.assertEqual(result["classification"], "packaged-queries-passed")
            self.assertEqual(result["queries"], 7)
            self.assertEqual(result["source_commit"], "2" * 40)
            self.assertEqual(result["metadata"], self.document)
            self.assertEqual(
                result["archive_sha256"], acceptance.server_artifacts.digest(self.archive)
            )
            self.assertNotEqual(result["source_commit"], result["metadata"]["build_commit"])
            self.assertFalse(result["service_lifecycle_tested"])
            self.assertFalse(result["runtime_readiness_tested"])

        def test_packaged_queries_must_match_version_and_usage(self):
            for changes in ({"version": "9.9.9"}, {"help_text": "unrelated command usage"}):
                with self.subTest(changes=changes):
                    for path in (self.archive, self.root / "installation.json"):
                        path.unlink(missing_ok=True)
                    self.archive_probe(**changes)
                    with self.assertRaisesRegex(TaskError, "packaged-query-mismatch"):
                        acceptance.run(self.archive, "2" * 40)

        def test_packaged_metadata_mismatch_fails_without_relaying_native_output(self):
            changed = {**self.document, "build_commit": "3" * 40}
            self.archive_probe(document=changed)
            with self.assertRaisesRegex(TaskError, "packaged-metadata-mismatch") as failure:
                acceptance.run(self.archive, "2" * 40)
            self.assertNotIn(str(self.directory), str(failure.exception))
            self.assertNotIn("private native error", str(failure.exception))

        def test_queries_that_mutate_private_state_are_not_accepted(self):
            self.archive_probe(mutate=True)
            with self.assertRaisesRegex(TaskError, "packaged-query-mutated-state"):
                acceptance.run(self.archive, "2" * 40)

        def test_corrupt_archive_is_refused_before_running_any_entry_point(self):
            self.archive_probe()
            self.binary.write_bytes(b"changed")
            self.binary.chmod(0o700)
            self.archive.unlink()
            package_server.write_archive(self.root, self.archive)
            with (
                patch.object(
                    acceptance.native, "run_tool", wraps=acceptance.native.run_tool
                ) as run,
                self.assertRaises(TaskError),
            ):
                acceptance.run(self.archive, "2" * 40)
            self.assertFalse(any(call.args[0][0] == "bwrap" for call in run.call_args_list))

        def test_archive_with_a_client_is_not_accepted_as_server_only_evidence(self):
            self.archive_probe()
            client = self.root / "bin/spaceterm-cli"
            client.write_bytes(self.binary.read_bytes())
            client.chmod(0o700)
            (self.root / "installation.json").unlink()
            package_server.write_manifest(self.root, self.document)
            self.archive.unlink()
            package_server.write_archive(self.root, self.archive)
            with self.assertRaisesRegex(TaskError, "packaged-client-present"):
                acceptance.run(self.archive, "2" * 40)

        def test_script_entry_point_cannot_borrow_a_host_interpreter(self):
            self.binary.write_text("#!/bin/sh\nprintf 'borrowed interpreter'\n")
            self.binary.chmod(0o700)
            package_server.write_manifest(self.root, self.document)
            package_server.write_archive(self.root, self.archive)
            with self.assertRaises(TaskError):
                acceptance.run(self.archive, "2" * 40)
