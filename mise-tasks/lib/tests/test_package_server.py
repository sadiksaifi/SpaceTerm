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
        "schema_version": 2,
        "component": "server",
        "build_identity": build,
        "platform": platform,
        "version": "1.2.3",
        "build_commit": "1" * 40,
        "protocol_majors": [1, 2],
        "storage": {"read": [1], "write": 1, "migrate_from": []},
        "native_identifier": identifier(build),
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
                path.chmod(0o700 if name == "bin/spaceterm-server" else 0o555)

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

    def test_server_only_build_is_copied_with_owner_execution(self):
        destination = self.root / "destination"
        copied = package_server.copy_built_executables(
            {"spaceterm-server": self.root / "bin/spaceterm-server"},
            destination,
            self.metadata,
        )
        self.assertEqual(
            [p.relative_to(destination).as_posix() for p in copied], ["bin/spaceterm-server"]
        )
        self.assertEqual(copied[0].stat().st_mode & 0o777, 0o700)
        self.assertEqual(copied[0].read_bytes(), b"bin/spaceterm-server")

    def test_privileged_helpers_are_rejected_before_copy(self):
        for name in (
            "spaceterm-server-launcher",
            "spaceterm-server-manager",
            "spaceterm-server-spawner",
        ):
            with self.subTest(name=name), self.assertRaises(TaskError):
                package_server.copy_built_executables(
                    {
                        "spaceterm-server": self.root / "bin/spaceterm-server",
                        name: self.root / "bin/spaceterm-server",
                    },
                    self.root / "destination",
                    self.metadata,
                )

    def test_production_manifest_refuses_evidence_only_executables(self):
        evidence = self.root / "libexec/spaceterm-native-evidence-client"
        evidence.parent.mkdir(exist_ok=True)
        evidence.write_bytes(b"evidence-only code")
        with self.assertRaises(TaskError):
            package_server.write_manifest(self.root, self.metadata)

    def test_manifest_rejects_resource_substitution(self):
        package_server.write_manifest(self.root, self.metadata)
        (self.root / "bin/spaceterm-server").unlink()
        (self.root / "bin/spaceterm-server").write_bytes(b"substitution")
        (self.root / "bin/spaceterm-server").chmod(0o700)
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
        copied.chmod(0o700)
        package_server.verify_executable_metadata(root, record)
        record["protocol_majors"] = [1]
        with self.assertRaises(TaskError):
            package_server.verify_executable_metadata(root, record)


class ImplementedServerCompatibilityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        import json
        import os
        import shutil

        from spaceterm_tasks.cargo import build_executable
        from spaceterm_tasks.macos_signing import signing_environment

        temporary = tempfile.TemporaryDirectory(prefix="spaceterm-compatibility-")
        cls.addClassCleanup(temporary.cleanup)
        project = Path(temporary.name)
        dependencies = "\n".join(
            f"{name} = {{ path = {json.dumps(str(ROOT / 'crates' / name))} }}"
            for name in ("spaceterm-host", "spaceterm-protocol")
        )
        (project / "Cargo.toml").write_text(
            '[package]\nname = "spaceterm-implemented-compatibility"\n'
            'version = "0.0.0"\nedition = "2024"\n'
            '[[bin]]\nname = "compatibility"\npath = "main.rs"\n'
            f'[dependencies]\n{dependencies}\nserde_json = "1.0"\n'
        )
        shutil.copyfile(
            Path(__file__).with_name("implemented_server_compatibility.rs"), project / "main.rs"
        )
        environment = signing_environment(os.environ)
        environment["CARGO_BUILD_JOBS"] = "4"
        environment["CARGO_TARGET_DIR"] = os.environ.get("CARGO_TARGET_DIR", str(ROOT / "target"))
        cls.probe = build_executable(
            "--offline",
            "--manifest-path",
            project / "Cargo.toml",
            binary="compatibility",
            env=environment,
        )

    def compare(self, document):
        import json
        import subprocess

        return subprocess.run(
            [self.probe],
            input=json.dumps(document, separators=(",", ":")).encode(),
            env={},
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=5,
            check=False,
        ).returncode

    def test_actual_server_metadata_matches_implemented_protocol_and_store(self):
        from spaceterm_tasks.cargo import build_executable
        from spaceterm_tasks.macos_signing import signing_environment
        from spaceterm_tasks.packaging import packaged_environment

        environment = signing_environment(packaged_environment(None))
        environment["CARGO_BUILD_JOBS"] = "4"
        executable = build_executable(
            "--locked", "-p", "spaceterm-server", binary="spaceterm-server", env=environment
        )
        platform = "darwin-arm64" if sys.platform == "darwin" else "linux-x86_64"
        document = package_server.executable_metadata(executable, platform, "preflight")
        self.assertEqual(self.compare(document), 0, "server advertises unimplemented compatibility")

    def test_unimplemented_protocol_and_store_claims_are_rejected(self):
        import copy

        document = metadata()
        self.assertEqual(self.compare(document), 0, "implemented compatibility was rejected")
        for field, claim in (
            ("protocol_majors", [1, 2, 3]),
            ("storage", {"read": [1, 2], "write": 1, "migrate_from": []}),
            ("storage", {"read": [1, 2], "write": 2, "migrate_from": [1]}),
            ("storage", {"read": [1], "write": 1, "migrate_from": [1]}),
        ):
            altered = copy.deepcopy(document)
            altered[field] = claim
            with self.subTest(field=field, claim=claim):
                self.assertNotEqual(
                    self.compare(altered), 0, "unimplemented compatibility was accepted"
                )


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
                "spaceterm-server-spawner",
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
                    package_server.build(),
                    {"spaceterm-server": (target / "spaceterm-server").resolve()},
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
            for name in ("spaceterm-server",):
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
                    ("headless-iconv", ["-liconv"]),
                    ("headless-account", ["-Wl,-needed_framework,OpenDirectory"]),
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
                        if case.startswith("headless"):
                            verify_macho_closure(root, [executable])
                        else:
                            with self.assertRaises(TaskError):
                                verify_macho_closure(root, [executable])


if sys.platform == "linux":

    class LinuxElfClosureTests(unittest.TestCase):
        def fixture(self, rpath=False):
            import subprocess

            temporary = tempfile.TemporaryDirectory()
            self.addCleanup(temporary.cleanup)
            root = Path(temporary.name) / "package"
            binary = root / "bin/spaceterm-server"
            binary.parent.mkdir(parents=True)
            command = ["cc", "-x", "c", "-", "-o", binary]
            if rpath:
                command.append("-Wl,-rpath,$ORIGIN/../lib")
            subprocess.run(
                command,
                input=b"int main(void) { return 0; }\n",
                check=True,
                capture_output=True,
                timeout=30,
            )
            binary.chmod(0o700)
            return root, binary

        def test_actual_account_elf_links_only_baseline_system_libraries(self):
            from spaceterm_tasks.package_server_linux import verify_elf_closure

            root, _ = self.fixture()
            verify_elf_closure(root)

        def test_extra_native_code_and_loader_search_paths_are_rejected(self):
            import shutil

            from spaceterm_tasks.package_server_linux import verify_elf_closure

            root, binary = self.fixture()
            extra = root / "share/extra-executable"
            extra.parent.mkdir()
            shutil.copyfile(binary, extra)
            with self.assertRaises(TaskError):
                verify_elf_closure(root)
            root, _ = self.fixture(rpath=True)
            with self.assertRaises(TaskError):
                verify_elf_closure(root)
