"""Stage, finalize, and verify the status companion through fake native tools."""

import importlib
import inspect
import os
import plistlib
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import ROOT, TaskError, package_linux, package_macos, package_server
from spaceterm_tasks import server_artifacts as artifacts
from spaceterm_tasks import status_companion_packaging as companion_packaging
from spaceterm_tasks.macos_bundle import ICON_NAME, identity, identity_directory

DESKTOPS = {
    "spaceterm": ("io.github.sadiksaifi.spaceterm", "SpaceTerm"),
    "preflight": ("io.github.sadiksaifi.spaceterm-preflight", "SpaceTerm Preflight"),
    "development": ("io.github.sadiksaifi.spaceterm-development", "SpaceTerm Development"),
}
# Programs that register login items, launch agents, autostart entries, or services, or
# that request privilege. Packaging the companion runs none of them.
REGISTRATION_PROGRAMS = {
    "launchctl",
    "sfltool",
    "osascript",
    "systemctl",
    "loginctl",
    "sudo",
    "tccutil",
    "defaults",
    "open",
    "xdg-open",
}


def fake_executable(path, reported):
    """A shell script that answers --version like a packaged executable."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f"#!/bin/sh\nprintf '%s\\n' '{reported}'\n")
    path.chmod(0o755)
    return path


def snapshot(root):
    """Every path under `root` with its bytes or link target, to prove nothing changed."""
    result = {}
    for path in sorted(Path(root).rglob("*")):
        relative = path.relative_to(root).as_posix()
        if path.is_symlink():
            result[relative] = ("link", os.readlink(path))
        elif path.is_file():
            result[relative] = ("file", path.read_bytes(), path.stat().st_mode & 0o777)
        else:
            result[relative] = ("directory",)
    return result


class FakeTools:
    """Emulate the native tools packaging runs, and record each invocation in order."""

    def __init__(self, signature=b"Signature=adhoc\nCodeDirectory flags=0x10002(adhoc,runtime)\n"):
        self.calls = []
        self.signature = signature
        self.helpers_when_signed = {}
        self.disk_image_sources = []

    def __call__(self, arguments, failure, *, env=None, timeout=None, check=True):
        arguments = [str(argument) for argument in arguments]
        self.calls.append(arguments)
        program = arguments[1] if arguments[0] == "xcrun" else arguments[0]
        stdout = b""
        stderr = b""
        if program == "codesign" and "--force" in arguments:
            signed = Path(arguments[-1])
            helpers = signed / "Contents" / "Helpers"
            self.helpers_when_signed[signed.name] = (
                sorted(path.name for path in helpers.iterdir()) if helpers.is_dir() else []
            )
        elif program == "codesign" and "--verbose=4" in arguments:
            stderr = self.signature
        elif program == "ditto":
            shutil.copytree(arguments[1], arguments[2], symlinks=True)
        elif program == "hdiutil" and arguments[1] == "create":
            source = Path(arguments[arguments.index("-srcfolder") + 1])
            self.disk_image_sources.append(snapshot(source))
            Path(arguments[-1]).write_bytes(b"read-write image")
        elif program == "hdiutil" and arguments[1] == "convert":
            source = next(argument for argument in arguments[2:] if not argument.startswith("-"))
            shutil.copyfile(source, arguments[arguments.index("-o") + 1])
        elif program == "lipo":
            stdout = b"arm64\n"
        elif program == "vtool":
            stdout = b"Load command 10\n      cmd LC_BUILD_VERSION\n platform 1\n    minos 26.0\n"
        elif program == "objdump":
            stdout = b"\nspaceterm-status:     file format elf64-x86-64\n"
        return subprocess.CompletedProcess(arguments, 0, stdout, stderr)

    def programs(self):
        return {call[1] if call[0] == "xcrun" else call[0] for call in self.calls}


class CompanionFixture(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.tools = FakeTools()
        patcher = patch.object(companion_packaging, "run_tool", self.tools)
        patcher.start()
        self.addCleanup(patcher.stop)

    def packaged_app(self, name, version):
        """The bundle and disk image cargo-packager writes for one identity."""
        template = identity(name)
        output = self.root / "output"
        app = output / f"{template['CFBundleName']}.app"
        contents = app / "Contents"
        info = {**template, "CFBundleShortVersionString": version, "CFBundleVersion": version}
        resources = contents / "Resources"
        resources.mkdir(parents=True)
        (contents / "Info.plist").write_bytes(plistlib.dumps(info))
        fake_executable(
            contents / "MacOS" / template["CFBundleExecutable"],
            f"{template['CFBundleName']} {version}",
        )
        for resource in (f"{ICON_NAME}.icns", "Assets.car"):
            (resources / resource).write_bytes(resource.encode())
        dmg = output / f"{template['CFBundleName']}_{version}_aarch64.dmg"
        dmg.write_bytes(b"cargo-packager image")
        return output, app, dmg

    def companion_executable(self, name, version):
        return fake_executable(
            self.root / "target" / "spaceterm-status",
            f"{DESKTOPS[name][1]} Status {version}",
        )

    def finalize(self, name="preflight", version="0.0.0", release_tag=None):
        output, app, dmg = self.packaged_app(name, version)
        config = tomllib.loads((identity_directory(name) / "Packager.toml").read_text())
        companion_packaging.finalize_macos_package(
            output,
            self.companion_executable(name, version),
            name,
            version,
            config,
            {"PATH": os.environ.get("PATH", "")},
            release_tag,
        )
        return output, app, dmg

    def staged_tree(self, name):
        """A desktop installation tree as package_linux stages it, with the companion beside it."""
        tree = self.root / package_linux.DIRECTORIES[name]

        def tic(arguments, failure, **kwargs):
            Path(arguments[arguments.index("-o") + 1]).mkdir(parents=True)
            return b""

        with patch.object(package_linux, "checked", tic):
            package_linux.stage(name, self.root / "desktop", tree)
        return tree


class CompanionIdentityTests(CompanionFixture):
    def test_companion_identifiers_are_isolated_across_all_build_identities(self):
        servers = {artifacts.identifier(build) for build in artifacts.BUILDS}
        server_bundles = {artifacts.bundle_name(build) for build in artifacts.BUILDS}
        identifiers = set()
        for name, (desktop_identifier, desktop_name) in DESKTOPS.items():
            with self.subTest(identity=name):
                companion = companion_packaging.companion(name)
                self.assertEqual(companion.identifier, f"{desktop_identifier}.status")
                self.assertEqual(companion.name, f"{desktop_name} Status")
                self.assertEqual(companion.bundle, f"{desktop_name} Status.app")
                self.assertNotIn(companion.identifier, servers)
                self.assertNotIn(companion.bundle, server_bundles)
                identifiers.update((desktop_identifier, companion.identifier))
        self.assertEqual(len(identifiers), 2 * len(DESKTOPS))
        with self.assertRaises(TaskError):
            companion_packaging.companion("release")

    def test_desktop_identifiers_and_main_executable_names_are_unchanged(self):
        for name, (desktop_identifier, desktop_name) in DESKTOPS.items():
            with self.subTest(identity=name):
                template = identity(name)
                self.assertEqual(template["CFBundleIdentifier"], desktop_identifier)
                self.assertEqual(template["CFBundleExecutable"], desktop_name)
                self.assertEqual(
                    companion_packaging.companion(name).desktop_identifier, desktop_identifier
                )
        self.assertEqual(package_linux.DIRECTORIES["preflight"], "spaceterm-preflight")
        self.assertEqual(
            package_linux.archive_path("preflight").name, "spaceterm-preflight-linux-x86_64.tar.gz"
        )

    def test_bundle_template_is_an_agent_with_only_its_identity(self):
        info = plistlib.loads(companion_packaging.info_plist("preflight", "0.0.0"))
        self.assertEqual(
            info["CFBundleIdentifier"], "io.github.sadiksaifi.spaceterm-preflight.status"
        )
        self.assertEqual(info["CFBundleName"], "SpaceTerm Preflight Status")
        self.assertEqual(info["CFBundleExecutable"], "SpaceTerm Preflight Status")
        self.assertEqual(
            (info["CFBundleShortVersionString"], info["CFBundleVersion"]), ("0.0.0",) * 2
        )
        self.assertIs(info["LSUIElement"], True)
        self.assertEqual(info["CFBundleIconName"], ICON_NAME)
        self.assertEqual(info["LSMinimumSystemVersion"], package_macos.MINIMUM_MACOS)
        # No updater, background-only mode, or privileged helper registration.
        self.assertFalse(any(key.startswith(("SU", "SM")) for key in info))
        self.assertNotIn("LSBackgroundOnly", info)
        for version in ("", "1.2", "01.2.3", "1.2.3-beta", "1.2.3\n"):
            with self.subTest(version=version), self.assertRaises(TaskError):
                companion_packaging.info_plist("preflight", version)

    def test_desktop_entry_is_explicitly_launched_and_never_autostarts(self):
        entry = companion_packaging.desktop_entry("spaceterm")
        lines = entry.splitlines()
        self.assertEqual(lines[0], "[Desktop Entry]")
        self.assertIn("Name=SpaceTerm Status", lines)
        self.assertIn("Exec=spaceterm-status", lines)
        self.assertIn("Icon=io.github.sadiksaifi.spaceterm", lines)
        self.assertIn("StartupNotify=false", lines)
        self.assertNotIn("@", entry)
        for key in ("Autostart", "Hidden=", "OnlyShowIn", "DBusActivatable", "NoDisplay"):
            self.assertNotIn(key, entry)


class LinuxCompanionTests(CompanionFixture):
    def setUp(self):
        super().setUp()
        fake_executable(self.root / "desktop", "SpaceTerm Preflight 0.0.0")

    def test_linux_archive_includes_the_companion_without_changing_its_asset_name(self):
        tree = self.staged_tree("preflight")
        desktop = snapshot(tree)
        companion_packaging.stage_linux(
            tree, self.companion_executable("preflight", "0.0.0"), "preflight"
        )
        staged = snapshot(tree)
        self.assertEqual({key: staged[key] for key in desktop}, desktop)
        archive = self.root / package_linux.archive_path("preflight").name
        package_linux.write_archive(tree, archive)
        self.assertEqual(archive.name, "spaceterm-preflight-linux-x86_64.tar.gz")
        with tarfile.open(archive) as source:
            members = {member.name: member for member in source.getmembers()}
        executable = members["spaceterm-preflight/bin/spaceterm-status"]
        self.assertTrue(executable.isfile())
        self.assertEqual(executable.mode, 0o755)
        self.assertIn("spaceterm-preflight/bin/spaceterm", members)
        applications = [
            name for name in members if name.startswith("spaceterm-preflight/share/applications/")
        ]
        self.assertEqual(
            applications,
            [
                "spaceterm-preflight/share/applications/io.github.sadiksaifi.spaceterm-preflight.desktop"
            ],
        )
        entry = (
            "spaceterm-preflight/share/spaceterm/companion/"
            "io.github.sadiksaifi.spaceterm-preflight.status.desktop"
        )
        self.assertEqual(members[entry].mode, 0o644)

    def test_linux_verification_accepts_the_companion_and_rejects_drift(self):
        companion = self.companion_executable("preflight", "0.0.0")

        def staged():
            shutil.rmtree(self.root / "spaceterm-preflight", ignore_errors=True)
            tree = self.staged_tree("preflight")
            companion_packaging.stage_linux(tree, companion, "preflight")
            return tree

        companion_packaging.verify_companion(staged(), "preflight")
        self.assertIn(["desktop-file-validate"], [call[:1] for call in self.tools.calls])
        entry = "share/spaceterm/companion/io.github.sadiksaifi.spaceterm-preflight.status.desktop"
        drifts = {
            "missing companion": lambda tree: (tree / "bin/spaceterm-status").unlink(),
            "other version": lambda tree: fake_executable(
                tree / "bin/spaceterm-status", "SpaceTerm Preflight Status 9.9.9"
            ),
            "desktop name": lambda tree: fake_executable(
                tree / "bin/spaceterm-status", "SpaceTerm Preflight 0.0.0"
            ),
            "second menu entry": lambda tree: shutil.copyfile(
                tree / entry,
                tree / "share/applications/io.github.sadiksaifi.spaceterm-preflight.status.desktop",
            ),
            "edited entry": lambda tree: (tree / entry).write_text("[Desktop Entry]\n"),
            "autostart": lambda tree: (
                (tree / "etc/xdg/autostart").mkdir(parents=True),
                shutil.copyfile(tree / entry, tree / "etc/xdg/autostart/status.desktop"),
            ),
            "user service": lambda tree: (
                (tree / "share/spaceterm/units").mkdir(),
                (tree / "share/spaceterm/units/spaceterm-status.service").write_text(""),
            ),
            "extra executable": lambda tree: fake_executable(tree / "bin/helper", "helper 1"),
        }
        for drift, change in drifts.items():
            with self.subTest(drift=drift):
                tree = staged()
                change(tree)
                with self.assertRaises(TaskError) as raised:
                    companion_packaging.verify_companion(tree, "preflight")
                self.assertNotIn(str(self.root), str(raised.exception))

    def test_release_companion_meets_the_glibc_baseline(self):
        fake_executable(self.root / "desktop", "SpaceTerm 1.2.3")
        tree = self.staged_tree("spaceterm")
        companion_packaging.stage_linux(
            tree, self.companion_executable("spaceterm", "1.2.3"), "spaceterm"
        )
        with patch.object(companion_packaging, "verify_glibc_baseline") as baseline:
            companion_packaging.verify_companion(tree, "spaceterm")
        baseline.assert_called_once_with(tree / "bin/spaceterm-status", "companion")

    def test_staging_never_replaces_an_existing_executable(self):
        tree = self.staged_tree("preflight")
        companion = self.companion_executable("preflight", "0.0.0")
        companion_packaging.stage_linux(tree, companion, "preflight")
        with self.assertRaises(TaskError):
            companion_packaging.stage_linux(tree, companion, "preflight")
        shutil.rmtree(tree)
        tree.mkdir()
        with self.assertRaises(TaskError):
            companion_packaging.stage_linux(tree, companion, "preflight")


class MacosCompanionTests(CompanionFixture):
    def test_nested_bundle_is_staged_and_signed_before_the_outer_bundle(self):
        _, app, dmg = self.finalize()
        companion = companion_packaging.companion("preflight")
        nested = app / "Contents/Helpers" / companion.bundle
        signs = [call for call in self.tools.calls if call[:2] == ["codesign", "--force"]]
        self.assertEqual(
            [Path(call[-1]).name for call in signs], [companion.bundle, app.name, dmg.name]
        )
        self.assertEqual(
            signs[0],
            [
                "codesign",
                "--force",
                "--sign",
                "-",
                "--options",
                "runtime",
                "--timestamp",
                str(nested),
            ],
        )
        entitlements = ROOT / "packaging/macos/preflight/Entitlements.plist"
        self.assertEqual(
            signs[1],
            [
                "codesign",
                "--force",
                "--sign",
                "-",
                "--entitlements",
                str(entitlements),
                "--options",
                "runtime",
                "--timestamp",
                str(app),
            ],
        )
        self.assertEqual(self.tools.helpers_when_signed[app.name], [companion.bundle])
        self.assertLess(
            self.tools.calls.index(signs[1]),
            [call[:2] for call in self.tools.calls].index(["hdiutil", "create"]),
        )
        executable = nested / "Contents/MacOS" / companion.name
        self.assertTrue(os.access(executable, os.X_OK))
        self.assertEqual(
            (nested / "Contents/Resources" / f"{ICON_NAME}.icns").read_bytes(),
            f"{ICON_NAME}.icns".encode(),
        )
        self.assertEqual(
            plistlib.loads((nested / "Contents/Info.plist").read_bytes()),
            plistlib.loads(companion_packaging.info_plist("preflight", "0.0.0")),
        )

    def test_disk_image_is_rebuilt_from_the_signed_bundle(self):
        output, app, dmg = self.finalize()
        companion = companion_packaging.companion("preflight")
        [volume] = self.tools.disk_image_sources
        self.assertEqual(volume["Applications"], ("link", "/Applications"))
        self.assertIn(
            f"{app.name}/Contents/Helpers/{companion.bundle}/Contents/MacOS/{companion.name}",
            volume,
        )
        self.assertIn(".VolumeIcon.icns", volume)
        self.assertEqual(dmg.read_bytes(), b"read-write image")
        self.assertEqual(
            sorted(path.name for path in output.iterdir()), sorted([app.name, dmg.name])
        )
        self.assertIn(["hdiutil", "detach", "-quiet"], [call[:3] for call in self.tools.calls])

    def test_finalizing_keeps_the_desktop_identity_and_designated_requirement_inputs(self):
        output, app, _ = self.packaged_app("preflight", "0.0.0")
        info = (app / "Contents/Info.plist").read_bytes()
        executables = sorted(path.name for path in (app / "Contents/MacOS").iterdir())
        shutil.rmtree(output)
        output, app, _ = self.finalize()
        self.assertEqual((app / "Contents/Info.plist").read_bytes(), info)
        self.assertEqual(
            sorted(path.name for path in (app / "Contents/MacOS").iterdir()), executables
        )
        for call in self.tools.calls:
            self.assertNotIn("--identifier", call)
            self.assertNotIn("--deep", call)
            self.assertNotIn("--requirements", call)

    def test_release_bundles_are_signed_with_the_pinned_release_certificate(self):
        fingerprint = tomllib.loads(
            (identity_directory("spaceterm") / "Packager.toml").read_text()
        )["macos"]["signing-identity"]
        _, app, _ = self.finalize("spaceterm", "1.2.3", "v1.2.3")
        signers = {call[call.index("--sign") + 1] for call in self.tools.calls if "--sign" in call}
        self.assertEqual(signers, {fingerprint})
        self.assertTrue((app / "Contents/Helpers/SpaceTerm Status.app").is_dir())

    def test_finalizing_rejects_a_signer_that_differs_from_the_identity(self):
        for name, version, release_tag in (
            ("spaceterm", "0.0.0", None),
            ("preflight", "1.2.3", "v1.2.3"),
        ):
            with self.subTest(identity=name):
                shutil.rmtree(self.root / "output", ignore_errors=True)
                with self.assertRaises(TaskError):
                    self.finalize(name, version, release_tag)
                self.assertEqual(self.tools.calls, [])

    def test_macos_verification_accepts_the_companion_and_rejects_drift(self):
        companion = companion_packaging.companion("preflight")

        def finalized():
            shutil.rmtree(self.root / "output", ignore_errors=True)
            return self.finalize()[1]

        companion_packaging.verify_companion(finalized(), "preflight")
        nested = Path("Contents/Helpers") / companion.bundle

        def edit_info(app, **changes):
            path = app / nested / "Contents/Info.plist"
            info = plistlib.loads(path.read_bytes())
            info.update(changes)
            path.write_bytes(plistlib.dumps(info))

        drifts = {
            "not an agent": lambda app: edit_info(app, LSUIElement=False),
            "desktop identifier": lambda app: edit_info(
                app, CFBundleIdentifier="io.github.sadiksaifi.spaceterm-preflight"
            ),
            "second helper": lambda app: (app / "Contents/Helpers/Other.app").mkdir(),
            "login item": lambda app: (app / "Contents/Library/LoginItems").mkdir(parents=True),
            "launch agent": lambda app: (app / nested / "Contents/Library/LaunchAgents").mkdir(
                parents=True
            ),
            "other version": lambda app: fake_executable(
                app / nested / "Contents/MacOS" / companion.name,
                "SpaceTerm Preflight Status 9.9.9",
            ),
            "missing companion": lambda app: shutil.rmtree(app / nested),
        }
        for drift, change in drifts.items():
            with self.subTest(drift=drift):
                app = finalized()
                change(app)
                with self.assertRaises(TaskError) as raised:
                    companion_packaging.verify_companion(app, "preflight")
                self.assertNotIn(str(self.root), str(raised.exception))
        self.tools.signature = b"Signature=adhoc\nCodeDirectory flags=0x2(adhoc)\n"
        with self.assertRaises(TaskError):
            companion_packaging.verify_companion(finalized(), "preflight")

    def test_release_verification_requires_the_pinned_signer_and_companion_identifier(self):
        fingerprint = tomllib.loads(
            (identity_directory("spaceterm") / "Packager.toml").read_text()
        )["macos"]["signing-identity"]
        self.tools.signature = b"Authority=SpaceTerm\nCodeDirectory flags=0x10000(runtime)\n"
        _, app, _ = self.finalize("spaceterm", "1.2.3", "v1.2.3")
        with patch.object(companion_packaging.macos_signing, "verify_release_signature") as verify:
            companion_packaging.verify_companion(app, "spaceterm")
        verify.assert_called_once_with(
            app / "Contents/Helpers/SpaceTerm Status.app",
            fingerprint,
            "io.github.sadiksaifi.spaceterm.status",
            "companion",
        )


class CompanionBoundaryTests(CompanionFixture):
    def test_headless_artifacts_contain_no_companion(self):
        built = []

        def build(*cargo_args, binary="spaceterm", env=None):
            built.append((binary, cargo_args))
            return Path("/nonexistent") / binary

        with patch.object(package_server, "build_executable", build):
            package_server.build()
        self.assertEqual(
            {binary for binary, _ in built},
            {"spaceterm-server"},
        )
        for _, cargo_args in built:
            self.assertNotIn(companion_packaging.FEATURE, cargo_args)
            self.assertNotIn("spaceterm", cargo_args)
        for platform in artifacts.PLATFORMS:
            for build_identity in artifacts.BUILDS:
                for path in artifacts.entry_points(platform, build_identity).values():
                    self.assertNotIn("status", path or "")

    def test_companion_build_selects_only_its_feature_gated_binary(self):
        with patch.object(
            companion_packaging, "build_executable", return_value=Path("/built")
        ) as build:
            built = companion_packaging.build_companion(
                "--release", "--locked", "--no-default-features", env={"SPACETERM_PACKAGED": "1"}
            )
        self.assertEqual(built, Path("/built"))
        build.assert_called_once_with(
            "--release",
            "--locked",
            "--no-default-features",
            "-p",
            "spaceterm",
            "--features",
            "status-companion",
            binary="spaceterm-status",
            env={"SPACETERM_PACKAGED": "1"},
        )

    def test_client_replacement_cannot_replace_the_approved_server_installation(self):
        server = self.root / "approved" / "spaceterm-server"
        for name in artifacts.entry_points("linux-x86_64", "release").values():
            if name:
                fake_executable(server / name, "SpaceTerm Server 1.2.3")
        (server / "installation.json").write_text("{}")
        approved = snapshot(server.parent)
        fake_executable(self.root / "desktop", "SpaceTerm Preflight 0.0.0")
        tree = self.staged_tree("preflight")
        companion_packaging.stage_linux(
            tree, self.companion_executable("preflight", "0.0.0"), "preflight"
        )
        _, app, _ = self.finalize()
        self.assertEqual(snapshot(server.parent), approved)
        server_names = {
            Path(name).name
            for platform in artifacts.PLATFORMS
            for build in artifacts.BUILDS
            for name in artifacts.entry_points(platform, build).values()
            if name
        } | {artifacts.bundle_name(build) for build in artifacts.BUILDS}
        for package in (tree, app):
            self.assertFalse(server_names & {path.name for path in package.rglob("*")})
        fake_executable(tree / "bin/spaceterm-server", "SpaceTerm Server 1.2.3")
        with self.assertRaises(TaskError):
            companion_packaging.verify_companion(tree, "preflight")
        (app / "Contents/Helpers/SpaceTerm Preflight Server.app").mkdir()
        with self.assertRaises(TaskError):
            companion_packaging.verify_companion(app, "preflight")

    def test_packaging_and_launch_register_no_login_item_or_supervision(self):
        home = self.root / "home"
        home.mkdir()
        with patch.dict(os.environ, {"HOME": str(home)}):
            fake_executable(self.root / "desktop", "SpaceTerm Preflight 0.0.0")
            tree = self.staged_tree("preflight")
            companion_packaging.stage_linux(
                tree, self.companion_executable("preflight", "0.0.0"), "preflight"
            )
            companion_packaging.verify_companion(tree, "preflight")
            _, app, _ = self.finalize()
            companion_packaging.verify_companion(app, "preflight")
        self.assertEqual(list(home.iterdir()), [])
        self.assertFalse(self.tools.programs() & REGISTRATION_PROGRAMS)
        for package in (tree, app):
            names = {path.name.lower() for path in package.rglob("*")}
            self.assertFalse(names & {"loginitems", "launchagents", "launchdaemons", "autostart"})
        # The companion itself never reaches a login-item, launch-agent, or autostart API.
        sources = [
            ROOT / "src/bin/spaceterm-status.rs",
            *sorted((ROOT / "src/status_companion").glob("*.rs")),
        ]
        self.assertGreater(len(sources), 2)
        for source in sources:
            text = source.read_text()
            for api in (
                "SMAppService",
                "SMLoginItemSetEnabled",
                "LSSharedFileList",
                "ServiceManagement",
                "LaunchAgents",
                "launchctl",
                "systemctl",
                "autostart",
                "Autostart",
            ):
                with self.subTest(source=source.name, api=api):
                    self.assertNotIn(api, text)

    def test_hooks_match_the_frozen_packager_declarations(self):
        signatures = {
            "finalize_macos_package": [
                "output",
                "executable",
                "identity",
                "version",
                "config",
                "environment",
                "release_tag",
            ],
            "stage_linux": ["tree", "executable", "identity"],
            "stage_macos": ["stage", "executable", "identity", "version"],
            "verify_companion": ["package", "identity"],
        }
        for packager in (package_macos, package_linux):
            module = importlib.import_module(packager.COMPANION_EXTENSION_MODULE)
            for hook in packager.COMPANION_HOOK_DECLARATIONS:
                with self.subTest(packager=packager.__name__, hook=hook):
                    self.assertEqual(
                        list(inspect.signature(getattr(module, hook)).parameters),
                        signatures[hook],
                    )
        self.assertEqual(
            list(inspect.signature(companion_packaging.stage_macos).parameters),
            signatures["stage_macos"],
        )
