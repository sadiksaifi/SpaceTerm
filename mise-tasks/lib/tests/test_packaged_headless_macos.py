"""Exercise signed archives and native query isolation without a service or Keychain."""

import contextlib
import io
import json
import os
import plistlib
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import TaskError, package_server, package_server_macos, server_artifacts
from spaceterm_tasks import packaged_headless_macos as acceptance

from tests.test_package_server import metadata


class PackagedMacosAdmissionTests(unittest.TestCase):
    def test_unsupported_hosts_and_root_are_refused_before_archive_access(self):
        for platform, uid in (("linux", 501), ("darwin", 0)):
            with (
                self.subTest(platform=platform, uid=uid),
                patch.object(sys, "platform", platform),
                patch.object(os, "getuid", return_value=uid),
                patch.object(package_server, "verify_archive") as verify,
                self.assertRaisesRegex(TaskError, "unsupported-headless-host"),
            ):
                acceptance.run(Path("/unused/archive.tar.gz"), "1" * 40)
            verify.assert_not_called()

    def test_invalid_revision_is_refused_before_archive_access(self):
        for revision in ("HEAD", "1234567", "0" * 40, "1" * 40 + "\n"):
            with (
                self.subTest(revision=revision),
                patch.object(sys, "platform", "darwin"),
                patch.object(os, "getuid", return_value=501),
                patch.object(os, "geteuid", return_value=501),
                patch.object(
                    os, "uname", return_value=os.uname_result(("Darwin", "", "", "", "arm64"))
                ),
                patch.object(package_server, "verify_archive") as verify,
                self.assertRaisesRegex(TaskError, "invalid-source-commit"),
            ):
                acceptance.run(Path("/unused/archive.tar.gz"), revision)
            verify.assert_not_called()

    def test_unknown_cli_arguments_never_disclose_paths_or_execute(self):
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

    def test_invalid_option_values_and_missing_flags_have_content_free_diagnostics(self):
        for arguments in (
            [
                "--archive",
                "/unused/archive.tar.gz",
                "--source-commit",
                "1" * 40,
                "--allow-ad-hoc=/private/account/credential",
            ],
            ["--archive", "/unused/archive.tar.gz"],
        ):
            with self.subTest(arguments=arguments):
                diagnostics = io.StringIO()
                with (
                    contextlib.redirect_stderr(diagnostics),
                    patch.object(acceptance, "run") as execute,
                    self.assertRaisesRegex(TaskError, "invalid-headless-arguments"),
                ):
                    acceptance.cli(arguments)
                execute.assert_not_called()
                self.assertEqual(diagnostics.getvalue(), "")


if sys.platform == "darwin":

    class PackagedMacosExecutionTests(unittest.TestCase):
        def setUp(self):
            temporary = tempfile.TemporaryDirectory(prefix="spaceterm-macos-probe-")
            self.addCleanup(temporary.cleanup)
            self.directory = Path(temporary.name).resolve()
            self.root = self.directory / "spaceterm-server"
            self.document = metadata("darwin-arm64", "preflight")
            self.app = self.root / server_artifacts.bundle_name("preflight")
            self.binary = self.app / "Contents/MacOS/spaceterm-server"
            self.binary.parent.mkdir(parents=True)
            self.archive = self.directory / "server.tar.gz"
            self.sentinel = self.directory / "host-private-data"
            self.sentinel.write_bytes(b"host-private-data")
            info = plistlib.loads(package_server_macos.TEMPLATE.read_bytes())
            for key, value in list(info.items()):
                if isinstance(value, str):
                    info[key] = (
                        value.replace("@SERVER_IDENTIFIER@", self.document["native_identifier"])
                        .replace("@SERVER_NAME@", self.app.stem)
                        .replace("@BUNDLE_VERSION@", "0.0.0")
                    )
            (self.app / "Contents/Info.plist").write_bytes(plistlib.dumps(info))

        def archive_probe(
            self, *, document=None, mutate=False, accept_invalid=False, check_isolation=True
        ):
            document = self.document if document is None else document
            encoded = json.dumps(json.dumps(document))
            # The probe exercises the actual inherited sandbox, not its profile
            # text. It must not read host files, launch a tool, or open a socket.
            checks = (
                f"if (getuid() != {os.getuid()} || geteuid() != {os.geteuid()} "
                "|| getuid() == 0) return 10;\n"
                'if (getenv("DISPLAY") || getenv("WAYLAND_DISPLAY") '
                '|| getenv("DYLD_INSERT_LIBRARIES") || getenv("PYTHONPATH")) return 11;\n'
                f'FILE *f = fopen({json.dumps(str(self.sentinel))}, "r"); '
                "if (f || (errno != EPERM && errno != EACCES)) return 12;\n"
                'char cwd[4096]; if (!getcwd(cwd, sizeof(cwd)) || strstr(cwd, ".worktrees") '
                '|| strstr(cwd, "/Projects/")) return 13;\n'
                "int s = socket(AF_INET, SOCK_STREAM, 0); "
                "if (s >= 0) { struct sockaddr_in addr = {0}; addr.sin_family = AF_INET; "
                "addr.sin_port = htons(9); addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK); "
                "if (connect(s, (struct sockaddr *)&addr, sizeof(addr)) == 0 "
                "|| (errno != EPERM && errno != EACCES)) return 14; close(s); } "
                "else if (errno != EPERM && errno != EACCES) return 14;\n"
                'char *child[] = {"/usr/bin/true", NULL}; pid_t pid; '
                'int spawn = posix_spawn(&pid, "/usr/bin/true", NULL, NULL, child, environ); '
                "if (spawn != EPERM && spawn != EACCES) return 15;\n"
            )
            source = (
                "#include <stdio.h>\n#include <stdlib.h>\n#include <string.h>\n"
                "#include <unistd.h>\n#include <errno.h>\n#include <sys/socket.h>\n"
                "#include <netinet/in.h>\n#include <spawn.h>\nextern char **environ;\n"
                "int main(int argc, char **argv) {\n"
                + (checks if check_isolation else "")
                + (
                    'char path[4096]; snprintf(path, sizeof(path), "%s/side-effect", '
                    'getenv("HOME")); fclose(fopen(path, "w"));\n'
                    if mutate
                    else ""
                )
                + 'if (argc == 2 && !strcmp(argv[1], "--distribution-metadata")) {\n'
                + f"puts({encoded}); return 0; }}\n"
                + 'if (argc == 2 && !strcmp(argv[1], "--help")) { '
                'puts("usage: spaceterm-server serve | --help"); return 0; }\n'
                + 'if (argc == 2 && !strcmp(argv[1], "--version")) { '
                + f"puts({json.dumps('spaceterm-server ' + document['version'])}); return 0; }}\n"
                + (
                    "return 0;\n"
                    if accept_invalid
                    else 'fputs("private native error /account/secret", stderr); return 2;\n'
                )
                + "}\n"
            )
            subprocess.run(
                ["/usr/bin/cc", "-x", "c", "-", "-o", self.binary],
                input=source.encode(),
                check=True,
                capture_output=True,
                timeout=30,
            )
            self.binary.chmod(0o700)
            for path in (self.binary, self.app):
                subprocess.run(
                    [
                        "/usr/bin/codesign",
                        "--force",
                        "--sign",
                        "-",
                        "--identifier",
                        self.document["native_identifier"],
                        "--options",
                        "runtime",
                        "--entitlements",
                        package_server_macos.ENTITLEMENTS,
                        "--timestamp=none",
                        path,
                    ],
                    check=True,
                    capture_output=True,
                    timeout=30,
                )
            package_server.write_manifest(self.root, self.document)
            package_server.write_archive(self.root, self.archive)

        def test_signed_archive_runs_outside_checkout_without_gui_or_host_authority(self):
            self.archive_probe()
            with patch.dict(os.environ, {"DISPLAY": "unavailable", "PYTHONPATH": "unavailable"}):
                result = acceptance.run(self.archive, "2" * 40, allow_ad_hoc=True)
            self.assertEqual(result["classification"], "packaged-queries-passed")
            self.assertEqual(result["queries"], 7)
            self.assertEqual(result["source_commit"], "2" * 40)
            self.assertEqual(result["metadata"], self.document)
            self.assertEqual(result["archive_sha256"], server_artifacts.digest(self.archive))
            self.assertEqual(result["signature"], "ad-hoc")
            self.assertFalse(result["release_signature_verified"])
            self.assertFalse(result["service_lifecycle_tested"])
            self.assertFalse(result["runtime_readiness_tested"])

        def test_prepare_archive_copies_resources_and_produces_a_verifiable_test_candidate(self):
            self.archive_probe(check_isolation=False)
            prepared = self.directory / "prepared.tar.gz"
            self.assertEqual(acceptance.prepare_archive(self.binary, prepared), self.document)
            manifest = package_server.verify_archive(prepared)
            server_file = next(
                item
                for item in manifest["files"]
                if item["name"] == manifest["entry_points"]["server"]
            )
            self.assertEqual(server_file["mode"], 0o700)
            self.assertTrue(any("shell-integration/" in item["name"] for item in manifest["files"]))
            self.assertTrue(any("terminfo/" in item["name"] for item in manifest["files"]))
            result = acceptance.run(prepared, "2" * 40, allow_ad_hoc=True)
            self.assertEqual(result["metadata"], self.document)
            self.assertEqual(result["signature"], "ad-hoc")
            self.assertFalse(result["release_signature_verified"])

        def test_release_identity_is_never_ad_hoc_prepared_or_admitted(self):
            self.document = metadata("darwin-arm64", "release")
            release_app = self.root / server_artifacts.bundle_name("release")
            self.app.rename(release_app)
            self.app = release_app
            self.binary = self.app / "Contents/MacOS/spaceterm-server"
            info_path = self.app / "Contents/Info.plist"
            info = plistlib.loads(info_path.read_bytes())
            info["CFBundleIdentifier"] = self.document["native_identifier"]
            info_path.write_bytes(plistlib.dumps(info))
            self.archive_probe(check_isolation=False)
            prepared = self.directory / "not-created.tar.gz"
            with self.assertRaisesRegex(TaskError, "release-ad-hoc-signing-refused"):
                acceptance.prepare_archive(self.binary, prepared)
            self.assertFalse(prepared.exists())
            with self.assertRaisesRegex(TaskError, "ad-hoc-signature-not-approved"):
                acceptance.run(self.archive, "2" * 40, allow_ad_hoc=True)

        def test_ad_hoc_candidate_requires_explicit_opt_in(self):
            self.archive_probe()
            with self.assertRaises(TaskError):
                acceptance.run(self.archive, "2" * 40)

        def test_metadata_substitution_fails_without_native_output_disclosure(self):
            self.archive_probe(document={**self.document, "build_commit": "3" * 40})
            with self.assertRaisesRegex(TaskError, "packaged-metadata-mismatch") as failure:
                acceptance.run(self.archive, "2" * 40, allow_ad_hoc=True)
            self.assertNotIn(str(self.directory), str(failure.exception))
            self.assertNotIn("private native error", str(failure.exception))

        def test_query_mutations_are_not_accepted(self):
            self.archive_probe(mutate=True)
            with self.assertRaisesRegex(TaskError, "packaged-query-mutated-state"):
                acceptance.run(self.archive, "2" * 40, allow_ad_hoc=True)

        def test_invalid_arguments_cannot_be_silently_accepted(self):
            self.archive_probe(accept_invalid=True)
            with self.assertRaisesRegex(TaskError, "invalid-packaged-arguments-accepted"):
                acceptance.run(self.archive, "2" * 40, allow_ad_hoc=True)

        def test_corrupt_archive_is_rejected_before_query_execution(self):
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
                acceptance.run(self.archive, "2" * 40, allow_ad_hoc=True)
            self.assertFalse(
                any(call.args[0][0] == "/usr/bin/sandbox-exec" for call in run.call_args_list)
            )

        def test_changed_signed_resource_fails_even_with_rebuilt_manifest(self):
            self.archive_probe()
            info_path = self.app / "Contents/Info.plist"
            info = plistlib.loads(info_path.read_bytes())
            info["CFBundleName"] = "Changed signed resource"
            info_path.write_bytes(plistlib.dumps(info))
            (self.root / "installation.json").unlink()
            package_server.write_manifest(self.root, self.document)
            self.archive.unlink()
            package_server.write_archive(self.root, self.archive)
            with self.assertRaises(TaskError):
                acceptance.run(self.archive, "2" * 40, allow_ad_hoc=True)
