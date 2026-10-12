"""Exercise real archives through the unprivileged package interface."""

import io
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, package_server
from spaceterm_tasks.server_artifacts import entry_points, identifier


def metadata(platform="linux-x86_64", build="release"):
    return {
        "schema_version": 1,
        "component": "server",
        "build_identity": build,
        "platform": platform,
        "version": "1.2.3",
        "build_commit": "1" * 40,
        "protocol_majors": [1, 2],
        "storage": {"read": [1], "write": 1, "migrate_from": [1]},
        "native_identifier": identifier(build),
        "launcher_abi": 2,
    }


class ServerPackageTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name) / "spaceterm-server"
        self.root.mkdir()
        self.archive = Path(temporary.name) / "server.tar.gz"
        self.metadata = metadata()
        for name in entry_points("linux-x86_64", "release").values():
            if name:
                path = self.root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(name.encode())
                path.chmod(0o501 if name == "bin/spaceterm-server" else 0o555)

    def test_manifest_matches_packaged_executable_metadata(self):
        manifest = package_server.write_manifest(self.root, self.metadata)
        package_server.write_archive(self.root, self.archive)
        self.assertEqual(package_server.verify_archive(self.archive), manifest)
        self.assertEqual(manifest["native_identifier"], self.metadata["native_identifier"])

    def test_extraction_rejects_links_devices_traversal_duplicates_and_extra_files(self):
        manifest = package_server.write_manifest(self.root, self.metadata)
        package_server.write_archive(self.root, self.archive)
        with tarfile.open(self.archive) as source:
            members = []
            for member in source:
                stream = source.extractfile(member) if member.isfile() else None
                members.append((member, stream.read() if stream is not None else b""))
        for name, kind in (
            ("spaceterm-server/link", tarfile.SYMTYPE),
            ("spaceterm-server/device", tarfile.CHRTYPE),
            ("spaceterm-server/../escape", tarfile.REGTYPE),
            ("spaceterm-server/extra", tarfile.REGTYPE),
            ("spaceterm-server/bin/spaceterm-server", tarfile.REGTYPE),
        ):
            with self.subTest(name=name):
                bad = self.archive.parent / "bad.tar.gz"
                with tarfile.open(bad, "w:gz", format=tarfile.GNU_FORMAT) as target:
                    for member, data in members:
                        target.addfile(member, io.BytesIO(data))
                    extra = tarfile.TarInfo(name)
                    extra.type = kind
                    extra.mode = 0o555
                    target.addfile(extra, io.BytesIO())
                with self.assertRaises(TaskError):
                    package_server.verify_archive(bad)
        self.assertEqual(manifest["entry_points"]["server"], "bin/spaceterm-server")

    def test_manifest_rejects_resource_substitution(self):
        package_server.write_manifest(self.root, self.metadata)
        (self.root / "bin/spaceterm-server").unlink()
        (self.root / "bin/spaceterm-server").write_bytes(b"substitution")
        (self.root / "bin/spaceterm-server").chmod(0o501)
        package_server.write_archive(self.root, self.archive)
        with self.assertRaises(TaskError):
            package_server.verify_archive(self.archive)

    def test_headless_archive_has_no_desktop_or_sparkle_resources(self):
        resource = self.root / "Sparkle.framework/Sparkle"
        resource.parent.mkdir()
        resource.write_bytes(b"desktop resource")
        with self.assertRaises(TaskError):
            package_server.write_manifest(self.root, self.metadata)

    def test_server_info_plist_has_no_desktop_activation(self):
        import plistlib

        plist = plistlib.loads((ROOT / "packaging/server/macos/Info.plist").read_bytes())
        self.assertTrue(plist["LSBackgroundOnly"])
        self.assertNotIn("NSPrincipalClass", plist)
        self.assertNotIn("SUPublicEDKey", plist)
        self.assertNotIn("CFBundleIconFile", plist)

    def test_packaging_never_fabricates_store_compatibility(self):
        malformed = dict(self.metadata)
        malformed["storage"] = {"read": [], "write": 1, "migrate_from": [1]}
        with self.assertRaises(TaskError):
            package_server.write_manifest(self.root, malformed)


class CredentialClosureStagingTests(unittest.TestCase):
    def fixture(self, temporary):
        import copy
        import hashlib

        from spaceterm_tasks import package_server_linux

        root = Path(temporary)
        closure = root / "credential-closure"
        closure.mkdir()
        credential_notice = b"systemd LGPL-2.1-or-later and dependency attribution\n"
        inputs = {
            "libexec/credential-tools/systemd-creds": b"authenticated credential-tool bytes",
            "share/sources/systemd.tar.gz": b"exact corresponding source fixture",
            "share/spaceterm-server/THIRD-PARTY-NOTICES.txt": credential_notice,
        }
        for name, data in inputs.items():
            path = closure / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            path.chmod(0o755 if name.startswith("libexec/") else 0o644)
        pin = copy.deepcopy(package_server_linux.credential_tool_pin())
        pin["build"].update(
            complete=True,
            compiler="test-compiler",
            compiler_version="test-version",
            build_image="test-image",
            build_image_sha256="1" * 64,
            meson_version="test-version",
            ninja_version="test-version",
            files=[
                {"name": name, "sha256": hashlib.sha256(data).hexdigest()}
                for name, data in sorted(inputs.items())
            ],
            dependencies=[
                {
                    "name": "systemd",
                    "version": "260.5",
                    "source_identity": package_server_linux.SOURCE_COMMIT,
                    "source_sha256": hashlib.sha256(
                        inputs["share/sources/systemd.tar.gz"]
                    ).hexdigest(),
                    "build_options": [],
                    "license": "LGPL-2.1-or-later",
                    "notice_file": "share/spaceterm-server/THIRD-PARTY-NOTICES.txt",
                    "corresponding_source": "share/sources/systemd.tar.gz",
                }
            ],
        )
        pin["runtime"].update(jammy_result="passed", host_key_result="passed")
        binaries = {}
        for name in ("spaceterm-server", "spaceterm-server-launcher", "spaceterm-server-manager"):
            path = root / name
            path.write_bytes(name.encode())
            binaries[name] = path
        return closure, pin, binaries, credential_notice

    def package(self, temporary, mutate=False):
        from unittest.mock import patch

        from spaceterm_tasks import package_server_linux

        root = Path(temporary)
        closure, pin, binaries, notice = self.fixture(temporary)
        validate = package_server_linux.validate_credential_closure
        original_resources = package_server.copy_resources

        def resources(destination, **kwargs):
            result = original_resources(destination, **kwargs)
            if mutate == "resources":
                tool = destination.parents[1] / "libexec/credential-tools/systemd-creds"
                tool.chmod(0o755)
                tool.write_bytes(b"late replacement")
            return result

        def build(_release_tag):
            if mutate is True:
                (closure / "libexec/credential-tools/systemd-creds").write_bytes(b"replacement")
            return binaries

        with (
            patch.object(package_server_linux.sys, "platform", "linux"),
            patch.object(package_server_linux, "DIST", root),
            patch.object(package_server_linux, "CLOSURE", closure),
            patch.object(package_server_linux, "credential_tool_pin", return_value=pin),
            patch.object(
                package_server_linux,
                "validate_credential_closure",
                side_effect=lambda *args: validate(closure),
            ),
            patch.object(package_server_linux, "verify_elf_closure"),
            patch.object(package_server, "DIST", root),
            patch.object(package_server, "build", side_effect=build),
            patch.object(package_server, "copy_resources", side_effect=resources),
            patch.object(
                package_server, "executable_metadata", return_value=metadata(build="preflight")
            ),
        ):
            package_server_linux.package()
        return root / "spaceterm-server-preflight-linux-x86_64.tar.gz", notice

    def test_copied_credential_tool_must_still_match_the_retained_binary_pin(self):
        with tempfile.TemporaryDirectory() as temporary, self.assertRaises(TaskError):
            self.package(temporary, mutate=True)

    def test_credential_binary_substitution_during_resource_compilation_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary, self.assertRaises(TaskError):
            self.package(temporary, mutate="resources")

    def test_archive_preserves_credential_tool_and_desktop_notices(self):
        import os

        from spaceterm_tasks.packaging import NOTICES

        with tempfile.TemporaryDirectory() as temporary:
            previous = os.umask(0o002)
            try:
                archive, credential_notice = self.package(temporary)
            finally:
                os.umask(previous)
            with tarfile.open(archive) as packaged:
                notices = packaged.extractfile(
                    "spaceterm-server/share/spaceterm-server/THIRD-PARTY-NOTICES.txt"
                )
                assert notices is not None
                actual = notices.read()
            self.assertIn(credential_notice, actual)
            self.assertIn(NOTICES.read_bytes(), actual)


class CredentialToolBuildTests(unittest.TestCase):
    def test_source_authentication_precedes_extraction_and_build_commands(self):
        import importlib.util
        from argparse import Namespace
        from unittest.mock import patch

        script = ROOT / "packaging/server/linux/credential-tools/build.py"
        spec = importlib.util.spec_from_file_location("credential_tool_builder", script)
        assert spec is not None and spec.loader is not None
        builder = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(builder)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source.tar.gz"
            source.write_bytes(b"unauthenticated source")
            arguments = Namespace(scratch=root / "scratch", source=source)
            with patch.object(builder, "run", return_value="glibc 2.35") as commands:
                with self.assertRaisesRegex(
                    builder.BuildFailure, "credential-tool-source-rejected"
                ):
                    builder.build(arguments)
            self.assertEqual(commands.call_count, 1)
            self.assertEqual(list((root / "scratch").iterdir()), [])

    def test_build_attempts_only_the_owned_pinned_image_without_signing_inputs(self):
        import os
        from unittest.mock import patch

        from spaceterm_tasks import package_server_linux
        from spaceterm_tasks.macos_signing import SIGNING_INPUTS

        calls = []

        def docker(arguments, _failure, **keywords):
            calls.append(arguments)
            self.assertEqual(arguments[:2], ["docker", "build"])
            self.assertFalse(set(SIGNING_INPUTS) & set(keywords["env"]))
            self.assertIn("linux/amd64", arguments)
            self.assertIn(
                package_server_linux.ROOT
                / "packaging/server/linux/credential-tools/Jammy.Dockerfile",
                arguments,
            )
            raise TaskError("test docker boundary")

        with tempfile.TemporaryDirectory() as temporary:
            with (
                patch.object(package_server_linux.sys, "platform", "linux"),
                patch.object(package_server_linux, "DIST", Path(temporary)),
                patch.object(package_server_linux, "checked", side_effect=docker),
                patch.dict(os.environ, {key: "sentinel-signing-input" for key in SIGNING_INPUTS}),
                self.assertRaisesRegex(TaskError, "test docker boundary"),
            ):
                package_server_linux.build_credential_tool()
            self.assertEqual(len(calls), 1)
            self.assertEqual(list(Path(temporary).iterdir()), [])


class CredentialToolPinTests(unittest.TestCase):
    def test_credential_tool_and_dependencies_require_exact_source_pin(self):
        from spaceterm_tasks.package_server_linux import (
            SOURCE_COMMIT,
            SOURCE_SHA256,
            SOURCE_TAG,
            credential_tool_pin,
        )

        pin = credential_tool_pin()
        self.assertEqual(pin["source"]["commit"], SOURCE_COMMIT)
        self.assertEqual(pin["source"]["annotated_tag"], SOURCE_TAG)
        self.assertEqual(pin["source"]["archive_sha256"], SOURCE_SHA256)
        self.assertIn("-Dauto_features=disabled", pin["build"]["options"])
        self.assertIn("-Dtpm2=enabled", pin["build"]["options"])

    def test_completed_build_does_not_replace_native_baseline_and_credential_evidence(self):
        from spaceterm_tasks.package_server_linux import validate_credential_closure

        with self.assertRaisesRegex(TaskError, "Jammy evidence are unavailable"):
            validate_credential_closure()

    def test_lgpl_material_covers_the_recorded_build_without_claiming_native_evidence(self):
        from spaceterm_tasks.package_server_linux import credential_tool_pin

        pin = credential_tool_pin()
        self.assertEqual(pin["licensing"]["linking"], "private-shared-libraries")
        self.assertEqual(pin["runtime"]["jammy_result"], "unrun")
        self.assertTrue(pin["build"]["complete"])
        self.assertEqual(pin["build"]["compiler_version"], "11.4.0")
        self.assertEqual(len(pin["build"]["dependencies"]), 7)
        self.assertTrue(pin["build"]["files"])
        self.assertTrue(all(d["source_sha256"] for d in pin["build"]["dependencies"]))
        self.assertEqual(pin["runtime"]["host_key_result"], "unrun")
        self.assertEqual(pin["runtime"]["tpm_result"], "unrun")
        licenses = ROOT / "packaging/server/linux/credential-tools"
        self.assertTrue((licenses / "LGPL-2.1-or-later.txt").is_file())
        self.assertIn(
            "exact systemd source archive", (licenses / "THIRD-PARTY-NOTICES.txt").read_text()
        )

    def test_credential_tool_cannot_override_required_features_with_duplicate_options(self):
        from unittest.mock import patch

        from spaceterm_tasks import package_server_linux

        with tempfile.TemporaryDirectory() as temporary:
            pin = Path(temporary) / "pin.toml"
            pin.write_text(
                package_server_linux.PIN.read_text().replace(
                    '"-Dopenssl=enabled",', '"-Dopenssl=enabled", "-Dopenssl=disabled",'
                )
            )
            with patch.object(package_server_linux, "PIN", pin), self.assertRaises(TaskError):
                package_server_linux.credential_tool_pin()


class ExecutableMetadataTests(unittest.TestCase):
    def fixture(self, content):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        executable = Path(temporary.name) / "metadata-fixture"
        executable.write_text("#!/bin/sh\n" + content)
        executable.chmod(0o755)
        return executable

    def test_packaged_executable_metadata_is_bounded_and_matches_the_manifest(self):
        import json
        import shlex

        data = json.dumps(metadata())
        executable = self.fixture("printf '%s' " + shlex.quote(data) + "\n")
        self.assertEqual(
            package_server.executable_metadata(executable, "linux-x86_64", "release", "1.2.3"),
            metadata(),
        )
        with self.assertRaises(TaskError):
            package_server.executable_metadata(executable, "linux-x86_64", "preflight")

    def test_oversized_metadata_and_native_error_text_are_not_relayed(self):
        import shlex

        for content in (
            "printf '%s' " + shlex.quote("x" * 4097),
            "printf 'private native error' >&2",
        ):
            executable = self.fixture(content)
            with self.assertRaises(TaskError) as failure:
                package_server.executable_metadata(executable, "linux-x86_64", "release")
            self.assertNotIn("private native error", str(failure.exception))
            self.assertNotIn(str(executable), str(failure.exception))

    def test_invalid_metadata_scalars_fail_with_a_content_free_classification(self):
        from spaceterm_tasks.server_artifacts import validate_metadata

        for field, value in (
            ("build_identity", {}),
            ("schema_version", True),
            ("protocol_majors", [True]),
            ("storage", None),
        ):
            record = metadata()
            record[field] = value
            with self.assertRaises(TaskError):
                validate_metadata(record)

    def test_copied_executable_compatibility_matches_every_manifest_metadata_field(self):
        import json
        import shlex

        record = metadata()
        executable = self.fixture("printf '%s' " + shlex.quote(json.dumps(record)))
        root = executable.parent
        (root / "bin").mkdir()
        copied = root / "bin/spaceterm-server"
        copied.write_bytes(executable.read_bytes())
        copied.chmod(0o501)
        package_server.verify_executable_metadata(root, record)
        record["protocol_majors"] = [1]
        with self.assertRaises(TaskError):
            package_server.verify_executable_metadata(root, record)


class CargoServerArtifactTests(unittest.TestCase):
    def test_cargo_reported_executables_override_stale_default_target_artifacts(self):
        import os
        from unittest.mock import patch

        from spaceterm_tasks import cargo
        from spaceterm_tasks.macos_signing import SIGNING_INPUTS

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "external-target"
            target.mkdir()
            stale = root / "target/release"
            stale.mkdir(parents=True)
            names = (
                "spaceterm-server",
                "spaceterm-server-launcher",
                "spaceterm-server-manager",
            )
            for name in names:
                (stale / name).write_bytes(b"stale artifact")
                (target / name).write_text("#!/bin/sh\nexit 0\n")
                (target / name).chmod(0o755)
            tools = root / "tools"
            tools.mkdir()
            tool = tools / "cargo"
            tool.write_text(
                "#!/bin/sh\n"
                + "\n".join(
                    f'if [ "${{{key}+present}}" = present ]; then exit 53; fi'
                    for key in SIGNING_INPUTS
                )
                + '\nwhile [ "$#" -gt 0 ]; do\n'
                + 'if [ "$1" = --bin ]; then shift; binary=$1; fi\nshift\ndone\n'
                + 'printf \'{"reason":"compiler-artifact","target":{"name":"%s"},'
                + '"executable":"%s/%s"}\\n\' "$binary" "$CARGO_TARGET_DIR" "$binary"\n'
            )
            tool.chmod(0o755)
            with (
                patch.dict(
                    os.environ,
                    {
                        **{key: "sentinel-signing-input" for key in SIGNING_INPUTS},
                        "PATH": str(tools),
                        "CARGO_TARGET_DIR": str(target),
                    },
                ),
                patch.object(package_server, "ROOT", root),
                patch.object(cargo, "ROOT", root),
            ):
                self.assertEqual(
                    package_server.build(), {name: (target / name).resolve() for name in names}
                )


class PackageSubprocessEnvironmentTests(unittest.TestCase):
    def test_resource_compiler_and_signature_verifiers_never_receive_signing_inputs(self):
        import os
        from unittest.mock import patch

        from spaceterm_tasks import package_server_macos
        from spaceterm_tasks.macos_signing import SIGNING_INPUTS
        from spaceterm_tasks.server_artifacts import PINNED_CERTIFICATE

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            tools = root / "tools"
            tools.mkdir()
            reject_inputs = "\n".join(
                f'if [ "${{{key}+present}}" = present ]; then exit 53; fi' for key in SIGNING_INPUTS
            )
            requirement = (
                'designated => identifier "io.github.sadiksaifi.spaceterm.server" '
                f'and certificate leaf = H"{PINNED_CERTIFICATE.lower()}"'
            )
            for name, body in (
                ("tic", 'printf compiled > "$3/fixture"'),
                ("lipo", "printf arm64"),
                (
                    "codesign",
                    'case "$2" in\n'
                    f"--requirements) printf '%s' '{requirement}';;\n"
                    "--verbose=4) printf 'flags=0x10000(runtime)' >&2;;\n"
                    "esac",
                ),
            ):
                tool = tools / name
                tool.write_text("#!/bin/sh\n" + reject_inputs + "\n" + body + "\n")
                tool.chmod(0o755)
            binary = root / "server"
            binary.write_bytes(b"fixture")
            with patch.dict(
                os.environ,
                {**{key: "sentinel-signing-input" for key in SIGNING_INPUTS}, "PATH": str(tools)},
            ):
                package_server.copy_resources(root / "resources")
                package_server_macos.verify_server_signature(
                    binary, "io.github.sadiksaifi.spaceterm.server"
                )
            self.assertEqual((root / "resources/terminfo/fixture").read_text(), "compiled")


class MacosDependencyClosureTests(unittest.TestCase):
    def test_graphical_external_and_rpath_dependencies_block_package_before_signing(self):
        from contextlib import nullcontext
        from unittest.mock import Mock, patch

        from spaceterm_tasks import package_server_macos

        loads = (
            "Load command 1\n      cmd LC_LOAD_DYLIB\n     name /System/Library/Frameworks/AppKit.framework/Versions/C/AppKit (offset 24)\n",
            "Load command 1\n      cmd LC_LOAD_DYLIB\n     name /opt/local/lib/libfixture.dylib (offset 24)\n",
            "Load command 1\n      cmd LC_RPATH\n     path /account/lib (offset 12)\n",
            "Load command 1\n      cmd LC_LOAD_DYLIB\n     name @rpath/libfixture.dylib (offset 24)\n",
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binaries = {}
            for name in (
                "spaceterm-server",
                "spaceterm-server-launcher",
                "spaceterm-server-manager",
            ):
                binary = root / name
                binary.write_bytes(bytes.fromhex("cffaedfe") + b"fixture")
                binaries[name] = binary
            for load in loads:
                signing = Mock(return_value=nullcontext())
                with (
                    self.subTest(load=load),
                    patch.object(package_server_macos.sys, "platform", "darwin"),
                    patch.object(package_server_macos, "DIST", root),
                    patch.object(package_server, "DIST", root),
                    patch.object(package_server, "build", return_value=binaries),
                    patch.object(
                        package_server,
                        "executable_metadata",
                        return_value=metadata("darwin-arm64", "preflight"),
                    ),
                    patch.object(package_server, "copy_resources"),
                    patch.object(package_server_macos, "release_keychain", signing),
                    patch.object(package_server_macos, "verify_server_signature"),
                    patch.object(package_server_macos, "checked", return_value=load),
                ):
                    with self.assertRaises(TaskError):
                        package_server_macos.package()
                    signing.assert_not_called()


class MacosServerSignaturePolicyTests(unittest.TestCase):
    def test_server_signature_requires_pinned_identifier_runtime_arm64_and_empty_entitlements(self):
        import plistlib
        from unittest.mock import Mock, patch

        from spaceterm_tasks import package_server_macos
        from spaceterm_tasks.server_artifacts import PINNED_CERTIFICATE

        identifier = "io.github.sadiksaifi.spaceterm.server"
        designated = f'designated => identifier "{identifier}" and certificate leaf = H"{PINNED_CERTIFICATE.lower()}"'
        with tempfile.TemporaryDirectory() as temporary:
            binary = Path(temporary) / "server"
            binary.write_bytes(b"fixture")
            for case in (
                "valid",
                "wrong-identifier",
                "no-runtime",
                "debug-entitlement",
                "wrong-architecture",
            ):
                requirement = (
                    designated if case != "wrong-identifier" else designated.replace(".server", "")
                )
                flags = b"flags=0x10000(runtime)" if case != "no-runtime" else b"flags=0x0(none)"
                entitlements = (
                    {}
                    if case != "debug-entitlement"
                    else {"com.apple.security.get-task-allow": True}
                )
                architecture = "arm64" if case != "wrong-architecture" else "x86_64"
                with (
                    patch("spaceterm_tasks.macos_signing.checked", side_effect=[b"", requirement]),
                    patch.object(
                        package_server_macos,
                        "checked",
                        side_effect=[architecture, plistlib.dumps(entitlements)],
                    ),
                    patch.object(
                        package_server_macos.subprocess,
                        "run",
                        return_value=Mock(returncode=0, stderr=flags),
                    ),
                ):
                    if case == "valid":
                        package_server_macos.verify_server_signature(binary, identifier)
                    else:
                        with self.subTest(case=case), self.assertRaises(TaskError):
                            package_server_macos.verify_server_signature(binary, identifier)


if sys.platform == "darwin":

    class NativeMachOClosureTests(unittest.TestCase):
        def test_actual_macho_graphical_external_and_rpath_linkage_is_rejected(self):
            import subprocess

            from spaceterm_tasks.package_server_macos import verify_macho_closure

            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary) / "package"
                root.mkdir()
                executable = root / "server"
                external = Path(temporary) / "libexternal.dylib"
                subprocess.run(
                    ["/usr/bin/clang", "-x", "c", "-", "-dynamiclib", "-o", external],
                    input=b"int fixture(void) { return 7; }\n",
                    env={},
                    capture_output=True,
                    check=True,
                )
                for case, flags in (
                    ("headless", []),
                    ("graphical", ["-Wl,-needed_framework,AppKit"]),
                    ("external", [f"-Wl,-needed_library,{external}"]),
                    ("rpath", [f"-Wl,-rpath,{external.parent}"]),
                ):
                    subprocess.run(
                        ["/usr/bin/clang", "-x", "c", "-", "-o", executable, *flags],
                        input=b"int main(void) { return 0; }\n",
                        env={},
                        capture_output=True,
                        check=True,
                    )
                    with self.subTest(case=case):
                        if case == "headless":
                            verify_macho_closure(root, [executable])
                        else:
                            with self.assertRaises(TaskError):
                                verify_macho_closure(root, [executable])


if sys.platform == "linux":

    class LinuxElfClosureTests(unittest.TestCase):
        def fixture(self):
            import shutil
            import subprocess

            temporary_directory = tempfile.TemporaryDirectory()
            self.addCleanup(temporary_directory.cleanup)
            temporary = temporary_directory.name
            root = Path(temporary) / "package"
            build = Path(temporary) / "build"
            libraries = root / "lib/credential-tools"
            tool = root / "libexec/credential-tools/systemd-creds"
            build.mkdir()
            libraries.mkdir(parents=True)
            tool.parent.mkdir(parents=True)
            # This is a credential-free ELF fixture, not the pinned upstream tool.
            subprocess.run(
                [
                    "gcc",
                    "-x",
                    "c",
                    "-",
                    "-shared",
                    "-fPIC",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-Wl,-rpath,$ORIGIN",
                    "-o",
                    str(build / "libfixture.so"),
                ],
                input=b"int fixture(void) { return 7; }\n",
                check=True,
                capture_output=True,
            )
            subprocess.run(
                [
                    "gcc",
                    "-x",
                    "c",
                    "-",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-L" + str(build),
                    "-lfixture",
                    "-Wl,-rpath,$ORIGIN/../../lib/credential-tools",
                    "-o",
                    str(tool),
                ],
                input=b'#include <stdio.h>\nextern int fixture(void);\nint main(void) { printf("%d\\n", fixture()); return 0; }\n',
                check=True,
                capture_output=True,
            )
            shutil.copyfile(build / "libfixture.so", libraries / "libfixture.so")
            shutil.rmtree(build)
            return root, tool, libraries / "libfixture.so"

        def test_private_elf_fixture_runs_without_build_host_libraries(self):
            import subprocess

            from spaceterm_tasks.package_server_linux import verify_elf_closure

            root, tool, _ = self.fixture()
            verify_elf_closure(root)
            result = subprocess.run([tool], env={}, check=True, capture_output=True, timeout=5)
            self.assertEqual(result.stdout, b"7\n")

        def test_actual_elf_search_paths_reject_nested_missing_duplicate_and_non_elf_libraries(
            self,
        ):
            import shutil

            from spaceterm_tasks.package_server_linux import verify_elf_closure

            for case in ("nested", "missing", "duplicate", "non-elf"):
                root, _, library = self.fixture()
                nested = library.parent / "nested" / library.name
                nested.parent.mkdir()
                if case == "nested":
                    library.rename(nested)
                elif case == "missing":
                    library.unlink()
                elif case == "duplicate":
                    shutil.copyfile(library, nested)
                else:
                    library.write_bytes(b"not an ELF dependency")
                with self.subTest(case=case), self.assertRaises(TaskError):
                    verify_elf_closure(root)
