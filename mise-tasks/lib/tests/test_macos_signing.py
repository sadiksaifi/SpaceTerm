"""Exercise native signing imports, rejection, cleanup, signing order and cross-build identity."""

import os
import plistlib
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import TaskError, macos_codesign
from spaceterm_tasks.macos_signing import release_keychain, signing_environment
from spaceterm_tasks.update_signing import DRY_RUN

from tests.macos_signing_fixture import NAME, TEAM, certificate


def setUpModule():
    # These tests sign as a release does. A caller's dry run must not change that.
    patcher = patch.dict(os.environ)
    patcher.start()
    unittest.addModuleCleanup(patcher.stop)
    os.environ.pop(DRY_RUN, None)


CORE_ENTITLEMENTS = plistlib.loads(macos_codesign.CORE_ENTITLEMENTS.read_bytes())


def compile_executable(path, source=b"int main(){return 0;}"):
    path.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["xcrun", "clang", "-x", "c", "-", "-o", str(path)],
        input=source,
        env=signing_environment(os.environ),
        check=True,
        capture_output=True,
    )


def fixture_app(root, identifier, version=1):
    app = root / "Fixture.app"
    contents = app / "Contents"
    (contents / "Info.plist").parent.mkdir(parents=True)
    (contents / "Info.plist").write_bytes(
        plistlib.dumps(
            {
                "CFBundleIdentifier": identifier,
                "CFBundleExecutable": "Fixture",
                "CFBundleVersion": str(version),
                "CFBundlePackageType": "APPL",
            }
        )
    )
    compile_executable(contents / "MacOS/Fixture", f"int main(){{return {version};}}".encode())
    return app


class CoreEntitlementTests(unittest.TestCase):
    def test_core_grants_each_hardened_runtime_resource_and_describes_it(self):
        self.assertEqual(
            CORE_ENTITLEMENTS,
            {
                f"com.apple.security.{name}": True
                for name in (
                    "automation.apple-events",
                    "device.audio-input",
                    "device.camera",
                    "personal-information.addressbook",
                    "personal-information.calendars",
                    "personal-information.location",
                    "personal-information.photos-library",
                )
            },
        )
        info = plistlib.loads((macos_codesign.CORE_ENTITLEMENTS.parent / "Info.plist").read_bytes())
        for key in (
            "NSAppleEventsUsageDescription",
            "NSMicrophoneUsageDescription",
            "NSCameraUsageDescription",
            "NSContactsUsageDescription",
            "NSCalendarsFullAccessUsageDescription",
            "NSCalendarsWriteOnlyAccessUsageDescription",
            "NSLocationUsageDescription",
            "NSPhotoLibraryUsageDescription",
            "NSLocalNetworkUsageDescription",
        ):
            with self.subTest(key=key):
                self.assertTrue(info[key].startswith("SpaceTerm Core "))


class SigningArgumentTests(unittest.TestCase):
    def signed(self, identity, environment=None, **keywords):
        # An explicit environment, so the caller's release mode never reaches the signer.
        with patch.object(macos_codesign, "checked") as checked:
            macos_codesign.sign(
                Path("Fixture.app"), identity, environment=environment or {}, **keywords
            )
        return checked.call_args.args[0]

    def test_certificate_signatures_carry_a_secure_timestamp_and_the_team_requirement(self):
        arguments = self.signed(NAME, identifier="io.example", team=TEAM)
        self.assertIn("--timestamp", arguments)
        self.assertNotIn("--timestamp=none", arguments)
        self.assertEqual(arguments[arguments.index("--options") + 1], "runtime")
        self.assertEqual(
            arguments[arguments.index("--requirements") + 1],
            "=" + macos_codesign.designated_requirement("io.example", TEAM),
        )

    def test_adhoc_signatures_have_no_timestamp_and_no_team_requirement(self):
        arguments = self.signed("-", identifier="io.example", team=TEAM)
        self.assertIn("--timestamp=none", arguments)
        self.assertNotIn("--timestamp", arguments)
        self.assertNotIn("--requirements", arguments)
        self.assertEqual(arguments[arguments.index("--options") + 1], "runtime")

    def test_a_dry_run_signs_without_the_hardened_runtime(self):
        # Library validation would refuse the ad hoc app's Sparkle.framework under the runtime.
        arguments = self.signed("-", {DRY_RUN: "1"}, identifier="io.example", team=TEAM)
        self.assertNotIn("--options", arguments)
        self.assertNotIn("runtime", arguments)


class IdentityTests(unittest.TestCase):
    def test_release_identity_names_the_teams_developer_id_certificate(self):
        self.assertEqual(
            macos_codesign.signing_identity(
                {macos_codesign.TEAM_ID: TEAM, macos_codesign.SIGNING_IDENTITY: NAME}
            ),
            NAME,
        )
        self.assertEqual(
            macos_codesign.signing_identity(
                {macos_codesign.TEAM_ID: TEAM, macos_codesign.SIGNING_IDENTITY: "-"}
            ),
            "-",
        )
        for team, identity in (
            ("", NAME),
            ("abcde12345", NAME),
            ("ABCDE1234", NAME),
            (TEAM, ""),
            (TEAM, "Developer ID Application: SpaceTerm Test (ZZZZZ99999)"),
            (TEAM, f"Apple Development: SpaceTerm Test ({TEAM})"),
            (TEAM, "9E07CD5C15C5C862A21B93EA55D4303BC19B7366"),
        ):
            with self.subTest(team=team, identity=identity), self.assertRaises(TaskError):
                macos_codesign.signing_identity(
                    {macos_codesign.TEAM_ID: team, macos_codesign.SIGNING_IDENTITY: identity}
                )

    @unittest.skipUnless(sys.platform == "darwin", "requires macOS csreq")
    def test_team_requirement_is_the_developer_id_requirement(self):
        self.assertEqual(
            macos_codesign.canonical_requirement(
                macos_codesign.designated_requirement("com.peblon.spaceterm", TEAM)
            ),
            'designated => identifier "com.peblon.spaceterm" and anchor apple generic'
            " and certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */"
            " and certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */"
            f" and certificate leaf[subject.OU] = {TEAM}",
        )


@unittest.skipUnless(sys.platform == "darwin", "requires macOS codesign")
class AdhocSigningTests(unittest.TestCase):
    def test_core_signature_checks_identity_runtime_entitlements_and_tampering(self):
        with tempfile.TemporaryDirectory() as temporary:
            executable = Path(temporary) / "core"
            compile_executable(executable)
            identifier = "com.peblon.spaceterm-preflight.core"
            macos_codesign.sign_core(executable, "-", identifier)
            macos_codesign.verify(executable, identifier, CORE_ENTITLEMENTS, label="core")
            # A dry run signs ad hoc and still compiles the team's requirement.
            macos_codesign.verify(executable, identifier, CORE_ENTITLEMENTS, team=TEAM)
            for arguments, keywords in (
                (("com.peblon.spaceterm.core", CORE_ENTITLEMENTS), {}),
                ((identifier, {}), {}),
                ((identifier, CORE_ENTITLEMENTS), {"team": TEAM, "identity": NAME}),
            ):
                with self.subTest(arguments=arguments), self.assertRaises(TaskError):
                    macos_codesign.verify(executable, *arguments, **keywords)
            subprocess.run(
                ["codesign", "--force", "--sign", "-", "--identifier", identifier, str(executable)],
                check=True,
                capture_output=True,
            )
            with self.assertRaises(TaskError):
                macos_codesign.verify(executable, identifier, {}, label="core without runtime")
            macos_codesign.sign_core(executable, "-", identifier)
            with executable.open("r+b") as output:
                output.write(b"tampered")
            with self.assertRaises(TaskError):
                macos_codesign.verify(executable, identifier, CORE_ENTITLEMENTS)

    def test_desktop_signs_sparkle_inside_out_without_its_xpc_services(self):
        from spaceterm_tasks import sparkle

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            identifier = "com.peblon.spaceterm.signing-test"
            app = fixture_app(root, identifier)
            frameworks = app / "Contents/Frameworks"
            frameworks.mkdir()
            subprocess.run(
                [
                    "ditto",
                    str(sparkle.directory() / "Sparkle.framework"),
                    str(frameworks / "Sparkle.framework"),
                ],
                check=True,
            )
            entitlements = root / "Entitlements.plist"
            entitlements.write_bytes(plistlib.dumps({}))
            macos_codesign.sign_desktop(app, "-", entitlements)
            framework = frameworks / "Sparkle.framework"
            self.assertFalse((framework / "XPCServices").exists())
            self.assertFalse((framework / "Versions/B/XPCServices").exists())
            macos_codesign.verify(app, identifier, {}, label="app")
            for code in ("Versions/B/Autoupdate", "Versions/B/Updater.app", ""):
                # Sparkle's own Autoupdate entitlement needs a provisioning profile; it is gone.
                path = framework / code
                macos_codesign.verify(path, macos_codesign.signing_identifier(path), {})
            (frameworks / "Other.framework").mkdir()
            with self.assertRaises(TaskError):
                macos_codesign.sign_desktop(app, "-", entitlements)


@unittest.skipUnless(sys.platform == "darwin", "requires macOS Keychain and codesign")
class ReleaseSigningTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        temporary = tempfile.TemporaryDirectory(prefix="spaceterm-signing-test-")
        cls.addClassCleanup(temporary.cleanup)
        cls.root = Path(temporary.name)
        cls.fingerprint, cls.credentials = certificate(cls.root / "identity")

    def run_command(self, *arguments, **kwargs):
        return subprocess.run(arguments, check=True, capture_output=True, **kwargs).stdout

    def search_list(self):
        return self.run_command("security", "list-keychains", "-d", "user")

    def test_invalid_credentials_fail_without_leaving_a_keychain(self):
        original = self.search_list()
        for credentials, identity in (
            ({}, NAME),
            ({**self.credentials, "MACOS_SIGNING_CERTIFICATE_P12": "not-base64"}, NAME),
            ({**self.credentials, "MACOS_SIGNING_CERTIFICATE_PASSWORD": "incorrect"}, NAME),
            (self.credentials, "Developer ID Application: Someone Else (ABCDE12345)"),
        ):
            with self.subTest(identity=identity), self.assertRaises(TaskError):
                with release_keychain(identity, {**os.environ, **credentials}):
                    self.fail("invalid signing inputs must not enter the packaging operation")
            self.assertEqual(self.search_list(), original)
        _, credentials = certificate(self.root / "public-only", private=False)
        with self.assertRaises(TaskError):
            with release_keychain(NAME, {**os.environ, **credentials}):
                self.fail("a certificate without its private key must be rejected")
        self.assertEqual(self.search_list(), original)

    def test_designated_requirement_survives_rebuilds_and_cleanup_runs_on_interruption(self):
        original = self.search_list()
        environment = {**os.environ, **self.credentials}
        clean = signing_environment(environment)
        self.assertNotIn("MACOS_SIGNING_CERTIFICATE_P12", clean)
        self.assertNotIn("MACOS_SIGNING_CERTIFICATE_PASSWORD", clean)
        identifier = "com.peblon.spaceterm.signing-test"
        expected = macos_codesign.canonical_requirement(
            macos_codesign.designated_requirement(identifier, TEAM)
        )
        entitlements = self.root / "Entitlements.plist"
        entitlements.write_bytes(plistlib.dumps({}))
        requirements = []
        hashes = []
        with self.assertRaises(KeyboardInterrupt):
            with release_keychain(NAME, environment):
                imported = self.search_list()
                self.assertNotEqual(imported, original)
                for version in (1, 2):
                    app = fixture_app(self.root / f"build-{version}", identifier, version)
                    macos_codesign.sign_desktop(
                        app, NAME, entitlements, team=TEAM, environment=clean
                    )
                    requirements.append(
                        self.run_command(
                            "codesign", "--display", "--requirements", "-", str(app), text=True
                        ).strip()
                    )
                    details = macos_codesign.details(app)
                    hashes.append(
                        next(line for line in details.splitlines() if line.startswith("CDHash="))
                    )
                # Keychain items, privacy grants and Login Items record this requirement, and
                # every later build from the team satisfies it.
                self.assertEqual(requirements, [expected, expected])
                self.assertNotEqual(hashes[0], hashes[1])
                # A self-signed certificate has no Team ID and no Apple chain.
                with self.assertRaises(TaskError):
                    macos_codesign.verify(app, identifier, {}, team=TEAM, identity=NAME)
                raise KeyboardInterrupt()
        self.assertEqual(self.search_list(), original)
        for path in imported.decode().splitlines():
            if "spaceterm-release-signing-" in path:
                self.assertFalse(Path(path.strip().strip('"')).exists())
